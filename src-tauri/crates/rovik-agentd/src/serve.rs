//! `rovik-agentd serve`: start-up in the order of section 5.2, then serving
//! until SIGTERM or SIGINT. Then the runs pause at their next safe point,
//! for the next start to continue them (R02), while the clients still hear
//! of it, and the core shuts down.

use crate::auth::Tokens;
use crate::cli::{Options, VERSION};
use crate::config::Config;
use crate::daemon::Daemon;
use crate::events::Events;
use crate::gc;
use crate::home::{self, Home};
use crate::host;
use crate::installer;
use crate::limits::{AdminFile, Caps, ADMIN_FILE};
use crate::logfile;
use crate::network;
use crate::packs::Projects;
use crate::pages;
use crate::restart;
use crate::results::{self, Results};
use crate::server::{self, OwnUser};
use crate::store::Store;
use crate::work;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::{self, candidates, recovery};
use rovik_core::chat_db::store::now_ms;
use rovik_core::{process, rt, workload, Core, EventSink, Paths};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long a stopping daemon waits for its runs to pause (R02): a
/// command at its default limit (2 minutes) may still be cut. systemd
/// gives the unit 90 s (`TimeoutStopSec`), then kills what's left.
pub const DRAIN_WAIT: Duration = Duration::from_secs(80);

pub fn serve(options: Options) -> Result<(), String> {
    // 1. While the process is still single-threaded: it sets the
    //    environment the agent's commands inherit.
    // SAFETY: umask can't fail.
    unsafe { libc::umask(0o077) };
    login_shell();
    process::adopt_login_shell_path();

    // 2. The folders, which must be this user's alone, and the lock.
    let home = Home::locate(options.data)?;
    home.prepare()?;
    // The daemon's own folder (keys.json among it) is hidden from every
    // command the agent runs, whatever $HOME says (overlay/confine.rs).
    rovik_core::overlay::confine::protect([home.root.clone()]);
    let _lock = home.lock()?;
    let config = Config::read(&home.config_file())?;
    logfile::init(&home.logs(), config.level()?)?;

    // 3. The agent's temporary files, out of a shared /tmp (K13).
    std::env::set_var("TMPDIR", home.tmp());

    // 4. The computers' token hashes, read only here; and on a team
    //    server, the administrators' limits (5.5).
    let tokens = Tokens::new(&config.tokens)?;
    let admin_file = AdminFile::read(Path::new(ADMIN_FILE))?;
    let installed = admin_file.is_some();
    let admin = admin_file.unwrap_or_default();
    let limits = Limits {
        // Each folder once: under `--data DIR` they're one, and a folder
        // counted twice would trip the disk cap at half.
        caps: Caps::new(
            &admin,
            home.folders().iter().map(|f| f.to_path_buf()).collect(),
        ),
        admin,
        user_slots: config.max_slots,
        installed,
    };

    // 5. The runtime and the core.
    let socket = options.socket.unwrap_or_else(|| home.socket());
    let result = rt::block_on(run(
        home,
        socket,
        tokens,
        limits,
        options.same_machine,
        options.no_virtual_files,
    ));
    if let Err(e) = &result {
        log::error!("{e}");
    }
    result
}

/// What a team server's administrators and the user set (5.5).
struct Limits {
    admin: AdminFile,
    caps: Caps,
    /// The slots the user's config.toml asks for.
    user_slots: Option<usize>,
    /// Rovik is installed here for everyone (agentd.toml is there).
    installed: bool,
}

async fn run(
    home: Home,
    socket: PathBuf,
    tokens: Tokens,
    limits: Limits,
    same_machine: bool,
    no_virtual_files: bool,
) -> Result<(), String> {
    // Heard from here on: before, SIGTERM would end the process at once,
    // in the middle of whatever start-up does.
    let mut stop = Stop::listen();
    let store = Arc::new(Store::open(&home.root)?);
    let owners = store.clone();
    let events = Events::new(
        uuid::Uuid::new_v4().to_string(),
        Arc::new(move |chat| owners.owner(chat)),
    );
    let paths = Paths::new(home.data(), home.core_config());
    let results = Results::new(
        home.results(),
        home.core_config(),
        store.clone(),
        events.clone(),
    );
    let sink = Tap {
        store: store.clone(),
        events: events.clone(),
        results: results.clone(),
        notifier: crate::notify::Notifier::new(store.clone()),
    };
    let core = Core::new(paths, Arc::new(sink))?;
    // `ChatChanged` names the database's clock (6.3).
    events.set_db(core.db.clone());
    // A run follows its chat's computer's configuration (D16).
    let configs = store.clone();
    core.agents
        .set_resolver(Arc::new(move |chat| configs.config_of(chat)));
    // A run that leaves conflict markers is reminded of them (6.7).
    let marks = store.clone();
    core.agents
        .set_markers_resolver(Arc::new(move |chat| results::marked_left(&marks, chat)));
    core.agents.set_on_server();
    // A cloud host's shared toolchains: where the versions are, the caches
    // this daemon may write, and how a chat has a missing one installed.
    // Before any run, whose commands get their environment (6).
    if let Some(shared) = shared_toolchains(
        &limits.admin,
        Path::new(installer::TOOLCHAINS),
        Path::new(installer::CACHES),
        &home.bin(),
    ) {
        log::info!(
            "shared toolchains in {}, caches: {}",
            shared.root.display(),
            match &shared.caches {
                Some(caches) => caches.display().to_string(),
                None => "this user's own".into(),
            }
        );
        rovik_core::toolchains::set(shared);
    }
    // A delivered task's result goes to its branch when its computer asked
    // (D15, H04).
    core.agents
        .set_deliverer(Arc::new(crate::deliver::Pushing(results.clone())));
    // The heavy-work slots, again whenever other users' daemons start or
    // stop.
    tokio::spawn(apply_slots(limits.admin, limits.user_slots));
    // On a team server, this user's private network (D20), before any run:
    // every command of theirs starts in it, so their servers' ports are
    // theirs alone. Where it can't be made, they share the server's.
    if network::wanted(limits.installed) {
        let (dir, opener) = (network::dir(&home), network::opener());
        let made = rt::spawn_blocking(move || {
            rovik_core::overlay::confine::network::enable(&dir, &opener)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|made| made);
        match made {
            Ok(()) => log::info!("the agent's commands run in this user's private network"),
            Err(why) => log::error!(
                "no private network: the agent's commands share the server's network with \
                 other users' ({why}); sudo {} admin pasta installs pasta",
                installer::INSTALLED
            ),
        }
    }
    // This user's project services (D21), whose ports are in that network:
    // started by this daemon when a command needs one, or the agent is
    // told why this server can't.
    let services = crate::services::set_up(&home, limits.installed);

    // 6. The virtual files, so CLM chats can use them (K15).
    let virtual_files_problem = if no_virtual_files {
        Some("The daemon runs without virtual files (--no-virtual-files).".to_string())
    } else {
        mount_virtual_files(&core, home.share.join("vfs")).await
    };

    // A stop that gets as far as the end of its drain is a clean one (5.4).
    let stops = store.clone();
    let opener = pages::Opener::new(events.clone());
    let daemon = Arc::new(Daemon {
        core: core.clone(),
        tokens,
        events,
        store,
        projects: Projects::new(home.root.join("projects")),
        worktrees: home.work(),
        tmp: home.tmp(),
        results,
        same_machine,
        virtual_files_problem,
        imports: tokio::sync::Mutex::new(()),
        caps: limits.caps,
        making: tokio::sync::Mutex::new(()),
        pages: opener,
    });
    // The pages this user's commands open, in their network's browser: on
    // the computer each one's chat belongs to (D20).
    if let Some(network) = rovik_core::overlay::confine::network::current() {
        let opener = Arc::new(daemon.pages.clone());
        if let Err(e) = rovik_core::overlay::confine::pages::serve(&network.pages_socket(), opener)
        {
            log::error!("the commands' pages won't open on the user's computer: {e}");
        }
    }

    // 7. What a killed process left: lock files on the repositories and
    //    the result indexes, which would refuse every later write. Then
    //    what deleted and forgotten chats left (R01), the temporary files
    //    a day unused, and the runs the last process left going (5.4).
    //    The cleanup comes again each day.
    //    A stop meanwhile (an upgrade, a reboot) ends start-up where it
    //    is, and the runs started so far pause for the next start.
    // T05: a Thorough task's candidates work in worktrees beside its chat's.
    core.agents
        .set_copies(Arc::new(crate::candidates::Worktrees(Arc::downgrade(
            &daemon,
        ))));
    daemon.projects.clear_stale_locks();
    daemon.results.clear_stale_locks();
    let starting_up = async {
        work::collect(&daemon, now_ms() - gc::KEEP_RELEASED_MS).await;
        gc::at_start(&daemon).await;
        restart::restart_interrupted(&daemon).await;
    };
    tokio::select! {
        () = starting_up => {}
        () = stop.wait() => {
            log::info!("stopping during start-up: the runs pause first, for up to {DRAIN_WAIT:?}");
            let paused = agent_run::drain(&core, DRAIN_WAIT).await;
            log::info!("{paused} paused for the next start");
            if let Some(services) = &services {
                services.stop_all().await;
            }
            core.shutdown().await;
            stops.stopped_cleanly(now_ms());
            return Ok(());
        }
    }
    gc::daily(daemon.clone());

    // 8. The socket, then serving.
    let listener = server::bind(&socket)?;
    log::info!(
        "rovik-agentd {VERSION} listening at {} for {} computers{}",
        socket.display(),
        daemon.tokens.len(),
        if same_machine { ", same machine" } else { "" }
    );
    let listener = OwnUser {
        listener,
        uid: home::uid(),
    };
    let draining = core.clone();
    let served = axum::serve(listener, server::router(daemon))
        .with_graceful_shutdown(async move {
            stop.wait().await;
            log::info!("stopping: the runs pause first, for up to {DRAIN_WAIT:?}");
            let paused = agent_run::drain(&draining, DRAIN_WAIT).await;
            let runs = if paused == 1 { "run" } else { "runs" };
            log::info!("{paused} {runs} paused for the next start");
        })
        .await;
    log::info!("stopping");
    if let Some(services) = &services {
        services.stop_all().await;
    }
    core.shutdown().await;
    stops.stopped_cleanly(now_ms());
    let _ = std::fs::remove_file(&socket);
    served.map_err(|e| e.to_string())
}

/// The shared toolchains this daemon's chats use (rovik_core::toolchains,
/// docs/cloud-hosts.md): the versions under `root`, the caches at
/// `caches` when its settings share them and it may write them, the root
/// command that installs what's missing when its settings allow that, and
/// `bin`, this user's folder of Yarn and pnpm. None where they aren't
/// Rovik's: the user's own computer.
fn shared_toolchains(
    admin: &AdminFile,
    root: &Path,
    caches: &Path,
    bin: &Path,
) -> Option<rovik_core::toolchains::Shared> {
    if !root.is_dir() {
        return None;
    }
    // A daemon outside the `rovik` group, such as one on a host without
    // systemd, can't write them: it keeps the user's own caches.
    let writable = |dir: &Path| can_write(dir);
    let caches = (admin.toolchains.shared_caches
        && writable(&caches.join("npm"))
        && writable(&caches.join("crates")))
    .then(|| caches.to_path_buf());
    let installer = admin
        .toolchains
        .user_install
        .then(|| Arc::new(root_installer(installer::INSTALLED)) as Arc<rovik_core::toolchains::Installer>);
    Some(rovik_core::toolchains::Shared {
        root: root.to_path_buf(),
        caches,
        installer,
        bin: Some(bin.to_path_buf()),
    })
}

/// Whether this process may write into `dir` (`W_OK`, and it's a folder).
fn can_write(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        dir.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// A chat's missing version installed as root, through the sudo rule every
/// user has: `rovik-agentd user toolchain …`, given the words the
/// toolchain's kind, version, components and targets make, and nothing a
/// chat wrote.
fn root_installer(binary: &'static str) -> impl Fn(rovik_core::toolchains::Want) -> Result<(), String> + Send + Sync {
    move |want| {
        let mut command = rovik_core::process::command("sudo");
        command
            .arg("-n")
            .arg(binary)
            .arg("user")
            .arg("toolchain")
            .args(want.words());
        let out = command
            .output()
            .map_err(|e| format!("sudo {binary} user toolchain: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        let lines: Vec<&str> = said
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let tail = &lines[lines.len().saturating_sub(6)..];
        Err(match tail.is_empty() {
            true => format!("sudo {binary} user toolchain exited with {}", out.status),
            false => tail.join("\n"),
        })
    }
}

async fn mount_virtual_files(core: &Core, at: PathBuf) -> Option<String> {
    let status = rt::spawn_blocking(rovik_core::vfs::driver::status)
        .await
        .ok()?;
    if !status.installed {
        let why = status
            .detail
            .unwrap_or_else(|| "FUSE isn't available on this server.".into());
        log::info!("no virtual files: {why}");
        return Some(why);
    }
    let core = core.clone();
    match rt::spawn_blocking(move || core.vfs.try_mount(Some(at))).await {
        Ok(Ok(at)) => {
            log::info!("virtual files mounted at {}", at.display());
            None
        }
        Ok(Err(problem)) => {
            log::warn!("virtual files: {}", problem.message());
            Some(problem.message())
        }
        Err(e) => Some(e.to_string()),
    }
}

/// The heavy-work slots (5.5): the user's number, or one while other users'
/// daemons run here, else the machine's rule; the administrators' cap over
/// all. Other daemons are looked for each minute.
async fn apply_slots(admin: AdminFile, user: Option<usize>) {
    let mut last = None;
    let name = home::passwd().map(|p| p.name).unwrap_or_default();
    loop {
        let name = name.clone();
        let others = rt::spawn_blocking(move || {
            let (cgroups, proc) = (Path::new("/sys/fs/cgroup"), Path::new("/proc"));
            host::other_daemons(cgroups, proc, home::uid(), &name)
        });
        let slots = admin.slots(user, others.await.unwrap_or(0));
        if last != Some(slots) {
            log::info!("heavy-work slots: {slots:?}");
            workload::set_manual_slots(slots.manual);
            workload::set_slot_cap(slots.cap);
            last = Some(slots);
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// SIGTERM (systemd stopping the unit) and SIGINT, heard from the moment
/// it's made.
struct Stop {
    signals: Vec<tokio::signal::unix::Signal>,
}

impl Stop {
    fn listen() -> Stop {
        use tokio::signal::unix::{signal, SignalKind};
        let signals = [SignalKind::terminate(), SignalKind::interrupt()]
            .into_iter()
            .filter_map(|kind| signal(kind).ok())
            .collect();
        Stop { signals }
    }

    /// When one of them comes; never, when none could be listened for.
    async fn wait(&mut self) {
        if self.signals.is_empty() {
            return std::future::pending().await;
        }
        let waits = self
            .signals
            .iter_mut()
            .map(|signal| Box::pin(signal.recv()));
        futures_util::future::select_all(waits).await;
    }
}

/// A service starts without $SHELL when there's no systemd: the agent's
/// commands and the login-shell PATH need it, so it comes from the passwd
/// entry (K12).
fn login_shell() {
    if rovik_core::process::user_shell().is_none() {
        if let Some(shell) = home::passwd().map(|p| p.shell).filter(|s| !s.is_empty()) {
            std::env::set_var("SHELL", shell);
        }
    }
}

/// The core's events: the store hears of runs that start and end, then
/// they go out.
struct Tap {
    store: Arc<Store>,
    events: Arc<Events>,
    results: Arc<Results>,
    /// A task's end and its failed stages, told where its computer asked
    /// (H06).
    notifier: Arc<crate::notify::Notifier>,
}

impl EventSink for Tap {
    fn agent(&self, event: &AgentEvent) {
        match event {
            AgentEvent::RunStarted {
                dialog_id, run_id, ..
            } => self.store.run_started(dialog_id, run_id),
            // A run the daemon's stop paused stays running, for the next
            // start to continue (5.4), and has no result yet.
            AgentEvent::RunEnded { outcome, .. } if drained(outcome) => {}
            AgentEvent::RunEnded {
                dialog_id, outcome, ..
            } => self.store.ended(dialog_id, Some(outcome)),
            _ => {}
        }
        self.events.agent(event);
        self.notifier.agent(event);
        // The run's end goes out first, then its result: not an Autopilot
        // task's own stages', which change nothing that's a result yet (the
        // installs it prepared, the tests it wrote first: S03, S04; its
        // candidates' implementations, which its pick takes from: T05).
        if let AgentEvent::RunEnded {
            dialog_id,
            outcome,
            autopilot,
            stage,
            ..
        } = event
        {
            let candidates = outcome == candidates::CANDIDATES_OUTCOME;
            if !drained(outcome) && !makes_no_result(stage.as_deref()) && !candidates {
                self.results.after_run(dialog_id, outcome, *autopilot);
            }
        }
    }
}

/// An Autopilot stage the IDE runs itself, before the implementation:
/// what it changes comes with the implementation's result (S03).
fn makes_no_result(stage: Option<&str>) -> bool {
    matches!(stage, Some("prepare" | "tests" | "deliver"))
}

/// A run's outcome when the daemon's stop paused it (R02).
fn drained(outcome: &str) -> bool {
    outcome.strip_prefix("paused-") == Some(recovery::DRAIN)
}

#[cfg(test)]
mod tests {
    use super::{makes_no_result, shared_toolchains};
    use crate::limits::AdminFile;

    /// A daemon outside the `rovik` group, as on a host without systemd,
    /// can't write the shared caches: it shares the versions and the root
    /// command, and keeps the user's own caches.
    #[test]
    fn the_shared_caches_are_used_only_where_this_daemon_may_write_them() {
        let dir = rovik_core::testing::temp_dir();
        let toolchains = dir.0.join("toolchains");
        std::fs::create_dir_all(&toolchains).unwrap();
        let caches = dir.0.join("caches");
        let admin = AdminFile::default();
        // Not there yet: the daemon keeps its own.
        assert_eq!(
            shared_toolchains(&admin, &toolchains, &caches, &dir.0.join("bin"))
                .unwrap()
                .caches,
            None
        );
        // Made, and this account writes them.
        for folder in ["npm", "crates"] {
            std::fs::create_dir_all(caches.join(folder)).unwrap();
        }
        let shared = shared_toolchains(&admin, &toolchains, &caches, &dir.0.join("bin")).unwrap();
        assert_eq!(shared.caches, Some(caches.clone()));
        assert!(shared.installer.is_some(), "chats may install one");

        // Turned off in agentd.toml: the same folders, not shared.
        let mut off = admin.clone();
        off.toolchains.shared_caches = false;
        assert_eq!(
            shared_toolchains(&off, &toolchains, &caches, &dir.0.join("bin"))
                .unwrap()
                .caches,
            None
        );
        off.toolchains.user_install = false;
        assert!(
            shared_toolchains(&off, &toolchains, &caches, &dir.0.join("bin"))
                .unwrap()
                .installer
                .is_none()
        );

        // No shared versions on this host at all: nothing to set.
        assert!(shared_toolchains(
            &admin,
            &dir.0.join("elsewhere"),
            &caches,
            &dir.0.join("bin")
        )
        .is_none());

        // A daemon that can't write them (another user's folder) keeps the
        // user's own caches, and still shares the versions.
        let theirs = dir.0.join("theirs");
        for folder in ["npm", "crates"] {
            std::fs::create_dir_all(theirs.join(folder)).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Root writes anything, and the test runners that are root
            // skip this part.
            for folder in ["npm", "crates"] {
                std::fs::set_permissions(
                    theirs.join(folder),
                    std::fs::Permissions::from_mode(0o500),
                )
                .unwrap();
            }
            let shared =
                shared_toolchains(&admin, &toolchains, &theirs, &dir.0.join("bin")).unwrap();
            assert_eq!(shared.caches, None);
        }
    }

    /// S03: the IDE's own stages of an Autopilot task bring no result; the
    /// runs that change the project do, as every other run does.
    #[test]
    fn only_the_ides_own_stages_bring_no_result() {
        assert!(makes_no_result(Some("prepare")) && makes_no_result(Some("tests")));
        assert!(makes_no_result(Some("deliver")));
        for stage in [None, Some("plan"), Some("implement"), Some("later")] {
            assert!(!makes_no_result(stage), "{stage:?}");
        }
    }
}

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
use crate::limits::{AdminFile, Caps, ADMIN_FILE};
use crate::logfile;
use crate::packs::Projects;
use crate::restart;
use crate::results::{self, Results};
use crate::server::{self, OwnUser};
use crate::store::Store;
use crate::work;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::{self, recovery};
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
    let _lock = home.lock()?;
    let config = Config::read(&home.config_file())?;
    logfile::init(&home.logs(), config.level()?)?;

    // 3. The agent's temporary files, out of a shared /tmp (K13).
    std::env::set_var("TMPDIR", home.tmp());

    // 4. The computers' token hashes, read only here; and on a team
    //    server, the administrators' limits (5.5).
    let tokens = Tokens::new(&config.tokens)?;
    let admin = AdminFile::read(Path::new(ADMIN_FILE))?.unwrap_or_default();
    let limits = Limits {
        caps: Caps::new(&admin, vec![home.root.clone(), home.share.clone()]),
        admin,
        user_slots: config.max_slots,
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
    // The heavy-work slots, again whenever other users' daemons start or
    // stop.
    tokio::spawn(apply_slots(limits.admin, limits.user_slots));

    // 6. The virtual files, so CLM chats can use them (K15).
    let virtual_files_problem = if no_virtual_files {
        Some("The daemon runs without virtual files (--no-virtual-files).".to_string())
    } else {
        mount_virtual_files(&core, home.share.join("vfs")).await
    };

    let daemon = Arc::new(Daemon {
        core: core.clone(),
        tokens,
        events,
        store,
        projects: Projects::new(home.root.join("projects")),
        worktrees: home.work(),
        results,
        same_machine,
        virtual_files_problem,
        imports: tokio::sync::Mutex::new(()),
        caps: limits.caps,
    });

    // 7. What a killed process left: lock files on the repositories and
    //    the result indexes, which would refuse every later write. Then
    //    what deleted and forgotten chats left (R01), and the runs the
    //    last process left going (5.4). The cleanup comes again each day.
    //    A stop meanwhile (an upgrade, a reboot) ends start-up where it
    //    is, and the runs started so far pause for the next start.
    daemon.projects.clear_stale_locks();
    daemon.results.clear_stale_locks();
    let starting_up = async {
        work::collect(&daemon, now_ms() - gc::KEEP_RELEASED_MS).await;
        restart::restart_interrupted(&daemon).await;
    };
    tokio::select! {
        () = starting_up => {}
        () = stop.wait() => {
            log::info!("stopping during start-up: the runs pause first, for up to {DRAIN_WAIT:?}");
            let paused = agent_run::drain(&core, DRAIN_WAIT).await;
            log::info!("{paused} paused for the next start");
            core.shutdown().await;
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
    core.shutdown().await;
    let _ = std::fs::remove_file(&socket);
    served.map_err(|e| e.to_string())
}

/// Mounts the virtual files at `at` when FUSE works here; else why not.
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
    loop {
        let others = rt::spawn_blocking(|| host::other_daemons(Path::new("/proc"), home::uid()));
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
}

impl EventSink for Tap {
    fn agent(&self, event: &AgentEvent) {
        match event {
            AgentEvent::RunStarted { dialog_id, run_id } => {
                self.store.run_started(dialog_id, run_id)
            }
            // A run the daemon's stop paused stays running, for the next
            // start to continue (5.4), and has no result yet.
            AgentEvent::RunEnded { outcome, .. } if drained(outcome) => {}
            AgentEvent::RunEnded {
                dialog_id, outcome, ..
            } => self.store.ended(dialog_id, Some(outcome)),
            _ => {}
        }
        self.events.agent(event);
        // The run's end goes out first, then its result.
        if let AgentEvent::RunEnded {
            dialog_id,
            outcome,
            autopilot,
            ..
        } = event
        {
            if !drained(outcome) {
                self.results.after_run(dialog_id, outcome, *autopilot);
            }
        }
    }
}

/// A run's outcome when the daemon's stop paused it (R02).
fn drained(outcome: &str) -> bool {
    outcome.strip_prefix("paused-") == Some(recovery::DRAIN)
}

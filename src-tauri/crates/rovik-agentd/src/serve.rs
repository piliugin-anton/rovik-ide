//! `rovik-agentd serve`: start-up in the order of section 5.2, then serving
//! until SIGTERM or SIGINT, then the core's shutdown.

use crate::auth::Tokens;
use crate::cli::{Options, VERSION};
use crate::config::Config;
use crate::daemon::Daemon;
use crate::events::Events;
use crate::home::{self, Home};
use crate::logfile;
use crate::server::{self, OwnUser};
use crate::store::Store;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::{process, rt, Core, EventSink, Paths};
use std::path::PathBuf;
use std::sync::Arc;

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

    // 4. The computers' token hashes, read only here.
    let tokens = Tokens::new(&config.tokens)?;

    // 5. The runtime and the core.
    let socket = options.socket.unwrap_or_else(|| home.socket());
    let result = rt::block_on(run(
        home,
        socket,
        tokens,
        options.same_machine,
        options.no_virtual_files,
    ));
    if let Err(e) = &result {
        log::error!("{e}");
    }
    result
}

async fn run(
    home: Home,
    socket: PathBuf,
    tokens: Tokens,
    same_machine: bool,
    no_virtual_files: bool,
) -> Result<(), String> {
    let store = Arc::new(Store::open(&home.root)?);
    let owners = store.clone();
    let events = Events::new(
        uuid::Uuid::new_v4().to_string(),
        Arc::new(move |chat| owners.owner(chat)),
    );
    let paths = Paths::new(home.data(), home.core_config());
    let sink = Tap {
        store: store.clone(),
        events: events.clone(),
    };
    let core = Core::new(paths, Arc::new(sink))?;
    // A run follows its chat's computer's configuration (D16).
    let configs = store.clone();
    core.agents
        .set_resolver(Arc::new(move |chat| configs.config_of(chat)));

    // 6. The virtual files, so CLM chats can use them (K15).
    let virtual_files_problem = if no_virtual_files {
        Some("The daemon runs without virtual files (--no-virtual-files).".to_string())
    } else {
        mount_virtual_files(&core, home.share.join("vfs")).await
    };

    // 7. Interrupted runs restart here (5.4, P06).

    // 8. The socket, then serving.
    let listener = server::bind(&socket)?;
    let daemon = Arc::new(Daemon {
        core: core.clone(),
        tokens,
        events,
        store,
        same_machine,
        virtual_files_problem,
    });
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
    let served = axum::serve(listener, server::router(daemon))
        .with_graceful_shutdown(stopped())
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

/// When SIGTERM (systemd stopping the unit) or SIGINT comes.
async fn stopped() {
    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
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

/// The core's events: the store hears of runs that end, then they go out.
struct Tap {
    store: Arc<Store>,
    events: Arc<Events>,
}

impl EventSink for Tap {
    fn agent(&self, event: &AgentEvent) {
        if let AgentEvent::RunEnded { dialog_id, .. } = event {
            self.store.ended(dialog_id);
        }
        self.events.agent(event);
    }
}

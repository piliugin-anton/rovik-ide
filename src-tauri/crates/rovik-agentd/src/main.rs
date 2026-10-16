//! rovik-agentd: the agent on a server (docs/design/cloud-agent-dev-plan.md,
//! section 5). A `Core` behind a Unix socket that only its own user can
//! reach, serving the protocol of rovik-protocol, so the chats a computer
//! moves here keep running while the computer sleeps.
//!
//!   cli.rs      the commands and their arguments
//!   serve.rs    `serve`: start-up (5.2) and shutting down
//!   home.rs     the daemon's folders and its lock (5.1)
//!   config.rs   config.toml, read once at start-up
//!   auth.rs     the computers' tokens, by their hashes (D5)
//!   daemon.rs   what the daemon does with a request
//!   server.rs   HTTP on the socket: /health, /rpc, /call, /blobs, /packs
//!   session.rs  one WebSocket and its frames (4.1)
//!   events.rs   the events: numbered, paced, kept, routed (4.3)
//!   store.rs    state.json: each chat's computer and settings (D16)
//!   restart.rs  the runs a crash or an upgrade cut, at start-up (5.4)
//!   results.rs  each chat's result: its worktree committed (6.5)
//!   sync.rs     a chat's worktree merged with a new snapshot (6.7)
//!   keys.rs     keys.json: the providers' keys (D11)
//!   packs.rs    each project's repository and its packs (6.4)
//!   work.rs     each chat's worktree (6.4)
//!   changes.rs  a chat's changes there, for the isolation bar (6.1)
//!   gc.rs       what the folders no longer need goes, each day (R01)
//!   host.rs     `HostInfo`
//!   limits.rs   /etc/rovik/agentd.toml: the administrators and the limits
//!   installer.rs  `install`, `user` and `admin`: Rovik for everyone (5.3)
//!   attach.rs   `attach --stdio`: a byte bridge to the socket
//!   doctor.rs   `doctor`: what this server lacks for the daemon
//!   logfile.rs  logs/agentd.log

#[cfg(unix)]
mod attach;
#[cfg(unix)]
mod auth;
#[cfg(unix)]
mod changes;
#[cfg(unix)]
mod cli;
#[cfg(unix)]
mod config;
#[cfg(unix)]
mod daemon;
#[cfg(unix)]
mod doctor;
#[cfg(unix)]
mod events;
#[cfg(unix)]
mod gc;
#[cfg(unix)]
mod home;
#[cfg(unix)]
mod host;
#[cfg(unix)]
mod installer;
#[cfg(unix)]
mod keys;
#[cfg(unix)]
mod limits;
#[cfg(unix)]
mod logfile;
#[cfg(unix)]
mod moves;
#[cfg(unix)]
mod packs;
#[cfg(unix)]
mod restart;
#[cfg(unix)]
mod results;
#[cfg(unix)]
mod serve;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod session;
#[cfg(unix)]
mod store;
#[cfg(unix)]
mod sync;
#[cfg(unix)]
mod work;

#[cfg(unix)]
fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cli::run(&argv));
}

/// Servers run Linux (D16); the daemon builds on macOS too, for development.
#[cfg(not(unix))]
fn main() {
    eprintln!("rovik-agentd runs on Linux.");
    std::process::exit(1);
}

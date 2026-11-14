//! rovik-client: the app's half of the protocol (D1, section 4), without
//! Tauri, so it's tested against the real daemon. A `Client` keeps one
//! host's connection: calls and events on the WebSocket, `POST /call` for
//! what is too big for a frame, and a new connection whenever the link
//! drops. `Hosts` keeps each host's client, and `Remote` the app's remote
//! chats.
//!
//!   connector.rs  how a connection reaches the daemon: a stream each time
//!   client.rs     one host's connection, its calls and its events
//!   http.rs       `POST /call`, `/blobs`, `/packs` and `/forward`
//!   hosts.rs      each host's client and state
//!   remote.rs     the app's remote chats: routing, starts and events (6.1),
//!                 their isolation bar, the project synced before each
//!                 message (6.7), their results, imported and landed
//!                 here (6.5), and the pages their commands open (D20)
//!   shadow.rs     each project's shadow repository and its snapshots (6.4)
//!   packs.rs      packs between a shadow repository and a host (6.4)
//!   host_settings.rs  the hosts this computer knows (cloud-hosts.json)
//!   secrets.rs    passwords, passphrases, tokens and API keys, in the
//!                 keychain or a file encrypted in its place
//!   provider_keys.rs  the desktop's API keys out of ai-settings.json, into
//!                 the secrets
//!   ssh.rs        SSH to a host: a channel to its daemon for each stream,
//!                 and the commands that set it up
//!   system_ssh.rs the user's own ssh, forwarding a socket to the daemon
//!   connection_test.rs  the hosts page's Test connection, a step at a time,
//!                 and the probe that says what a server offers
//!   deploy.rs     the daemon put on a host, checked, upgraded and removed
//!   hosts_page.rs what Settings → Cloud hosts does with all of the above

mod client;
pub mod connection_test;
mod connector;
pub mod deploy;
pub mod desktop_api;
pub mod desktop_backend;
pub mod desktop_connection;
mod desktop_events;
pub mod desktop_session;
pub mod host_settings;
pub mod host_shell;
mod hosts;
pub mod hosts_page;
mod http;
pub mod local_backend;
pub mod mcp_sign_in;
mod packs;
pub mod provider_keys;
pub mod release;
mod remote;
pub mod secrets;
pub mod shadow;
pub mod ssh;
pub mod system_ssh;
#[cfg(test)]
mod testing;
pub mod wsl;

pub use client::{welcome, Client, DaemonInfo, HostState, Identity, Incoming, Sink, CALL_TIMEOUT};
#[cfg(unix)]
pub use connector::UnixConnector;
pub use connector::{Connecting, Connector, Io, Stream};
pub use hosts::Hosts;
pub use remote::{
    ApprovalPolicy, CloudEvent, CloudSink, ForwardedPort, HandedSync, MovePreflight, OpenPage,
    PageToOpen, Remote, LOCAL_ONLY,
};

pub mod terminal;
pub mod util;

pub mod filesystem;

mod desktop_task;

pub mod shared_state;

/// Replayable shared desktop events.
pub mod desktop_journal;

/// Shared Git panel service.
pub mod git;
/// Shared Git merge and rebase service.
pub mod git_sync;

/// Backend-owned language servers and rust-analyzer management.
pub mod lsp;

/// Backend-owned Chrome installation and client progress observers.
pub mod browser_install;
pub mod docker_install;
mod install_job;

/// Private routing of one-shot remote page requests to UI clients.
pub mod desktop_pages;

/// Backend-owned editor formatter discovery and execution.
pub mod formatter;

/// Shared workload controls and client-owned priority.
pub mod workload;

mod maintenance;

/// Embedded backend compatibility with native project claims.
pub mod project_ownership;

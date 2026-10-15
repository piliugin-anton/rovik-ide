//! rovik-client: the app's half of the protocol (D1, section 4), without
//! Tauri, so it's tested against the real daemon. A `Client` keeps one
//! host's connection: calls and events on the WebSocket, `POST /call` for
//! what is too big for a frame, and a new connection whenever the link
//! drops. `Hosts` keeps each host's client, and `Remote` the app's remote
//! chats.
//!
//!   connector.rs  how a connection reaches the daemon: a stream each time
//!   client.rs     one host's connection, its calls and its events
//!   http.rs       `POST /call`, `/blobs` and `/packs`
//!   hosts.rs      each host's client and state
//!   remote.rs     the app's remote chats: routing, starts and events (6.1),
//!                 their isolation bar, the project synced before each
//!                 message (6.7), and their results, imported and landed
//!                 here (6.5)
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
pub mod host_settings;
mod hosts;
pub mod hosts_page;
mod http;
mod packs;
pub mod provider_keys;
mod remote;
pub mod secrets;
pub mod shadow;
pub mod ssh;
pub mod system_ssh;
#[cfg(test)]
mod testing;

pub use client::{welcome, Client, DaemonInfo, HostState, Identity, Incoming, Sink, CALL_TIMEOUT};
#[cfg(unix)]
pub use connector::UnixConnector;
pub use connector::{Connecting, Connector, Io, Stream};
pub use hosts::Hosts;
pub use remote::{ApprovalPolicy, CloudEvent, CloudSink, MovePreflight, Remote, LOCAL_ONLY};

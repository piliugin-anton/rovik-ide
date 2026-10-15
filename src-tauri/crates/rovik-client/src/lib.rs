//! rovik-client: the app's half of the protocol (D1, section 4), without
//! Tauri, so it's tested against the real daemon. A `Client` keeps one
//! host's connection: calls and events on the WebSocket, `POST /call` for
//! what is too big for a frame, and a new connection whenever the link
//! drops. `Hosts` keeps each host's client.
//!
//!   connector.rs  how a connection reaches the daemon: a stream each time
//!   client.rs     one host's connection, its calls and its events
//!   http.rs       `POST /call`
//!   hosts.rs      each host's client and state

mod client;
mod connector;
mod hosts;
mod http;
#[cfg(test)]
mod testing;

pub use client::{Client, DaemonInfo, HostState, Identity, Incoming, Sink, CALL_TIMEOUT};
#[cfg(unix)]
pub use connector::UnixConnector;
pub use connector::{Connecting, Connector, Io, Stream};
pub use hosts::Hosts;

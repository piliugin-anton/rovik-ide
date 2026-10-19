//! How a client reaches its daemon: a fresh byte stream each time it asks,
//! a Unix socket now and an SSH channel later (H02). The WebSocket takes
//! one, and each `POST /call` one of its own, so a big transfer never holds
//! up the events (K41).

use std::future::Future;
use std::io;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

/// A connection's bytes, whatever carries them.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

pub type Stream = Box<dyn Io>;

/// A stream on its way.
pub type Connecting<'a> = Pin<Box<dyn Future<Output = io::Result<Stream>> + Send + 'a>>;

/// Opens a new stream to the daemon. An error that carries a `Refusal`
/// says the host turned this computer away: the client shows why
/// (`HostState::Refused`). Any other error is a link that didn't come up
/// (the network, a key or agent not there yet), tried again soon.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self) -> Connecting<'_>;
}

/// The host turned this computer away, as a `Connector`'s error says
/// (`Refusal::into_error`).
#[derive(Debug)]
pub struct Refusal {
    pub why: String,
    /// It may come right without the user (a release's list of builds
    /// published, the host's pinned key back): the client tries again now
    /// and then. Else (the credentials, a host key never pinned, this
    /// computer's token) only something the user changes connects it:
    /// trying again would only send the credentials once more.
    pub may_pass: bool,
}

impl Refusal {
    /// As a `Connector`'s error.
    pub fn into_error(self) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, self)
    }

    /// The refusal `e` carries, if it's one: an error of the system's own
    /// (a socket's permissions, say) never is.
    pub fn of(e: &io::Error) -> Option<&Refusal> {
        e.get_ref()?.downcast_ref()
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for Refusal {}

/// The daemon's socket on this machine.
#[cfg(unix)]
pub struct UnixConnector(pub std::path::PathBuf);

#[cfg(unix)]
impl Connector for UnixConnector {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(&self.0).await?;
            Ok(Box::new(stream) as Stream)
        })
    }
}

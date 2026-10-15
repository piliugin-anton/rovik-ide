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

/// Opens a new stream to the daemon.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self) -> Connecting<'_>;
}

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

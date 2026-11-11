//! Readiness is independent of the operating system's authenticated stream.
use super::{
    platform::{connect_endpoint, Endpoint},
    read_ready, write_ready, Lease, Ready, Stream, PROTOCOL,
};
use std::{io, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
impl Lease {
    /// Publishes readiness only after the lifetime owner initializes its services.
    pub fn listen(&self, version: &str) -> io::Result<Listener<'_>> {
        Ok(Listener {
            socket: Endpoint::bind(self.directory(), "rpc")?,
            _lease: self,
            boot: uuid::Uuid::new_v4().to_string(),
            version: version.into(),
        })
    }
}
/// A ready daemon's listener. Dropping it closes the socket before releasing ownership.
pub struct Listener<'a> {
    socket: Endpoint,
    _lease: &'a Lease,
    boot: String,
    version: String,
}

impl Listener<'_> {
    pub(super) fn boot(&self) -> &str {
        &self.boot
    }

    /// Accepts a same-user client and sends its bounded readiness handshake.
    pub async fn accept(&self) -> io::Result<Stream> {
        self.accept_pending().await?.finish().await
    }

    /// Accepts and checks peer ownership without waiting for handshake IO.
    /// Safe to use in a select loop: handshake work belongs to its client task.
    pub async fn accept_pending(&self) -> io::Result<Pending> {
        let stream = self.socket.accept().await?;
        Ok(Pending {
            stream,
            ready: Ready {
                protocol: PROTOCOL,
                boot: self.boot.clone(),
                version: self.version.clone(),
                session: uuid::Uuid::new_v4().to_string(),
            },
        })
    }
}

/// A same-user connection awaiting protocol acknowledgement.
pub struct Pending {
    stream: Stream,
    ready: Ready,
}

impl Pending {
    /// The identity used by handlers and the separate private event stream.
    pub fn session(&self) -> &str {
        &self.ready.session
    }

    /// Completes a bounded handshake before any operation can be dispatched.
    pub async fn finish(mut self) -> io::Result<Stream> {
        tokio::time::timeout(Duration::from_secs(5), async {
            write_ready(&mut self.stream, &self.ready).await?;
            let protocol = self.stream.read_u32().await?;
            if protocol != PROTOCOL {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Incompatible local client protocol",
                ));
            }
            Ok(self.stream)
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Backend client did not complete its handshake",
            )
        })?
    }
}

/// Connects to an initialized same-user backend. Protocol mismatch is
/// returned as Unsupported, never as permission to launch another owner.
pub async fn connect(directory: &Path) -> io::Result<(Stream, Ready)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = connect_endpoint(directory, "rpc").await?;
        let ready = read_ready(&mut stream).await?;
        stream.write_u32(PROTOCOL).await?;
        Ok((stream, ready))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Backend connection timed out"))?
}

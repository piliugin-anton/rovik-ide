use super::{read_ready, write_ready, Ready, PROTOCOL};
use std::fs::{File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

/// Exclusive ownership of a backend directory. Held for the daemon's
/// lifetime; the lock file is never deleted or replaced.
pub struct Lease {
    _lock: File,
    directory: PathBuf,
}

impl Lease {
    /// Acquires ownership before any backend services are initialized.
    /// None means another process owns this directory.
    pub fn acquire(directory: &Path) -> io::Result<Option<Self>> {
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Backend directory must be private and owned by the current user",
            ));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("owner.lock"))?;
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid backend ownership lock",
            ));
        }
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self {
                _lock: lock,
                directory: directory.into(),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Publishes readiness after service initialization. Only the owner
    /// may remove a stale endpoint left by a crashed predecessor.
    pub fn listen(&self, version: &str) -> io::Result<Listener<'_>> {
        let path = self.directory.join("rpc.sock");
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let socket = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        Ok(Listener {
            socket,
            lease: self,
            boot: uuid::Uuid::new_v4().to_string(),
            version: version.into(),
        })
    }
}

/// A ready daemon's listener. Dropping it closes the socket before releasing ownership.
pub struct Listener<'a> {
    socket: UnixListener,
    lease: &'a Lease,
    boot: String,
    version: String,
}

impl Listener<'_> {
    /// Accepts a same-user client and sends its bounded readiness handshake.
    pub async fn accept(&self) -> io::Result<UnixStream> {
        let (mut stream, _) = self.socket.accept().await?;
        if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Backend client belongs to another user",
            ));
        }
        let ready = Ready {
            protocol: PROTOCOL,
            boot: self.boot.clone(),
            version: self.version.clone(),
            session: uuid::Uuid::new_v4().to_string(),
        };
        tokio::time::timeout(Duration::from_secs(5), write_ready(&mut stream, &ready))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "Backend handshake timed out")
            })??;
        Ok(stream)
    }
}

impl Drop for Listener<'_> {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(self.lease.directory.join("rpc.sock")) {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!("Couldn't remove the backend endpoint: {e}");
            }
        }
    }
}

/// Connects to an initialized same-user backend. Protocol mismatch is
/// returned as Unsupported, never as permission to launch another owner.
pub async fn connect(directory: &Path) -> io::Result<(UnixStream, Ready)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(directory.join("rpc.sock")).await?;
        if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Backend belongs to another user",
            ));
        }
        let ready = read_ready(&mut stream).await?;
        Ok((stream, ready))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Backend connection timed out"))?
}

/// Connects first, otherwise starts the packaged daemon once and waits for
/// readiness. Concurrent launchers are arbitrated by the daemon's lease.
pub async fn ensure_backend(
    executable: &Path,
    data: &Path,
    config: &Path,
) -> io::Result<(UnixStream, Ready)> {
    let directory = super::directory(data);
    match connect(&directory).await {
        Ok(connected) => return Ok(connected),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e),
    }
    let mut command = rovik_core::process::command(executable);
    command.arg("--data").arg(data).arg("--config").arg(config);
    command.stdin(std::process::Stdio::null());
    rovik_core::process::own_session(&mut command);
    rovik_core::process::close_inherited(&mut command);
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        if let Err(e) = child.wait() {
            log::warn!("Couldn't reap the local backend launcher: {e}");
        }
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match connect(&directory).await {
                Ok(connected) => return Ok(connected),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(e) => return Err(e),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Local backend did not become ready within 30 seconds",
        )
    })?
}

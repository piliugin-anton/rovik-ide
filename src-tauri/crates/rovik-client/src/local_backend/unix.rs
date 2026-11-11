//! Same-user Unix socket transport and lifetime ownership.
use std::fs::{File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;
/// Authenticated local transport used by the common RPC layer.
pub type Stream = tokio::net::UnixStream;

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
        Self::acquire_named(directory, "owner.lock")
    }

    pub(super) fn acquire_named(directory: &Path, name: &str) -> io::Result<Option<Self>> {
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
            .open(directory.join(name))?;
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

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
}

pub(super) struct Endpoint {
    socket: UnixListener,
    path: PathBuf,
}
impl Endpoint {
    pub fn bind(directory: &Path, name: &str) -> io::Result<Self> {
        let path = directory.join(format!("{name}.sock"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let socket = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        Ok(Self { socket, path })
    }
    pub async fn accept(&self) -> io::Result<Stream> {
        let (stream, _) = self.socket.accept().await?;
        check_peer(&stream)?;
        Ok(stream)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!("Couldn't remove backend endpoint: {e}");
            }
        }
    }
}
pub(super) async fn connect_endpoint(directory: &Path, name: &str) -> io::Result<Stream> {
    let stream = Stream::connect(directory.join(format!("{name}.sock"))).await?;
    check_peer(&stream)?;
    Ok(stream)
}
fn check_peer(stream: &Stream) -> io::Result<()> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Backend peer belongs to another user",
        ));
    }
    Ok(())
}

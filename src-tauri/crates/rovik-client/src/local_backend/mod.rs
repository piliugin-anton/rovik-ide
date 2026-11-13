//! Local daemon discovery and readiness. The endpoint is published only
//! after the host has initialized its services under the lifetime lock.
//! Desktop command migration is separate from this bootstrap protocol.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
#[cfg(any(unix, windows))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(any(unix, windows))]
pub mod control;
#[cfg(any(unix, windows))]
mod handshake;
#[cfg(any(unix, windows))]
pub mod host;
#[cfg(any(windows, test))]
mod image;
#[cfg(any(unix, windows))]
mod launch;
#[cfg(unix)]
#[path = "unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
#[cfg(any(unix, windows))]
pub mod rpc;
#[cfg(any(unix, windows))]
pub use handshake::{connect, Listener, Pending};
#[cfg(any(unix, windows))]
pub use launch::{ensure_backend, ensure_command};
#[cfg(any(unix, windows))]
pub use platform::{Lease, Stream};

/// Version of local desktop requests and readiness, independent of remote RPC
/// and the polyglot harness's v1 artifact schemas.
pub const PROTOCOL: u32 = 21;
#[cfg(any(unix, windows))]
const MAX_HANDSHAKE: usize = 4096;

/// Identifies one initialized backend and the capabilities it serves.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ready {
    /// The local protocol supported by this executable.
    pub protocol: u32,
    /// Changes on every backend start.
    pub boot: String,
    /// Version of the backend executable.
    pub version: String,
    /// A new identity for this client connection.
    pub session: String,
}

/// Checks compatibility before a window can use the discovered backend.
pub fn compatible(ready: Ready) -> io::Result<Ready> {
    if ready.protocol != PROTOCOL {
        return Err(io::Error::new(io::ErrorKind::Unsupported,
            format!("Local backend protocol {} is incompatible with {PROTOCOL}. Stop the existing desktop backend after its work finishes, then reopen Rovik. Restarting a window alone does not replace the backend.", ready.protocol)));
    }
    Ok(ready)
}

#[cfg(any(unix, windows))]
async fn read_ready(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Ready> {
    let len = stream.read_u32().await? as usize;
    if len == 0 || len > MAX_HANDSHAKE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid backend handshake size",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    let ready = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    compatible(ready)
}

#[cfg(any(unix, windows))]
async fn write_ready(stream: &mut (impl AsyncWrite + Unpin), ready: &Ready) -> io::Result<()> {
    let bytes = serde_json::to_vec(ready).map_err(io::Error::other)?;
    if bytes.len() > MAX_HANDSHAKE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Backend handshake is too large",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}

/// The backend's private files, scoped to the application's data directory.
pub fn directory(data: &Path) -> std::path::PathBuf {
    data.join("desktop-backend")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn an_incompatible_backend_never_triggers_another_launcher() {
        let root = tempfile::tempdir().unwrap();
        let dir = directory(root.path());
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("rpc.sock")).unwrap();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_ready(
                &mut stream,
                &Ready {
                    protocol: PROTOCOL + 1,
                    boot: "new".into(),
                    version: "new".into(),
                    session: "s".into(),
                },
            )
            .await
            .unwrap();
        };
        let command = std::process::Command::new("/missing/must-not-spawn");
        let (_, result) = tokio::join!(server, ensure_command(command, root.path(), root.path()));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn an_existing_backend_is_used_without_spawning_the_command() {
        let root = tempfile::tempdir().unwrap();
        let lease = Lease::acquire(&directory(root.path())).unwrap().unwrap();
        let listener = lease.listen("test").unwrap();
        let command = std::process::Command::new("/missing/launcher-must-not-run");
        let (accepted, attached) = tokio::join!(
            listener.accept(),
            ensure_command(command, root.path(), root.path())
        );
        accepted.unwrap();
        attached.unwrap();
    }

    #[tokio::test]
    async fn a_failed_launcher_is_reported_without_waiting_for_startup_timeout() {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ensure_command(command, root.path(), root.path()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("exited with"), "{error}");
    }

    #[tokio::test]
    async fn clients_share_one_boot_and_have_distinct_sessions() {
        let root = tempfile::tempdir().unwrap();
        let dir = directory(root.path());
        let lease = Lease::acquire(&dir).unwrap().unwrap();
        assert!(Lease::acquire(&dir).unwrap().is_none());
        let listener = lease.listen("test").unwrap();
        let (server, first) = tokio::join!(listener.accept(), connect(&dir));
        drop(server.unwrap());
        let (_, first) = first.unwrap();
        let (server, second) = tokio::join!(listener.accept(), connect(&dir));
        drop(server.unwrap());
        let (_, second) = second.unwrap();
        assert_eq!(first.boot, second.boot);
        assert_ne!(first.session, second.session);
        drop(listener);
        assert!(
            Lease::acquire(&dir).unwrap().is_none(),
            "shutdown retains ownership"
        );
        drop(lease);
        let next = Lease::acquire(&dir).unwrap().unwrap();
        let listener = next.listen("test").unwrap();
        let (server, restarted) = tokio::join!(listener.accept(), connect(&dir));
        drop(server.unwrap());
        assert_ne!(first.boot, restarted.unwrap().1.boot);
    }

    #[test]
    fn an_insecure_backend_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = directory(root.path());
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            matches!(Lease::acquire(&dir), Err(e) if e.kind() == io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn a_symlink_cannot_substitute_the_ownership_lock() {
        let root = tempfile::tempdir().unwrap();
        let dir = directory(root.path());
        let lease = Lease::acquire(&dir).unwrap().unwrap();
        drop(lease);
        std::fs::remove_file(dir.join("owner.lock")).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("owner.lock")).unwrap();
        assert!(Lease::acquire(&dir).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }

    #[tokio::test]
    async fn handshake_sizes_and_incompatible_versions_are_rejected() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_u32((MAX_HANDSHAKE + 1) as u32).await.unwrap();
        assert_eq!(
            read_ready(&mut reader).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let ready = Ready {
            protocol: PROTOCOL + 1,
            boot: "boot".into(),
            version: "new".into(),
            session: "session".into(),
        };
        assert_eq!(
            compatible(ready).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}

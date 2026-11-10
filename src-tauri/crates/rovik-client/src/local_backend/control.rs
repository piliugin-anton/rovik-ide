//! Stable, bounded upgrade negotiation, independent of desktop command RPC.
use super::{
    platform::{connect_endpoint, Endpoint},
    Lease, Stream, PROTOCOL,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{io, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONTROL: u32 = 1;
const LIMIT: usize = 4096;
/// Bounds a complete control exchange, including a stalled peer.
pub(super) const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
/// Management readiness for a single running host.
pub(super) struct Hello {
    control: u32,
    /// Same identity as the command endpoint.
    pub boot: String,
    /// Executable release version.
    pub version: String,
    /// Desktop command protocol, independent of control framing.
    pub protocol: u32,
}
impl Hello {
    /// Describes this executable without admitting a command session.
    pub fn new(boot: &str) -> Self {
        Self {
            control: CONTROL,
            boot: boot.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL,
        }
    }
}

#[derive(Serialize, Deserialize)]
/// Desired executable, bound to the boot observed by its launcher.
pub(super) struct Candidate {
    control: u32,
    boot: String,
    version: String,
    protocol: u32,
}
impl Candidate {
    /// Rejects stale identities and permits only monotonic upgrades.
    pub fn upgrades(&self, host: &Hello) -> io::Result<bool> {
        if self.control != CONTROL || self.boot != host.boot {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Backend upgrade identity or control protocol changed",
            ));
        }
        let parse = |s: &str| {
            semver::Version::parse(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        };
        let old = parse(&host.version)?;
        let new = parse(&self.version)?;
        Ok(new >= old
            && self.protocol >= host.protocol
            && (new > old || self.protocol > host.protocol))
    }
}

#[derive(Debug, Serialize, Deserialize)]
/// Replacement decision; refusal never transfers ownership.
pub(super) enum Reply {
    /// No monotonic upgrade was requested.
    Current,
    /// The existing owner must retain its work or resources.
    Busy(String),
    /// Cleanup has been accepted; ownership is retained until it finishes.
    Retiring,
}

/// Control endpoint held under the same lifetime lease as command RPC.
pub(super) struct Listener<'a> {
    socket: Endpoint,
    _lease: &'a Lease,
}
impl<'a> Listener<'a> {
    pub fn bind(lease: &'a Lease, directory: &Path) -> io::Result<Self> {
        Ok(Self {
            socket: Endpoint::bind(directory, "control")?,
            _lease: lease,
        })
    }
    pub async fn accept(&self) -> io::Result<Stream> {
        self.socket.accept().await
    }
}

/// Reads a bounded frame before allocating its payload.
pub(super) async fn read<T: DeserializeOwned>(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> io::Result<T> {
    let len = stream.read_u32().await? as usize;
    if len == 0 || len > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid backend control frame size",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
/// Writes one bounded management frame.
pub(super) async fn write<T: Serialize>(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Backend control frame too large",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}

/// Missing endpoints belong to old hosts or hosts not yet ready. Never kill them.
pub(super) async fn negotiate(directory: &Path) -> io::Result<Option<(Hello, Reply)>> {
    tokio::time::timeout(TIMEOUT, async {
        let mut stream = match connect_endpoint(directory, "control").await {
            Ok(stream) => stream,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };
        let host: Hello = read(&mut stream).await?;
        if host.control != CONTROL {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Incompatible backend upgrade control protocol",
            ));
        }
        let candidate = Candidate {
            control: CONTROL,
            boot: host.boot.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL,
        };
        write(&mut stream, &candidate).await?;
        let reply = read(&mut stream).await?;
        Ok(Some((host, reply)))
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Backend upgrade negotiation timed out",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_requires_the_same_boot_and_a_monotonic_upgrade() {
        let mut host = Hello::new("boot");
        host.version = "1.2.3".into();
        host.protocol = 4;
        for (version, protocol, upgrade) in [
            ("1.2.3", 4, false),
            ("1.2.4", 4, true),
            ("1.2.3", 5, true),
            ("1.2.2", 5, false),
            ("2.0.0", 3, false),
        ] {
            let candidate = Candidate {
                control: CONTROL,
                boot: "boot".into(),
                version: version.into(),
                protocol,
            };
            assert_eq!(candidate.upgrades(&host).unwrap(), upgrade);
        }
        let mut candidate = Candidate {
            control: CONTROL,
            boot: "other".into(),
            version: "2.0.0".into(),
            protocol: 5,
        };
        assert!(candidate.upgrades(&host).is_err());
        candidate.boot = "boot".into();
        candidate.control += 1;
        assert!(candidate.upgrades(&host).is_err());
    }
    #[tokio::test]
    async fn a_compatible_busy_host_is_reused_and_an_incompatible_one_is_not_replaced() {
        for protocol in [PROTOCOL, PROTOCOL - 1] {
            let root = tempfile::tempdir().unwrap();
            let dir = super::super::directory(root.path());
            let lease = Lease::acquire(&dir).unwrap().unwrap();
            let listener = Listener::bind(&lease, &dir).unwrap();
            let rpc = lease.listen("0.0.1").unwrap();
            let server = async {
                let mut stream = listener.accept().await.unwrap();
                let mut hello = Hello::new(rpc.boot());
                hello.version = "0.0.1".into();
                hello.protocol = protocol;
                write(&mut stream, &hello).await.unwrap();
                let candidate: Candidate = read(&mut stream).await.unwrap();
                assert!(candidate.upgrades(&hello).unwrap());
                write(&mut stream, &Reply::Busy("retained mount".into()))
                    .await
                    .unwrap();
                if protocol == PROTOCOL {
                    rpc.accept().await.unwrap();
                }
            };
            let command = std::process::Command::new("/missing/must-not-spawn");
            let (_, result) = tokio::join!(
                server,
                super::super::ensure_command(command, root.path(), root.path())
            );
            if protocol == PROTOCOL {
                assert!(result.is_ok());
            } else {
                assert!(result.unwrap_err().to_string().contains("retained mount"));
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn automatic_replacement_waits_for_the_old_owners_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let dir = super::super::directory(root.path());
        let lease = Lease::acquire(&dir).unwrap().unwrap();
        let listener = Listener::bind(&lease, &dir).unwrap();
        let marker = root.path().join("launched");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "touch \"$1\"; exit 7", "sh"])
            .arg(&marker);
        let data = root.path().to_path_buf();
        let launch =
            tokio::spawn(async move { super::super::ensure_command(command, &data, &data).await });
        let mut stream = listener.accept().await.unwrap();
        let mut hello = Hello::new("old");
        hello.version = "0.0.1".into();
        hello.protocol = PROTOCOL - 1;
        write(&mut stream, &hello).await.unwrap();
        let candidate: Candidate = read(&mut stream).await.unwrap();
        assert!(candidate.upgrades(&hello).unwrap());
        write(&mut stream, &Reply::Retiring).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!launch.is_finished());
        assert!(
            !marker.exists(),
            "a replacement cannot start during cleanup"
        );
        drop(listener);
        drop(lease);
        let error = tokio::time::timeout(TIMEOUT, launch)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("exited with"), "{error}");
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn control_frames_reject_unbounded_allocations() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_u32((LIMIT + 1) as u32).await.unwrap();
        assert_eq!(
            read::<Hello>(&mut reader).await.err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

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

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Management readiness for a single running host.
pub struct Hello {
    control: u32,
    /// Same identity as the command endpoint.
    pub boot: String,
    /// Executable release version.
    pub version: String,
    /// Desktop command protocol, independent of control framing.
    pub protocol: u32,
    /// Hash of the executable loaded at startup; absent on older hosts.
    #[serde(default)]
    pub build: String,
    /// Whether this host can stop itself after an explicit user request.
    #[serde(default)]
    pub restart: bool,
}
impl Hello {
    /// Describes this executable without admitting a command session.
    pub(super) fn new(boot: &str) -> Self {
        Self {
            control: CONTROL,
            boot: boot.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL,
            build: String::new(),
            restart: true,
        }
    }
}

/// Exact host identity captured by the update notification.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendIdentity {
    /// Changes on every backend start.
    pub boot: String,
    /// Running backend release.
    pub version: String,
    /// Running command protocol.
    pub protocol: u32,
    /// Running executable hash, empty for legacy hosts.
    #[serde(default)]
    pub build: String,
}

impl Hello {
    /// Returns the identity that an explicit restart must match exactly.
    pub fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            boot: self.boot.clone(),
            version: self.version.clone(),
            protocol: self.protocol,
            build: self.build.clone(),
        }
    }
}

/// Installed and running backend versions for the update notification.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendUpdateStatus {
    /// Whether the installed executable can replace this older host.
    pub restart_required: bool,
    /// Running host boot identity.
    pub backend_boot: String,
    /// Running release version.
    pub backend_version: String,
    /// Installed application release version.
    pub app_version: String,
    /// Running command protocol.
    pub backend_protocol: u32,
    /// Installed command protocol.
    pub app_protocol: u32,
    /// Running executable hash.
    pub backend_build: String,
    /// Installed executable hash.
    pub app_build: String,
}

fn can_replace(host: &Hello, version: &str, protocol: u32, build: &str) -> io::Result<bool> {
    let parse = |value: &str| {
        semver::Version::parse(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    };
    let old = parse(&host.version)?;
    let new = parse(version)?;
    Ok(new >= old
        && protocol >= host.protocol
        && (new > old || protocol > host.protocol || build != host.build))
}

#[derive(Serialize, Deserialize)]
/// Desired executable, bound to the boot observed by its launcher.
pub(super) struct Candidate {
    control: u32,
    boot: String,
    version: String,
    protocol: u32,
    #[serde(default)]
    restart: Option<BackendIdentity>,
    #[serde(default)]
    build: String,
}
impl Candidate {
    /// Checks explicit authorization against the owner seen at the button click.
    pub fn restarts(&self, host: &Hello) -> io::Result<bool> {
        let Some(expected) = &self.restart else {
            return Ok(false);
        };
        if self.control != CONTROL || self.boot != host.boot || expected != &host.identity() {
            return Err(io::Error::other(
                "The backend changed; refresh the update notification before restarting",
            ));
        }
        can_replace(host, &self.version, self.protocol, &self.build)
    }
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
pub enum Reply {
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

/// Requests a monotonic upgrade to the supplied executable version and protocol.
/// Missing endpoints return `None`; they never authorize killing an existing host.
/// `Retiring` acknowledges cleanup, not ownership transfer: callers must wait for
/// the lifetime lease to become available before starting a replacement.
pub async fn negotiate(
    directory: &Path,
    version: &str,
    protocol: u32,
) -> io::Result<Option<(Hello, Reply)>> {
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
            version: version.into(),
            protocol,
            restart: None,
            build: String::new(),
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

/// Inspects the independent control endpoint even when command RPC is incompatible.
pub async fn update_status(
    executable: &Path,
    data: &Path,
) -> io::Result<Option<BackendUpdateStatus>> {
    let Some(host) = inspect(&super::directory(data)).await? else {
        return Ok(None);
    };
    let build = executable_build(executable).await?;
    Ok(Some(BackendUpdateStatus {
        restart_required: can_replace(&host, env!("CARGO_PKG_VERSION"), PROTOCOL, &build)?,
        backend_boot: host.boot,
        backend_version: host.version,
        app_version: env!("CARGO_PKG_VERSION").into(),
        backend_protocol: host.protocol,
        app_protocol: PROTOCOL,
        backend_build: host.build,
        app_build: build,
    }))
}

/// Fingerprints a packaged executable off the async executor. Native windows can
/// retain this at startup to detect an installation changed beneath their code.
pub async fn executable_build(executable: &Path) -> io::Result<String> {
    super::image::fingerprint(executable).await
}

async fn open(directory: &Path) -> io::Result<Option<(Stream, Hello)>> {
    let mut stream = match connect_endpoint(directory, "control").await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let host: Hello = read(&mut stream).await?;
    if host.control != CONTROL {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Incompatible backend upgrade control protocol",
        ));
    }
    Ok(Some((stream, host)))
}

async fn inspect(directory: &Path) -> io::Result<Option<Hello>> {
    tokio::time::timeout(TIMEOUT, async {
        let Some((mut stream, host)) = open(directory).await? else {
            return Ok(None);
        };
        // A no-op candidate completes the legacy exchange without requesting an upgrade.
        write(
            &mut stream,
            &Candidate {
                control: CONTROL,
                boot: host.boot.clone(),
                version: host.version.clone(),
                protocol: host.protocol,
                restart: None,
                build: String::new(),
            },
        )
        .await?;
        let _: Reply = read(&mut stream).await?;
        Ok(Some(host))
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Backend update inspection timed out",
        )
    })?
}

/// Stops only the owner whose full identity the user approved. The caller retains
/// the launch lease until this owner exits and the replacement becomes ready.
pub(super) async fn stop(
    directory: &Path,
    expected: &BackendIdentity,
    build: &str,
) -> io::Result<()> {
    let (owner, retiring) = tokio::time::timeout(TIMEOUT, async {
        let Some((mut stream, host)) = open(directory).await? else {
            return Err(io::Error::other("The backend already stopped; reconnect this window"));
        };
        if host.identity() != *expected {
            return Err(io::Error::other("The backend changed; refresh the update notification before restarting"));
        }
        if !can_replace(&host, env!("CARGO_PKG_VERSION"), PROTOCOL, build)? {
            return Err(io::Error::other("This backend is already current or newer; reconnect this window"));
        }
        // Capture before the challenge. Its reply proves the peer was still alive
        // after the handle was pinned, so a reused PID cannot select another process.
        let owner = if host.restart { None } else { super::owner::Owner::capture(&stream)? };
        write(&mut stream, &Candidate {
            control: CONTROL, boot: host.boot.clone(), version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL, restart: Some(expected.clone()), build: build.into(),
        }).await?;
        let reply: Reply = read(&mut stream).await?;
        match reply {
            Reply::Retiring => Ok((owner, true)),
            Reply::Current | Reply::Busy(_) if !host.restart && owner.is_some() => Ok((owner, false)),
            Reply::Busy(reason) => Err(io::Error::other(format!("This older backend cannot be stopped safely while busy: {reason}. Finish its work and close older Rovik windows, then retry"))),
            Reply::Current => Err(io::Error::other("This older backend cannot restart itself; close older Rovik windows and retry")),
        }
    }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Backend restart authorization timed out"))??;
    if !retiring {
        if let Some(owner) = &owner {
            owner.terminate(false)?;
        }
    }
    let wait = async {
        let started = tokio::time::Instant::now();
        let mut forced = false;
        loop {
            if !forced && started.elapsed() >= Duration::from_secs(5) {
                if let Some(owner) = &owner {
                    owner.terminate(true)?;
                }
                forced = true;
            }
            if Lease::acquire(directory)?.is_some()
                && owner.as_ref().map_or(Ok(true), |owner| owner.exited())?
            {
                return Ok::<_, io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(15), wait)
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "The backend is still stopping; wait and retry before starting a replacement",
            )
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_restart_requires_the_full_identity_and_never_downgrades() {
        let mut host = Hello::new("one");
        host.version = "1.2.3".into();
        host.protocol = 4;
        host.build = "old-image".into();
        let mut candidate = Candidate {
            control: CONTROL,
            boot: host.boot.clone(),
            version: host.version.clone(),
            protocol: host.protocol,
            restart: Some(host.identity()),
            build: "new-image".into(),
        };
        assert!(
            candidate.restarts(&host).unwrap(),
            "same release rebuilds are updates"
        );
        candidate.version = "1.2.2".into();
        assert!(!candidate.restarts(&host).unwrap());
        candidate.version = host.version.clone();
        candidate.protocol -= 1;
        assert!(!candidate.restarts(&host).unwrap());
        candidate.protocol = host.protocol;
        for field in ["boot", "version", "protocol", "build"] {
            let mut changed = host.identity();
            match field {
                "boot" => changed.boot = "replacement".into(),
                "version" => changed.version = "1.2.4".into(),
                "protocol" => changed.protocol += 1,
                _ => changed.build = "another-image".into(),
            }
            candidate.restart = Some(changed);
            assert!(candidate.restarts(&host).is_err(), "{field}");
        }
    }

    #[test]
    fn legacy_control_hellos_default_to_no_self_restart_capability() {
        let host: Hello = serde_json::from_value(serde_json::json!({
            "control": 1, "boot": "legacy", "version": "0.2.17", "protocol": 22,
        }))
        .unwrap();
        assert!(!host.restart);
        assert!(host.build.is_empty());
    }

    #[tokio::test]
    async fn status_uses_control_without_requesting_an_upgrade_or_command_session() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("installed");
        std::fs::write(&executable, b"new executable").unwrap();
        let directory = super::super::directory(root.path());
        let lease = Lease::acquire(&directory).unwrap().unwrap();
        let listener = Listener::bind(&lease, &directory).unwrap();
        let server = async {
            let mut stream = listener.accept().await.unwrap();
            let mut host = Hello::new("old");
            host.protocol = PROTOCOL - 1;
            write(&mut stream, &host).await.unwrap();
            let candidate: Candidate = read(&mut stream).await.unwrap();
            assert!(!candidate.upgrades(&host).unwrap());
            assert!(!candidate.restarts(&host).unwrap());
            write(&mut stream, &Reply::Current).await.unwrap();
        };
        let (_, status) = tokio::join!(server, update_status(&executable, root.path()));
        let status = status.unwrap().unwrap();
        assert!(status.restart_required);
        assert_eq!(status.backend_boot, "old");
        assert!(!status.app_build.is_empty());
    }

    #[tokio::test]
    async fn stale_restart_does_not_send_a_stop_request() {
        let root = tempfile::tempdir().unwrap();
        let directory = super::super::directory(root.path());
        let lease = Lease::acquire(&directory).unwrap().unwrap();
        let listener = Listener::bind(&lease, &directory).unwrap();
        let expected = Hello::new("previous").identity();
        let server = async {
            let mut stream = listener.accept().await.unwrap();
            write(&mut stream, &Hello::new("replacement"))
                .await
                .unwrap();
            assert!(read::<Candidate>(&mut stream).await.is_err());
        };
        let (_, error) = tokio::join!(server, stop(&directory, &expected, "new"));
        assert!(error.unwrap_err().to_string().contains("backend changed"));
        assert!(Lease::acquire(&directory).unwrap().is_none());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn legacy_macos_retires_idle_hosts_but_preserves_busy_hosts() {
        for busy in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let directory = super::super::directory(root.path());
            let lease = Lease::acquire(&directory).unwrap().unwrap();
            let listener = Listener::bind(&lease, &directory).unwrap();
            let mut host = Hello::new("legacy");
            host.protocol -= 1;
            host.restart = false;
            let expected = host.identity();
            let server = async {
                let mut stream = listener.accept().await.unwrap();
                write(&mut stream, &host).await.unwrap();
                let candidate: Candidate = read(&mut stream).await.unwrap();
                assert!(candidate.upgrades(&host).unwrap());
                write(
                    &mut stream,
                    &if busy {
                        Reply::Busy("running work".into())
                    } else {
                        Reply::Retiring
                    },
                )
                .await
                .unwrap();
                drop(listener);
                drop(lease);
            };
            let (_, result) = tokio::join!(server, stop(&directory, &expected, "new"));
            if busy {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("cannot be stopped safely while busy"));
            } else {
                result.unwrap();
            }
        }
    }
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
                restart: None,
                build: String::new(),
            };
            assert_eq!(candidate.upgrades(&host).unwrap(), upgrade);
        }
        let mut candidate = Candidate {
            control: CONTROL,
            boot: "other".into(),
            version: "2.0.0".into(),
            protocol: 5,
            restart: None,
            build: String::new(),
        };
        assert!(candidate.upgrades(&host).is_err());
        candidate.boot = "boot".into();
        candidate.control += 1;
        assert!(candidate.upgrades(&host).is_err());
    }

    #[test]
    fn protocol_twenty_is_rejected_before_sending_new_desktop_requests() {
        let error = super::super::compatible(super::super::Ready {
            protocol: 20,
            boot: "old-desktop".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            session: "old-session".into(),
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("protocol 20"));
    }

    #[tokio::test]
    async fn a_compatible_busy_host_is_reused_and_an_incompatible_one_is_not_replaced() {
        for (protocol, version, reusable) in [
            (PROTOCOL, "0.0.1", true),
            (PROTOCOL - 1, "0.0.1", false),
            // Protocol 20 predates Docker and harness desktop requests. A
            // rebuild may retain the same application release version.
            (20, env!("CARGO_PKG_VERSION"), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let dir = super::super::directory(root.path());
            let lease = Lease::acquire(&dir).unwrap().unwrap();
            let listener = Listener::bind(&lease, &dir).unwrap();
            let rpc = lease.listen(version).unwrap();
            let server = async {
                let mut stream = listener.accept().await.unwrap();
                let mut hello = Hello::new(rpc.boot());
                hello.version = version.into();
                hello.protocol = protocol;
                write(&mut stream, &hello).await.unwrap();
                let candidate: Candidate = read(&mut stream).await.unwrap();
                assert!(candidate.upgrades(&hello).unwrap());
                write(&mut stream, &Reply::Busy("retained mount".into()))
                    .await
                    .unwrap();
                if reusable {
                    rpc.accept().await.unwrap();
                }
            };
            let command = std::process::Command::new("/missing/must-not-spawn");
            let (_, result) = tokio::join!(
                server,
                super::super::ensure_command(command, root.path(), root.path())
            );
            if reusable {
                assert!(result.is_ok());
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                assert!(error.to_string().contains("retained mount"));
            }
            assert!(
                Lease::acquire(&dir).unwrap().is_none(),
                "the busy host keeps ownership"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn automatic_replacement_waits_for_the_old_owners_cleanup() {
        assert_replacement_waits_for_cleanup("0.0.1", PROTOCOL - 1).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_release_protocol_twenty_replacement_waits_for_the_old_owners_cleanup() {
        assert_replacement_waits_for_cleanup(env!("CARGO_PKG_VERSION"), 20).await;
    }

    #[cfg(unix)]
    async fn assert_replacement_waits_for_cleanup(version: &str, protocol: u32) {
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
        hello.version = version.into();
        hello.protocol = protocol;
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

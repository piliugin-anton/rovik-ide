//! Serializes discovery, guarded replacement and packaged daemon startup.
use super::{connect, Lease, Ready, Stream, PROTOCOL};
use std::{io, path::Path, time::Duration};

/// Connects first, otherwise starts the packaged daemon once and waits for
/// readiness. Windows runs a retained copy so installation can replace its source.
/// Concurrent launchers are arbitrated by the daemon's lease.
pub async fn ensure_backend(
    executable: &Path,
    data: &Path,
    config: &Path,
) -> io::Result<(Stream, Ready)> {
    ensure_prepared(
        || {
            #[cfg(windows)]
            let executable =
                super::image::stage(executable, &super::directory(data).join("images"))?;
            Ok(rovik_core::process::command(executable))
        },
        data,
        config,
    )
    .await
}

/// Attaches first, or launches a caller-prepared packaged command. AppImages
/// use their original launcher; no second private Core is ever a fallback.
pub async fn ensure_command(
    command: std::process::Command,
    data: &Path,
    config: &Path,
) -> io::Result<(Stream, Ready)> {
    ensure_prepared(|| Ok(command), data, config).await
}

async fn ensure_prepared(
    prepare: impl FnOnce() -> io::Result<std::process::Command>,
    data: &Path,
    config: &Path,
) -> io::Result<(Stream, Ready)> {
    let directory = super::directory(data);
    std::fs::create_dir_all(data)?;
    // Serialize upgrade and launch, without taking the host's lifetime lease.
    let _launch = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(lease) = Lease::acquire_named(&directory, "launch.lock")? {
                return Ok::<_, io::Error>(lease);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Another backend launcher is still starting or upgrading",
        )
    })??;
    if let Some((host, reply)) =
        super::control::negotiate(&directory, env!("CARGO_PKG_VERSION"), super::PROTOCOL).await?
    {
        match reply {
            super::control::Reply::Retiring => {
                // The retiring host retains ownership through all cleanup. A
                // timeout must never turn into permission to force replacement.
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        if Lease::acquire(&directory)?.is_some() {
                            return Ok::<_, io::Error>(());
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Backend is still shutting down; reopen Rovik after it finishes",
                    )
                })??;
            }
            super::control::Reply::Busy(reason) if host.protocol != PROTOCOL => {
                return Err(io::Error::new(io::ErrorKind::Unsupported, format!("Backend upgrade deferred: {reason}. Finish its work and close its windows, then reopen Rovik.")));
            }
            super::control::Reply::Current | super::control::Reply::Busy(_) => {}
        }
    }
    match connect(&directory).await {
        Ok(connected) => return Ok(connected),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e),
    }
    #[cfg(windows)]
    super::host::wait_for_legacy_windows(data).await?;
    let mut command = prepare()?;
    command.arg("--data").arg(data).arg("--config").arg(config);
    // A dev launcher closes its captured output pipes with the first window.
    // The shared host must own independent handles, including for diagnostics.
    let log_path = directory.join("backend.log");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let output = options.open(&log_path)?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    rovik_core::process::own_session(&mut command);
    rovik_core::process::close_inherited(&mut command);
    #[cfg(windows)]
    let image = std::path::PathBuf::from(command.get_program());
    let mut child = command.spawn()?;
    super::diagnostics::record(
        &directory,
        &format!("launcher_started child_pid={}", child.id()),
    );
    let (exited, exit) = tokio::sync::oneshot::channel();
    let diagnostic_directory = directory.clone();
    std::thread::spawn(move || {
        let result = child.wait();
        super::diagnostics::record(
            &diagnostic_directory,
            &match &result {
                Ok(status) => format!("launcher_exited status={status}"),
                Err(error) => format!(
                    "launcher_wait_failed io_kind={:?} os_code={:?}",
                    error.kind(),
                    error.raw_os_error()
                ),
            },
        );
        match &result {
            Ok(status) if !status.success() => {
                log::warn!("Local backend launcher exited with {status}")
            }
            Err(error) => log::warn!("Couldn't reap the local backend launcher: {error}"),
            _ => {}
        }
        let _ = exited.send(result);
    });
    let connected = tokio::time::timeout(
        Duration::from_secs(30),
        wait_ready(|| connect(&directory), exit, Duration::from_millis(50)),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Local backend did not become ready within 30 seconds",
        )
    })??;
    #[cfg(windows)]
    {
        let images = directory.join("images");
        if image.parent() == Some(images.as_path()) {
            if let Err(error) = super::image::cleanup(&images, &image) {
                log::warn!("Could not clean obsolete backend images: {error}");
            }
        }
    }
    Ok(connected)
}

// Only after spawning: an exposed pipe can time out while the new host is busy.
// Retry within the caller's startup budget, without preparing or spawning again.
// Discovery above still fails closed on timeouts from an existing owner.
async fn wait_ready<T, F, Fut>(
    mut connect: F,
    mut exit: tokio::sync::oneshot::Receiver<io::Result<std::process::ExitStatus>>,
    poll: Duration,
) -> io::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = io::Result<T>>,
{
    let mut launcher_done = false;
    loop {
        match connect().await {
            Ok(connected) => return Ok(connected),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        tokio::select! {
            result = &mut exit, if !launcher_done => {
                launcher_done = true;
                match result {
                    Ok(Ok(status)) if status.success() => {}, // A competing launcher won.
                    Ok(Ok(status)) => return Err(io::Error::other(format!("Local backend launcher exited with {status}"))),
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
            _ = tokio::time::sleep(poll) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn a_slow_new_host_can_become_ready_after_a_handshake_timeout() {
        let (_sender, exit) = tokio::sync::oneshot::channel();
        let calls = Cell::new(0);
        let result = wait_ready(
            || {
                let attempt = calls.get();
                calls.set(attempt + 1);
                async move {
                    match attempt {
                        0 => Err(io::Error::from(io::ErrorKind::NotFound)),
                        1 => Err(io::Error::from(io::ErrorKind::TimedOut)),
                        2 => Err(io::Error::from(io::ErrorKind::ConnectionRefused)),
                        _ => Ok("ready"),
                    }
                }
            },
            exit,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(result, "ready");
        assert_eq!(calls.get(), 4);
    }

    #[tokio::test]
    async fn startup_does_not_retry_authentication_or_protocol_failures() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Unsupported,
            io::ErrorKind::InvalidData,
        ] {
            let (_sender, exit) = tokio::sync::oneshot::channel();
            let calls = Cell::new(0);
            let error = wait_ready(
                || {
                    calls.set(calls.get() + 1);
                    async move { Err::<(), _>(io::Error::from(kind)) }
                },
                exit,
                Duration::ZERO,
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(calls.get(), 1);
        }
    }

    #[tokio::test]
    async fn a_launcher_failure_is_reported_even_after_a_handshake_timeout() {
        let (sender, exit) = tokio::sync::oneshot::channel();
        sender
            .send(Err(io::Error::other("launcher failed")))
            .unwrap();
        let error = wait_ready(
            || async { Err::<(), _>(io::Error::from(io::ErrorKind::TimedOut)) },
            exit,
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "launcher failed");
    }
}

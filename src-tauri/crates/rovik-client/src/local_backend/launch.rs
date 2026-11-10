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
    let mut command = prepare()?;
    command.arg("--data").arg(data).arg("--config").arg(config);
    command.stdin(std::process::Stdio::null());
    rovik_core::process::own_session(&mut command);
    rovik_core::process::close_inherited(&mut command);
    let mut child = command.spawn()?;
    let (exited, mut exit) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = child.wait();
        match &result {
            Ok(status) if !status.success() => {
                log::warn!("Local backend launcher exited with {status}")
            }
            Err(error) => log::warn!("Couldn't reap the local backend launcher: {error}"),
            _ => {}
        }
        let _ = exited.send(result);
    });
    let mut launcher_done = false;
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
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
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

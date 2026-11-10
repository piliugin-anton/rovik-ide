//! Shared desktop host used by the packaged app and standalone daemon.
use super::{control, directory, rpc, Lease};
use crate::desktop_backend::DesktopBackend;
use rovik_core::{agent_run::events::AgentEvent, EventSink, Paths};
use std::{ffi::OsString, path::PathBuf, sync::Arc};

/// Selects headless backend hosting before any native window is initialized.
pub const HOST_FLAG: &str = "--desktop-backend";

/// Parses the explicit directories supplied by a desktop launcher.
pub fn paths(args: impl IntoIterator<Item = OsString>) -> Result<Paths, String> {
    let mut args = args.into_iter();
    let (mut data, mut config) = (None, None);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--data") if data.is_none() => data = args.next().map(PathBuf::from),
            Some("--config") if config.is_none() => config = args.next().map(PathBuf::from),
            _ => {
                return Err(
                    "Usage: --desktop-backend --data <directory> --config <directory>".into(),
                )
            }
        }
    }
    match (data, config) {
        (Some(data), Some(config)) if data.is_absolute() && config.is_absolute() => {
            Ok(Paths::new(data, config))
        }
        _ => Err("Absolute --data and --config directories are required".into()),
    }
}

/// Owns execution until explicitly terminated, independently of UI lifetimes.
pub async fn serve(paths: Paths) -> Result<(), Box<dyn std::error::Error>> {
    struct Events;
    impl EventSink for Events {
        fn agent(&self, _: &AgentEvent) {}
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    let directory = directory(&paths.data_dir);
    let Some(lease) = Lease::acquire(&directory)? else {
        // Another launcher won. It alone initializes the database and mounts.
        return Ok(());
    };
    let backend = Arc::new(
        DesktopBackend::open(
            paths,
            Arc::new(Events),
            Arc::new(|_| {}),
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(std::io::Error::other)?,
    );
    let mut retirement = None;
    let result = async {
        backend.start_services().map_err(std::io::Error::other)?;
        let listener = lease.listen(env!("CARGO_PKG_VERSION"))?;
        let control = control::Listener::bind(&lease, &directory)?;
        let hello = control::Hello::new(listener.boot());
        #[cfg(unix)]
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let termination = async {
            #[cfg(unix)]
            terminate.recv().await;
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        tokio::pin!(termination);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = control.accept() => {
                    match accepted {
                        Ok(mut stream) => {
                            // Do not accept new RPC clients between checking the
                            // active tasks and committing to retirement.
                            let result = async {
                                tokio::time::timeout(control::TIMEOUT, control::write(&mut stream, &hello)).await??;
                                let candidate: control::Candidate = tokio::time::timeout(control::TIMEOUT, control::read(&mut stream)).await??;
                                let mut pause = None;
                                let reply = if !candidate.upgrades(&hello)? {
                                    control::Reply::Current
                                } else {
                                    while let Some(result) = clients.try_join_next() {
                                        if let Err(error) = result { eprintln!("Local backend client task: {error}"); }
                                    }
                                    if !clients.is_empty() {
                                        control::Reply::Busy("Windows or accepted requests are still connected".into())
                                    } else {
                                        match tokio::time::timeout(std::time::Duration::from_secs(1), backend.pause_producers()).await {
                                            Err(_) => control::Reply::Busy("Host maintenance or local remote work is still finishing".into()),
                                            Ok(guard) => {
                                                if let Err(reason) = backend.prepare_replacement().await {
                                                    control::Reply::Busy(reason)
                                                } else {
                                                    pause = Some(guard);
                                                    control::Reply::Retiring
                                                }
                                            }
                                        }
                                    }
                                };
                                tokio::time::timeout(control::TIMEOUT, control::write(&mut stream, &reply)).await??;
                                Ok::<_, std::io::Error>(pause)
                            }.await;
                            match result {
                                Ok(Some(guard)) => { retirement = Some(guard); break; },
                                Ok(None) => {},
                                Err(error) => eprintln!("Local backend upgrade: {error}"),
                            }
                        }
                        Err(error) => eprintln!("Local backend control connection: {error}"),
                    }
                },
                result = listener.accept_pending(), if clients.len() < 64 => {
                    match result {
                        Ok(pending) => {
                            let backend = backend.clone();
                            clients.spawn(async move {
                                let result = async {
                                    let session = pending.session().to_string();
                                    let stream = pending.finish().await?;
                                    rpc::serve(stream, backend, session).await
                                }.await;
                                if let Err(error) = result {
                                    eprintln!("Local backend RPC: {error}");
                                }
                            });
                        },
                        Err(error) => eprintln!("Local backend connection: {error}"),
                    }
                },
                result = clients.join_next(), if !clients.is_empty() => {
                    if let Some(Err(error)) = result {
                        eprintln!("Local backend client task: {error}");
                    }
                },
                _ = &mut termination => break,
                result = tokio::signal::ctrl_c() => { result?; break; },
            }
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        Ok::<_, std::io::Error>(())
    }
    .await;
    backend.shutdown().await;
    drop(retirement);
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_startup_requires_explicit_unambiguous_absolute_directories() {
        let parse = |args: &[&str]| paths(args.iter().map(OsString::from));
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let config = root.path().join("config with spaces");
        let valid = paths([
            OsString::from("--config"),
            config.clone().into_os_string(),
            OsString::from("--data"),
            data.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(valid.data_dir, data);
        assert_eq!(valid.config_dir, config);
        let data = data.to_str().unwrap();
        let config = config.to_str().unwrap();
        for args in [
            vec![],
            vec!["--data", data],
            vec!["--data", "relative", "--config", config],
            vec!["--data", data, "--config", config, "--data", data],
            vec!["--data", data, "--config", config, "--new-window"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}

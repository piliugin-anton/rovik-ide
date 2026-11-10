//! Shared desktop host used by the packaged app and standalone daemon.
use super::{directory, rpc, Lease};
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
    let result = async {
        backend.start_services().map_err(std::io::Error::other)?;
        let listener = lease.listen(env!("CARGO_PKG_VERSION"))?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
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
                _ = terminate.recv() => break,
                result = tokio::signal::ctrl_c() => { result?; break; },
            }
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        Ok::<_, std::io::Error>(())
    }
    .await;
    backend.shutdown().await;
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_startup_requires_explicit_unambiguous_absolute_directories() {
        let parse = |args: &[&str]| paths(args.iter().map(OsString::from));
        let valid = parse(&["--config", "/config with spaces", "--data", "/data"]).unwrap();
        assert_eq!(valid.data_dir, PathBuf::from("/data"));
        assert_eq!(valid.config_dir, PathBuf::from("/config with spaces"));
        for args in [
            vec![],
            vec!["--data", "/data"],
            vec!["--data", "relative", "--config", "/config"],
            vec!["--data", "/data", "--config", "/config", "--data", "/other"],
            vec!["--data", "/data", "--config", "/config", "--new-window"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}

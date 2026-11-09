//! The local desktop backend host, serving authenticated typed RPC.
//! Window attachment and event subscription are separate migration steps.

#[cfg(unix)]
fn main() {
    rovik_core::process::adopt_login_shell_path();
    rovik_core::overlay::confine::require();
    if let Err(error) = rovik_core::rt::block_on(serve()) {
        eprintln!("Local backend: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!(
        "Local backend hosting needs the Windows named-pipe transport, which is not available yet."
    );
    std::process::exit(1);
}

#[cfg(unix)]
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    use rovik_client::{desktop_backend::DesktopBackend, local_backend};
    use rovik_core::{agent_run::events::AgentEvent, EventSink, Paths};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Events;
    impl EventSink for Events {
        fn agent(&self, _: &AgentEvent) {}
    }

    let mut args = std::env::args_os().skip(1);
    let mut data = None;
    let mut config = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--data") if data.is_none() => data = args.next().map(PathBuf::from),
            Some("--config") if config.is_none() => config = args.next().map(PathBuf::from),
            _ => {
                return Err("Usage: rovik-desktopd --data <directory> --config <directory>".into())
            }
        }
    }
    let (data, config) = match (data, config) {
        (Some(data), Some(config)) => (data, config),
        _ => return Err("Both --data and --config are required".into()),
    };
    std::fs::create_dir_all(&data)?;
    let directory = local_backend::directory(&data);
    let Some(lease) = local_backend::Lease::acquire(&directory)? else {
        // Another launcher won. It alone initializes the database and mounts.
        return Ok(());
    };
    let backend = Arc::new(
        DesktopBackend::open(
            Paths::new(data, config),
            Arc::new(Events),
            Arc::new(|_| {}),
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(std::io::Error::other)?,
    );
    let result = async {
        let listener = lease.listen(env!("CARGO_PKG_VERSION"))?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut clients = tokio::task::JoinSet::new();
        let mut refresh = tokio::task::JoinSet::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                result = listener.accept_pending(), if clients.len() < 64 => {
                    match result {
                        Ok(pending) => {
                            let backend = backend.clone();
                            clients.spawn(async move {
                                let result = async {
                                    let stream = pending.finish().await?;
                                    local_backend::rpc::serve(stream, backend).await
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
                _ = tick.tick(), if refresh.is_empty() => {
                    let hosts = backend.hosts.clone();
                    refresh.spawn_blocking(move || hosts.follow());
                },
                result = refresh.join_next(), if !refresh.is_empty() => {
                    if let Some(Err(error)) = result {
                        eprintln!("Local backend host refresh: {error}");
                    }
                },
                _ = terminate.recv() => break,
                result = tokio::signal::ctrl_c() => { result?; break; },
            }
        }
        while refresh.join_next().await.is_some() {}
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        Ok::<_, std::io::Error>(())
    }
    .await;
    backend.shutdown().await;
    result?;
    Ok(())
}

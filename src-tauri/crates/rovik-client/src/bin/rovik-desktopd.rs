//! The local desktop backend host. The initial endpoint advertises
//! readiness only; execution RPC is enabled by the command-routing phase.

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
    let backend = DesktopBackend::open(
        Paths::new(data, config),
        Arc::new(Events),
        Arc::new(|_| {}),
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .map_err(std::io::Error::other)?;
    let result = async {
        let listener = lease.listen(env!("CARGO_PKG_VERSION"))?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok(stream) => drop(stream),
                        Err(error) => eprintln!("Local backend connection: {error}"),
                    }
                },
                _ = terminate.recv() => break,
                result = tokio::signal::ctrl_c() => { result?; break; },
            }
        }
        Ok::<_, std::io::Error>(())
    }
    .await;
    backend.shutdown().await;
    result?;
    Ok(())
}

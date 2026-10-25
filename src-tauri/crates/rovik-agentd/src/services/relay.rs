//! A service's published ports, as ports of the user's private network
//! (D20, D21): what the project's code reaches at `localhost:5432` is the
//! user's own database, whatever other users of the server run. Nothing is
//! published on the server: each port listens in the network (the socket
//! made inside it, `Network::listen_on`), and each connection to it is
//! carried to the service's port in its container, a connection made in
//! the container's own namespaces, which are this user's
//! (`network::Namespaces`). No other user reaches it: their processes are
//! in networks of their own, and can't enter this user's namespaces.

use super::podman::Podman;
use rovik_core::overlay::confine::network::{Namespaces, Network};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The ports relayed, each for one service.
pub struct Relays {
    network: Arc<Network>,
    ports: Mutex<HashMap<u16, Relay>>,
}

/// One port's relay: what it carries to, and the tasks accepting on it.
struct Relay {
    project: String,
    service: String,
    target: u16,
    accepting: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        for task in &self.accepting {
            task.abort();
        }
    }
}

impl Relays {
    /// The relays into `network`.
    pub fn new(network: Arc<Network>) -> Relays {
        Relays {
            network,
            ports: Mutex::new(HashMap::new()),
        }
    }

    /// Port `published` of the network carried to port `target` of
    /// `service`'s container in the Compose project `project`, from now on:
    /// why not, when something else holds the port in the network.
    pub async fn open(
        &self,
        podman: Arc<Podman>,
        project: &str,
        service: &str,
        published: u16,
        target: u16,
    ) -> Result<(), String> {
        {
            let ports = self.ports.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(relay) = ports.get(&published) {
                if relay.project == project && relay.service == service && relay.target == target {
                    return Ok(());
                }
                return Err(format!(
                    "port {published} of this user's network is another service's: `{}` of the \
                     Compose project {}, which keeps it until it's stopped (`docker compose \
                     down` in its project)",
                    relay.service, relay.project
                ));
            }
        }
        let net = self.network.clone();
        let listening = tokio::task::spawn_blocking(move || net.listen_on(published))
            .await
            .map_err(|e| e.to_string())?;
        let listening = match listening {
            Ok(listening) => listening,
            Err(e) if e.raw_os_error() == Some(libc::EADDRINUSE) => {
                let net = self.network.clone();
                let holders = tokio::task::spawn_blocking(move || net.listening_on(published))
                    .await
                    .unwrap_or_default();
                let held = holders
                    .iter()
                    .map(|(pid, command)| match command {
                        Some(command) => format!("`{command}` (pid {pid})"),
                        None => format!("pid {pid}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" and ");
                return Err(format!(
                    "port {published} of this user's network is held by {}: stop it, and the \
                     service's port is relayed when the service starts again",
                    match held.is_empty() {
                        true => "another process of theirs".to_string(),
                        false => held,
                    }
                ));
            }
            Err(e) => {
                return Err(format!(
                    "port {published} couldn't listen in this user's network: {e}"
                ))
            }
        };
        let mut accepting = Vec::new();
        for listener in listening {
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("port {published}: {e}"))?;
            let listener = tokio::net::TcpListener::from_std(listener)
                .map_err(|e| format!("port {published}: {e}"))?;
            let (podman, project, service) =
                (podman.clone(), project.to_string(), service.to_string());
            accepting.push(tokio::spawn(accept(
                listener, podman, project, service, target,
            )));
        }
        let relay = Relay {
            project: project.to_string(),
            service: service.to_string(),
            target,
            accepting,
        };
        self.ports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(published, relay);
        Ok(())
    }
}

/// Accepts on `listener` until the relay closes, each connection carried to
/// `target` in `service`'s container.
async fn accept(
    listener: tokio::net::TcpListener,
    podman: Arc<Podman>,
    project: String,
    service: String,
    target: u16,
) {
    let held: Arc<Mutex<Option<Arc<Namespaces>>>> = Arc::new(Mutex::new(None));
    loop {
        let (client, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                log::warn!("services: accepting for {project}'s {service}: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let (podman, project, service, held) = (
            podman.clone(),
            project.clone(),
            service.clone(),
            held.clone(),
        );
        tokio::spawn(async move {
            match reach(&podman, &project, &service, target, &held).await {
                Ok(inside) => carry(client, inside).await,
                // The client sees its connection closed, as a stopped
                // service's would be refused.
                Err(why) => log::info!("services: {project}'s {service}: {why}"),
            }
        });
    }
}

/// A connection to `target` in `service`'s container, made in its
/// namespaces: those `held` from an earlier connection, or the container's
/// looked up again when those reach nothing (it was started again, and
/// has namespaces of its own).
async fn reach(
    podman: &Podman,
    project: &str,
    service: &str,
    target: u16,
    held: &Mutex<Option<Arc<Namespaces>>>,
) -> Result<std::net::TcpStream, String> {
    let known = held.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(known) = known {
        if let Ok(Ok(inside)) = tokio::task::spawn_blocking(move || known.connect(target)).await {
            return Ok(inside);
        }
    }
    let pid = podman
        .pid_of(project, service)
        .await
        .ok_or_else(|| "its container doesn't run".to_string())?;
    let found = tokio::task::spawn_blocking(move || Namespaces::of(pid))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("its container's namespaces: {e}"))?;
    let found = Arc::new(found);
    *held.lock().unwrap_or_else(|e| e.into_inner()) = Some(found.clone());
    tokio::task::spawn_blocking(move || found.connect(target))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("port {target} in its container: {e}"))
}

/// Bytes both ways between `client` and `inside` until either closes.
async fn carry(mut client: tokio::net::TcpStream, inside: std::net::TcpStream) {
    let inside = inside
        .set_nonblocking(true)
        .and_then(|()| tokio::net::TcpStream::from_std(inside));
    match inside {
        Ok(mut inside) => {
            let _ = tokio::io::copy_bidirectional(&mut client, &mut inside).await;
        }
        Err(e) => log::warn!("services: a relayed connection: {e}"),
    }
}

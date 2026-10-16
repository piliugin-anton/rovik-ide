//! Each host's client, by the host's id: what the app keeps of its servers
//! (P08 makes it the app's state), with each one's connection state.

use crate::client::{Client, HostState, Identity, Sink};
use crate::connector::Connector;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
pub struct Hosts {
    clients: Mutex<BTreeMap<String, Client>>,
}

impl Hosts {
    /// Connects to `host`, in place of an earlier connection to it.
    pub fn connect(
        &self,
        host: &str,
        connector: impl Connector,
        identity: Identity,
        sink: Sink,
    ) -> Client {
        let client = Client::connect(connector, identity, sink);
        // The one before stops now, whatever calls still hold a clone of it:
        // two connections would feed the same events twice.
        if let Some(old) = self.lock().insert(host.to_string(), client.clone()) {
            old.stop();
        }
        client
    }

    pub fn client(&self, host: &str) -> Option<Client> {
        self.lock().get(host).cloned()
    }

    /// The hosts connected now, with their clients.
    pub fn ready(&self) -> Vec<(String, Client)> {
        let clients = self.lock();
        clients
            .iter()
            .filter(|(_, client)| client.state() == HostState::Ready)
            .map(|(host, client)| (host.clone(), client.clone()))
            .collect()
    }

    /// Each host's state, by its id.
    pub fn states(&self) -> Vec<(String, HostState)> {
        let clients = self.lock();
        clients
            .iter()
            .map(|(host, client)| (host.clone(), client.state()))
            .collect()
    }

    /// Forgets the host: its connection closes now.
    pub fn disconnect(&self, host: &str) {
        if let Some(old) = self.lock().remove(host) {
            old.stop();
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Client>> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{identity, Nowhere};
    use std::sync::Arc;

    #[tokio::test]
    async fn each_host_has_its_client_and_its_state() {
        let hosts = Hosts::default();
        let sink: Sink = Arc::new(|_| {});
        hosts.connect("b", Nowhere, identity("b"), sink.clone());
        hosts.connect("a", Nowhere, identity("a"), sink.clone());
        let named: Vec<String> = hosts.states().into_iter().map(|(h, _)| h).collect();
        assert_eq!(named, ["a", "b"]);
        assert!(hosts.client("a").is_some());
        hosts.disconnect("a");
        assert!(hosts.client("a").is_none());
        assert_eq!(hosts.states().len(), 1);
    }
}

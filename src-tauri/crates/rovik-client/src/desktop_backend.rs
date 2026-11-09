//! Desktop execution services, independent of the native window host.
//!
//! During migration the desktop hosts this service in-process. A local
//! daemon can construct the same service with its own event publishers.

use crate::hosts_page::HostsPage;
use crate::provider_keys::ProviderKeys;
use crate::secrets::{SecretError, SecretName, SecretStore};
use crate::{CloudSink, Remote};
use rovik_core::{Core, EventSink, Paths};
use std::sync::Arc;

/// The services owned together by the desktop execution backend.
/// Construct once per process; core overlay and network initialization
/// configure process-wide services.
pub struct DesktopBackend {
    /// Chat persistence, execution, MCP, and virtual files.
    pub core: Core,
    /// Credentials shared by provider, MCP, and host services.
    pub secrets: Arc<SecretStore>,
    /// The provider settings and their credential source.
    pub keys: Arc<ProviderKeys>,
    /// Routing and synchronization for chats on remote hosts.
    pub remote: Arc<Remote>,
    /// Host configuration and connection management.
    pub hosts: Arc<HostsPage>,
}

impl DesktopBackend {
    /// Initializes execution services. Event publishers are installed
    /// before saved remote chat locations are loaded.
    pub async fn open(
        paths: Paths,
        events: Arc<dyn EventSink>,
        cloud_events: CloudSink,
        version: &str,
    ) -> Result<Self, String> {
        let config = paths.config_dir.clone();
        let data = paths.data_dir.clone();
        rovik_core::os_lock::take_slot(&data.join("slots"));
        rovik_core::chat_hold::init(data.join("chats-at-work"));
        let secrets = Arc::new(SecretStore::open(config.join("secrets.json")));
        let store = secrets.clone();
        rovik_core::net::init(
            config.join("network-settings.json"),
            Arc::new(move |proxy: &str| {
                match store.get(&SecretName::ProxyPassword {
                    proxy: proxy.into(),
                }) {
                    Ok(password) => Ok(Some(password)),
                    Err(SecretError::Missing(_)) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }),
        );
        let keys = Arc::new(ProviderKeys::new(
            config.join("ai-settings.json"),
            secrets.clone(),
        ));
        let core = Core::new(paths, events)?;
        let store = secrets.clone();
        core.mcp
            .set_secrets(Arc::new(move |server: &str, key: &str| {
                match store.get(&SecretName::McpValue {
                    server: server.into(),
                    key: key.into(),
                }) {
                    Ok(value) => Ok(Some(value.0.clone())),
                    Err(SecretError::Missing(_)) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }));
        core.mcp
            .set_grants(Arc::new(crate::mcp_sign_in::KeychainGrants(
                secrets.clone(),
            )));
        let remote = Remote::new(core.clone());
        remote.on_cloud(cloud_events);
        if let Err(error) = remote.load().await {
            core.shutdown().await;
            return Err(format!("Couldn't load remote chat locations: {error}"));
        }
        let hosts = Arc::new(HostsPage::new(
            config,
            data.join("ssh"),
            secrets.clone(),
            remote.clone(),
            version,
        ));
        let page = Arc::downgrade(&hosts);
        remote.on_refresh(Arc::new(move || {
            if let Some(hosts) = page.upgrade() {
                hosts.follow_policies();
            }
        }));
        remote.on_sign_outs(Arc::new(Arc::downgrade(&hosts)));
        Ok(Self {
            core,
            secrets,
            keys,
            remote,
            hosts,
        })
    }

    /// Routes a typed operation using its own chat identity, never the
    /// project or chat selected in a window.
    pub async fn dispatch(
        &self,
        request: rovik_protocol::Request,
    ) -> Result<rovik_protocol::Response, rovik_protocol::Fail> {
        use rovik_protocol::{Request, Response, Target};
        match request {
            // The desktop checklist reads the local rows (including remote
            // mirrors), so it remains available while the host is offline.
            request @ Request::ChatChecklist { .. } => {
                return rovik_core::dispatch(&self.core, request).await
            }
            Request::AgentStatus => return self.remote.status().await.map(Response::AgentStatus),
            Request::AgentStart(start) => return self.remote.start(*start).await,
            Request::AgentSteer { dialog_id, message } => {
                return self.remote.steer(&dialog_id, message).await
            }
            Request::AgentUnsteer {
                dialog_id,
                message_id,
            } => return self.remote.unsteer(&dialog_id, message_id).await,
            Request::ChatDelete { dialog_id } => {
                return self
                    .remote
                    .delete(&dialog_id)
                    .await
                    .map(|()| Response::Done)
            }
            request @ (Request::AgentConfigure(_) | Request::AgentSetEffort { .. }) => {
                return self.remote.everywhere(request).await
            }
            _ => {}
        }
        match request.target().map(|(target, _)| target) {
            Some(Target::Chat(id)) => {
                let id = id.to_string();
                self.remote.route(&id, request).await
            }
            Some(Target::Message(id)) => {
                let id = id.to_string();
                self.remote.route_message(&id, request).await
            }
            None => rovik_core::dispatch(&self.core, request).await,
        }
    }

    /// Stops local execution and flushes its persisted state before the
    /// hosting process exits. Window disconnection must not call this
    /// once the service moves into a shared process.
    pub async fn shutdown(&self) {
        self.core.shutdown().await;
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::testing::{identity, Fake};
    use rovik_core::agent_run::events::AgentEvent;
    use rovik_core::chat_db::{api, store};
    use rovik_protocol::{Request, Response};
    use std::time::Duration;

    struct Events;
    impl EventSink for Events {
        fn agent(&self, _: &AgentEvent) {}
    }

    // Core::open avoids process-wide network and overlay initialization,
    // so these routing tests can run beside other client unit tests.
    fn backend(root: &std::path::Path) -> DesktopBackend {
        let config = root.join("config");
        let core = Core::open(
            Paths::new(root.join("data"), config.clone()),
            Arc::new(Events),
        )
        .unwrap();
        let secrets = Arc::new(SecretStore::open(config.join("secrets.json")));
        let keys = Arc::new(ProviderKeys::new(
            config.join("ai-settings.json"),
            secrets.clone(),
        ));
        let remote = Remote::new(core.clone());
        let hosts = Arc::new(HostsPage::new(
            config,
            root.join("ssh"),
            secrets.clone(),
            remote.clone(),
            "test",
        ));
        DesktopBackend {
            core,
            secrets,
            keys,
            remote,
            hosts,
        }
    }

    #[tokio::test]
    async fn routing_uses_chat_and_message_ownership_and_includes_remote_status() {
        let dir = tempfile::tempdir().unwrap();
        let backend = backend(dir.path());
        for id in ["local", "remote"] {
            api::chat_create_dialog(
                &backend.core.db,
                store::NewDialog {
                    id: id.into(),
                    title: "original".into(),
                    created_at: 1,
                    updated_at: 1,
                    project_id: None,
                },
            )
            .await
            .unwrap();
        }
        let fake = Fake::default();
        backend
            .remote
            .connect("host", fake.clone(), identity("host"));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !backend.remote.caught_up("host") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        backend.remote.place("remote", "host");
        backend
            .core
            .db
            .write(|conn| {
                let message: store::NewMessage = serde_json::from_value(serde_json::json!({
                    "id": "remote-message", "role": "assistant", "content": "old", "createdAt": 1
                }))
                .unwrap();
                store::append_messages(conn, "remote", &[message]).map(|_| ())
            })
            .await
            .unwrap();
        fake.requests.lock().unwrap().clear();

        let local = Request::ChatRename {
            dialog_id: "local".into(),
            title: "local change".into(),
        };
        backend.dispatch(local.clone()).await.unwrap();
        assert!(!fake.requests.lock().unwrap().contains(&local));
        assert_eq!(
            api::chat_get_dialog(&backend.core.db, "local".into())
                .await
                .unwrap()
                .unwrap()
                .title,
            "local change"
        );

        let remote = Request::ChatRename {
            dialog_id: "remote".into(),
            title: "remote change".into(),
        };
        backend.dispatch(remote.clone()).await.unwrap();
        assert!(fake.requests.lock().unwrap().contains(&remote));

        let update = Request::ChatUpdateMessage {
            message_id: "remote-message".into(),
            patch: serde_json::from_value(serde_json::json!({ "content": "new" })).unwrap(),
        };
        backend.dispatch(update.clone()).await.unwrap();
        assert!(fake.requests.lock().unwrap().contains(&update));

        fake.requests.lock().unwrap().clear();
        backend
            .dispatch(Request::ChatChecklist {
                dialog_id: "remote".into(),
            })
            .await
            .unwrap();
        assert!(!fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::ChatChecklist { .. })));
        assert!(matches!(
            backend.dispatch(Request::AgentStatus).await.unwrap(),
            Response::AgentStatus(_)
        ));
        assert!(fake
            .requests
            .lock()
            .unwrap()
            .contains(&Request::AgentStatus));
        backend.remote.disconnect("host");
        use crate::desktop_api::{overlay::OverlayRequest, DesktopRequest};
        // A disconnected remote chat must never mutate a local overlay as a
        // fallback. Redo also retains its existing local-only restriction.
        assert!(DesktopRequest::from(OverlayRequest::Discard {
            dialog_id: "remote".into(),
        })
        .dispatch(&backend)
        .await
        .is_err());
        assert_eq!(
            DesktopRequest::from(OverlayRequest::RedoGroup {
                dialog_id: "remote".into(),
                group: "group".into(),
            })
            .dispatch(&backend)
            .await
            .unwrap_err(),
            crate::LOCAL_ONLY
        );
    }
}

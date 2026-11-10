//! Desktop execution services, independent of the native window host.
//!
//! Unix desktops use a local daemon. Platforms awaiting IPC transport host
//! the same service in-process with identical event publishers.

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
    maintenance: crate::maintenance::Maintenance,
    /// Shared workload settings and session-owned focus selections.
    pub workload: Arc<crate::workload::WorkloadState>,
    /// Cancellable editor formatter workers.
    pub formatters: Arc<crate::formatter::FormatterState>,
    /// Private, one-shot page requests for the owning UI client.
    pub pages: Arc<crate::desktop_pages::PageRouter>,
    /// Shared Chrome installation, independent of its observing windows.
    pub browser_install: Arc<crate::browser_install::BrowserInstaller>,
    /// Replay history for all UI clients.
    pub journal: Arc<crate::desktop_journal::Journal>,
    /// Serializes ownership checks with project opens and destructive changes.
    pub(crate) project_changes: tokio::sync::Mutex<()>,
    /// Language servers and their process-tree cleanup workers.
    pub lsp: Arc<crate::lsp::LspState>,
    /// Terminals run by this backend, with per-client ownership.
    pub terminals: Arc<crate::terminal::TerminalState>,
    /// Client resource lifetimes and private event streams.
    pub sessions: crate::desktop_session::Sessions,
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
        let journal = Arc::new(crate::desktop_journal::Journal::default());
        struct Published {
            journal: Arc<crate::desktop_journal::Journal>,
            observer: Arc<dyn EventSink>,
        }
        impl EventSink for Published {
            fn agent(&self, event: &rovik_core::agent_run::events::AgentEvent) {
                self.journal.agent(event);
                self.observer.agent(event);
            }
        }
        let events = Arc::new(Published {
            journal: journal.clone(),
            observer: events,
        });
        let journal_cloud = journal.clone();
        let observer = cloud_events;
        let cloud_events: CloudSink = Arc::new(move |event| {
            journal_cloud.cloud(&event);
            observer(event);
        });
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
        let pages = Arc::new(crate::desktop_pages::PageRouter::default());
        let opener = pages.clone();
        let remote = Remote::new(core.clone());
        remote.on_open_page(Arc::new(move |page| opener.open(page)));
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
        let workload = crate::workload::WorkloadState::new(remote.clone());
        Ok(Self {
            maintenance: Default::default(),
            workload,
            formatters: Default::default(),
            pages,
            journal,
            browser_install: Default::default(),
            project_changes: Default::default(),
            sessions: Default::default(),
            lsp: Default::default(),
            terminals: Arc::new(crate::terminal::terminals()),
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

    /// Starts host connections after the host has installed ownership callbacks.
    /// Repeated calls share one worker; shutdown permanently prevents restart.
    pub fn start_services(&self) -> Result<(), String> {
        self.maintenance
            .start(self.remote.clone(), self.hosts.clone())
    }

    /// Freezes host discovery and drains accepted local remote work before pausing results.
    #[cfg(unix)]
    pub(crate) async fn pause_producers(
        &self,
    ) -> (
        tokio::sync::OwnedMutexGuard<()>,
        tokio::sync::OwnedMutexGuard<()>,
    ) {
        let maintenance = self.maintenance.pause().await;
        let results = self.remote.pause_for_upgrade().await;
        (maintenance, results)
    }

    /// The host must keep admission closed and producers paused until this
    /// returns, including if the negotiating client disconnects.
    #[cfg(unix)]
    pub(crate) async fn prepare_replacement(&self) -> Result<(), String> {
        if let Some(reason) = self.service_replacement_blocker() {
            return Err(reason.into());
        }
        let before = self.remote.states();
        let drained = self
            .remote
            .drain_for_upgrade(std::time::Duration::from_secs(1))
            .await;
        let after = self.remote.states();
        let removed: Vec<String> = before
            .into_iter()
            .filter(|(id, _)| !after.iter().any(|(current, _)| current == id))
            .map(|(id, _)| id)
            .collect();
        self.hosts.retry_drained(&removed);
        drained?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let core = self.core.clone();
            // Join this work without a cancellation timeout: a detached unmount
            // must never overlap a resumed host accepting requests.
            tokio::task::spawn_blocking(move || core.prepare_replacement())
                .await
                .map_err(|error| format!("Could not drain the virtual filesystem: {error}"))?
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        self.core
            .replacement_blocker()
            .map_or(Ok(()), |reason| Err(reason.into()))
    }

    /// The host must stop accepting clients and hold the maintenance pause first.
    #[cfg(unix)]
    fn service_replacement_blocker(&self) -> Option<&'static str> {
        if crate::desktop_task::active() {
            return Some("Client operations are still finishing");
        }
        if self.browser_install.active() {
            return Some("Browser installation is still running");
        }
        None
    }

    /// Stops local execution and flushes its persisted state before the
    /// hosting process exits. Window disconnection must not call this
    /// once the service moves into a shared process.
    pub async fn shutdown(&self) {
        self.maintenance.stop().await;
        self.remote.stop_results();
        self.sessions.close_all();
        self.workload.shutdown().await;
        self.terminals.end_all();
        let lsp = self.lsp.clone();
        if let Err(error) = rovik_core::rt::spawn_blocking(move || lsp.shutdown()).await {
            log::error!("Could not wait for language-server cleanup: {error}");
        }
        let formatters = self.formatters.clone();
        if let Err(error) = rovik_core::rt::spawn_blocking(move || formatters.shutdown()).await {
            log::error!("Could not wait for formatter cleanup: {error}");
        }
        self.browser_install.shutdown().await;
        self.core.shutdown().await;
        let terminals = self.terminals.clone();
        if let Err(error) = rovik_core::rt::spawn_blocking(move || terminals.wait_ended()).await {
            log::error!("Could not wait for terminal cleanup: {error}");
        }
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
        let pages = Arc::new(crate::desktop_pages::PageRouter::default());
        let opener = pages.clone();
        let remote = Remote::new(core.clone());
        remote.on_open_page(Arc::new(move |page| opener.open(page)));
        let hosts = Arc::new(HostsPage::new(
            config,
            root.join("ssh"),
            secrets.clone(),
            remote.clone(),
            "test",
        ));
        let workload = crate::workload::WorkloadState::new(remote.clone());
        DesktopBackend {
            maintenance: Default::default(),
            workload,
            formatters: Default::default(),
            pages,
            journal: Default::default(),
            browser_install: Default::default(),
            project_changes: Default::default(),
            sessions: Default::default(),
            lsp: Default::default(),
            terminals: Arc::new(crate::terminal::terminals()),
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
        *fake.status.lock().unwrap() = Some(rovik_protocol::agent::AgentStatus {
            runs: vec![rovik_protocol::agent::RunningRun {
                dialog_id: "remote".into(),
                run_id: "running".into(),
                steerable: true,
            }],
            questions: vec![rovik_protocol::events::QuestionsRequest {
                dialog_id: "remote".into(),
                tool_call_id: "question".into(),
                questions: vec![],
            }],
            ..Default::default()
        });
        let unrelated = crate::desktop_journal::Scope {
            project_id: Some("another-project".into()),
            chats: vec![],
        };
        let hidden = crate::desktop_journal::status(&backend, unrelated.clone())
            .await
            .unwrap();
        assert!(hidden.runs.is_empty() && hidden.questions.is_empty());
        let followed = crate::desktop_journal::status(
            &backend,
            crate::desktop_journal::Scope {
                chats: vec!["remote".into()],
                ..unrelated
            },
        )
        .await
        .unwrap();
        assert_eq!(followed.runs.len(), 1);
        assert_eq!(followed.questions.len(), 1);
        let no_project = crate::desktop_journal::status(&backend, Default::default())
            .await
            .unwrap();
        assert_eq!(no_project.runs.len(), 1);
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

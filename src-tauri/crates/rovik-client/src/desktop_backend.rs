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

    /// Stops local execution and flushes its persisted state before the
    /// hosting process exits. Window disconnection must not call this
    /// once the service moves into a shared process.
    pub async fn shutdown(&self) {
        self.core.shutdown().await;
    }
}

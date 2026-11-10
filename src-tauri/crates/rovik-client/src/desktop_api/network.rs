//! Backend-owned network operations.

use crate::desktop_backend::DesktopBackend;
use crate::secrets::{SecretName, StoreKind};
use rovik_core::net::{self, NetworkSettings, NetworkView};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Desktop network operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum NetworkRequest {
    /// network_settings on the backend.
    Settings,
    /// Resolved HTTP settings for the native updater, with memory-only credentials.
    ClientSettings,
    /// network_save on the backend.
    Save {
        settings: NetworkSettings,
        passwords: Option<HashMap<String, Secret>>,
    },
}

impl NetworkRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::ClientSettings => {
                super::encode_result(Ok::<_, String>(net::client_settings().await))
            }
            Self::Settings => super::encode_result(network_settings(backend).await),
            Self::Save {
                settings,
                passwords,
            } => super::encode_result(network_save(backend, settings, passwords).await),
        }
    }
}

/// The page: the settings, and where the proxies' passwords go.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPage {
    #[serde(flatten)]
    view: NetworkView,
    /// The keychain, or the encrypted file in its place and whether its
    /// passphrase was given (Cloud hosts' `hosts_unlock`).
    store: StoreKind,
}

async fn network_settings(backend: &DesktopBackend) -> Result<NetworkPage, String> {
    let secrets = &backend.secrets;
    let secrets = Arc::clone(secrets);
    rovik_core::rt::spawn_blocking(move || NetworkPage {
        view: net::view(),
        store: secrets.kind(),
    })
    .await
    .map_err(|e| e.to_string())
}

async fn network_save(
    backend: &DesktopBackend,
    settings: NetworkSettings,
    passwords: Option<HashMap<String, Secret>>,
) -> Result<NetworkPage, String> {
    let secrets = &backend.secrets;
    let remote = &backend.remote;
    let theirs = net::host_network().await;
    let secrets = Arc::clone(secrets);
    let page = rovik_core::rt::spawn_blocking(move || {
        let passwords: HashMap<String, Option<Secret>> = passwords
            .unwrap_or_default()
            .into_iter()
            .map(|(id, password)| (id, Some(password).filter(|p| !p.0.is_empty())))
            .collect();
        net::check(&settings, &passwords)?;
        for (id, password) in &passwords {
            let name = SecretName::ProxyPassword { proxy: id.clone() };
            match password {
                Some(password) => secrets.set(&name, password),
                None => secrets.delete(&name),
            }
            .map_err(|e| e.to_string())?;
        }
        let before = net::view().settings.proxies;
        let view = net::save(settings, &passwords)?;
        for gone in before
            .iter()
            .filter(|p| view.settings.proxy(&p.id).is_none())
        {
            let name = SecretName::ProxyPassword {
                proxy: gone.id.clone(),
            };
            if let Err(e) = secrets.delete(&name) {
                log::warn!(
                    "network: the password of the proxy {} removed stays: {e}",
                    gone.id
                );
            }
        }
        Ok::<_, String>(NetworkPage {
            view,
            store: secrets.kind(),
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    if net::host_network().await != theirs {
        remote.send_host_network().await;
    }
    Ok(page)
}

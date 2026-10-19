//! Settings → Network: how long connecting may take, the proxy, and
//! whether cloud hosts' connections and the agent's browsers go through
//! it. Thin wrappers over `rovik_core::net`; the custom proxy's password
//! goes in the keychain, and the hosts are told their browsers' proxy.

use rovik_client::secrets::{SecretName, SecretStore, StoreKind};
use rovik_client::Remote;
use rovik_core::net::{self, NetworkSettings, NetworkView};
use rovik_protocol::Secret;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// The page: the settings, and where the proxy's password goes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPage {
    #[serde(flatten)]
    view: NetworkView,
    /// The keychain, or the encrypted file in its place and whether its
    /// passphrase was given (Cloud hosts' `hosts_unlock`).
    store: StoreKind,
}

/// The settings, the proxy "System proxy" uses now, and where the
/// password goes. Off the async threads, as finding the keychain may take
/// a moment.
#[tauri::command]
pub async fn network_settings(secrets: State<'_, Arc<SecretStore>>) -> Result<NetworkPage, String> {
    let secrets = Arc::clone(&secrets);
    rovik_core::rt::spawn_blocking(move || NetworkPage {
        view: net::view(),
        store: secrets.kind(),
    })
    .await
    .map_err(|e| e.to_string())
}

/// Keeps the settings, and the proxy's password: none leaves it as it is,
/// an empty one forgets it. Off the async threads, as the keychain may
/// take a moment or ask the user. When what the cloud hosts' browsers go
/// through changed, the connected hosts are told.
#[tauri::command]
pub async fn network_save(
    secrets: State<'_, Arc<SecretStore>>,
    remote: State<'_, Arc<Remote>>,
    settings: NetworkSettings,
    password: Option<Secret>,
) -> Result<NetworkPage, String> {
    let theirs = net::cloud_browser_proxy().await;
    let secrets = Arc::clone(&secrets);
    let page = rovik_core::rt::spawn_blocking(move || {
        if let (Some(password), Some(proxy)) = (&password, &settings.custom) {
            proxy.check_password(password)?;
        }
        // The settings first: ones that can't be used change nothing.
        let view = net::save(settings)?;
        let view = match password {
            None => view,
            Some(password) if password.0.is_empty() => {
                secrets
                    .delete(&SecretName::ProxyPassword)
                    .map_err(|e| e.to_string())?;
                net::password_saved(None)?
            }
            Some(password) => {
                secrets
                    .set(&SecretName::ProxyPassword, &password)
                    .map_err(|e| e.to_string())?;
                net::password_saved(Some(password))?
            }
        };
        Ok::<_, String>(NetworkPage {
            view,
            store: secrets.kind(),
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    if net::cloud_browser_proxy().await != theirs {
        remote.send_browser_proxy().await;
    }
    Ok(page)
}

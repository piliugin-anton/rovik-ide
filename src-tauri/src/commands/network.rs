//! Settings → Network: how long connecting may take, the proxies, and the
//! way each kind of connection goes, here and on cloud hosts. Thin
//! wrappers over `rovik_core::net`; the proxies' passwords go in the
//! keychain, and the connected hosts are told how to connect.

use rovik_client::secrets::{SecretName, SecretStore, StoreKind};
use rovik_client::Remote;
use rovik_core::net::{self, NetworkSettings, NetworkView};
use rovik_protocol::Secret;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;

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

/// The settings, this computer's proxy now, and where the passwords go.
/// Off the async threads, as finding the keychain may take a moment.
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

/// Keeps the settings, and the passwords given, by proxy: an empty one
/// forgets it, one not given stays as it is. A proxy left out of the
/// settings goes, its password with it. The passwords go in first, so
/// settings that can't be used, or a keychain that's locked, change
/// nothing. Off the async threads, as the keychain may take a moment or
/// ask the user. When how cloud hosts connect changed, the connected hosts
/// are told.
#[tauri::command]
pub async fn network_save(
    secrets: State<'_, Arc<SecretStore>>,
    remote: State<'_, Arc<Remote>>,
    settings: NetworkSettings,
    passwords: Option<HashMap<String, Secret>>,
) -> Result<NetworkPage, String> {
    let theirs = net::host_network().await;
    let secrets = Arc::clone(&secrets);
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

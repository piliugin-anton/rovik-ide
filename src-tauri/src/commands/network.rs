//! Settings → Network: how long connecting may take, the proxies, and the
//! way each kind of connection goes, here and on cloud hosts. Thin
//! wrappers over `rovik_core::net`; the proxies' passwords go in the
//! keychain, and the connected hosts are told how to connect.

use rovik_client::desktop_api::{network::NetworkRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_core::net::NetworkSettings;
use rovik_protocol::Secret;
use std::collections::HashMap;
use tauri::State;

#[tauri::command]
pub async fn network_settings(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(NetworkRequest::Settings))
        .await
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
    backend: State<'_, DesktopConnection>,
    settings: NetworkSettings,
    passwords: Option<HashMap<String, Secret>>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(NetworkRequest::Save {
            settings,
            passwords,
        }))
        .await
}

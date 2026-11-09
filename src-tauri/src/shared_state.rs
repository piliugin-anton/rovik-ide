//! Shared UI state is merged by the backend, one map entry at a time.
use rovik_client::{
    desktop_api::settings::SettingsRequest, desktop_connection::DesktopConnection,
    shared_state::Entries,
};
use serde_json::Value;
use tauri::State;

/// Forwards the shared state read to the backend.
#[tauri::command]
pub async fn shared_state_read(
    backend: State<'_, DesktopConnection>,
    name: String,
) -> Result<Value, String> {
    backend
        .desktop(SettingsRequest::ReadShared { name }.into())
        .await
}

/// Forwards the shared state start to the backend.
#[tauri::command]
pub async fn shared_state_start(
    backend: State<'_, DesktopConnection>,
    name: String,
    entries: Entries,
) -> Result<Value, String> {
    backend
        .desktop(SettingsRequest::StartShared { name, entries }.into())
        .await
}

/// Forwards the shared state merge to the backend.
#[tauri::command]
pub async fn shared_state_merge(
    backend: State<'_, DesktopConnection>,
    name: String,
    patch: Entries,
) -> Result<Value, String> {
    backend
        .desktop(SettingsRequest::MergeShared { name, patch }.into())
        .await
}

//! WSL setup and lifecycle are owned by the backend alongside other hosts.
use rovik_client::{
    desktop_api::hosts::HostsRequest, desktop_connection::DesktopConnection, hosts_page::WSL_HOST,
};
use serde_json::Value;
use tauri::{State, WebviewWindow};

/// Reads the backend's WSL status.
#[tauri::command]
pub async fn wsl_status(backend: State<'_, DesktopConnection>) -> Result<Value, Value> {
    backend.desktop_raw(HostsRequest::WslStatus.into()).await
}

/// Sets up the backend in the selected WSL distribution.
#[tauri::command]
pub async fn wsl_setup(
    backend: State<'_, DesktopConnection>,
    distro: String,
    user: Option<String>,
) -> Result<Value, Value> {
    backend
        .desktop_raw(HostsRequest::WslSetup { distro, user }.into())
        .await
}

/// Reports WSL test progress only to the requesting window.
#[tauri::command]
pub async fn wsl_test(
    window: WebviewWindow,
    backend: State<'_, DesktopConnection>,
) -> Result<Value, Value> {
    super::hosts::hosts_test(window, backend, WSL_HOST.into()).await
}

/// Runs the OS installer; Windows still asks the user for elevation.
#[tauri::command]
pub async fn wsl_install(
    backend: State<'_, DesktopConnection>,
    distro: Option<String>,
) -> Result<Value, Value> {
    backend
        .desktop_raw(HostsRequest::WslInstall { distro }.into())
        .await
}

/// Updates Rovik in its WSL distribution.
#[tauri::command]
pub async fn wsl_update(backend: State<'_, DesktopConnection>) -> Result<Value, Value> {
    backend.desktop_raw(HostsRequest::WslUpdate.into()).await
}

/// Removes Rovik from its WSL distribution.
#[tauri::command]
pub async fn wsl_remove(backend: State<'_, DesktopConnection>) -> Result<Value, Value> {
    backend.desktop_raw(HostsRequest::WslRemove.into()).await
}

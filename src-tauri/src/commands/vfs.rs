//! The virtual files (vfs): the volume's driver and mount, its files and
//! their history, and its events streamed to the frontend.

use rovik_client::desktop_api::{vfs::VfsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;

use rovik_core::vfs;
use rovik_core::vfs::history::HistoryLimits;
use rovik_core::vfs::problem::SettingsPane;
use tauri::ipc::Channel;

#[tauri::command]
pub async fn vfs_status(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::Status))
        .await
}

#[tauri::command]
pub async fn vfs_install_driver(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::InstallDriver))
        .await
}

#[tauri::command]
pub async fn vfs_mount(
    backend: State<'_, DesktopConnection>,
    mountpoint: Option<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::Mount { mountpoint }))
        .await
}

/// Opens the System Settings page a mount problem points to (macOS).
#[tauri::command]
pub async fn vfs_open_settings(pane: SettingsPane) -> Result<(), String> {
    vfs::vfs_open_settings(pane).await
}

#[tauri::command]
pub async fn vfs_unmount(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::Unmount))
        .await
}

/// Writes a virtual file. `encoding: "base64"` for binary content.
#[tauri::command]
pub async fn vfs_write_file(
    backend: State<'_, DesktopConnection>,
    path: String,
    content: String,
    encoding: Option<String>,
    executable: Option<bool>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::WriteFile {
            path,
            content,
            encoding,
            executable,
        }))
        .await
}

#[tauri::command]
pub async fn vfs_read_file(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::ReadFile { path }))
        .await
}

#[tauri::command]
pub async fn vfs_list(
    backend: State<'_, DesktopConnection>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::List { path }))
        .await
}

/// Streams events to the frontend in batches until `vfs_unsubscribe`, or
/// until the page that subscribed is gone.
#[tauri::command]
pub async fn vfs_subscribe(
    backend: State<'_, DesktopConnection>,
    kinds: Option<Vec<String>>,
    on_events: Channel<serde_json::Value>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_stream(
            |sink| VfsRequest::Subscribe { kinds, sink }.into(),
            move |batch| on_events.send(batch).is_ok(),
        )
        .await
}

#[tauri::command]
pub async fn vfs_unsubscribe(
    backend: State<'_, DesktopConnection>,
    id: u64,
) -> Result<serde_json::Value, String> {
    backend.desktop(VfsRequest::Unsubscribe { id }.into()).await
}

/// A file's revisions, oldest first.
#[tauri::command]
pub async fn vfs_revisions(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::Revisions { path }))
        .await
}

/// A revision's content: text (lossy UTF-8), or `encoding: "base64"`.
#[tauri::command]
pub async fn vfs_read_revision(
    backend: State<'_, DesktopConnection>,
    path: String,
    revision: u64,
    encoding: Option<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::ReadRevision {
            path,
            revision,
            encoding,
        }))
        .await
}

/// Makes a revision the file's content again (itself a new revision).
#[tauri::command]
pub async fn vfs_restore_revision(
    backend: State<'_, DesktopConnection>,
    path: String,
    revision: u64,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::RestoreRevision {
            path,
            revision,
        }))
        .await
}

/// Changes how much history is kept (`maxBytes: 0` turns it off).
#[tauri::command]
pub async fn vfs_set_history_limits(
    backend: State<'_, DesktopConnection>,
    limits: HistoryLimits,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::SetHistoryLimits {
            limits,
        }))
        .await
}

#[tauri::command]
pub async fn vfs_remove(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::from(VfsRequest::Remove { path }))
        .await
}

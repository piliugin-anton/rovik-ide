//! The virtual files (vfs): the volume's driver and mount, its files and
//! their history, and its events streamed to the frontend.

use crate::core::Core;
use crate::vfs::driver::InstallOutcome;
use crate::vfs::history::{HistoryLimits, RevisionInfo};
use crate::vfs::problem::SettingsPane;
use crate::vfs::{self, EventBatch, VfsEntry, VfsFile, VfsStatus};
use tauri::ipc::Channel;
use tauri::State;

#[tauri::command]
pub async fn vfs_status(core: State<'_, Core>) -> Result<VfsStatus, String> {
    vfs::vfs_status(&core.vfs).await
}

#[tauri::command]
pub async fn vfs_install_driver() -> Result<InstallOutcome, String> {
    vfs::vfs_install_driver().await
}

#[tauri::command]
pub async fn vfs_mount(
    core: State<'_, Core>,
    mountpoint: Option<String>,
) -> Result<String, String> {
    vfs::vfs_mount(&core.vfs, mountpoint).await
}

/// Opens the System Settings page a mount problem points to (macOS).
#[tauri::command]
pub async fn vfs_open_settings(pane: SettingsPane) -> Result<(), String> {
    vfs::vfs_open_settings(pane).await
}

#[tauri::command]
pub async fn vfs_unmount(core: State<'_, Core>) -> Result<(), String> {
    vfs::vfs_unmount(&core.vfs).await
}

/// Writes a virtual file. `encoding: "base64"` for binary content.
#[tauri::command]
pub async fn vfs_write_file(
    core: State<'_, Core>,
    path: String,
    content: String,
    encoding: Option<String>,
    executable: Option<bool>,
) -> Result<VfsFile, String> {
    vfs::vfs_write_file(&core.vfs, path, content, encoding, executable).await
}

#[tauri::command]
pub async fn vfs_read_file(core: State<'_, Core>, path: String) -> Result<String, String> {
    vfs::vfs_read_file(&core.vfs, path).await
}

#[tauri::command]
pub async fn vfs_list(
    core: State<'_, Core>,
    path: Option<String>,
) -> Result<Vec<VfsEntry>, String> {
    vfs::vfs_list(&core.vfs, path).await
}

/// Streams events to the frontend in batches until `vfs_unsubscribe`, or
/// until the page that subscribed is gone.
#[tauri::command]
pub async fn vfs_subscribe(
    core: State<'_, Core>,
    kinds: Option<Vec<String>>,
    on_events: Channel<EventBatch>,
) -> Result<u64, String> {
    vfs::vfs_subscribe(&core.vfs, kinds, move |batch| on_events.send(batch).is_ok()).await
}

#[tauri::command]
pub async fn vfs_unsubscribe(core: State<'_, Core>, id: u64) -> Result<bool, String> {
    vfs::vfs_unsubscribe(&core.vfs, id).await
}

/// A file's revisions, oldest first.
#[tauri::command]
pub async fn vfs_revisions(
    core: State<'_, Core>,
    path: String,
) -> Result<Vec<RevisionInfo>, String> {
    vfs::vfs_revisions(&core.vfs, path).await
}

/// A revision's content: text (lossy UTF-8), or `encoding: "base64"`.
#[tauri::command]
pub async fn vfs_read_revision(
    core: State<'_, Core>,
    path: String,
    revision: u64,
    encoding: Option<String>,
) -> Result<String, String> {
    vfs::vfs_read_revision(&core.vfs, path, revision, encoding).await
}

/// Makes a revision the file's content again (itself a new revision).
#[tauri::command]
pub async fn vfs_restore_revision(
    core: State<'_, Core>,
    path: String,
    revision: u64,
) -> Result<VfsFile, String> {
    vfs::vfs_restore_revision(&core.vfs, path, revision).await
}

/// Changes how much history is kept (`maxBytes: 0` turns it off).
#[tauri::command]
pub async fn vfs_set_history_limits(
    core: State<'_, Core>,
    limits: HistoryLimits,
) -> Result<(), String> {
    vfs::vfs_set_history_limits(&core.vfs, limits).await
}

#[tauri::command]
pub async fn vfs_remove(core: State<'_, Core>, path: String) -> Result<(), String> {
    vfs::vfs_remove(&core.vfs, path).await
}

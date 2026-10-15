//! Isolated chats' changes (overlay/chats.rs): their status, landing them in
//! the project, undoing a landing, and their conflicts. What changes a
//! chat's files is refused while its run is going. A remote chat's bar
//! asks its host for what's there (rovik-client's `remote/bar.rs`), and
//! works on its imported result here for the rest (6.1).

use rovik_client::Remote;
use rovik_core::merge::ConflictFile;
use rovik_core::overlay::chats::{
    self, FileVersions, HandedOver, LandResult, OverlayStatus, UndoneMerge,
};
use rovik_core::overlay::land::Conflict;
use rovik_core::Core;
use std::sync::Arc;
use tauri::State;

/// Polled while a run goes: off the UI thread, it reads the overlay.
#[tauri::command]
pub async fn overlay_status(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<OverlayStatus, String> {
    if remote.host_of(&dialog_id).is_some() {
        return remote.overlay_status(&dialog_id).await;
    }
    chats::overlay_status(dialog_id).await
}

/// A changed file as the project has it and as the chat has it, for a diff.
#[tauri::command]
pub async fn overlay_file(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    path: String,
) -> Result<FileVersions, String> {
    if remote.host_of(&dialog_id).is_some() {
        return remote.overlay_file(&dialog_id, &path).await;
    }
    chats::overlay_file(dialog_id, path).await
}

#[tauri::command]
pub async fn overlay_discard(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<(), String> {
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return remote.discard(&dialog_id).await;
    }
    chats::overlay_discard(&core.agents, dialog_id).await
}

#[tauri::command]
pub async fn overlay_land(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    keep_chat: bool,
) -> Result<LandResult, String> {
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return Ok(LandResult::from(remote.land(&dialog_id, keep_chat).await));
    }
    chats::overlay_land(&core.agents, dialog_id, keep_chat).await
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since.
#[tauri::command]
pub async fn overlay_undo(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    id: String,
) -> Result<Vec<Conflict>, String> {
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
    }
    chats::overlay_undo(&core.agents, dialog_id, id).await
}

#[tauri::command]
pub async fn overlay_undo_merge(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<UndoneMerge, String> {
    remote.local_only(&dialog_id)?;
    chats::overlay_undo_merge(&core.agents, dialog_id).await
}

/// Takes markers just handed to the chat back out (it couldn't take them):
/// only those files, nothing merged before.
#[tauri::command]
pub async fn overlay_unmark(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    files: Vec<String>,
) -> Result<UndoneMerge, String> {
    remote.local_only(&dialog_id)?;
    chats::overlay_unmark(&core.agents, dialog_id, files).await
}

#[tauri::command]
pub async fn overlay_conflict_file(
    dialog_id: String,
    path: String,
) -> Result<ConflictFile, String> {
    chats::overlay_conflict_file(dialog_id, path).await
}

#[tauri::command]
pub async fn overlay_conflict_resolve(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<Conflict>, String> {
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
    }
    chats::overlay_conflict_resolve(&core.agents, dialog_id, path, content, side).await
}

/// Puts conflict markers in the chat's files for it to resolve; returns
/// the message that asks it to.
#[tauri::command]
pub async fn overlay_conflicts_to_chat(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<HandedOver, String> {
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return remote.conflicts_to_chat(&dialog_id).await;
    }
    chats::overlay_conflicts_to_chat(&core.agents, dialog_id).await
}

/// The Redo sequentially button: returns the message to send the chat.
#[tauri::command]
pub async fn overlay_redo_group(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    group: String,
) -> Result<String, String> {
    remote.local_only(&dialog_id)?;
    chats::overlay_redo_group(&core.agents, dialog_id, group).await
}

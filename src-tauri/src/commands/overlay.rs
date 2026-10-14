//! Isolated chats' changes (overlay/chats.rs): their status, landing them in
//! the project, undoing a landing, and their conflicts. What changes a
//! chat's files is refused while its run is going.

use crate::core::Core;
use crate::merge::ConflictFile;
use crate::overlay::chats::{
    self, FileVersions, HandedOver, LandResult, OverlayStatus, UndoneMerge,
};
use crate::overlay::land::Conflict;
use tauri::State;

/// Polled while a run goes: off the UI thread, it reads the overlay.
#[tauri::command]
pub async fn overlay_status(dialog_id: String) -> Result<OverlayStatus, String> {
    chats::overlay_status(dialog_id).await
}

/// A changed file as the project has it and as the chat has it, for a diff.
#[tauri::command]
pub async fn overlay_file(dialog_id: String, path: String) -> Result<FileVersions, String> {
    chats::overlay_file(dialog_id, path).await
}

#[tauri::command]
pub async fn overlay_discard(core: State<'_, Core>, dialog_id: String) -> Result<(), String> {
    chats::overlay_discard(&core.agents, dialog_id).await
}

#[tauri::command]
pub async fn overlay_land(
    core: State<'_, Core>,
    dialog_id: String,
    keep_chat: bool,
) -> Result<LandResult, String> {
    chats::overlay_land(&core.agents, dialog_id, keep_chat).await
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since.
#[tauri::command]
pub async fn overlay_undo(
    core: State<'_, Core>,
    dialog_id: String,
    id: String,
) -> Result<Vec<Conflict>, String> {
    chats::overlay_undo(&core.agents, dialog_id, id).await
}

#[tauri::command]
pub async fn overlay_undo_merge(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<UndoneMerge, String> {
    chats::overlay_undo_merge(&core.agents, dialog_id).await
}

/// Takes markers just handed to the chat back out (it couldn't take them):
/// only those files, nothing merged before.
#[tauri::command]
pub async fn overlay_unmark(
    core: State<'_, Core>,
    dialog_id: String,
    files: Vec<String>,
) -> Result<UndoneMerge, String> {
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
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<Conflict>, String> {
    chats::overlay_conflict_resolve(&core.agents, dialog_id, path, content, side).await
}

/// Puts conflict markers in the chat's files for it to resolve; returns
/// the message that asks it to.
#[tauri::command]
pub async fn overlay_conflicts_to_chat(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<HandedOver, String> {
    chats::overlay_conflicts_to_chat(&core.agents, dialog_id).await
}

/// The Redo sequentially button: returns the message to send the chat.
#[tauri::command]
pub async fn overlay_redo_group(
    core: State<'_, Core>,
    dialog_id: String,
    group: String,
) -> Result<String, String> {
    chats::overlay_redo_group(&core.agents, dialog_id, group).await
}

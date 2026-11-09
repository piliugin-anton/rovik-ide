//! Isolated chats' changes (overlay/chats.rs): their status, landing them in
//! the project, undoing a landing, and their conflicts. What changes a
//! chat's files is refused while its run is going. A remote chat's bar
//! asks its host for what's there (rovik-client's `remote/bar.rs`), and
//! works on its imported result here for the rest (6.1). It also has the
//! chat's sync with the project (`remote/sync.rs`) and a newer result held
//! back while its conflicts are resolved (U05).

use rovik_client::desktop_api::{overlay::OverlayRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;

#[tauri::command]
pub async fn overlay_status(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::Status {
            dialog_id,
        }))
        .await
}

/// A changed file as the project has it and as the chat has it, for a diff.
#[tauri::command]
pub async fn overlay_file(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::File {
            dialog_id,
            path,
        }))
        .await
}

#[tauri::command]
pub async fn overlay_discard(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::Discard {
            dialog_id,
        }))
        .await
}

#[tauri::command]
pub async fn overlay_land(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    keep_chat: bool,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::Land {
            dialog_id,
            keep_chat,
        }))
        .await
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since.
#[tauri::command]
pub async fn overlay_undo(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::Undo {
            dialog_id,
            id,
        }))
        .await
}

#[tauri::command]
pub async fn overlay_undo_merge(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::UndoMerge {
            dialog_id,
        }))
        .await
}

/// Takes markers just handed to the chat back out (it couldn't take them):
/// only those files, nothing merged before.
#[tauri::command]
pub async fn overlay_unmark(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    files: Vec<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::Unmark {
            dialog_id,
            files,
        }))
        .await
}

#[tauri::command]
pub async fn overlay_conflict_file(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::ConflictFile {
            dialog_id,
            path,
        }))
        .await
}

#[tauri::command]
pub async fn overlay_conflict_resolve(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::ConflictResolve {
            dialog_id,
            path,
            content,
            side,
        }))
        .await
}

/// Puts conflict markers in the chat's files for it to resolve; returns
/// the message that asks it to.
#[tauri::command]
pub async fn overlay_conflicts_to_chat(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::ConflictsToChat {
            dialog_id,
        }))
        .await
}

/// The Redo sequentially button: returns the message to send the chat.
#[tauri::command]
pub async fn overlay_redo_group(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    group: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::RedoGroup {
            dialog_id,
            group,
        }))
        .await
}

/// A remote chat's Sync now: its worktree takes the project as it is here.
/// None: it had it already.
#[tauri::command]
pub async fn overlay_sync_now(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::SyncNow {
            dialog_id,
        }))
        .await
}

/// A remote chat's Hand to the chat: the sync's text conflicts marked in
/// its files, and the message that asks it to settle them.
#[tauri::command]
pub async fn overlay_sync_to_chat(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::SyncToChat {
            dialog_id,
        }))
        .await
}

/// A remote chat's Keep the older copy: its bar stops asking about the
/// sync that waits until the next one.
#[tauri::command]
pub async fn overlay_keep_older(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::KeepOlder {
            dialog_id,
        }))
        .await
}

/// A remote chat's Take the newer result, in place of the conflicts being
/// resolved.
#[tauri::command]
pub async fn overlay_take_newer(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(DesktopRequest::Overlay(OverlayRequest::TakeNewer {
            dialog_id,
        }))
        .await
}

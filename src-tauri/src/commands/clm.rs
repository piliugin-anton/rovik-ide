//! Context Language Models (clm): a chat's context as a file the agent
//! edits, and its revisions, which a chat on a host keeps there (6.1).

use rovik_client::desktop_api::{clm::ClmRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_core::clm::{store::RevisionRow, ClmStatus};
use rovik_protocol::Request;
use tauri::State;

#[tauri::command]
pub async fn clm_status(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<ClmStatus, String> {
    let request = Request::ClmStatus {
        dialog_id: dialog_id.clone(),
    };
    backend.call(request).await
}

#[tauri::command]
pub async fn clm_set_mode(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    mode: String,
) -> Result<(), String> {
    let request = Request::ClmSetMode {
        dialog_id: dialog_id.clone(),
        mode,
    };
    backend.call(request).await
}

/// Drops the agent's context; the chat is sent as stored again.
#[tauri::command]
pub async fn clm_reset(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<(), String> {
    let request = Request::ClmReset {
        dialog_id: dialog_id.clone(),
    };
    backend.call(request).await
}

#[tauri::command]
pub async fn clm_list_revisions(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<Vec<RevisionRow>, String> {
    let request = Request::ClmListRevisions {
        dialog_id: dialog_id.clone(),
    };
    backend.call(request).await
}

#[tauri::command]
pub async fn clm_read_revision(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    number: u64,
) -> Result<String, String> {
    let request = Request::ClmReadRevision {
        dialog_id: dialog_id.clone(),
        number,
    };
    backend.call(request).await
}

/// Puts revision `number` back as the context file: it's read back like an
/// edit before the next request.
#[tauri::command]
pub async fn clm_restore_revision(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    number: u64,
) -> Result<(), String> {
    let request = Request::ClmRestoreRevision {
        dialog_id: dialog_id.clone(),
        number,
    };
    backend.call(request).await
}

#[tauri::command]
pub async fn clm_tool_read(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(ClmRequest::ToolRead {
            dialog_id,
            path,
            start_line,
            end_line,
        }))
        .await
}

#[tauri::command]
pub async fn clm_tool_edit(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(ClmRequest::ToolEdit {
            dialog_id,
            path,
            old_text,
            new_text,
            start_line,
            end_line,
        }))
        .await
}

#[tauri::command]
pub async fn clm_tool_write(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    path: String,
    content: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(ClmRequest::ToolWrite {
            dialog_id,
            path,
            content,
        }))
        .await
}

/// When the agent repeats a call whose earlier result it removed from its
/// context, a note offering the revision that still holds that result.
#[tauri::command]
pub async fn clm_recall_hint(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    tool_call_id: String,
    name: String,
    arguments: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(ClmRequest::RecallHint {
            dialog_id,
            tool_call_id,
            name,
            arguments,
        }))
        .await
}

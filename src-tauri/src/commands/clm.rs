//! Context Language Models (clm): a chat's context as a file the agent
//! edits, and its revisions, which a chat on a host keeps there (6.1).

use super::answer;
use rovik_client::Remote;
use rovik_core::clm::{self, store::RevisionRow, ClmStatus};
use rovik_core::Core;
use rovik_protocol::Request;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn clm_status(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<ClmStatus, String> {
    let request = Request::ClmStatus {
        dialog_id: dialog_id.clone(),
    };
    answer(remote.route(&dialog_id, request).await)
}

#[tauri::command]
pub async fn clm_set_mode(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    mode: String,
) -> Result<(), String> {
    let request = Request::ClmSetMode {
        dialog_id: dialog_id.clone(),
        mode,
    };
    answer(remote.route(&dialog_id, request).await)
}

/// Drops the agent's context; the chat is sent as stored again.
#[tauri::command]
pub async fn clm_reset(remote: State<'_, Arc<Remote>>, dialog_id: String) -> Result<(), String> {
    let request = Request::ClmReset {
        dialog_id: dialog_id.clone(),
    };
    answer(remote.route(&dialog_id, request).await)
}

#[tauri::command]
pub async fn clm_list_revisions(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<Vec<RevisionRow>, String> {
    let request = Request::ClmListRevisions {
        dialog_id: dialog_id.clone(),
    };
    answer(remote.route(&dialog_id, request).await)
}

#[tauri::command]
pub async fn clm_read_revision(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    number: u64,
) -> Result<String, String> {
    let request = Request::ClmReadRevision {
        dialog_id: dialog_id.clone(),
        number,
    };
    answer(remote.route(&dialog_id, request).await)
}

/// Puts revision `number` back as the context file: it's read back like an
/// edit before the next request.
#[tauri::command]
pub async fn clm_restore_revision(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    number: u64,
) -> Result<(), String> {
    let request = Request::ClmRestoreRevision {
        dialog_id: dialog_id.clone(),
        number,
    };
    answer(remote.route(&dialog_id, request).await)
}

#[tauri::command]
pub async fn clm_tool_read(
    core: State<'_, Core>,
    dialog_id: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    clm::tool_read(&core.db, &core.vfs, &dialog_id, &path, start_line, end_line).await
}

#[tauri::command]
pub async fn clm_tool_edit(
    core: State<'_, Core>,
    dialog_id: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    clm::tool_edit(
        &core.vfs, &dialog_id, &path, &old_text, &new_text, start_line, end_line,
    )
}

#[tauri::command]
pub async fn clm_tool_write(
    core: State<'_, Core>,
    dialog_id: String,
    path: String,
    content: String,
) -> Result<String, String> {
    clm::tool_write(&core.vfs, &dialog_id, &path, &content)
}

/// When the agent repeats a call whose earlier result it removed from its
/// context, a note offering the revision that still holds that result.
#[tauri::command]
pub async fn clm_recall_hint(
    core: State<'_, Core>,
    dialog_id: String,
    tool_call_id: String,
    name: String,
    arguments: String,
) -> Result<Option<String>, String> {
    clm::recall_hint_of(&core.db, dialog_id, tool_call_id, name, arguments).await
}

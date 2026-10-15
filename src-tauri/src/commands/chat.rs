//! Chat data: chats, their messages and projects (chat_db/api.rs), and the
//! files attached in the chat input. A chat on a host is read from the
//! mirror here, and written to on the host (6.1).

use super::{answer, call};
use rovik_client::Remote;
use rovik_core::chat_db::search::SearchHit;
use rovik_core::chat_db::store::{
    ChatModel, Cursor, DialogPage, MessagePage, MessagePatch, NewDialog, NewMessage,
};
use rovik_core::chat_db::{api, attachments, projects};
use rovik_core::Core;
use rovik_protocol::Request;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::ipc::InvokeBody;
use tauri::State;

#[tauri::command]
pub async fn chat_list_dialogs(
    core: State<'_, Core>,
    project_id: Option<String>,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<DialogPage, String> {
    api::chat_list_dialogs(&core.db, project_id, cursor, limit).await
}

/// The project a folder is, with a suggestion when it looks like a copy or
/// a moved project. It's the open project from now on: remote chats'
/// results land by themselves only there (6.5).
#[tauri::command]
pub async fn chat_open_project(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    root: String,
) -> Result<projects::Opened, String> {
    let opened = api::chat_open_project(&core.db, root.clone()).await?;
    let open = (opened.project.id.clone(), PathBuf::from(root));
    remote.set_open_project(Some(open));
    Ok(opened)
}

/// A chat's plan (`meta.planFile`): a local chat's file, or a remote one's
/// in its worktree on its host (K09).
#[tauri::command]
pub async fn chat_read_plan(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    path: String,
) -> Result<String, String> {
    if remote.host_of(&dialog_id).is_some() {
        return remote.read_plan(&dialog_id, &path).await;
    }
    std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
}

/// "Looks like a copy": share the chats, or a separate history.
#[tauri::command]
pub async fn chat_answer_copy(
    core: State<'_, Core>,
    root: String,
    original_id: String,
    share: bool,
) -> Result<projects::ProjectInfo, String> {
    api::chat_answer_copy(&core.db, root, original_id, share).await
}

/// "Looks like a moved project": use its chats here, which merges this
/// folder's project into that one.
#[tauri::command]
pub async fn chat_answer_moved(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    root: String,
    current_id: String,
    target_id: String,
) -> Result<projects::ProjectInfo, String> {
    remote.keep_project(&current_id, "merging it").await?;
    api::chat_answer_moved(&core.db, root, current_id, target_id).await
}

/// "Not these": not suggested for this folder again.
#[tauri::command]
pub async fn chat_decline_projects(
    core: State<'_, Core>,
    root: String,
    project_ids: Vec<String>,
) -> Result<(), String> {
    api::chat_decline_projects(&core.db, root, project_ids).await
}

/// Every project, most recently opened first, missing ones marked.
#[tauri::command]
pub async fn chat_list_projects(
    core: State<'_, Core>,
) -> Result<Vec<projects::ProjectSummary>, String> {
    api::chat_list_projects(&core.db).await
}

/// A missing project's chats at another folder (merged, if it's another
/// project already and `merge` is set).
#[tauri::command]
pub async fn chat_relink_project(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    project_id: String,
    root: String,
    merge: bool,
) -> Result<projects::Relinked, String> {
    if merge {
        remote.keep_project(&project_id, "merging it").await?;
    }
    api::chat_relink_project(&core.db, project_id, root, merge).await
}

/// A project and all its chats.
#[tauri::command]
pub async fn chat_delete_project(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    project_id: String,
) -> Result<(), String> {
    remote.keep_project(&project_id, "deleting it").await?;
    api::chat_delete_project(&core, project_id).await
}

#[tauri::command]
pub async fn chat_get_messages(
    core: State<'_, Core>,
    dialog_id: String,
    before_seq: Option<i64>,
    limit: Option<usize>,
) -> Result<MessagePage, String> {
    api::chat_get_messages(&core.db, dialog_id, before_seq, limit).await
}

#[tauri::command]
pub async fn chat_get_messages_around(
    core: State<'_, Core>,
    message_id: String,
    before: Option<usize>,
    after: Option<usize>,
) -> Result<MessagePage, String> {
    api::chat_get_messages_around(&core.db, message_id, before, after).await
}

#[tauri::command]
pub async fn chat_get_message_body(
    core: State<'_, Core>,
    message_id: String,
) -> Result<String, String> {
    api::chat_get_message_body(&core.db, message_id).await
}

#[tauri::command]
pub async fn chat_create_dialog(core: State<'_, Core>, dialog: NewDialog) -> Result<(), String> {
    api::chat_create_dialog(&core.db, dialog).await
}

#[tauri::command]
pub async fn chat_append_messages(
    core: State<'_, Core>,
    dialog_id: String,
    messages: Vec<NewMessage>,
) -> Result<Vec<i64>, String> {
    api::chat_append_messages(&core.db, dialog_id, messages).await
}

#[tauri::command]
pub async fn chat_update_message(
    remote: State<'_, Arc<Remote>>,
    message_id: String,
    patch: MessagePatch,
) -> Result<(), String> {
    let request = Request::ChatUpdateMessage {
        message_id: message_id.clone(),
        patch,
    };
    answer(remote.route_message(&message_id, request).await)
}

#[tauri::command]
pub async fn chat_rename_dialog(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    title: String,
) -> Result<(), String> {
    let request = Request::ChatRename {
        dialog_id: dialog_id.clone(),
        title,
    };
    answer(remote.route(&dialog_id, request).await)
}

#[tauri::command]
pub async fn chat_delete_dialog(core: State<'_, Core>, dialog_id: String) -> Result<(), String> {
    call(&core, Request::ChatDelete { dialog_id }).await
}

#[tauri::command]
pub async fn chat_search(
    core: State<'_, Core>,
    query: String,
    project_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    api::chat_search(&core.db, query, project_id, limit).await
}

#[tauri::command]
pub async fn chat_messages_answered(
    core: State<'_, Core>,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    api::chat_messages_answered(&core.db, message_ids).await
}

/// An image or a PDF attached in the chat input, stored until the message
/// that sends it references it (or cleanup removes it). The bytes are the
/// request's raw body, the file as it is: an image is made a PNG at most
/// 768 pixels a side. Returns its id, its type, a PDF's pages and an
/// image's size.
#[tauri::command]
pub async fn chat_store_attachment(
    core: State<'_, Core>,
    request: tauri::ipc::Request<'_>,
) -> Result<attachments::Stored, String> {
    attachments::store_attached(&core.db, body_bytes(request.body())?).await
}

/// A command's raw body as bytes. Where the webview blocks the IPC's custom
/// protocol, Tauri falls back to postMessage, which sends the bytes as a
/// JSON array.
fn body_bytes(body: &InvokeBody) -> Result<Vec<u8>, String> {
    match body {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(serde_json::Value::Array(values)) => values
            .iter()
            .map(|v| v.as_u64().and_then(|b| u8::try_from(b).ok()))
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| "The file's bytes are malformed.".to_string()),
        _ => Err("The file's bytes are missing.".into()),
    }
}

/// Debounced save of the chat input's unsent text (one row per chat).
#[tauri::command]
pub async fn chat_save_composer_draft(
    core: State<'_, Core>,
    dialog_id: String,
    content: String,
) -> Result<(), String> {
    api::chat_save_composer_draft(&core.db, dialog_id, content).await
}

/// The chat's unsent input text, if any (restored on restart / tab switch).
#[tauri::command]
pub async fn chat_get_composer_draft(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<Option<String>, String> {
    api::chat_get_composer_draft(&core.db, dialog_id).await
}

/// Saves the model and effort a chat uses, so reopening it continues with them.
#[tauri::command]
pub async fn chat_set_model(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    model: ChatModel,
) -> Result<(), String> {
    let request = Request::ChatSetModel {
        dialog_id: dialog_id.clone(),
        model,
    };
    answer(remote.route(&dialog_id, request).await)
}

/// The chat's saved model and effort, if any.
#[tauri::command]
pub async fn chat_get_model(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<Option<ChatModel>, String> {
    api::chat_get_model(&core.db, dialog_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An attached file reaches chat_store_attachment as the IPC's raw body,
    /// or as a JSON array where the webview blocks the custom protocol.
    #[test]
    fn a_files_bytes_come_from_either_ipc_body() {
        let bytes = vec![0x89, b'P', b'N', b'G', 0, 255];
        assert_eq!(
            body_bytes(&InvokeBody::Raw(bytes.clone())),
            Ok(bytes.clone())
        );
        assert_eq!(body_bytes(&InvokeBody::Json(json!(bytes))), Ok(bytes));
        let malformed = body_bytes(&InvokeBody::Json(json!([137, 999]))).unwrap_err();
        assert!(malformed.contains("malformed"), "{malformed}");
        let missing = body_bytes(&InvokeBody::Json(json!({ "file": 1 }))).unwrap_err();
        assert!(missing.contains("missing"), "{missing}");
    }
}

//! Tauri commands for chat data. Writes go to the writer thread, reads to a
//! read-only connection; both off the main thread.

use super::search::{self, SearchHit};
use super::store::{self, Cursor, DialogPage, MessagePage, MessagePatch, NewDialog, NewMessage};
use super::{attachments, DbState};
use tauri::State;

#[tauri::command]
pub async fn chat_list_dialogs(
    db: State<'_, DbState>,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<DialogPage, String> {
    db.read(move |c| store::list_dialogs(c, cursor.as_ref(), limit.unwrap_or(50))).await
}

#[tauri::command]
pub async fn chat_get_messages(
    db: State<'_, DbState>,
    dialog_id: String,
    before_seq: Option<i64>,
    limit: Option<usize>,
) -> Result<MessagePage, String> {
    db.read(move |c| store::get_messages(c, &dialog_id, before_seq, limit.unwrap_or(50))).await
}

#[tauri::command]
pub async fn chat_get_messages_around(
    db: State<'_, DbState>,
    message_id: String,
    before: Option<usize>,
    after: Option<usize>,
) -> Result<MessagePage, String> {
    db.read(move |c| store::get_messages_around(c, &message_id, before.unwrap_or(25), after.unwrap_or(25)))
        .await
}

#[tauri::command]
pub async fn chat_get_message_body(db: State<'_, DbState>, message_id: String) -> Result<String, String> {
    db.read(move |c| store::get_message_body(c, &message_id)).await
}

#[tauri::command]
pub async fn chat_create_dialog(db: State<'_, DbState>, dialog: NewDialog) -> Result<(), String> {
    db.write(move |c| store::create_dialog(c, &dialog)).await
}

#[tauri::command]
pub async fn chat_append_messages(
    db: State<'_, DbState>,
    dialog_id: String,
    messages: Vec<NewMessage>,
) -> Result<Vec<i64>, String> {
    db.write(move |c| store::append_messages(c, &dialog_id, &messages)).await
}

#[tauri::command]
pub async fn chat_update_message(
    db: State<'_, DbState>,
    message_id: String,
    patch: MessagePatch,
) -> Result<(), String> {
    db.write(move |c| store::update_message(c, &message_id, &patch)).await
}

#[tauri::command]
pub async fn chat_rename_dialog(db: State<'_, DbState>, dialog_id: String, title: String) -> Result<(), String> {
    db.write(move |c| store::rename_dialog(c, &dialog_id, &title)).await
}

#[tauri::command]
pub async fn chat_delete_dialog(db: State<'_, DbState>, dialog_id: String) -> Result<(), String> {
    let data_dir = db.data_dir().to_path_buf();
    db.write(move |c| {
        store::delete_dialog(c, &dialog_id)?;
        // Best effort; anything missed is collected at the next start.
        let _ = attachments::collect_garbage(c, &data_dir);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn chat_search(
    db: State<'_, DbState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    db.read(move |c| search::search(c, &query, limit.unwrap_or(50))).await
}

#[tauri::command]
pub async fn chat_messages_answered(db: State<'_, DbState>, message_ids: Vec<String>) -> Result<Vec<String>, String> {
    db.read(move |c| store::messages_answered(c, &message_ids)).await
}

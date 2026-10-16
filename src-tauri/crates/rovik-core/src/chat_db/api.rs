//! What the chat commands do (the app's src/commands/chat.rs). Writes go
//! to the writer thread, reads to a read-only connection; both off the
//! main thread.

use super::search::{self, SearchHit};
use super::store::{
    self, Cursor, DialogPage, DialogSummary, MessagePage, MessagePatch, NewDialog, NewMessage,
};
use super::{attachments, projects, DbState};
use crate::core::Core;

pub async fn chat_list_dialogs(
    db: &DbState,
    project_id: Option<String>,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<DialogPage, String> {
    db.read(move |c| {
        store::list_dialogs(
            c,
            project_id.as_deref(),
            cursor.as_ref(),
            limit.unwrap_or(50),
        )
    })
    .await
}

/// The project a folder is (chat_db/projects.rs), with a suggestion when it
/// looks like a copy or a moved project. In two steps around the git
/// fingerprint, which walks the history: off the database thread.
/// One chat's summary as the list shows it, as it is now.
pub async fn chat_get_dialog(
    db: &DbState,
    dialog_id: String,
) -> Result<Option<DialogSummary>, String> {
    db.read(move |c| store::get_dialog(c, &dialog_id)).await
}

pub async fn chat_open_project(db: &DbState, root: String) -> Result<projects::Opened, String> {
    let path = std::path::PathBuf::from(&root);
    let resolve_path = path.clone();
    let resolved = db
        .write(move |c| projects::resolve(c, &resolve_path))
        .await?;
    if !resolved.needs_fingerprint {
        return Ok(resolved.opened);
    }
    let fp_path = path.clone();
    let fp = crate::rt::spawn_blocking(move || projects::fingerprint(&fp_path))
        .await
        .map_err(|e| e.to_string())?;
    db.write(move |c| projects::finish_open(c, resolved, fp, &path))
        .await
}

/// "Looks like a copy": share the chats, or a separate history.
pub async fn chat_answer_copy(
    db: &DbState,
    root: String,
    original_id: String,
    share: bool,
) -> Result<projects::ProjectInfo, String> {
    db.write(move |c| projects::answer_copy(c, std::path::Path::new(&root), &original_id, share))
        .await
}

/// "Looks like a moved project": use its chats here.
pub async fn chat_answer_moved(
    db: &DbState,
    root: String,
    current_id: String,
    target_id: String,
) -> Result<projects::ProjectInfo, String> {
    db.write(move |c| {
        projects::answer_moved(c, std::path::Path::new(&root), &current_id, &target_id)
    })
    .await
}

/// "Not these": not suggested for this folder again.
pub async fn chat_decline_projects(
    db: &DbState,
    root: String,
    project_ids: Vec<String>,
) -> Result<(), String> {
    db.write(move |c| projects::decline(c, std::path::Path::new(&root), &project_ids))
        .await
}

/// Every project, most recently opened first, missing ones marked.
pub async fn chat_list_projects(db: &DbState) -> Result<Vec<projects::ProjectSummary>, String> {
    db.read(projects::list).await
}

/// A missing project's chats at another folder (merged, if it's another
/// project already and `merge` is set).
pub async fn chat_relink_project(
    db: &DbState,
    project_id: String,
    root: String,
    merge: bool,
) -> Result<projects::Relinked, String> {
    db.write(move |c| projects::relink(c, &project_id, std::path::Path::new(&root), merge))
        .await
}

/// A project and all its chats.
pub async fn chat_delete_project(core: &Core, project_id: String) -> Result<(), String> {
    let db = &core.db;
    let id = project_id.clone();
    let chats = db
        .read(move |c| store::dialogs_in_project(c, Some(&id)))
        .await?;
    for chat in &chats {
        remove_chat_and_children_files(core, chat).await?;
    }
    let data_dir = db.data_dir().to_path_buf();
    db.write(move |c| {
        projects::delete(c, &project_id)?;
        let _ = attachments::collect_garbage(c, &data_dir);
        Ok(())
    })
    .await
}

pub async fn chat_get_messages(
    db: &DbState,
    dialog_id: String,
    before_seq: Option<i64>,
    limit: Option<usize>,
) -> Result<MessagePage, String> {
    db.read(move |c| store::get_messages(c, &dialog_id, before_seq, limit.unwrap_or(50)))
        .await
}

pub async fn chat_get_messages_around(
    db: &DbState,
    message_id: String,
    before: Option<usize>,
    after: Option<usize>,
) -> Result<MessagePage, String> {
    db.read(move |c| {
        store::get_messages_around(c, &message_id, before.unwrap_or(25), after.unwrap_or(25))
    })
    .await
}

pub async fn chat_get_message_body(db: &DbState, message_id: String) -> Result<String, String> {
    db.read(move |c| store::get_message_body(c, &message_id))
        .await
}

pub async fn chat_create_dialog(db: &DbState, dialog: NewDialog) -> Result<(), String> {
    db.write(move |c| store::create_dialog(c, &dialog)).await
}

pub async fn chat_append_messages(
    db: &DbState,
    dialog_id: String,
    messages: Vec<NewMessage>,
) -> Result<Vec<i64>, String> {
    db.write(move |c| store::append_messages(c, &dialog_id, &messages))
        .await
}

pub async fn chat_update_message(
    db: &DbState,
    message_id: String,
    patch: MessagePatch,
) -> Result<(), String> {
    db.write(move |c| store::update_message(c, &message_id, &patch))
        .await
}

pub async fn chat_rename_dialog(
    db: &DbState,
    dialog_id: String,
    title: String,
) -> Result<(), String> {
    db.write(move |c| store::rename_dialog(c, &dialog_id, &title))
        .await
}

pub async fn chat_delete_dialog(core: &Core, dialog_id: String) -> Result<(), String> {
    let data_dir = core.db.data_dir().to_path_buf();
    remove_chat_and_children_files(core, &dialog_id).await?;
    core.db
        .write(move |c| {
            store::delete_dialog(c, &dialog_id)?;
            // Best effort; anything missed is collected at the next start.
            let _ = attachments::collect_garbage(c, &data_dir);
            Ok(())
        })
        .await
}

/// P6.7: a chat's files and its subagents' chats' files (their copies,
/// landing records and context), the children's first. A daemon clears a
/// chat's earlier stay with them when a move brings it again (6.8).
pub async fn remove_chat_and_children_files(core: &Core, dialog_id: &str) -> Result<(), String> {
    let id = dialog_id.to_string();
    let children = core.db.read(move |c| store::descendants(c, &id)).await?;
    for child in &children {
        remove_chat_files(core, child).await?;
    }
    remove_chat_files(core, dialog_id).await
}

/// What a deleted chat leaves outside the database: its run (stopped
/// first: its view is about to go), its isolated changes and landing
/// history, and its CLM context on the volume.
async fn remove_chat_files(core: &Core, dialog_id: &str) -> Result<(), String> {
    let agent = &core.agents;
    if agent.stop(core, dialog_id) {
        for _ in 0..100 {
            if !agent.is_running(dialog_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Still going: its files can't go from under it.
        if agent.is_running(dialog_id) {
            return Err(
                "this chat's run didn't stop yet: try deleting it again in a moment".into(),
            );
        }
    }
    agent.drop_carry(dialog_id);
    let id = dialog_id.to_string();
    let _ = crate::rt::spawn_blocking(move || crate::overlay::chats::forget(&id)).await;
    // Only an id the context folder's name keeps whole: one it shortens
    // ("a.b" → "ab") could name another chat's folder.
    if !dialog_id.is_empty()
        && dialog_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        let _ = core.vfs.remove(&crate::clm::context_dir(dialog_id));
    }
    Ok(())
}

pub async fn chat_search(
    db: &DbState,
    query: String,
    project_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    db.read(move |c| search::search(c, &query, project_id.as_deref(), limit.unwrap_or(50)))
        .await
}

pub async fn chat_messages_answered(
    db: &DbState,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    db.read(move |c| store::messages_answered(c, &message_ids))
        .await
}

/// Debounced save of the chat input's unsent text (one row per chat).
pub async fn chat_save_composer_draft(
    db: &DbState,
    dialog_id: String,
    content: String,
) -> Result<(), String> {
    db.write(move |c| store::save_composer_draft(c, &dialog_id, &content))
        .await
}

/// The chat's unsent input text, if any (restored on restart / tab switch).
pub async fn chat_get_composer_draft(
    db: &DbState,
    dialog_id: String,
) -> Result<Option<String>, String> {
    db.read(move |c| store::get_composer_draft(c, &dialog_id))
        .await
}

/// Saves the model and effort a chat uses, so reopening it continues with them.
pub async fn chat_set_model(
    db: &DbState,
    dialog_id: String,
    model: store::ChatModel,
) -> Result<(), String> {
    db.write(move |c| store::set_chat_model(c, &dialog_id, &model))
        .await
}

/// The chat's saved model and effort, if any.
pub async fn chat_get_model(
    db: &DbState,
    dialog_id: String,
) -> Result<Option<store::ChatModel>, String> {
    db.read(move |c| store::chat_model(c, &dialog_id)).await
}

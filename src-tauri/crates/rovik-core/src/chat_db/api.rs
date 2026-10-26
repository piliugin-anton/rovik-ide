//! What the chat commands do (the app's src/commands/chat.rs). Writes go
//! to the writer thread, reads to a read-only connection; both off the
//! main thread.

use super::search::{self, SearchHit};
use super::store::{self, Cursor, DialogPage, DialogSummary, MessagePage, MessagePatch, NewDialog};
use super::{attachments, projects, workspaces, DbState};
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

/// A project and all its chats, or a workspace and its own chats (D6).
/// `open`: the projects this window has open, which it doesn't delete from
/// under itself (3.4, B06).
pub async fn chat_delete_project(
    core: &Core,
    project_id: String,
    open: Vec<Option<String>>,
) -> Result<(), String> {
    if open.iter().flatten().any(|id| id == &project_id) {
        return Err(
            "That project is open in this window: open another one first, then delete it.".into(),
        );
    }
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

/* ---------------------------------------------------------------------------
 * Workspaces (workspaces-plan.md, 3.1)
 * ------------------------------------------------------------------------- */

/// Every workspace, most recently opened first, each folder marked when it's
/// missing.
pub async fn workspace_list(db: &DbState) -> Result<Vec<workspaces::Summary>, String> {
    db.read(workspaces::list).await
}

/// Reads folders from the file system off the database thread: their
/// canonical paths, whether they're folders, their ID files.
async fn prepare(folders: Vec<String>) -> Result<Vec<workspaces::Prepared>, String> {
    crate::rt::spawn_blocking(move || {
        folders
            .iter()
            .map(|folder| workspaces::prepare(folder))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Makes a workspace named `name` holding `folders`, in this order, each
/// named as the workspace names it (D8); a folder that can't join (D5)
/// refuses the whole.
pub async fn workspace_create(
    db: &DbState,
    name: String,
    folders: Vec<String>,
) -> Result<workspaces::Workspace, String> {
    let prepared = prepare(folders).await?;
    db.write(move |c| workspaces::create(c, &name, &prepared))
        .await
}

/// Adds `folders` to the workspace, each named as it joins (D8).
pub async fn workspace_add_folders(
    db: &DbState,
    workspace_id: String,
    folders: Vec<String>,
) -> Result<workspaces::Workspace, String> {
    let known = prepared_members(db, &workspace_id).await?;
    let prepared = prepare(folders).await?;
    db.write(move |c| workspaces::add_folders(c, &workspace_id, &known, &prepared))
        .await
}

/// Removes a folder from the workspace: refused for the last one, and while
/// a workspace chat that works in it has changes there or is at work
/// (D19, with workspace chats).
pub async fn workspace_remove_folder(
    db: &DbState,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    db.write(move |c| workspaces::remove_folder(c, &workspace_id, &name))
        .await
}

/// Locate…: the folder named `name` is at `to` from now on, by its name, and
/// D5 holds for the new place too.
pub async fn workspace_relocate_folder(
    db: &DbState,
    workspace_id: String,
    name: String,
    to: String,
) -> Result<workspaces::Workspace, String> {
    let known = prepared_members(db, &workspace_id).await?;
    let prepared = prepare(vec![to]).await?.remove(0);
    db.write(move |c| workspaces::relocate_folder(c, &workspace_id, &name, &known, &prepared))
        .await
}

/// The workspace's folders as they are now, prepared off the database
/// thread: their canonical paths and ID files, for checks against them (D5)
/// that then hold inside the write transaction.
async fn prepared_members(db: &DbState, id: &str) -> Result<Vec<workspaces::Prepared>, String> {
    let id = id.to_string();
    let members = db.read(move |c| workspaces::members(c, &id)).await?;
    crate::rt::spawn_blocking(move || {
        members
            .iter()
            .map(|member| workspaces::prepare(&member.path))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Whether `folder` can join the workspace (`workspace_id`: its folders are
/// the ones it can't double, hold or be held by); None when it can.
pub async fn workspace_check_folder(
    db: &DbState,
    workspace_id: Option<String>,
    folder: String,
) -> Result<Option<workspaces::FolderProblem>, String> {
    let members = match workspace_id {
        Some(id) => prepared_members(db, &id).await?,
        None => Vec::new(),
    };
    crate::rt::spawn_blocking(move || {
        workspaces::check_folder(&members, &workspaces::prepare(&folder))
    })
    .await
    .map_err(|e| e.to_string())
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

/// The sends of the project's chats a window that ended left unsettled
/// (`store::unsettled_sends`), but for those a run is at work on, in this
/// process or another (chat_hold): theirs may still be answered.
pub async fn chat_unsettled_sends(
    db: &DbState,
    project_id: Option<String>,
) -> Result<Vec<store::UnsettledSend>, String> {
    let sends = db
        .read(move |c| store::unsettled_sends(c, project_id.as_deref()))
        .await?;
    let here = crate::chat_hold::held_here();
    Ok(sends
        .into_iter()
        .filter(|s| !here.contains(&s.dialog_id) && !crate::chat_hold::elsewhere(&s.dialog_id))
        .collect())
}

pub async fn chat_messages_answered(
    db: &DbState,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    db.read(move |c| store::messages_answered(c, &message_ids))
        .await
}

/// Which of these messages are stored, in their order.
pub async fn chat_messages_stored(
    db: &DbState,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    db.read(move |c| {
        let rows = store::messages_by_id(c, &message_ids)?;
        Ok(rows.into_iter().map(|m| m.id).collect())
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn db(tag: &str) -> (DbState, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ws-api-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (DbState::open(&dir).unwrap(), dir)
    }

    fn folder(dir: &Path, name: &str) -> String {
        let path = dir.join(name);
        fs::create_dir_all(&path).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// The workspace commands reach the database: one is made with its
    /// folders named (D8), listed, added to, located, and its last folder
    /// stays (D5).
    #[tokio::test]
    async fn the_workspace_commands_reach_the_database() {
        let (db, dir) = db("commands");
        let (api, web) = (folder(&dir, "api"), folder(&dir, "web"));
        let made = workspace_create(&db, "Shop".into(), vec![api.clone()])
            .await
            .unwrap();
        assert_eq!(made.name, "Shop");
        let listed = workspace_list(&db).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].folders[0].name, "api");

        let added = workspace_add_folders(&db, made.id.clone(), vec![web.clone()])
            .await
            .unwrap();
        assert_eq!(added.folders.len(), 2);

        // A folder of the workspace can't join it again.
        let problem = workspace_check_folder(&db, Some(made.id.clone()), api.clone())
            .await
            .unwrap();
        assert!(matches!(
            problem,
            Some(workspaces::FolderProblem::Duplicate { .. })
        ));

        let moved = folder(&dir.join("moved"), "api");
        let after = workspace_relocate_folder(&db, made.id.clone(), "web".into(), moved)
            .await
            .unwrap();
        assert_eq!(after.folders[1].name, "web", "its name stayed");

        workspace_remove_folder(&db, made.id.clone(), "web".into())
            .await
            .unwrap();
        let refused = workspace_remove_folder(&db, made.id.clone(), "api".into()).await;
        assert!(refused.unwrap_err().contains("at least one folder"));
        assert_eq!(workspace_list(&db).await.unwrap()[0].folders.len(), 1);
    }

    /// A project the window has open isn't deleted from under it: the delete
    /// is refused, naming what to do, and its chats stay (3.4, B06).
    #[tokio::test]
    async fn a_project_the_window_has_open_isnt_deleted() {
        let test = crate::testing::TestCore::new();
        let core = &test.core;
        let dir = std::env::temp_dir().join(format!("ws-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.clone();
        core.db
            .write(move |c| projects::ensure(c, "p-open", "api", &path))
            .await
            .unwrap();
        core.db
            .write(|c| {
                store::create_dialog(
                    c,
                    &NewDialog {
                        id: "c1".into(),
                        title: "A chat".into(),
                        created_at: 1,
                        updated_at: 1,
                        project_id: Some("p-open".into()),
                        folders: None,
                    },
                )
            })
            .await
            .unwrap();

        let refused = chat_delete_project(core, "p-open".into(), vec![Some("p-open".into())]).await;
        assert!(refused.unwrap_err().contains("open in this window"));
        assert!(
            chat_get_dialog(&core.db, "c1".into())
                .await
                .unwrap()
                .is_some(),
            "its chats stay"
        );

        // Another project open, not this one: it goes, with its chats.
        chat_delete_project(core, "p-open".into(), vec![Some("p-other".into()), None])
            .await
            .unwrap();
        assert!(chat_get_dialog(&core.db, "c1".into())
            .await
            .unwrap()
            .is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A folder the workspace doesn't hold can join: nothing is reported.
    #[tokio::test]
    async fn a_folder_the_workspace_doesnt_hold_can_join() {
        let (db, dir) = db("check");
        let api = folder(&dir, "api");
        assert_eq!(
            workspace_check_folder(&db, None, api.clone())
                .await
                .unwrap(),
            None
        );
        let made = workspace_create(&db, "Shop".into(), vec![api])
            .await
            .unwrap();
        let web = folder(&dir, "web");
        assert_eq!(
            workspace_check_folder(&db, Some(made.id), web)
                .await
                .unwrap(),
            None
        );
    }
}

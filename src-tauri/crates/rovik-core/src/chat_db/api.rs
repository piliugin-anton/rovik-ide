//! What the chat commands do (the app's src/commands/chat.rs). Writes go
//! to the writer thread, reads to a read-only connection; both off the
//! main thread.

use super::search::{self, SearchHit};
use super::store::{self, Cursor, DialogPage, DialogSummary, MessagePage, MessagePatch, NewDialog};
use super::{attachments, projects, workspaces, DbState};
use crate::core::Core;

/// The chats of a window's owners, newest first (`owners` None: the chats
/// with no project), as `store::list_dialogs` pages them.
pub async fn chat_list_dialogs(
    db: &DbState,
    owners: Option<Vec<String>>,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<DialogPage, String> {
    db.read(move |c| {
        store::list_dialogs(c, owners.as_deref(), cursor.as_ref(), limit.unwrap_or(50))
    })
    .await
}

/// One chat's summary as the list shows it, as it is now.
pub async fn chat_get_dialog(
    db: &DbState,
    dialog_id: String,
) -> Result<Option<DialogSummary>, String> {
    db.read(move |c| store::get_dialog(c, &dialog_id)).await
}

/// The project a folder is (chat_db/projects.rs), with a suggestion when it
/// looks like a copy or a moved project. In two steps around the git
/// fingerprint, which walks the history: off the database thread. A
/// landing a crash cut in the folder while it was away (a drive not
/// mounted) is finished, now that it's there (workspaces-plan.md, 4.6).
pub async fn chat_open_project(db: &DbState, root: String) -> Result<projects::Opened, String> {
    let path = std::path::PathBuf::from(&root);
    let resolve_path = path.clone();
    let resolved = db
        .write(move |c| projects::resolve(c, &resolve_path))
        .await?;
    let opened = match resolved.needs_fingerprint {
        false => resolved.opened,
        true => {
            let fp_path = path.clone();
            let fp = crate::rt::spawn_blocking(move || projects::fingerprint(&fp_path))
                .await
                .map_err(|e| e.to_string())?;
            let at = path.clone();
            db.write(move |c| projects::finish_open(c, resolved, fp, &at))
                .await?
        }
    };
    finish_landings_in(path).await;
    Ok(opened)
}

/// Finishes the landings a crash cut in `folder` while it was away, now
/// that it's there: found again, or located (4.6).
async fn finish_landings_in(folder: std::path::PathBuf) {
    let Some(data) = crate::overlay::chats::data_dir() else {
        return;
    };
    let finished =
        crate::rt::spawn_blocking(move || crate::overlay::land::recover_in(&data, &folder)).await;
    match finished {
        Ok(paths) if !paths.is_empty() => {
            log::info!(
                "overlay: finished an interrupted landing: {}",
                paths.join(", ")
            )
        }
        Ok(_) => {}
        Err(e) => log::warn!("overlay: interrupted landings weren't looked at: {e}"),
    }
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
        .read(move |c| store::dialogs_of(c, Some(std::slice::from_ref(&id))))
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
/// a workspace chat that works in it is at work, has commands running or has
/// changes there that haven't landed (D19). The chats that had it let it go
/// at their next runs.
pub async fn workspace_remove_folder(
    db: &DbState,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    if let Some(why) = folder_held(db, &workspace_id, &name, "removed", true).await? {
        return Err(why);
    }
    db.write(move |c| workspaces::remove_folder(c, &workspace_id, &name))
        .await
}

/// Locate…: the folder named `name` is at `to` from now on, by its name, and
/// D5 holds for the new place too. Refused while a workspace chat that works
/// in it is at work or has commands running, and, when the folder found is
/// another project than the one that was there, while one has changes there
/// (D19). Its chats' changes there go with it, and a landing a crash cut
/// there is finished.
pub async fn workspace_relocate_folder(
    db: &DbState,
    workspace_id: String,
    name: String,
    to: String,
) -> Result<workspaces::Workspace, String> {
    let known = prepared_members(db, &workspace_id).await?;
    let prepared = prepare(vec![to]).await?.remove(0);
    let (id, folder) = (workspace_id.clone(), name.clone());
    let was = db
        .read(move |c| workspaces::folder_project(c, &id, &folder))
        .await?;
    // The same project: the folder found has the ID file of the one last
    // opened there.
    let same = was.is_some() && prepared.project == was;
    if let Some(why) = folder_held(db, &workspace_id, &name, "located elsewhere", !same).await? {
        return Err(why);
    }
    let (id, folder) = (workspace_id.clone(), name.clone());
    let chats = db
        .read(move |c| workspaces::chats_in_folder(c, &id, &folder))
        .await?;
    let path = std::path::PathBuf::from(&prepared.path);
    let folder = name.clone();
    let located = db
        .write(move |c| workspaces::relocate_folder(c, &workspace_id, &name, &known, &prepared))
        .await?;
    let moved = path.clone();
    let followed = crate::rt::spawn_blocking(move || {
        chats
            .iter()
            .filter_map(|(chat, title)| {
                crate::overlay::chats::relocate(chat, &folder, &moved)
                    .err()
                    .map(|e| format!("“{title}”: {e}"))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    // Its run opens the folder's view there all the same: what's left
    // behind is where its landings say they wrote, for Undo.
    if !followed.is_empty() {
        log::warn!(
            "workspace: chats' records of {} weren't moved: {}",
            path.display(),
            followed.join("; ")
        );
    }
    finish_landings_in(path).await;
    Ok(located)
}

/// D19: why the workspace's folder `name` can't be `removed` or `located
/// elsewhere` now, naming the chats that use it: one at work on this
/// computer, one with commands left running, and (`changes`) one with
/// changes there that haven't landed. None when none holds it.
async fn folder_held(
    db: &DbState,
    workspace_id: &str,
    name: &str,
    verb: &str,
    changes: bool,
) -> Result<Option<String>, String> {
    let (id, folder) = (workspace_id.to_string(), name.to_string());
    let chats = db
        .read(move |c| workspaces::chats_in_folder(c, &id, &folder))
        .await?;
    let folder = name.to_string();
    let why = crate::rt::spawn_blocking(move || -> Result<Vec<String>, String> {
        let here = crate::chat_hold::held_here();
        let mut why = Vec::new();
        for (chat, title) in chats {
            if here.contains(&chat) || crate::chat_hold::elsewhere(&chat) {
                why.push(format!(
                    "“{title}” is at work in it (wait for it, or stop it)"
                ));
            } else if !crate::command_tool::background::tasks(&chat).is_empty() {
                why.push(format!(
                    "“{title}” has commands running in it (stop them in its background tasks)"
                ));
            } else if changes
                && !crate::overlay::chats::changes_in_folder(&chat, &folder)?.is_empty()
            {
                why.push(format!(
                    "“{title}” has changes in it that haven't landed (land or discard them)"
                ));
            }
        }
        Ok(why)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok((!why.is_empty()).then(|| format!("{name} can't be {verb} now: {}.", why.join("; "))))
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

/// New chat reuses the empty chat open for the folder chosen: it belongs to
/// that folder's project from now on (`store::move_empty_dialog`).
pub async fn chat_move_empty_dialog(
    db: &DbState,
    dialog_id: String,
    project_id: Option<String>,
    folders: Option<String>,
) -> Result<(), String> {
    db.write(move |c| {
        store::move_empty_dialog(c, &dialog_id, project_id.as_deref(), folders.as_deref())
    })
    .await
}

/// Apply (D32): workspace chat `dialog_id` works in `folders` from now on:
/// `"all"`, following its workspace, or the names checked as JSON. Refused
/// for a folder chat, which keeps its folder, and for no folder of the
/// workspace checked; while the chat is at work (a run, or its Autopilot
/// task between two stages or waiting out a rate limit, here or in another
/// window) or has commands running; and when a folder it would leave has
/// changes that haven't landed, naming those folders. Its view takes the
/// folders at once, as each run's start would.
pub async fn chat_set_folders(
    core: &Core,
    dialog_id: String,
    folders: String,
) -> Result<(), String> {
    let (id, wanted) = (dialog_id.clone(), folders.clone());
    let found = core
        .db
        .read(move |c| {
            let Some(dialog) = store::get_dialog(c, &id)? else {
                return Ok(None);
            };
            let project = dialog.project_id.clone().unwrap_or_default();
            let Some(workspace) = workspaces::get(c, &project)? else {
                return Ok(None);
            };
            let had = workspaces::checked(workspace.folders.clone(), dialog.folders.as_deref())?;
            let next = workspaces::checked(workspace.folders, Some(&wanted))?;
            Ok(Some((dialog, had, next)))
        })
        .await?;
    let Some((dialog, had, next)) = found else {
        return Err("This chat works in one folder: it keeps it.".into());
    };
    let title = &dialog.title;
    if next.is_empty() {
        return Err("Check a folder for the chat to work in.".into());
    }
    let held = crate::chat_hold::held_here().contains(&dialog_id)
        || crate::chat_hold::elsewhere(&dialog_id);
    if held || task_waits(core, &dialog_id).await? {
        return Err(format!(
            "“{title}” is at work: its folders can change once it stops."
        ));
    }
    if !crate::command_tool::background::tasks(&dialog_id).is_empty() {
        return Err(format!(
            "“{title}” has commands running: stop them to change its folders."
        ));
    }
    let left: Vec<String> = had
        .iter()
        .filter(|f| !next.iter().any(|n| n.name == f.name))
        .map(|f| f.name.clone())
        .collect();
    let chat = dialog_id.clone();
    let changed = crate::rt::spawn_blocking(move || -> Result<Vec<String>, String> {
        let mut changed = Vec::new();
        for name in left {
            if !crate::overlay::chats::changes_in_folder(&chat, &name)?.is_empty() {
                changed.push(name);
            }
        }
        Ok(changed)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !changed.is_empty() {
        return Err(format!(
            "“{title}” has changes in {} that aren't landed: land or discard them first.",
            changed.join(", ")
        ));
    }
    let view = crate::folders::Folders::named(
        next.into_iter()
            .map(|f| crate::folders::Folder {
                name: f.name,
                path: f.path.into(),
            })
            .collect(),
    )?
    .in_workspace(dialog.project_id.as_deref().unwrap_or_default());
    let (id, was) = (dialog_id.clone(), dialog.folders.clone());
    core.db
        .write(move |c| store::set_dialog_folders(c, &id, Some(&folders)))
        .await?;
    let chat = dialog_id.clone();
    let set = crate::rt::spawn_blocking(move || crate::overlay::chats::set_folders(&chat, &view))
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = set {
        // As it was: the chat's view didn't take them.
        core.db
            .write(move |c| store::set_dialog_folders(c, &dialog_id, was.as_deref()))
            .await?;
        return Err(format!("The folders of “{title}” didn't change: {e}."));
    }
    Ok(())
}

/// Whether the chat's Autopilot task waits out the provider's rate limit
/// (H07): its latest stage's record says so, armed or not yet again after
/// a restart.
async fn task_waits(core: &Core, dialog_id: &str) -> Result<bool, String> {
    let task = crate::agent_run::autopilot::current_task(core, dialog_id).await?;
    Ok(task.is_some_and(|(_, records)| {
        records
            .last()
            .is_some_and(|r| r.record.waits_until().is_some())
    }))
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

/// Searches the chats of a window's owners together (`owners` None: the
/// chats with no project).
pub async fn chat_search(
    db: &DbState,
    query: String,
    owners: Option<Vec<String>>,
    limit: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    db.read(move |c| search::search(c, &query, owners.as_deref(), limit.unwrap_or(50)))
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

    /// D19's guard: a workspace `Shop` of `api` and `web` (each with
    /// `b.txt`), the overlays' data folder this test's, and chats of it.
    struct Guarded {
        test: crate::testing::TestCore,
        workspace: String,
        _data: tokio::sync::OwnedMutexGuard<()>,
    }

    impl Guarded {
        async fn new() -> Guarded {
            let test = crate::testing::TestCore::new();
            let _data = crate::overlay::chats::TEST_DATA.clone().lock_owned().await;
            crate::overlay::chats::init(&test.dir.0.join("overlays"));
            let base = test.dir.0.join("shop");
            let folders = vec![folder(&base, "api"), folder(&base, "web")];
            fs::write(base.join("web/b.txt"), "b\n").unwrap();
            let workspace = workspace_create(&test.core.db, "Shop".into(), folders)
                .await
                .unwrap()
                .id;
            Guarded {
                test,
                workspace,
                _data,
            }
        }

        fn path(&self, rel: &str) -> std::path::PathBuf {
            self.test.dir.0.join("shop").join(rel)
        }

        fn db(&self) -> &DbState {
            &self.test.core.db
        }

        /// A chat of the workspace titled `title`, working in `folders`.
        async fn chat(&self, id: &str, title: &str, folders: &str) {
            let dialog = NewDialog {
                id: id.into(),
                title: title.into(),
                created_at: 1,
                updated_at: 1,
                project_id: Some(self.workspace.clone()),
                folders: Some(folders.into()),
            };
            self.db()
                .write(move |c| store::create_dialog(c, &dialog))
                .await
                .unwrap();
        }

        /// The chat's change to web: `n.txt`, saved in web's part of its
        /// store, as between its runs.
        fn changed_in_web(&self, id: &str) {
            use crate::vfs::backend::Backend;
            let part = self
                .test
                .dir
                .0
                .join("overlays/overlays")
                .join(id)
                .join("parts/web");
            let fs =
                crate::overlay::fs::OverlayFs::open(&self.path("web"), &part, 1 << 30).unwrap();
            let file = fs
                .create(
                    crate::vfs::store::ROOT,
                    "n.txt",
                    crate::vfs::store::Kind::File,
                    0o644,
                    0,
                    0,
                    None,
                )
                .unwrap();
            fs.write(file.ino, 0, b"new\n").unwrap();
            fs.save();
        }

        async fn remove(&self, name: &str) -> Result<(), String> {
            workspace_remove_folder(self.db(), self.workspace.clone(), name.into()).await
        }

        async fn locate(&self, name: &str, to: &str) -> Result<workspaces::Workspace, String> {
            let to = self.path(to).to_string_lossy().into_owned();
            workspace_relocate_folder(self.db(), self.workspace.clone(), name.into(), to).await
        }

        async fn folders_of(&self, id: &str) -> Option<String> {
            let id = id.to_string();
            self.db()
                .read(move |c| store::dialog_folders(c, &id))
                .await
                .unwrap()
        }
    }

    impl Guarded {
        /// Apply on chat `id`'s folders (D32).
        async fn apply(&self, id: &str, folders: &str) -> Result<(), String> {
            chat_set_folders(&self.test.core, id.into(), folders.into()).await
        }

        /// The chat's Autopilot task, started by its message `task`, its
        /// Plan stage waiting out the provider's rate limit (H07).
        async fn task_waiting(&self, id: &str, task: &str) {
            use crate::agent_run::autopilot::{
                add_stage, Effort, RateLimitWait, Stage, StageRecord,
            };
            let start = store::NewMessage {
                id: task.into(),
                role: "user".into(),
                content: "Add the cart".into(),
                tool_calls: None,
                tool_call_id: None,
                meta: Some(serde_json::json!({ "mode": "autopilot" })),
                error: None,
                failed: false,
                attachments: Vec::new(),
                created_at: store::now_ms(),
                finalized: true,
            };
            let chat = id.to_string();
            self.db()
                .write(move |c| store::append_messages(c, &chat, &[start]).map(drop))
                .await
                .unwrap();
            let waiting = StageRecord {
                rate_limit: Some(RateLimitWait {
                    until: Some(store::now_ms() + 60_000),
                    waits: 1,
                    waited_ms: 0,
                }),
                ..StageRecord::running(task, Stage::Plan, 1, Effort::Balanced)
            };
            add_stage(&self.test.core, id, &waiting).await.unwrap();
        }
    }

    /// D32: Apply sets a workspace chat's folders, All folders too, and
    /// refuses a list with none of the workspace's folders, and a folder
    /// chat, which keeps its folder.
    #[tokio::test]
    async fn apply_sets_a_workspace_chats_folders() {
        let g = Guarded::new().await;
        g.chat("ap-set", "Cart", r#"["api","web"]"#).await;
        g.apply("ap-set", r#"["web"]"#).await.unwrap();
        assert_eq!(g.folders_of("ap-set").await.as_deref(), Some(r#"["web"]"#));
        g.apply("ap-set", "all").await.unwrap();
        assert_eq!(g.folders_of("ap-set").await.as_deref(), Some("all"));
        for none in [r#"["mobile"]"#, "[]"] {
            let refused = g.apply("ap-set", none).await.unwrap_err();
            assert_eq!(refused, "Check a folder for the chat to work in.");
        }
        assert_eq!(g.folders_of("ap-set").await.as_deref(), Some("all"));

        // api's own chat, its project's.
        let api = chat_open_project(g.db(), g.path("api").to_string_lossy().into_owned())
            .await
            .unwrap();
        let folder_chat = NewDialog {
            id: "ap-folder".into(),
            title: "Login".into(),
            created_at: 1,
            updated_at: 1,
            project_id: Some(api.project.id),
            folders: None,
        };
        g.db()
            .write(move |c| store::create_dialog(c, &folder_chat))
            .await
            .unwrap();
        let refused = g.apply("ap-folder", r#"["api"]"#).await.unwrap_err();
        assert!(refused.contains("works in one folder"), "{refused}");
        assert_eq!(g.folders_of("ap-folder").await, None);
    }

    /// D32: a workspace chat's folders don't change while it's at work, has
    /// commands running or its Autopilot task waits out a rate limit, nor
    /// when a folder it would leave has changes that haven't landed, which
    /// is named; a missing folder without changes can be unchecked.
    #[tokio::test]
    async fn apply_is_refused_while_the_chat_is_at_work_or_would_leave_changes() {
        let g = Guarded::new().await;
        g.chat("ap-busy", "Fix login", r#"["api","web"]"#).await;
        let hold = crate::chat_hold::take("ap-busy").unwrap();
        let refused = g.apply("ap-busy", r#"["api"]"#).await.unwrap_err();
        assert!(refused.contains("“Fix login” is at work"), "{refused}");
        drop(hold);

        g.chat("ap-waits", "Checkout", "all").await;
        g.task_waiting("ap-waits", "ap-task").await;
        let refused = g.apply("ap-waits", r#"["api"]"#).await.unwrap_err();
        assert!(refused.contains("“Checkout” is at work"), "{refused}");

        g.chat("ap-running", "Server", r#"["api","web"]"#).await;
        let queued = crate::command_tool::Queued::of(
            crate::workload::JobKind::Agent {
                owner: "ap-running".into(),
            },
            None,
        );
        let command = match cfg!(windows) {
            true => "for /L %i in (1,1,2000000000) do @rem",
            false => "sleep 60",
        };
        let root = g.path("api").to_string_lossy().into_owned();
        crate::command_tool::background::start(
            "ap-running",
            &root,
            command,
            &|| false,
            queued,
            std::sync::Arc::new(|| {}),
        )
        .unwrap();
        let refused = g.apply("ap-running", r#"["api"]"#).await.unwrap_err();
        crate::command_tool::background::stop_chat("ap-running");
        assert!(
            refused.contains("“Server” has commands running"),
            "{refused}"
        );

        g.chat("ap-changed", "Docs", r#"["api","web"]"#).await;
        g.changed_in_web("ap-changed");
        // web goes missing: its changes are still known, and kept.
        fs::rename(g.path("web"), g.path("web-away")).unwrap();
        let refused = g.apply("ap-changed", r#"["api"]"#).await.unwrap_err();
        assert_eq!(
            refused,
            "“Docs” has changes in web that aren't landed: land or discard them first."
        );
        assert_eq!(
            g.folders_of("ap-changed").await.as_deref(),
            Some(r#"["api","web"]"#)
        );
        // Without changes there, the missing folder is unchecked.
        crate::overlay::chats::discard("ap-changed").unwrap();
        g.apply("ap-changed", r#"["api"]"#).await.unwrap();
        assert_eq!(
            g.folders_of("ap-changed").await.as_deref(),
            Some(r#"["api"]"#)
        );
    }

    /// D19: a folder a workspace chat is at work in, runs a command in or has
    /// changes in isn't removed, the chat named; once none holds it, it goes,
    /// and leaves the folders of the chats that had it.
    #[tokio::test]
    async fn removing_a_folder_a_workspace_chat_uses_is_refused_naming_the_chat() {
        let g = Guarded::new().await;
        g.chat("rm-busy", "Fix login", "all").await;
        g.chat("rm-changed", "Docs", r#"["web"]"#).await;
        g.chat("rm-running", "Server", r#"["api","web"]"#).await;

        let hold = crate::chat_hold::take("rm-busy").unwrap();
        let refused = g.remove("web").await.unwrap_err();
        assert!(refused.contains("“Fix login” is at work"), "{refused}");
        drop(hold);

        let root = g.path("web").to_string_lossy().into_owned();
        let queued = crate::command_tool::Queued::of(
            crate::workload::JobKind::Agent {
                owner: "rm-running".into(),
            },
            None,
        );
        // Runs until it's stopped, in either shell: cmd.exe's own loop on
        // Windows, which needs no program from the PATH.
        let command = match cfg!(windows) {
            true => "for /L %i in (1,1,2000000000) do @rem",
            false => "sleep 60",
        };
        crate::command_tool::background::start(
            "rm-running",
            &root,
            command,
            &|| false,
            queued,
            std::sync::Arc::new(|| {}),
        )
        .unwrap();
        let refused = g.remove("web").await.unwrap_err();
        crate::command_tool::background::stop_chat("rm-running");
        assert!(
            refused.contains("“Server” has commands running"),
            "{refused}"
        );

        g.changed_in_web("rm-changed");
        let refused = g.remove("web").await.unwrap_err();
        assert!(refused.contains("“Docs” has changes in it"), "{refused}");
        crate::overlay::chats::discard("rm-changed").unwrap();

        g.remove("web").await.unwrap();
        assert_eq!(g.folders_of("rm-changed").await.as_deref(), Some("[]"));
        assert_eq!(
            g.folders_of("rm-running").await.as_deref(),
            Some(r#"["api"]"#)
        );
        assert_eq!(g.folders_of("rm-busy").await.as_deref(), Some("all"));
    }

    /// D19: Locate… waits for a chat at work in the folder, and isn't to
    /// another project while a chat has changes there; to the same project
    /// (its ID file), the changes go with the folder.
    #[tokio::test]
    async fn locate_is_refused_while_a_chat_is_at_work_and_to_another_project_while_it_has_changes()
    {
        let g = Guarded::new().await;
        // web is a project, with its ID file.
        chat_open_project(g.db(), g.path("web").to_string_lossy().into_owned())
            .await
            .unwrap();
        g.chat("loc-busy", "Fix login", "all").await;
        g.chat("loc-changed", "Docs", r#"["web"]"#).await;
        g.changed_in_web("loc-changed");

        let hold = crate::chat_hold::take("loc-busy").unwrap();
        fs::rename(g.path("web"), g.path("web-moved")).unwrap();
        let refused = g.locate("web", "web-moved").await.unwrap_err();
        assert!(refused.contains("“Fix login” is at work"), "{refused}");
        drop(hold);

        folder(&g.path(""), "elsewhere");
        let refused = g.locate("web", "elsewhere").await.unwrap_err();
        assert!(refused.contains("“Docs” has changes in it"), "{refused}");

        let located = g.locate("web", "web-moved").await.unwrap();
        assert!(located.folders[1].path.ends_with("web-moved"));
        assert_eq!(
            crate::overlay::chats::changes_in_folder("loc-changed", "web").unwrap(),
            ["web/n.txt"],
            "its changes went with it"
        );
        crate::overlay::chats::discard("loc-changed").unwrap();
    }

    /// A landing of a workspace chat that a crash cut while the chat's
    /// folder `web` was away and moved, for the next test.
    fn cut_landing(g: &Guarded, chat: &str) -> std::path::PathBuf {
        use crate::folders::{Folder, Folders};
        let data = g.test.dir.0.join("overlays");
        let store = g.test.dir.0.join("web-store");
        let web = crate::overlay::fs::OverlayFs::open(&g.path("web"), &store, 1 << 30).unwrap();
        {
            use crate::vfs::backend::Backend;
            let ino = web.resolve("b.txt").unwrap();
            let size = crate::vfs::store::SetAttr {
                size: Some(0),
                ..Default::default()
            };
            web.setattr(ino, &size).unwrap();
            web.write(ino, 0, b"B\n").unwrap();
        }
        let folders = Folders::named(vec![
            Folder {
                name: "api".into(),
                path: g.path("api"),
            },
            Folder {
                name: "web".into(),
                path: g.path("web"),
            },
        ])
        .unwrap();
        let changes: Vec<_> = web.changes().into_iter().map(|c| c.under("web")).collect();
        let landed = crate::overlay::land::land_in(&data, chat, &folders, &changes, false)
            .unwrap()
            .unwrap();
        // As a crash leaves it: the file as before, the journal applying.
        fs::write(g.path("web/b.txt"), "b\n").unwrap();
        let journal = data
            .join("landings")
            .join(chat)
            .join(&landed.id)
            .join("journal.json");
        let text = fs::read_to_string(&journal).unwrap();
        fs::write(&journal, text.replace("\"done\"", "\"applying\"")).unwrap();
        journal
    }

    /// 4.6: located, the folder gets the landing a crash cut there.
    #[tokio::test]
    async fn after_locate_a_landing_a_crash_interrupted_in_that_folder_is_finished() {
        let g = Guarded::new().await;
        g.chat("landed", "Docs", "all").await;
        let journal = cut_landing(&g, "landed");
        fs::rename(g.path("web"), g.path("web-moved")).unwrap();
        g.locate("web", "web-moved").await.unwrap();
        assert_eq!(
            fs::read_to_string(g.path("web-moved/b.txt")).unwrap(),
            "B\n"
        );
        assert!(fs::read_to_string(&journal).unwrap().contains("\"done\""));
    }

    /// 4.6: a folder found again (Try again, a drive mounted back) gets the
    /// landing a crash cut there while it was away.
    #[tokio::test]
    async fn a_folder_found_again_gets_the_landing_a_crash_cut_there() {
        let g = Guarded::new().await;
        g.chat("landed-again", "Docs", "all").await;
        let journal = cut_landing(&g, "landed-again");
        chat_open_project(g.db(), g.path("web").to_string_lossy().into_owned())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(g.path("web/b.txt")).unwrap(), "B\n");
        assert!(fs::read_to_string(&journal).unwrap().contains("\"done\""));
    }
}

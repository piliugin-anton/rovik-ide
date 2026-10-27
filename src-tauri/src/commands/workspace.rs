//! Workspaces: several folders in one window (docs/design/workspaces-plan.md).
//! Each command is a thin wrapper around the core's, as commands/chat.rs's are
//! around chat_db/api.rs's.

use rovik_client::Remote;
use rovik_core::chat_db::{api, workspaces};
use rovik_core::Core;
use std::sync::Arc;
use tauri::State;

/// Every workspace, most recently opened first, each folder marked when it's
/// missing.
#[tauri::command]
pub async fn workspace_list(core: State<'_, Core>) -> Result<Vec<workspaces::Summary>, String> {
    api::workspace_list(&core.db).await
}

/// Makes a workspace with these folders, in this order; the window opens it
/// after (`switchTarget`).
#[tauri::command]
pub async fn workspace_create(
    core: State<'_, Core>,
    name: String,
    folders: Vec<String>,
) -> Result<workspaces::Workspace, String> {
    api::workspace_create(&core.db, name, folders).await
}

/// Adds folders to the workspace, each checked as it joins (D5).
#[tauri::command]
pub async fn workspace_add_folders(
    core: State<'_, Core>,
    workspace_id: String,
    folders: Vec<String>,
) -> Result<workspaces::Workspace, String> {
    api::workspace_add_folders(&core.db, workspace_id, folders).await
}

/// Removes a folder from the workspace; the last one stays (D5). Refused
/// while a chat on a host works in it there, or has changes in it there,
/// or can't be asked (D19).
#[tauri::command]
pub async fn workspace_remove_folder(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    if let Some(why) = (remote.folder_held_there(&workspace_id, &name, "removed", true)).await? {
        return Err(why);
    }
    api::workspace_remove_folder(&core.db, workspace_id, name).await
}

/// Locate…: the folder is at another place from now on, by its name (D8).
/// Refused as removing it is while a chat on a host holds it, its changes
/// counting only when the folder found is another project (D19).
#[tauri::command]
pub async fn workspace_relocate_folder(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    workspace_id: String,
    name: String,
    to: String,
) -> Result<workspaces::Workspace, String> {
    let same = api::locates_same_project(&core.db, &workspace_id, &name, to.clone()).await?;
    let verb = "located elsewhere";
    if let Some(why) = (remote.folder_held_there(&workspace_id, &name, verb, !same)).await? {
        return Err(why);
    }
    api::workspace_relocate_folder(&core.db, workspace_id, name, to).await
}

/// Whether a folder can join the workspace, and why not when it can't.
#[tauri::command]
pub async fn workspace_check_folder(
    core: State<'_, Core>,
    workspace_id: Option<String>,
    folder: String,
) -> Result<Option<workspaces::FolderProblem>, String> {
    api::workspace_check_folder(&core.db, workspace_id, folder).await
}

/// Apply (D32): a workspace chat works in `folders` from now on (`"all"`, or
/// the names checked as JSON), refused while it's at work, has commands
/// running, or would leave changes that haven't landed: here, or on its
/// host, which is asked.
#[tauri::command]
pub async fn chat_set_folders(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    folders: String,
) -> Result<(), String> {
    if let Some(why) = remote.leaving_held_there(&dialog_id, &folders).await? {
        return Err(why);
    }
    api::chat_set_folders(&core, dialog_id, folders).await
}

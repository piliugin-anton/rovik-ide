//! Native Git commands forward to the shared desktop backend.

use rovik_client::desktop_api::git::GitRequest;
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;

/// Runs `git_branches` in the backend.
#[tauri::command]
pub async fn git_branches(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::Branches { path }.into())
        .await
}

/// Runs `git_head` in the backend.
#[tauri::command]
pub async fn git_head(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend.desktop_raw(GitRequest::Head { path }.into()).await
}

/// Runs `git_checkout` in the backend.
#[tauri::command]
pub async fn git_checkout(
    backend: State<'_, DesktopConnection>,
    path: String,
    branch: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::Checkout { path, branch }.into())
        .await
}

/// Runs `git_create_branch` in the backend.
#[tauri::command]
pub async fn git_create_branch(
    backend: State<'_, DesktopConnection>,
    path: String,
    name: String,
    checkout: bool,
    from: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::CreateBranch {
                path,
                name,
                checkout,
                from,
            }
            .into(),
        )
        .await
}

/// Runs `git_delete_branch` in the backend.
#[tauri::command]
pub async fn git_delete_branch(
    backend: State<'_, DesktopConnection>,
    path: String,
    name: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::DeleteBranch { path, name }.into())
        .await
}

/// Runs `git_commit` in the backend.
#[tauri::command]
pub async fn git_commit(
    backend: State<'_, DesktopConnection>,
    path: String,
    message: String,
    no_verify: Option<bool>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::Commit {
                path,
                message,
                no_verify,
            }
            .into(),
        )
        .await
}

/// Runs `git_discard_all` in the backend.
#[tauri::command]
pub async fn git_discard_all(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::DiscardAll { path }.into())
        .await
}

/// Runs `git_base_content` in the backend.
#[tauri::command]
pub async fn git_base_content(
    backend: State<'_, DesktopConnection>,
    path: String,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::BaseContent { path, file }.into())
        .await
}

/// Runs `git_revert_file` in the backend.
#[tauri::command]
pub async fn git_revert_file(
    backend: State<'_, DesktopConnection>,
    path: String,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::RevertFile { path, file }.into())
        .await
}

/// Runs `git_revert_folder` in the backend.
#[tauri::command]
pub async fn git_revert_folder(
    backend: State<'_, DesktopConnection>,
    path: String,
    folder: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::RevertFolder { path, folder }.into())
        .await
}

/// Runs `git_log` in the backend.
#[tauri::command]
pub async fn git_log(
    backend: State<'_, DesktopConnection>,
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
    tip: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::Log {
                path,
                limit,
                query,
                offset,
                tip,
            }
            .into(),
        )
        .await
}

/// Runs `git_commit_show` in the backend.
#[tauri::command]
pub async fn git_commit_show(
    backend: State<'_, DesktopConnection>,
    path: String,
    oid: String,
    query: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::CommitShow { path, oid, query }.into())
        .await
}

/// Runs `git_commit_file_preview` in the backend.
#[tauri::command]
pub async fn git_commit_file_preview(
    backend: State<'_, DesktopConnection>,
    path: String,
    oid: String,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::CommitFilePreview { path, oid, file }.into())
        .await
}

/// Runs `git_hooks` in the backend.
#[tauri::command]
pub async fn git_hooks(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend.desktop_raw(GitRequest::Hooks { path }.into()).await
}

/// Runs `git_hook_create` in the backend.
#[tauri::command]
pub async fn git_hook_create(
    backend: State<'_, DesktopConnection>,
    path: String,
    name: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::HookCreate { path, name }.into())
        .await
}

/// Runs `git_hook_make_executable` in the backend.
#[tauri::command]
pub async fn git_hook_make_executable(
    backend: State<'_, DesktopConnection>,
    path: String,
    name: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::HookMakeExecutable { path, name }.into())
        .await
}

/// Runs `git_hook_delete` in the backend.
#[tauri::command]
pub async fn git_hook_delete(
    backend: State<'_, DesktopConnection>,
    path: String,
    name: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::HookDelete { path, name }.into())
        .await
}

/// Runs `git_status` in the backend.
#[tauri::command]
pub async fn git_status(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::Status { path }.into())
        .await
}

/// Runs `git_init` in the backend.
#[tauri::command]
pub async fn git_init(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend.desktop_raw(GitRequest::Init { path }.into()).await
}

/// Runs `git_fetch` in the backend.
#[tauri::command]
pub async fn git_fetch(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend.desktop_raw(GitRequest::Fetch { path }.into()).await
}

/// Runs `git_push` in the backend.
#[tauri::command]
pub async fn git_push(
    backend: State<'_, DesktopConnection>,
    path: String,
    no_verify: Option<bool>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::Push { path, no_verify }.into())
        .await
}

/// Runs `git_stash_push` in the backend.
#[tauri::command]
pub async fn git_stash_push(
    backend: State<'_, DesktopConnection>,
    path: String,
    message: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashPush { path, message }.into())
        .await
}

/// Runs `git_stash_list` in the backend.
#[tauri::command]
pub async fn git_stash_list(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashList { path }.into())
        .await
}

/// Runs `git_stash_pop` in the backend.
#[tauri::command]
pub async fn git_stash_pop(
    backend: State<'_, DesktopConnection>,
    path: String,
    index: Option<usize>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashPop { path, index }.into())
        .await
}

/// Runs `git_stash_drop` in the backend.
#[tauri::command]
pub async fn git_stash_drop(
    backend: State<'_, DesktopConnection>,
    path: String,
    index: usize,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashDrop { path, index }.into())
        .await
}

/// Runs `git_stash_show` in the backend.
#[tauri::command]
pub async fn git_stash_show(
    backend: State<'_, DesktopConnection>,
    path: String,
    index: usize,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashShow { path, index }.into())
        .await
}

/// Runs `git_stash_file_content` in the backend.
#[tauri::command]
pub async fn git_stash_file_content(
    backend: State<'_, DesktopConnection>,
    path: String,
    index: usize,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashFileContent { path, index, file }.into())
        .await
}

/// Runs `git_stash_file_preview` in the backend.
#[tauri::command]
pub async fn git_stash_file_preview(
    backend: State<'_, DesktopConnection>,
    path: String,
    index: usize,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::StashFilePreview { path, index, file }.into())
        .await
}

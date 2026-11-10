//! Native Git commands forward to the shared desktop backend.

use rovik_client::desktop_api::git::GitRequest;
use rovik_client::desktop_connection::DesktopConnection;
use rovik_client::git_sync::PullStrategy;
use tauri::State;

/// Runs `git_pull_config` in the backend.
#[tauri::command]
pub async fn git_pull_config(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::PullConfig { path }.into())
        .await
}

/// Runs `git_pull` in the backend.
#[tauri::command]
pub async fn git_pull(
    backend: State<'_, DesktopConnection>,
    path: String,
    strategy: Option<PullStrategy>,
    remember: Option<bool>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::Pull {
                path,
                strategy,
                remember,
            }
            .into(),
        )
        .await
}

/// Runs `git_rebase` in the backend.
#[tauri::command]
pub async fn git_rebase(
    backend: State<'_, DesktopConnection>,
    path: String,
    onto: String,
    remote: bool,
    rewrite_pushed: Option<bool>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::Rebase {
                path,
                onto,
                remote,
                rewrite_pushed,
            }
            .into(),
        )
        .await
}

/// Runs `git_operation_continue` in the backend.
#[tauri::command]
pub async fn git_operation_continue(
    backend: State<'_, DesktopConnection>,
    path: String,
    message: Option<String>,
    no_verify: Option<bool>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::OperationContinue {
                path,
                message,
                no_verify,
            }
            .into(),
        )
        .await
}

/// Runs `git_operation_abort` in the backend.
#[tauri::command]
pub async fn git_operation_abort(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::OperationAbort { path }.into())
        .await
}

/// Runs `git_conflict_file` in the backend.
#[tauri::command]
pub async fn git_conflict_file(
    backend: State<'_, DesktopConnection>,
    path: String,
    file: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(GitRequest::ConflictFile { path, file }.into())
        .await
}

/// Runs `git_conflict_resolve` in the backend.
#[tauri::command]
pub async fn git_conflict_resolve(
    backend: State<'_, DesktopConnection>,
    path: String,
    file: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(
            GitRequest::ConflictResolve {
                path,
                file,
                content,
                side,
            }
            .into(),
        )
        .await
}

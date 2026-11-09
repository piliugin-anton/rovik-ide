//! Native filesystem commands forward operations and deliver private watch events.
mod settings;
pub use rovik_client::filesystem::to_trash;
use rovik_client::{
    desktop_api::filesystem::FilesystemRequest, desktop_connection::DesktopConnection,
    filesystem::OpenFile,
};
use serde_json::Value;
pub use settings::*;
use tauri::{Emitter, State, WebviewWindow};

/// Forwards read_dir_entries to this client’s backend session.
#[tauri::command]
pub async fn read_dir_entries(
    backend: State<'_, DesktopConnection>,
    root_path: String,
    path: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::ReadDirEntries { root_path, path }.into())
        .await
}

/// Forwards read_file_content to this client’s backend session.
#[tauri::command]
pub async fn read_file_content(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::ReadFileContent { path }.into())
        .await
}

/// Forwards write_file_content to this client’s backend session.
#[tauri::command]
pub async fn write_file_content(
    backend: State<'_, DesktopConnection>,
    path: String,
    content: String,
    expected: Option<String>,
) -> Result<Value, String> {
    backend
        .desktop(
            FilesystemRequest::WriteFileContent {
                path,
                content,
                expected,
            }
            .into(),
        )
        .await
}

/// Forwards dir_entry_count to this client’s backend session.
#[tauri::command]
pub async fn dir_entry_count(
    backend: State<'_, DesktopConnection>,
    path: String,
    limit: Option<usize>,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::DirEntryCount { path, limit }.into())
        .await
}

/// Forwards path_exists to this client’s backend session.
#[tauri::command]
pub async fn path_exists(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::PathExists { path }.into())
        .await
}

/// Forwards read_editorconfigs to this client’s backend session.
#[tauri::command]
pub async fn read_editorconfigs(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::ReadEditorconfigs { path }.into())
        .await
}

/// Forwards load_last_project to this client’s backend session.
#[tauri::command]
pub async fn load_last_project(backend: State<'_, DesktopConnection>) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::LoadLastProject.into())
        .await
}

/// Forwards save_last_project to this client’s backend session.
#[tauri::command]
pub async fn save_last_project(
    backend: State<'_, DesktopConnection>,
    path: String,
    opened_files: Option<Vec<OpenFile>>,
    expanded_paths: Option<Vec<String>>,
    terminal_collapsed: bool,
) -> Result<Value, String> {
    backend
        .desktop(
            FilesystemRequest::SaveLastProject {
                path,
                opened_files,
                expanded_paths,
                terminal_collapsed,
            }
            .into(),
        )
        .await
}

/// Forwards get_path_id to this client’s backend session.
#[tauri::command]
pub async fn get_path_id(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::GetPathId { path }.into())
        .await
}

/// Forwards create_file to this client’s backend session.
#[tauri::command]
pub async fn create_file(
    backend: State<'_, DesktopConnection>,
    dir_id: String,
    name: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::CreateFile { dir_id, name }.into())
        .await
}

/// Forwards create_dir to this client’s backend session.
#[tauri::command]
pub async fn create_dir(
    backend: State<'_, DesktopConnection>,
    dir_id: String,
    name: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::CreateDir { dir_id, name }.into())
        .await
}

/// Forwards delete_path to this client’s backend session.
#[tauri::command]
pub async fn delete_path(
    backend: State<'_, DesktopConnection>,
    id: String,
    permanent: Option<bool>,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::DeletePath { id, permanent }.into())
        .await
}

/// Forwards rename_path to this client’s backend session.
#[tauri::command]
pub async fn rename_path(
    backend: State<'_, DesktopConnection>,
    id: String,
    new_name: String,
) -> Result<Value, String> {
    backend
        .desktop(FilesystemRequest::RenamePath { id, new_name }.into())
        .await
}

/// Forwards move_path to this client’s backend session.
#[tauri::command]
pub async fn move_path(
    backend: State<'_, DesktopConnection>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<Value, String> {
    backend
        .desktop(
            FilesystemRequest::MovePath {
                id,
                dest_dir_id,
                new_name,
            }
            .into(),
        )
        .await
}

/// Forwards copy_path to this client’s backend session.
#[tauri::command]
pub async fn copy_path(
    backend: State<'_, DesktopConnection>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<Value, String> {
    backend
        .desktop(
            FilesystemRequest::CopyPath {
                id,
                dest_dir_id,
                new_name,
            }
            .into(),
        )
        .await
}

/// Keeps the native image response as bytes rather than a JSON integer array.
#[tauri::command]
pub async fn read_image_file(
    backend: State<'_, DesktopConnection>,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    let bytes = backend
        .desktop_bytes(FilesystemRequest::ReadImageFile { path }.into())
        .await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Delivers watch changes only to the window that requested them.
#[tauri::command]
pub async fn start_file_watch(
    window: WebviewWindow,
    backend: State<'_, DesktopConnection>,
    path_str: String,
) -> Result<Value, Value> {
    backend
        .desktop_stream(
            |sink| FilesystemRequest::StartFileWatch { path_str, sink }.into(),
            move |value| match value["event"].as_str() {
                Some("fs-changes") => value["payload"].as_array().is_some_and(|changes| {
                    changes
                        .iter()
                        .all(|change| window.emit_to(window.label(), "fs-changes", change).is_ok())
                }),
                Some("git-changed") => window.emit_to(window.label(), "git-changed", ()).is_ok(),
                _ => false,
            },
        )
        .await
}

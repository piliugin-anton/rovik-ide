//! Filesystem operations share execution services, but retain client-owned file IDs and watches.
use super::{blocking, encode, DesktopRequest};
use crate::{
    desktop_backend::DesktopBackend,
    desktop_session::Session,
    filesystem::{self as fs, OpenFile},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Filesystem requests preserve the existing file tree and editor contracts.
#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum FilesystemRequest {
    /// read_dir_entries runs in the backend.
    ReadDirEntries { root_path: String, path: String },
    /// read_file_content runs in the backend.
    ReadFileContent { path: String },
    /// write_file_content runs in the backend.
    WriteFileContent {
        path: String,
        content: String,
        expected: Option<String>,
    },
    /// dir_entry_count runs in the backend.
    DirEntryCount { path: String, limit: Option<usize> },
    /// path_exists runs in the backend.
    PathExists { path: String },
    /// read_editorconfigs runs in the backend.
    ReadEditorconfigs { path: String },
    /// load_last_project runs in the backend.
    LoadLastProject,
    /// save_last_project runs in the backend.
    SaveLastProject {
        path: String,
        opened_files: Option<Vec<OpenFile>>,
        expanded_paths: Option<Vec<String>>,
        terminal_collapsed: bool,
    },
    /// get_path_id runs in the backend.
    GetPathId { path: String },
    /// create_file runs in the backend.
    CreateFile { dir_id: String, name: String },
    /// create_dir runs in the backend.
    CreateDir { dir_id: String, name: String },
    /// delete_path runs in the backend.
    DeletePath { id: String, permanent: Option<bool> },
    /// rename_path runs in the backend.
    RenamePath { id: String, new_name: String },
    /// move_path runs in the backend.
    MovePath {
        id: String,
        dest_dir_id: String,
        new_name: Option<String>,
    },
    /// copy_path runs in the backend.
    CopyPath {
        id: String,
        dest_dir_id: String,
        new_name: Option<String>,
    },
    /// Reads preview image bytes, encoded compactly on the local wire.
    ReadImageFile { path: String },
    /// Replaces only this client's file watch and callback.
    StartFileWatch { path_str: String, sink: String },
}

impl FilesystemRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> Result<serde_json::Value, String> {
        let owner = session.ok_or("A client session is required for filesystem operations")?;
        let state = owner.filesystem.clone();
        let data = backend.core.db.data_dir().to_path_buf();
        match self {
            Self::ReadDirEntries { root_path, path } => {
                encode(fs::read_dir_entries(state, root_path, path).await?)
            }
            Self::ReadFileContent { path } => encode(fs::read_file_content(path).await?),
            Self::WriteFileContent {
                path,
                content,
                expected,
            } => encode(blocking(move || fs::write_file_content(path, content, expected)).await?),
            Self::DirEntryCount { path, limit } => {
                encode(blocking(move || fs::dir_entry_count(path, limit)).await?)
            }
            Self::PathExists { path } => encode(blocking(move || Ok(fs::path_exists(path))).await?),
            Self::ReadEditorconfigs { path } => encode(fs::read_editorconfigs(path).await),
            Self::LoadLastProject => encode(blocking(move || fs::load_last_project(&data)).await?),
            Self::SaveLastProject {
                path,
                opened_files,
                expanded_paths,
                terminal_collapsed,
            } => encode(
                blocking(move || {
                    fs::save_last_project(
                        &data,
                        path,
                        opened_files,
                        expanded_paths,
                        terminal_collapsed,
                    )
                })
                .await?,
            ),
            Self::GetPathId { path } => {
                encode(blocking(move || fs::get_path_id(&state, path)).await?)
            }
            Self::CreateFile { dir_id, name } => {
                encode(blocking(move || fs::create_file(&state, dir_id, name)).await?)
            }
            Self::CreateDir { dir_id, name } => {
                encode(blocking(move || fs::create_dir(&state, dir_id, name)).await?)
            }
            Self::DeletePath { id, permanent } => {
                encode(fs::delete_path(&state, id, permanent).await?)
            }
            Self::RenamePath { id, new_name } => {
                encode(fs::rename_path(&state, id, new_name).await?)
            }
            Self::MovePath {
                id,
                dest_dir_id,
                new_name,
            } => encode(fs::move_path(&state, id, dest_dir_id, new_name).await?),
            Self::CopyPath {
                id,
                dest_dir_id,
                new_name,
            } => encode(fs::copy_path(&state, id, dest_dir_id, new_name).await?),
            Self::ReadImageFile { path } => encode(
                base64::engine::general_purpose::STANDARD.encode(fs::read_image_file(path).await?),
            ),
            Self::StartFileWatch { path_str, sink } => {
                owner.begin(&sink)?;
                let context = fs::WatchContext {
                    state,
                    core: backend.core.clone(),
                    owner: Arc::downgrade(owner),
                    sink: sink.clone(),
                };
                let result = fs::start_file_watch(context, path_str).await;
                if result.is_err() {
                    owner.cancel_sink(&sink);
                }
                encode(result?)
            }
        }
    }
}

impl From<FilesystemRequest> for DesktopRequest {
    fn from(request: FilesystemRequest) -> Self {
        Self::Filesystem(Box::new(request))
    }
}

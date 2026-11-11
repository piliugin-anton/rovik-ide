//! Isolated chats' changes (overlay/chats.rs): their status, landing them in
//! the project, undoing a landing, and their conflicts. What changes a
//! chat's files is refused while its run is going. A remote chat's bar
//! asks its host for what's there (rovik-client's `remote/bar.rs`), and
//! works on its imported result here for the rest (6.1). It also has the
//! chat's sync with the project (`remote/sync.rs`) and a newer result held
//! back while its conflicts are resolved (U05).

use crate::HandedSync;
use rovik_core::merge::ConflictFile;
use rovik_core::overlay::chats::{
    self, FileVersions, HandedOver, LandResult, OverlayStatus, UndoneMerge,
};
use rovik_core::overlay::land::Conflict;
use rovik_protocol::work::ChatSync;

use super::encode;
use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

/// Desktop overlay operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum OverlayRequest {
    /// overlay_status on the backend.
    Status { dialog_id: String },
    /// overlay_file on the backend.
    File { dialog_id: String, path: String },
    /// overlay_discard on the backend.
    Discard { dialog_id: String },
    /// overlay_land on the backend.
    Land { dialog_id: String, keep_chat: bool },
    /// overlay_undo on the backend.
    Undo { dialog_id: String, id: String },
    /// overlay_undo_merge on the backend.
    UndoMerge { dialog_id: String },
    /// overlay_unmark on the backend.
    Unmark {
        dialog_id: String,
        files: Vec<String>,
    },
    /// overlay_conflict_file on the backend.
    ConflictFile { dialog_id: String, path: String },
    /// overlay_conflict_resolve on the backend.
    ConflictResolve {
        dialog_id: String,
        path: String,
        content: Option<String>,
        side: Option<String>,
    },
    /// overlay_conflicts_to_chat on the backend.
    ConflictsToChat { dialog_id: String },
    /// overlay_redo_group on the backend.
    RedoGroup { dialog_id: String, group: String },
    /// overlay_sync_now on the backend.
    SyncNow { dialog_id: String },
    /// overlay_sync_to_chat on the backend.
    SyncToChat { dialog_id: String },
    /// overlay_keep_older on the backend.
    KeepOlder { dialog_id: String },
    /// overlay_take_newer on the backend.
    TakeNewer { dialog_id: String },
}

impl OverlayRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Status { dialog_id } => encode(overlay_status(backend, dialog_id).await?),
            Self::File { dialog_id, path } => encode(overlay_file(backend, dialog_id, path).await?),
            Self::Discard { dialog_id } => encode(overlay_discard(backend, dialog_id).await?),
            Self::Land {
                dialog_id,
                keep_chat,
            } => encode(overlay_land(backend, dialog_id, keep_chat).await?),
            Self::Undo { dialog_id, id } => encode(overlay_undo(backend, dialog_id, id).await?),
            Self::UndoMerge { dialog_id } => encode(overlay_undo_merge(backend, dialog_id).await?),
            Self::Unmark { dialog_id, files } => {
                encode(overlay_unmark(backend, dialog_id, files).await?)
            }
            Self::ConflictFile { dialog_id, path } => {
                encode(overlay_conflict_file(backend, dialog_id, path).await?)
            }
            Self::ConflictResolve {
                dialog_id,
                path,
                content,
                side,
            } => encode(overlay_conflict_resolve(backend, dialog_id, path, content, side).await?),
            Self::ConflictsToChat { dialog_id } => {
                encode(overlay_conflicts_to_chat(backend, dialog_id).await?)
            }
            Self::RedoGroup { dialog_id, group } => {
                encode(overlay_redo_group(backend, dialog_id, group).await?)
            }
            Self::SyncNow { dialog_id } => encode(overlay_sync_now(backend, dialog_id).await?),
            Self::SyncToChat { dialog_id } => {
                encode(overlay_sync_to_chat(backend, dialog_id).await?)
            }
            Self::KeepOlder { dialog_id } => encode(overlay_keep_older(backend, dialog_id).await?),
            Self::TakeNewer { dialog_id } => encode(overlay_take_newer(backend, dialog_id).await?),
        }
    }
}

async fn overlay_status(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<OverlayStatus, String> {
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        return remote.overlay_status(&dialog_id).await;
    }
    chats::overlay_status(dialog_id).await
}

async fn overlay_file(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
) -> Result<FileVersions, String> {
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        return remote.overlay_file(&dialog_id, &path).await;
    }
    chats::overlay_file(dialog_id, path).await
}

async fn overlay_discard(backend: &DesktopBackend, dialog_id: String) -> Result<(), String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return remote.discard(&dialog_id).await;
    }
    chats::overlay_discard(&core.agents, dialog_id).await
}

async fn overlay_land(
    backend: &DesktopBackend,
    dialog_id: String,
    keep_chat: bool,
) -> Result<LandResult, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return Ok(LandResult::from(remote.land(&dialog_id, keep_chat).await));
    }
    chats::overlay_land(&core.agents, dialog_id, keep_chat).await
}

async fn overlay_undo(
    backend: &DesktopBackend,
    dialog_id: String,
    id: String,
) -> Result<Vec<Conflict>, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
    }
    chats::overlay_undo(&core.agents, dialog_id, id).await
}

async fn overlay_undo_merge(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<UndoneMerge, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        return remote.undo_merge(&dialog_id, None).await;
    }
    chats::overlay_undo_merge(&core.agents, dialog_id).await
}

async fn overlay_unmark(
    backend: &DesktopBackend,
    dialog_id: String,
    files: Vec<String>,
) -> Result<UndoneMerge, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        return remote.undo_merge(&dialog_id, Some(files)).await;
    }
    chats::overlay_unmark(&core.agents, dialog_id, files).await
}

async fn overlay_conflict_file(
    _backend: &DesktopBackend,
    dialog_id: String,
    path: String,
) -> Result<ConflictFile, String> {
    chats::overlay_conflict_file(dialog_id, path).await
}

async fn overlay_conflict_resolve(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<Conflict>, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
    }
    chats::overlay_conflict_resolve(&core.agents, dialog_id, path, content, side).await
}

async fn overlay_conflicts_to_chat(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<HandedOver, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        remote.check_idle(&dialog_id).await?;
        return remote.conflicts_to_chat(&dialog_id).await;
    }
    chats::overlay_conflicts_to_chat(&core.agents, dialog_id).await
}

async fn overlay_redo_group(
    backend: &DesktopBackend,
    dialog_id: String,
    group: String,
) -> Result<String, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    remote.local_only(&dialog_id)?;
    chats::overlay_redo_group(&core.agents, dialog_id, group).await
}

async fn overlay_sync_now(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<Option<ChatSync>, String> {
    let remote = &backend.remote;
    remote.check_idle(&dialog_id).await?;
    remote.sync_now(&dialog_id).await
}

async fn overlay_sync_to_chat(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<HandedSync, String> {
    let remote = &backend.remote;
    remote.check_idle(&dialog_id).await?;
    remote.sync_to_chat(&dialog_id).await
}

async fn overlay_keep_older(backend: &DesktopBackend, dialog_id: String) -> Result<(), String> {
    let remote = &backend.remote;
    remote.keep_older_copy(&dialog_id)
}

async fn overlay_take_newer(backend: &DesktopBackend, dialog_id: String) -> Result<(), String> {
    let remote = &backend.remote;
    remote.take_newer(&dialog_id).await
}

//! The virtual files (vfs): the volume's driver and mount, its files and
//! their history, and its events streamed to the frontend.

use rovik_core::vfs::driver::InstallOutcome;
use rovik_core::vfs::history::{HistoryLimits, RevisionInfo};
use rovik_core::vfs::{self, VfsEntry, VfsFile, VfsStatus};

use super::encode;
use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

/// Desktop vfs operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum VfsRequest {
    /// Subscribe before a producer starts sending events to this client's sink.
    Subscribe {
        kinds: Option<Vec<String>>,
        sink: String,
    },
    /// Stop only a subscription owned by this client.
    Unsubscribe { id: u64 },
    /// vfs_status on the backend.
    Status,
    /// vfs_install_driver on the backend.
    InstallDriver,
    /// vfs_mount on the backend.
    Mount { mountpoint: Option<String> },
    /// vfs_unmount on the backend.
    Unmount,
    /// vfs_write_file on the backend.
    WriteFile {
        path: String,
        content: String,
        encoding: Option<String>,
        executable: Option<bool>,
    },
    /// vfs_read_file on the backend.
    ReadFile { path: String },
    /// vfs_list on the backend.
    List { path: Option<String> },
    /// vfs_revisions on the backend.
    Revisions { path: String },
    /// vfs_read_revision on the backend.
    ReadRevision {
        path: String,
        revision: u64,
        encoding: Option<String>,
    },
    /// vfs_restore_revision on the backend.
    RestoreRevision { path: String, revision: u64 },
    /// vfs_set_history_limits on the backend.
    SetHistoryLimits { limits: HistoryLimits },
    /// vfs_remove on the backend.
    Remove { path: String },
}

impl VfsRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&std::sync::Arc<crate::desktop_session::Session>>,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Subscribe { kinds, sink } => {
                let session = session.ok_or("A client session is required for VFS events")?;
                session.begin(&sink)?;
                let target = std::sync::Arc::downgrade(session);
                let channel = sink.clone();
                let id = vfs::vfs_subscribe(&backend.core.vfs, kinds, move |batch| {
                    target
                        .upgrade()
                        .is_some_and(|session| session.emit(&channel, batch))
                })
                .await;
                let id = match id {
                    Ok(id) => id,
                    Err(error) => {
                        session.end(&sink);
                        return Err(error);
                    }
                };
                let volume = backend.core.vfs.clone();
                session.register(format!("vfs:{id}"), sink, move || {
                    volume.unsubscribe(rovik_core::vfs::SubscriptionId(id));
                })?;
                encode(id)
            }
            Self::Unsubscribe { id } => {
                let session = session.ok_or("A client session is required for VFS events")?;
                encode(session.remove(&format!("vfs:{id}")))
            }
            Self::Status => encode(vfs_status(backend).await?),
            Self::InstallDriver => encode(vfs_install_driver(backend).await?),
            Self::Mount { mountpoint } => encode(vfs_mount(backend, mountpoint).await?),
            Self::Unmount => encode(vfs_unmount(backend).await?),
            Self::WriteFile {
                path,
                content,
                encoding,
                executable,
            } => encode(vfs_write_file(backend, path, content, encoding, executable).await?),
            Self::ReadFile { path } => encode(vfs_read_file(backend, path).await?),
            Self::List { path } => encode(vfs_list(backend, path).await?),
            Self::Revisions { path } => encode(vfs_revisions(backend, path).await?),
            Self::ReadRevision {
                path,
                revision,
                encoding,
            } => encode(vfs_read_revision(backend, path, revision, encoding).await?),
            Self::RestoreRevision { path, revision } => {
                encode(vfs_restore_revision(backend, path, revision).await?)
            }
            Self::SetHistoryLimits { limits } => {
                encode(vfs_set_history_limits(backend, limits).await?)
            }
            Self::Remove { path } => encode(vfs_remove(backend, path).await?),
        }
    }
}

async fn vfs_status(backend: &DesktopBackend) -> Result<VfsStatus, String> {
    let core = &backend.core;
    vfs::vfs_status(&core.vfs).await
}

async fn vfs_install_driver(_backend: &DesktopBackend) -> Result<InstallOutcome, String> {
    vfs::vfs_install_driver().await
}

async fn vfs_mount(backend: &DesktopBackend, mountpoint: Option<String>) -> Result<String, String> {
    let core = &backend.core;
    vfs::vfs_mount(&core.vfs, mountpoint).await
}

async fn vfs_unmount(backend: &DesktopBackend) -> Result<(), String> {
    let core = &backend.core;
    vfs::vfs_unmount(&core.vfs).await
}

async fn vfs_write_file(
    backend: &DesktopBackend,
    path: String,
    content: String,
    encoding: Option<String>,
    executable: Option<bool>,
) -> Result<VfsFile, String> {
    let core = &backend.core;
    vfs::vfs_write_file(&core.vfs, path, content, encoding, executable).await
}

async fn vfs_read_file(backend: &DesktopBackend, path: String) -> Result<String, String> {
    let core = &backend.core;
    vfs::vfs_read_file(&core.vfs, path).await
}

async fn vfs_list(backend: &DesktopBackend, path: Option<String>) -> Result<Vec<VfsEntry>, String> {
    let core = &backend.core;
    vfs::vfs_list(&core.vfs, path).await
}

async fn vfs_revisions(
    backend: &DesktopBackend,
    path: String,
) -> Result<Vec<RevisionInfo>, String> {
    let core = &backend.core;
    vfs::vfs_revisions(&core.vfs, path).await
}

async fn vfs_read_revision(
    backend: &DesktopBackend,
    path: String,
    revision: u64,
    encoding: Option<String>,
) -> Result<String, String> {
    let core = &backend.core;
    vfs::vfs_read_revision(&core.vfs, path, revision, encoding).await
}

async fn vfs_restore_revision(
    backend: &DesktopBackend,
    path: String,
    revision: u64,
) -> Result<VfsFile, String> {
    let core = &backend.core;
    vfs::vfs_restore_revision(&core.vfs, path, revision).await
}

async fn vfs_set_history_limits(
    backend: &DesktopBackend,
    limits: HistoryLimits,
) -> Result<(), String> {
    let core = &backend.core;
    vfs::vfs_set_history_limits(&core.vfs, limits).await
}

async fn vfs_remove(backend: &DesktopBackend, path: String) -> Result<(), String> {
    let core = &backend.core;
    vfs::vfs_remove(&core.vfs, path).await
}

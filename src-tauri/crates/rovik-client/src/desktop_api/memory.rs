//! Backend-owned memory operations.

use crate::desktop_backend::DesktopBackend;
use crate::Remote;
use rovik_core::memory::{self, MemoryEntry, MemoryStats, SharedMemoryStore};
use rovik_core::Core;
use serde::{Deserialize, Serialize};

/// Desktop memory operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum MemoryRequest {
    /// memory_stats on the backend.
    Stats { root: String },
    /// memory_list on the backend.
    List {
        root: String,
        query: String,
        before: Option<String>,
    },
    /// memory_edit on the backend.
    Edit {
        root: String,
        id: String,
        title: String,
        content: String,
    },
    /// memory_delete on the backend.
    Delete { root: String, id: String },
    /// memory_clear on the backend.
    Clear { root: String },
}

impl MemoryRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::Stats { root } => super::encode_result(memory_stats(backend, root).await),
            Self::List {
                root,
                query,
                before,
            } => super::encode_result(memory_list(backend, root, query, before).await),
            Self::Edit {
                root,
                id,
                title,
                content,
            } => super::encode_result(memory_edit(backend, root, id, title, content).await),
            Self::Delete { root, id } => {
                super::encode_result(memory_delete(backend, root, id).await)
            }
            Self::Clear { root } => super::encode_result(memory_clear(backend, root).await),
        }
    }
}

/// The store of the project in `root`, or the page's error text: one SQLite
/// file per project, so opening it reads the disk.
fn open_store(core: &Core, root: &str) -> Result<SharedMemoryStore, String> {
    core.memory
        .open(&core.paths, std::path::Path::new(root))
        .ok_or_else(|| "The project's memory couldn't be opened.".to_string())
}

// A disconnected host must not make the local manager unavailable.
async fn refresh_remote(core: &Core, remote: &Remote, root: &str) {
    if let Err(error) = remote.sync_project_memory(std::path::Path::new(root)).await {
        core.emit(rovik_protocol::events::AgentEvent::Notice {
            dialog_id: None,
            kind: "warning".into(),
            title: "Memory synchronization".into(),
            message: format!("Showing the local memory copy; cloud refresh failed: {error}"),
        });
    }
}

async fn memory_stats(backend: &DesktopBackend, root: String) -> Result<MemoryStats, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    refresh_remote(core, remote, &root).await;
    let core = (*core).clone();
    // Opening the store reads a file: off the main thread.
    super::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::stats(&store, &root)
    })
    .await
}

async fn memory_list(
    backend: &DesktopBackend,
    root: String,
    query: String,
    before: Option<String>,
) -> Result<Vec<MemoryEntry>, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    refresh_remote(core, remote, &root).await;
    let core = (*core).clone();
    super::blocking(move || {
        let store = open_store(&core, &root)?;
        store
            .list_page(&root, &query, before.as_deref())
            .map(|rows| rows.iter().map(MemoryEntry::from).collect())
            .map_err(|e| e.to_string())
    })
    .await
}

async fn memory_edit(
    backend: &DesktopBackend,
    root: String,
    id: String,
    title: String,
    content: String,
) -> Result<(), String> {
    let core = &backend.core;
    let remote = &backend.remote;
    let core = (*core).clone();
    let sync_root = root.clone();
    super::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::edit(&store, &id, &title, &content)
    })
    .await?;
    remote
        .sync_project_memory(std::path::Path::new(&sync_root))
        .await
        .map_err(|e| {
            format!("The change was saved locally, but cloud memory synchronization failed: {e}")
        })?;
    Ok(())
}

async fn memory_delete(backend: &DesktopBackend, root: String, id: String) -> Result<(), String> {
    let core = &backend.core;
    let remote = &backend.remote;
    let core = (*core).clone();
    let sync_root = root.clone();
    super::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::remove(&store, &id)
    })
    .await?;
    remote
        .sync_project_memory(std::path::Path::new(&sync_root))
        .await
        .map_err(|e| {
            format!("The change was saved locally, but cloud memory synchronization failed: {e}")
        })?;
    Ok(())
}

async fn memory_clear(backend: &DesktopBackend, root: String) -> Result<u32, String> {
    let core = &backend.core;
    let remote = &backend.remote;
    let core = (*core).clone();
    let sync_root = root.clone();
    let result = super::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::clear(&store, &root)
    })
    .await?;
    remote
        .sync_project_memory(std::path::Path::new(&sync_root))
        .await
        .map_err(|e| {
            format!("The change was saved locally, but cloud memory synchronization failed: {e}")
        })?;
    Ok(result)
}

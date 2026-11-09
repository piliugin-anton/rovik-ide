//! The projects' long-term memory, as Settings shows it: how many memories
//! the open project has, and the list the manager lists, corrects and
//! removes (rovik-core/src/memory.rs). Whether the project uses its memory at
//! all is the project's own setting, saved with its other settings
//! (project_settings).

use rovik_client::desktop_api::{memory::MemoryRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;

#[tauri::command]
pub async fn memory_stats(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(MemoryRequest::Stats { root }))
        .await
}

/// Settings → Long-term memory: the project in `root`'s memories, newest
/// first — those mentioning `query`, or every one when it's empty.
#[tauri::command]
pub async fn memory_list(
    backend: State<'_, DesktopConnection>,
    root: String,
    query: String,
    before: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(MemoryRequest::List {
            root,
            query,
            before,
        }))
        .await
}

/// Settings → Long-term memory: replaces the title and text of the memory
/// `id` in the project in `root`.
#[tauri::command]
pub async fn memory_edit(
    backend: State<'_, DesktopConnection>,
    root: String,
    id: String,
    title: String,
    content: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(MemoryRequest::Edit {
            root,
            id,
            title,
            content,
        }))
        .await
}

/// Settings → Long-term memory: removes the memory `id` of the project in
/// `root` for good.
#[tauri::command]
pub async fn memory_delete(
    backend: State<'_, DesktopConnection>,
    root: String,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(MemoryRequest::Delete { root, id }))
        .await
}

/// Settings → Long-term memory: removes every memory of the project in
/// `root`, and says how many went.
#[tauri::command]
pub async fn memory_clear(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(MemoryRequest::Clear { root }))
        .await
}

//! The projects' long-term memory, as Settings shows it: how many memories
//! the open project has, and the list the manager lists, corrects and
//! removes (rovik-core/src/memory.rs). Whether the project uses its memory at
//! all is the project's own setting, saved with its other settings
//! (project_settings).

use rovik_core::memory::{self, MemoryEntry, MemoryStats, SharedMemoryStore};
use rovik_core::Core;
use tauri::State;

/// The store of the project in `root`, or the page's error text: one SQLite
/// file per project, so opening it reads the disk.
fn open_store(core: &Core, root: &str) -> Result<SharedMemoryStore, String> {
    core.memory
        .open(&core.paths, std::path::Path::new(root))
        .ok_or_else(|| "The project's memory couldn't be opened.".to_string())
}

/// Settings → Long-term memory: what the project in `root`'s memory holds.
#[tauri::command]
pub async fn memory_stats(core: State<'_, Core>, root: String) -> Result<MemoryStats, String> {
    let core = (*core).clone();
    // Opening the store reads a file: off the main thread.
    crate::util::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::stats(&store, &root)
    })
    .await
}

/// Settings → Long-term memory: the project in `root`'s memories, newest
/// first — those mentioning `query`, or every one when it's empty.
#[tauri::command]
pub async fn memory_list(
    core: State<'_, Core>,
    root: String,
    query: String,
) -> Result<Vec<MemoryEntry>, String> {
    let core = (*core).clone();
    crate::util::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::list(&store, &root, &query)
    })
    .await
}

/// Settings → Long-term memory: replaces the title and text of the memory
/// `id` in the project in `root`.
#[tauri::command]
pub async fn memory_edit(
    core: State<'_, Core>,
    root: String,
    id: String,
    title: String,
    content: String,
) -> Result<(), String> {
    let core = (*core).clone();
    crate::util::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::edit(&store, &id, &title, &content)
    })
    .await
}

/// Settings → Long-term memory: removes the memory `id` of the project in
/// `root` for good.
#[tauri::command]
pub async fn memory_delete(core: State<'_, Core>, root: String, id: String) -> Result<(), String> {
    let core = (*core).clone();
    crate::util::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::remove(&store, &id)
    })
    .await
}

/// Settings → Long-term memory: removes every memory of the project in
/// `root`, and says how many went.
#[tauri::command]
pub async fn memory_clear(core: State<'_, Core>, root: String) -> Result<u32, String> {
    let core = (*core).clone();
    crate::util::blocking(move || {
        let store = open_store(&core, &root)?;
        memory::clear(&store, &root)
    })
    .await
}

//! The projects' long-term memory, as Settings shows it: how many memories
//! the open project has (rovik-core/src/memory.rs). Whether the project uses
//! its memory at all is the project's own setting, saved with its other
//! settings (project_settings).

use rovik_core::memory::{self, MemoryStats};
use rovik_core::Core;
use tauri::State;

/// Settings → Long-term memory: what the project in `root`'s memory holds.
#[tauri::command]
pub async fn memory_stats(core: State<'_, Core>, root: String) -> Result<MemoryStats, String> {
    let core = (*core).clone();
    // Opening the store reads a file: off the main thread.
    crate::util::blocking(move || {
        let store = core
            .memory
            .open(&core.paths, std::path::Path::new(&root))
            .ok_or("The project's memory couldn't be opened.")?;
        memory::stats(&store, &root)
    })
    .await
}

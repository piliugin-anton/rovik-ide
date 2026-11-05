//! The project's long-term memory (`verified-code-memory`): what earlier
//! runs of the project's chats learned, kept in one SQLite file per project
//! in the app's data folder, and the setting that turns it off for one
//! project. The runs read and write it (agent_run/memory.rs); Settings shows
//! how much it holds and the switch.

use crate::core::Paths;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use verified_code_memory::SharedMemoryStore;

pub use verified_code_memory::{MemoryRecord, MemoryStats};

/// The key a project's settings use for its memory, beside `checks` and
/// `restrictedFiles` (project-settings.json).
const SETTINGS_KEY: &str = "memory";

/// Whether one project's chats use its memory. On unless the user turned it
/// off: a project that never chose keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySettings {
    /// The project's chats recall and record memories.
    pub enabled: bool,
}

impl Default for MemorySettings {
    fn default() -> Self {
        MemorySettings { enabled: true }
    }
}

/// The project's choice, as Settings saved it (`settings_root` is the
/// project's folder, which its settings are kept under).
pub fn load(config_dir: &Path, settings_root: &str) -> MemorySettings {
    let settings = crate::project_profile::load_project_settings(config_dir, settings_root);
    MemorySettings {
        enabled: settings
            .get(SETTINGS_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    }
}

/// Where the memory of the project in `project` is kept: a file of its own
/// in the data folder, named as the project's overlay folder is
/// (`overlay::chats::project_key`), so a project is the same file whichever
/// chat, isolated or not, asks.
pub fn store_path(paths: &Paths, project: &Path) -> PathBuf {
    paths.data_dir.join("memory").join(format!(
        "{}.db",
        crate::overlay::chats::project_key(project)
    ))
}

/// The memory stores open in this process, one per project file: a store is
/// a SQLite connection behind a lock, so the runs and Settings ask for the
/// project's and share it.
#[derive(Default)]
pub struct MemoryState {
    stores: Mutex<HashMap<PathBuf, SharedMemoryStore>>,
}

impl MemoryState {
    /// The store of the project in `project`, opened the first time it's
    /// asked for. None when its file can't be opened (permissions, a full
    /// disk, a damaged file): logged, and the run goes on without memory
    /// rather than stopping for it.
    pub fn open(&self, paths: &Paths, project: &Path) -> Option<SharedMemoryStore> {
        let path = store_path(paths, project);
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = stores.get(&path) {
            return Some(store.clone());
        }
        if let Some(folder) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(folder) {
                eprintln!("memory: {} couldn't be made: {e}", folder.display());
                return None;
            }
        }
        match SharedMemoryStore::open(&path) {
            Ok(store) => {
                stores.insert(path, store.clone());
                Some(store)
            }
            Err(e) => {
                eprintln!("memory: {} couldn't be opened: {e}", path.display());
                None
            }
        }
    }
}

/// How many memories the project in `repo` has (Settings → Long-term
/// memory). `repo` is the project's folder, as a run names it.
pub fn stats(store: &SharedMemoryStore, repo: &str) -> Result<MemoryStats, String> {
    store.stats(repo).map_err(|e| e.to_string())
}

//! The project's long-term memory (`verified-code-memory`): what earlier
//! runs of the project's chats learned, kept in one SQLite file per project
//! in the app's data folder, and the setting that turns it off for one
//! project. The runs read and write it (agent_run/memory.rs); Settings shows
//! how much it holds, the switch, and the manager that lists, corrects and
//! removes what's there.

use crate::core::Paths;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub use verified_code_memory::sync;
pub use verified_code_memory::{MemoryRecord, MemoryStats, SharedMemoryStore};

/// Omits claims bound to files the user does not allow the agent to read.
/// Deletion markers and clear epochs always propagate.
pub fn filter_sync_page(page: &mut sync::Page, root: &Path, settings: &serde_json::Value) {
    let restricted = crate::restricted_files::RestrictedFiles::from_settings(settings);
    page.changes.retain(|change| {
        change.record.as_ref().is_none_or(|record| {
            record.files.iter().all(|file| {
                restricted
                    .blocked(
                        &root.to_string_lossy(),
                        file,
                        crate::restricted_files::Op::Read,
                    )
                    .is_none()
            })
        })
    });
}

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

/// Whether new claims and run episodes are captured. Disabling capture leaves
/// retrieval available and provides an independent rollback for write behavior.
pub fn capture_enabled(config_dir: &Path, repo: &str) -> bool {
    load(config_dir, repo).enabled
        && crate::project_profile::load_project_settings(config_dir, repo)
            .get("memoryCapture")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
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
    aliases: Mutex<HashMap<PathBuf, String>>,
}

impl MemoryState {
    /// The storage scope of a folder. Cloud worktrees bind to a stable project
    /// identity; desktop folders retain their existing local history.
    pub fn repo(&self, project: &Path) -> String {
        self.aliases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(project)
            .cloned()
            .unwrap_or_else(|| project.to_string_lossy().into_owned())
    }

    /// A stable cloud project scope, independent of any chat's worktree.
    pub fn project_repo(project: &str) -> String {
        format!("project:{project}")
    }

    /// Opens the shared cloud store without requiring an existing worktree.
    pub fn open_project(&self, paths: &Paths, project: &str) -> Option<SharedMemoryStore> {
        self.open(paths, Path::new(&Self::project_repo(project)))
    }

    /// Binds a trusted daemon worktree to its registered project. Existing
    /// per-worktree history is migrated with durable progress; clear epochs
    /// prevent an old file from resurrecting cleared history on restart.
    pub fn bind_project(&self, paths: &Paths, root: &Path, project: &str) -> Result<(), String> {
        let repo = Self::project_repo(project);
        if self.repo(root) == repo {
            return Ok(());
        }
        let target = self
            .open_project(paths, project)
            .ok_or("The cloud project's memory couldn't be opened.")?;
        let legacy = store_path(paths, root);
        if legacy.is_file() {
            let source = SharedMemoryStore::open(legacy).map_err(|e| e.to_string())?;
            let replica = source.sync_replica().map_err(|e| e.to_string())?;
            let peer = format!("migration:{replica}");
            let mut progress = target
                .sync_progress(&repo, &peer)
                .map_err(|e| e.to_string())?;
            loop {
                let page = source
                    .sync_export(&root.to_string_lossy(), progress.received)
                    .map_err(|e| e.to_string())?;
                target.sync_apply(&repo, &page).map_err(|e| e.to_string())?;
                progress.replica = replica.clone();
                progress.received = page.through;
                target
                    .sync_ack(&repo, &peer, &progress)
                    .map_err(|e| e.to_string())?;
                if !page.more {
                    break;
                }
            }
        }
        self.aliases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(root.to_path_buf(), repo);
        Ok(())
    }

    /// The store of the project in `project`, opened the first time it's
    /// asked for. None when its file can't be opened (permissions, a full
    /// disk, a damaged file): logged, and the run goes on without memory
    /// rather than stopping for it.
    pub fn open(&self, paths: &Paths, project: &Path) -> Option<SharedMemoryStore> {
        let path = store_path(paths, Path::new(&self.repo(project)));
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

/// A memory as Settings' manager lists it: what it says, how much the project's
/// chats can trust it, and whether that still applies.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    /// The store's id for it, as the chat's panel shows it.
    pub id: String,
    /// Stable position for the next manager page; opaque to the frontend.
    pub cursor: String,
    /// `episode`, `project fact` or `procedure`.
    pub kind: String,
    /// One line: what it's about.
    pub title: String,
    /// What it says.
    pub content: String,
    /// The project files it's about.
    pub files: Vec<String>,
    /// The tags retrieval matches it by.
    pub tags: Vec<String>,
    /// `provisional`, `verified` or `deprecated`: what evidence supports it.
    pub verification: String,
    /// `current`, `needs_revalidation` or `unknown`: whether that holds now.
    pub applicability: String,
    /// What kind of source asserted it: the user, an observation, a test, a
    /// deduction, or unknown.
    pub source_kind: String,
    /// What promoted or invalidated it; absent: nothing did.
    pub evidence: Option<String>,
    /// When the claim was last confirmed by evidence, in milliseconds since
    /// the Unix epoch; absent: never.
    pub last_validated_at: Option<i64>,
    /// When its title or text last changed, in milliseconds since the Unix
    /// epoch.
    pub updated_at: i64,
}

impl From<&MemoryRecord> for MemoryEntry {
    fn from(record: &MemoryRecord) -> Self {
        MemoryEntry {
            id: record.id.to_string(),
            cursor: record.page_cursor(),
            kind: record.kind.to_string(),
            title: record.title.clone(),
            content: record.content.clone(),
            files: record.files.clone(),
            tags: record.tags.clone(),
            verification: record.verification.to_string(),
            applicability: record.applicability.to_string(),
            source_kind: record.source_kind.to_string(),
            evidence: record.evidence.clone(),
            last_validated_at: record.last_validated_at.map(|at| at.timestamp_millis()),
            updated_at: record.content_updated_at.timestamp_millis(),
        }
    }
}

/// The project's memories, newest first: those whose title, text, tags or
/// files mention `query`, or every one of them when it's empty. `repo` is the
/// project's folder, as a run names it.
///
/// # Errors
/// Returns an error when the store can't be read, or `repo` is empty.
pub fn list(
    store: &SharedMemoryStore,
    repo: &str,
    query: &str,
) -> Result<Vec<MemoryEntry>, String> {
    store
        .list(repo, query)
        .map(|records| records.iter().map(MemoryEntry::from).collect())
        .map_err(|e| e.to_string())
}

/// Replaces a memory's title and text: what its user can correct.
///
/// The correction is the user's own assertion, and it carries no evidence: the
/// memory keeps the evidence it had, for the record, but that evidence spoke
/// about the text it replaced, so the memory is marked to be checked again and
/// is no longer one a run may rely on. Its tags, its files and its kind stay as
/// the agent left them.
///
/// # Errors
/// Returns an error when `id` isn't a memory's, the title or text is blank,
/// or the store can't be written.
pub fn edit(store: &SharedMemoryStore, id: &str, title: &str, content: &str) -> Result<(), String> {
    store
        .edit(memory_id(id)?, title, content)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Removes one memory for good.
///
/// # Errors
/// Returns an error when `id` isn't a memory's, or the store can't be
/// written.
pub fn remove(store: &SharedMemoryStore, id: &str) -> Result<(), String> {
    store.delete(memory_id(id)?).map_err(|e| e.to_string())
}

/// Removes every memory of the project in `repo`, and says how many went.
///
/// # Errors
/// Returns an error when the store can't be written, or `repo` is empty.
pub fn clear(store: &SharedMemoryStore, repo: &str) -> Result<u32, String> {
    let removed = store.clear(repo).map_err(|e| e.to_string())?;
    u32::try_from(removed).map_err(|_| format!("{removed} memories is more than this can count."))
}

/// The memory `text` names: the ids the chat's panel and the manager carry
/// are the store's, kept as text.
fn memory_id(text: &str) -> Result<Uuid, String> {
    Uuid::parse_str(text).map_err(|_| format!("{text:?} is not a memory's id."))
}

#[cfg(test)]
mod synchronization_tests {
    use super::*;
    use verified_code_memory::NewMemory;

    #[test]
    fn cloud_worktrees_share_one_store_and_legacy_history_migrates_once() {
        let core = crate::testing::TestCore::new();
        let one = core.dir.0.join("one");
        let two = core.dir.0.join("two");
        let first = core.core.memory.open(&core.core.paths, &one).unwrap();
        let old = first
            .remember(NewMemory::project_fact(
                one.to_string_lossy(),
                "Old fact",
                "Preserve history",
            ))
            .unwrap();
        core.core
            .memory
            .bind_project(&core.core.paths, &one, "project-123")
            .unwrap();
        core.core
            .memory
            .bind_project(&core.core.paths, &two, "project-123")
            .unwrap();
        let shared = core.core.memory.open(&core.core.paths, &two).unwrap();
        let repo = MemoryState::project_repo("project-123");
        assert_eq!(shared.get(old.id).unwrap().repo, repo);
        assert_eq!(core.core.memory.repo(&one), core.core.memory.repo(&two));
        shared.clear(&repo).unwrap();
        let restarted = MemoryState::default();
        restarted
            .bind_project(&core.core.paths, &one, "project-123")
            .unwrap();
        assert!(restarted
            .open(&core.core.paths, &one)
            .unwrap()
            .list(&repo, "")
            .unwrap()
            .is_empty());
        assert!(
            first.get(old.id).is_ok(),
            "the migration does not delete original history"
        );
        restarted
            .bind_project(&core.core.paths, &two, "project-other")
            .unwrap();
        assert_ne!(restarted.repo(&one), restarted.repo(&two));
    }

    #[test]
    fn restricted_claims_stay_off_the_wire_without_suppressing_deletions() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let secret = store
            .remember(
                NewMemory::project_fact("repo", "Hidden", "Private source")
                    .with_files(["private.rs"]),
            )
            .unwrap();
        let visible = store
            .remember(
                NewMemory::project_fact("repo", "Visible", "Public source")
                    .with_files(["public.rs"]),
            )
            .unwrap();
        let settings = serde_json::json!({"restrictedFiles":[{"path":"private.rs","blockRead":true,"blockWrite":true}]});
        let mut page = store.sync_export("repo", 0).unwrap();
        filter_sync_page(&mut page, Path::new("/project"), &settings);
        assert!(page.changes.iter().any(|c| c.id == visible.id));
        assert!(!page.changes.iter().any(|c| c.id == secret.id));
        store.delete(secret.id).unwrap();
        let mut page = store.sync_export("repo", 0).unwrap();
        filter_sync_page(&mut page, Path::new("/project"), &settings);
        assert!(page.changes.iter().any(|c| c.id == secret.id && c.deleted));
    }
}

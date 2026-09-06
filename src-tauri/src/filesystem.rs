use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{self, Event, EventKind, RecursiveMode, Result as NotifyResult, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Emitter, Manager, State};

/// Stable identity for a directory entry, robust to case-only path
/// differences (case-insensitive filesystems). Prefers the OS's real file
/// identity; falls back to a lowercased path when that's unavailable
/// (broken symlinks, filesystems without file-index support, etc).
fn file_identity(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let meta = fs::symlink_metadata(path).ok()?;
        return Some(format!("u:{}:{}", meta.dev(), meta.ino()));
    }
    #[cfg(windows)]
    {
        let meta = fs::metadata(path).ok()?;
        let index = meta.file_index()?;
        let volume = meta.volume_serial_number().unwrap_or(0);
        return Some(format!("w:{}:{}", volume, index));
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn stable_file_id(path: &Path) -> String {
    file_identity(path).unwrap_or_else(|| format!("p:{}", path.to_string_lossy().to_lowercase()))
}

static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

// Clean, unified cross-platform operation types
#[derive(Clone, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")] // Turns variants into "create", "modify", "remove", "unknown"
enum WatchOp {
    Create,
    Modify,
    Remove,
    Unknown,
}

#[derive(Clone, Serialize)]
pub struct FileWatchPayload {
    path: String,
    operation: WatchOp,
    is_dir: bool, // Helps frontend know if it's a file or folder immediately
}

// Manual Hash/Eq so it can be used as HashMap key
impl Hash for FileWatchPayload {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
        self.operation.hash(state);
        self.is_dir.hash(state);
    }
}

impl PartialEq for FileWatchPayload {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.operation == other.operation && self.is_dir == other.is_dir
    }
}

impl Eq for FileWatchPayload {}

pub struct WatcherDebouncer {
    pub debounce_duration: Duration,
    pub pending: Mutex<HashMap<FileWatchPayload, Instant>>,
}

impl WatcherDebouncer {
    pub fn new(debounce_ms: u64) -> Self {
        Self {
            debounce_duration: Duration::from_millis(debounce_ms),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn should_emit(&self, payload: FileWatchPayload) -> bool {
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap();

        if let Some(last_seen) = pending.get_mut(&payload) {
            if now.duration_since(*last_seen) < self.debounce_duration {
                *last_seen = now;
                return false;
            } else {
                pending.remove(&payload);
                return true;
            }
        } else {
            pending.insert(payload, now);
            return false;
        }
    }
}

// Safe wrapper to store the active channel tx or the watcher itself
pub struct WatcherState {
    // Storing a generic box or token. If we drop this, notify closes the thread.
    // The easiest trick with notify is: dropping the RecommendedWatcher stops the watch loop.
    pub active_watcher: Mutex<Option<notify::RecommendedWatcher>>,
    pub debouncer: Arc<WatcherDebouncer>,
    pub gitignore: Mutex<Option<(PathBuf, Arc<Gitignore>)>>,
}

fn build_gitignore(root: &Path) -> Result<Gitignore, String> {
    let mut gitignore_builder = GitignoreBuilder::new(root);

    let gitignore_path = root.join(".gitignore");
    if gitignore_path.exists() {
        if let Some(err) = gitignore_builder.add(gitignore_path.clone()) {
            eprintln!("Warning loading gitignore: {:?}", err);
        }
    }
    if let Err(err) = gitignore_builder.add_line(Some(gitignore_path), ".git/") {
        eprintln!("Warning adding .git/ to ignore: {:?}", err);
    }

    gitignore_builder
        .build()
        .map_err(|e| format!("Failed to compile gitignore: {}", e))
}

/// Returns the matcher for `root`, building (and caching) it if this is the
/// first time we've seen this root, or the root has changed since last time.
fn get_or_build_gitignore(state: &WatcherState, root: &Path) -> Result<Arc<Gitignore>, String> {
    let mut cached = state.gitignore.lock().unwrap();
    if let Some((cached_root, matcher)) = cached.as_ref() {
        if cached_root == root {
            return Ok(matcher.clone());
        }
    }
    let matcher = Arc::new(build_gitignore(root)?);
    *cached = Some((root.to_path_buf(), matcher.clone()));
    Ok(matcher)
}

// A Tauri command to start watching a specific directory
#[tauri::command]
pub fn start_file_watch(
    app_handle: AppHandle,
    state: State<'_, WatcherState>, // Access our global state
    path_str: String,
) -> std::result::Result<(), String> {
    let root = Path::new(&path_str).to_path_buf();

    let gitignore_matcher = get_or_build_gitignore(&state, &root)?;

    // Lock and clean up any existing watcher first
    let mut current_watcher = state.active_watcher.lock().unwrap();
    *current_watcher = None; // Drops the old watcher, killing its channel thread automatically

    // Set up the new cross-thread channel
    let (tx, rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();

    let mut watcher = notify::recommended_watcher(tx).map_err(|e| e.to_string())?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    println!("Watching: {:?}", root);

    let app_handle = app_handle.clone();
    let debouncer = state.debouncer.clone();
    let gitignore_matcher = gitignore_matcher.clone();

    // Spawn the watcher in a background thread so it doesn't block the UI
    std::thread::spawn(move || {
        // Loop indefinitely listening for file system events
        for res in rx {
            match res {
                Ok(event) => {
                    // 1. Normalize cross-platform event kinds into our strict WatchOp enum
                    let operation = match event.kind {
                        EventKind::Create(_) => WatchOp::Create,
                        EventKind::Modify(_) => WatchOp::Modify,
                        EventKind::Remove(_) => WatchOp::Remove,
                        _ => WatchOp::Unknown,
                    };

                    // 💡 SKIP EMITTING IF WATCHOP IS UNKNOWN
                    if let WatchOp::Unknown = operation {
                        continue;
                    }

                    // Some events might contain multiple paths at once
                    for path in event.paths {
                        // Safely check if the target path is a directory
                        let is_dir = path.is_dir();
                        // 💡 CHECK IF PATH IS COVERED BY GITIGNORE RULES
                        // returns if the path is explicitly ignored
                        let is_ignored = gitignore_matcher
                            .matched_path_or_any_parents(&path, is_dir)
                            .is_ignore();

                        if is_ignored {
                            continue; // Skip emitting this event entirely!
                        }

                        let payload = FileWatchPayload {
                            path: path.to_string_lossy().into_owned(),
                            operation: operation.clone(),
                            is_dir,
                        };
                        if !debouncer.should_emit(payload.clone()) {
                            continue;
                        }
                        let _ = app_handle.emit("fs-changes", payload);
                    }
                }
                Err(e) => eprintln!("Watch error: {:?}", e),
            }
        }
    });

    // 4. Save the active watcher handle to state so it stays alive until the next switch
    *current_watcher = Some(watcher);

    Ok(())
}

const BINARY_SNIFF_BYTES: usize = 8000;

#[derive(Serialize)]
pub struct DirEntryInfo {
    id: String,
    name: String,
    path: String,
    is_dir: bool,
    ignored: bool,
}

/// Lists the immediate children of `path` — non-recursive by design. The
/// frontend calls this again for each folder the user expands, so cost
/// scales with what's actually opened, not with total project size.
#[tauri::command]
pub fn read_dir_entries(
    state: State<'_, WatcherState>,
    root_path: String,
    path: String,
) -> Result<Vec<DirEntryInfo>, String> {
    let n = CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    eprintln!("[debug] read_dir_entries call #{n} for {path}");

    let root = Path::new(&root_path);
    let matcher = get_or_build_gitignore(&state, root)?;

    let dir = Path::new(&path);
    let read = fs::read_dir(dir).map_err(|e| format!("{path}: {e}"))?;

    let mut entries: Vec<DirEntryInfo> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();

        if name == ".git" {
            continue;
        }

        let entry_path = entry.path();
        let ignored = matcher
            .matched_path_or_any_parents(&entry_path, file_type.is_dir())
            .is_ignore();

        entries.push(DirEntryInfo {
            id: stable_file_id(&entry_path),
            name,
            path: entry_path.to_string_lossy().to_string(),
            is_dir: file_type.is_dir(),
            ignored,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

fn looks_binary(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; BINARY_SNIFF_BYTES];
    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf[..n].contains(&0))
}

#[tauri::command]
pub fn read_file_content(path: String) -> Result<String, String> {
    let p = Path::new(&path);
    if looks_binary(p)? {
        return Err(format!("{path}: binary file, cannot open in editor"));
    }
    fs::read_to_string(p).map_err(|e| format!("{path}: {e}"))
}

#[tauri::command]
pub fn write_file_content(path: String, content: String) -> Result<(), String> {
    fs::write(&path, content).map_err(|e| format!("{path}: {e}"))
}

// --- AI chat: settings persistence + the actual API call ------------------

fn ai_settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("ai-settings.json"))
}

#[tauri::command]
pub fn load_ai_settings(app: AppHandle) -> Result<String, String> {
    let path = ai_settings_path(&app)?;
    if !path.exists() {
        return Ok(r#"{"profiles":[],"activeProfileId":null}"#.to_string());
    }
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_ai_settings(app: AppHandle, json: String) -> Result<(), String> {
    let path = ai_settings_path(&app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&path, json).map_err(|e| e.to_string())
}

fn chat_history_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data directory: {e}"))?;
    Ok(dir.join("chat-history.json"))
}

#[tauri::command]
pub fn load_chat_history(app: AppHandle) -> Result<String, String> {
    let path = chat_history_path(&app)?;
    if !path.exists() {
        return Ok(r#"{"dialogs":[]}"#.to_string());
    }
    fs::read_to_string(&path).map_err(|e| format!("Failed to read chat history: {e}"))
}

#[tauri::command]
pub fn save_chat_history(app: AppHandle, json: String) -> Result<(), String> {
    let path = chat_history_path(&app)?;
    fs::write(&path, json).map_err(|e| format!("Failed to save chat history: {e}"))
}

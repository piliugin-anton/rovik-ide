use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{self, Event, EventKind, RecursiveMode, Result as NotifyResult, Watcher};
use serde::{Deserialize, Serialize};
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

// Safe wrapper to store the active channel tx or the watcher itself
pub struct WatcherState {
    // Storing a generic box or token. If we drop this, notify closes the thread.
    // The easiest trick with notify is: dropping the RecommendedWatcher stops the watch loop.
    pub active_watcher: Mutex<Option<notify::RecommendedWatcher>>,
    pub gitignore: Mutex<Option<(PathBuf, Arc<Gitignore>)>>,
    // Maps a stable_file_id() back to the last path we saw it at. Populated
    // whenever we hand an id to the frontend (read_dir_entries, get_path_id)
    // and updated on rename/move, so mutating commands can always resolve an
    // id to a real path instead of trusting a path string the frontend may
    // be holding onto with stale or differently-cased casing.
    pub id_to_path: Mutex<HashMap<String, PathBuf>>,
}

fn resolve_id(state: &WatcherState, id: &str) -> Result<PathBuf, String> {
    state
        .id_to_path
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("{id}: unknown id (try refreshing the folder view)"))
}

fn remember_id(state: &WatcherState, id: &str, path: &Path) {
    state
        .id_to_path
        .lock()
        .unwrap()
        .insert(id.to_string(), path.to_path_buf());
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

    // Use Tauri's built-in async runtime to handle async sleeping & debounce intervals smoothly
    tauri::async_runtime::spawn(async move {
        // We'll collect events over a short window and evaluate them on the trailing edge
        let mut event_bucket: HashMap<FileWatchPayload, Instant> = HashMap::new();
        let debounce_duration = Duration::from_millis(300); // 300ms should be enough for file-system quiet down

        loop {
            // Step 1: Drain any available events from the native channel without blocking indefinitely
            while let Ok(res) = rx.try_recv() {
                if let Ok(event) = res {
                    let operation = match event.kind {
                        EventKind::Create(_) => WatchOp::Create,
                        EventKind::Modify(_) => WatchOp::Modify,
                        EventKind::Remove(_) => WatchOp::Remove,
                        _ => WatchOp::Unknown,
                    };

                    if let WatchOp::Unknown = operation {
                        continue;
                    }

                    for path in event.paths {
                        let is_dir = path.is_dir();
                        let payload = FileWatchPayload {
                            path: path.to_string_lossy().into_owned(),
                            operation: operation.clone(),
                            is_dir,
                        };

                        // Overwrite or update the timestamp. Every new activity resets the trailing timer.
                        event_bucket.insert(payload, Instant::now());
                    }
                }
            }

            // Step 2: Check if any debounced events are ready to emit
            let now = Instant::now();
            let mut ready_to_emit = Vec::new();

            // Retain only items that haven't expired yet; extract the ones that have
            event_bucket.retain(|payload, last_seen| {
                if now.duration_since(*last_seen) >= debounce_duration {
                    ready_to_emit.push(payload.clone());
                    false // Removes it from the bucket, preventing memory leaks!
                } else {
                    true // Keep waiting, it's still bouncing
                }
            });

            // Step 3: Emit the stabilized events to the frontend
            for payload in ready_to_emit {
                let _ = app_handle.emit("fs-changes", payload);
            }

            // Step 4: Short pause to rest the CPU before polling for new events/timeouts again
            tokio::time::sleep(Duration::from_millis(50)).await;
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

    // Remember the directory's own id too, so "new file/folder in this
    // directory" can resolve it even before any of its children are cached.
    remember_id(&state, &stable_file_id(dir), dir);

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

        let id = stable_file_id(&entry_path);
        remember_id(&state, &id, &entry_path);

        entries.push(DirEntryInfo {
            id,
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

// --- File tree mutations: create / delete / rename / copy ------------------
//
// All of these take a stable id (from DirEntryInfo.id / get_path_id), never
// a raw path, for anything that identifies an *existing* entry. The id is
// resolved to a real path via `resolve_id` right before touching disk, so a
// path string the frontend cached earlier (which may differ only in case on
// case-insensitive filesystems) can never be the thing that actually gets
// deleted, renamed, or copied.

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// Resolves (and caches) the id for an arbitrary path the frontend already
/// knows about directly — e.g. the project root, which never comes through
/// `read_dir_entries` itself.
#[tauri::command]
pub fn get_path_id(state: State<'_, WatcherState>, path: String) -> Result<String, String> {
    let p = Path::new(&path);
    let id = stable_file_id(p);
    remember_id(&state, &id, p);
    Ok(id)
}

#[tauri::command]
pub fn create_file(
    state: State<'_, WatcherState>,
    dir_id: String,
    name: String,
) -> Result<(), String> {
    let dir = resolve_id(&state, &dir_id)?;
    let p = dir.join(&name);
    if p.exists() {
        return Err(format!("{name}: already exists"));
    }
    fs::File::create(&p).map_err(|e| format!("{name}: {e}"))?;
    remember_id(&state, &stable_file_id(&p), &p);
    Ok(())
}

#[tauri::command]
pub fn create_dir(
    state: State<'_, WatcherState>,
    dir_id: String,
    name: String,
) -> Result<(), String> {
    let dir = resolve_id(&state, &dir_id)?;
    let p = dir.join(&name);
    if p.exists() {
        return Err(format!("{name}: already exists"));
    }
    fs::create_dir_all(&p).map_err(|e| format!("{name}: {e}"))?;
    remember_id(&state, &stable_file_id(&p), &p);
    Ok(())
}

#[tauri::command]
pub fn delete_path(state: State<'_, WatcherState>, id: String) -> Result<(), String> {
    let p = resolve_id(&state, &id)?;
    let meta = fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let result = if meta.is_dir() {
        fs::remove_dir_all(&p)
    } else {
        fs::remove_file(&p)
    };
    result.map_err(|e| format!("{}: {e}", p.display()))?;
    state.id_to_path.lock().unwrap().remove(&id);
    Ok(())
}

/// Renames an entry in place (same parent directory, new file/folder name).
/// The id is unchanged by the rename — same inode/file-index — so we just
/// update where it points. Returns the new absolute path so the frontend can
/// update any open tab.
#[tauri::command]
pub fn rename_path(
    state: State<'_, WatcherState>,
    id: String,
    new_name: String,
) -> Result<String, String> {
    let from = resolve_id(&state, &id)?;
    let parent = from
        .parent()
        .ok_or_else(|| "cannot rename the root folder".to_string())?;
    let to = parent.join(&new_name);
    if to.exists() {
        return Err(format!("{new_name}: already exists"));
    }
    fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
    remember_id(&state, &id, &to);
    Ok(to.to_string_lossy().to_string())
}

/// Moves an entry into a different directory (optionally renaming it),
/// i.e. what a cut-then-paste does. `dest_dir_id` must be a directory's id.
/// Returns the new absolute path so the frontend can update any open tab.
#[tauri::command]
pub fn move_path(
    state: State<'_, WatcherState>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<String, String> {
    let from = resolve_id(&state, &id)?;
    let dest_dir = resolve_id(&state, &dest_dir_id)?;
    let name = new_name.unwrap_or_else(|| {
        from.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let to = dest_dir.join(&name);
    if to.exists() {
        return Err(format!("{name}: already exists"));
    }
    fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
    remember_id(&state, &id, &to);
    Ok(to.to_string_lossy().to_string())
}

/// Generates a unique path by appending " - copy" or " - copy N" if the
/// target name already exists. Matches typical desktop OS behavior.
/// Generates a unique path by appending "copy" or "copyN" if the
/// target name already exists. Matches typical desktop OS behavior.
fn generate_unique_path(dest_dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let dest = dest_dir.join(name);

    // If path doesn't exist, use it as-is
    if !dest.exists() {
        return dest;
    }

    // Split into base and extension for files
    let (base, ext) = if is_dir {
        (name.to_string(), String::new())
    } else {
        let path = Path::new(name);
        let base = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        (base, ext)
    };

    // Check if the base already ends with "copy" or "copyN"
    // If so, extract the original base and starting counter
    let (original_base, start_counter) = if let Some(idx) = base.rfind("copy") {
        let before_copy = &base[..idx];
        let after_copy = &base[idx + 4..]; // skip "copy"

        // Check if there's a number after "copy"
        if !after_copy.is_empty() {
            // "copyN" format - try to parse the number
            if let Ok(num) = after_copy.parse::<u32>() {
                (before_copy.to_string(), num)
            } else {
                // "copy" followed by non-number, treat as copy1
                (base.clone(), 1)
            }
        } else {
            // Just "copy" with nothing after
            (before_copy.to_string(), 1)
        }
    } else {
        // No "copy" suffix, start from 1
        (base.clone(), 1)
    };

    // Try incrementing numbers until we find a free name
    let mut counter = start_counter;
    loop {
        let new_name = format!("{}copy{}{}", original_base, counter, ext);
        let new_path = dest_dir.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
        counter += 1;

        // Safety: prevent infinite loops in pathological cases
        if counter > 1000 {
            return new_path; // Let the OS error if it truly can't create
        }
    }
}

/// Copies an entry into a different directory (optionally renaming it), i.e.
/// what a copy-then-paste does. `dest_dir_id` must be a directory's id.
/// If a file/folder with the target name already exists, automatically
/// generates a unique name by appending " - copy" or " - copy N".
#[tauri::command]
pub fn copy_path(
    state: State<'_, WatcherState>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<String, String> {
    let src = resolve_id(&state, &id)?;
    let dest_dir = resolve_id(&state, &dest_dir_id)?;

    let base_name = new_name.unwrap_or_else(|| {
        src.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // Generate a unique destination path if collision exists
    let dest = generate_unique_path(&dest_dir, &base_name, src.is_dir());

    let meta = fs::symlink_metadata(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        copy_dir_recursive(&src, &dest).map_err(|e| format!("{}: {e}", src.display()))?;
    } else {
        fs::copy(&src, &dest).map_err(|e| format!("{}: {e}", src.display()))?;
    }

    remember_id(&state, &stable_file_id(&dest), &dest);

    // Return the actual path used so frontend can update open tabs if needed
    Ok(dest.to_string_lossy().to_string())
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

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastProjectData {
    path: Option<String>,
    opened_files: Option<Vec<OpenFile>>,
    expanded_paths: Option<Vec<String>>,
}

fn last_project_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data directory: {e}"))?;
    Ok(dir.join("last-project.json"))
}

#[tauri::command]
pub fn load_last_project(app: AppHandle) -> Result<String, String> {
    let path = last_project_path(&app)?;
    if !path.exists() {
        return Ok(r#"{"path":null,"opened_files":null,"expanded_paths":null}"#.to_string());
    }
    fs::read_to_string(&path).map_err(|e| format!("Failed to read last project: {e}"))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenFile {
    pub id: String,
    pub path: String,
    pub name: String,
    pub language: String,
}

#[tauri::command]
pub fn save_last_project(
    app: AppHandle,
    path: String,
    opened_files: Option<Vec<OpenFile>>,
    expanded_paths: Option<Vec<String>>,
) -> Result<(), String> {
    let project_path = last_project_path(&app)?;
    let data = LastProjectData {
        path: Some(path),
        opened_files,
        expanded_paths,
    };
    let json = serde_json::to_string(&data)
        .map_err(|e| format!("Failed to serialize last project: {e}"))?;
    fs::write(&project_path, json).map_err(|e| format!("Failed to save last project: {e}"))
}

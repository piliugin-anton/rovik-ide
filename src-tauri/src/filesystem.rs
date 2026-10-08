use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{self, Event, EventKind, RecursiveMode, Result as NotifyResult, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
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
        // std's `MetadataExt::file_index` is still unstable, so ask Windows
        // directly. Backup semantics lets the handle be a directory's.
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        };
        let file = fs::OpenOptions::new()
            .access_mode(0) // metadata only: works on files others hold open
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(path)
            .ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        return Some(format!("w:{}:{}", info.dwVolumeSerialNumber, index));
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
#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")] // Turns variants into "create", "modify", "remove", "unknown"
enum WatchOp {
    Create,
    Modify,
    Remove,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
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

/// What a changed path is, for the watcher.
#[derive(Debug, PartialEq)]
enum WatchPath {
    /// Inside `.git/`: git's own bookkeeping, reported as one "git-changed"
    /// (visible changes are followed by one too: they change the status).
    Git,
    /// Matched by a .gitignore (build output, dependencies): not reported.
    Ignored,
    Visible,
}

/// The project's .gitignore files for the watcher, each matching under its
/// own folder: the root's and every package's. In a workspace, a package's
/// build output is often ignored only by `packages/x/.gitignore`; with the
/// root's alone, every rebuild of a dev server reached the app as the
/// user's changes. (A pattern in one file doesn't undo another's here.)
struct WatchIgnores {
    matchers: Vec<(PathBuf, Gitignore)>,
}

impl WatchIgnores {
    /// The root's .gitignore (and `.git/`): at once, while `all` looks for the rest.
    fn root_only(root: &Path) -> Self {
        WatchIgnores { matchers: build_gitignore(root).ok().map(|m| (root.to_path_buf(), m)).into_iter().collect() }
    }

    /// Every .gitignore in the project, found without entering what's ignored.
    fn all(root: &Path) -> Self {
        let mut ignores = Self::root_only(root);
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .require_git(false)
            .parents(false)
            .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "node_modules")
            .build();
        for entry in walker.flatten() {
            let (path, depth) = (entry.path(), entry.depth());
            if depth == 1 || entry.file_name() != ".gitignore" || !entry.file_type().is_some_and(|t| t.is_file()) {
                continue; // the root's is in already
            }
            let Some(dir) = path.parent() else { continue };
            let mut builder = GitignoreBuilder::new(dir);
            if builder.add(path).is_none() {
                if let Ok(matcher) = builder.build() {
                    ignores.matchers.push((dir.to_path_buf(), matcher));
                }
            }
        }
        ignores
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.matchers
            .iter()
            .any(|(dir, m)| path.starts_with(dir) && m.matched_path_or_any_parents(path, is_dir).is_ignore())
    }
}

fn watch_path_kind(root: &Path, ignores: &WatchIgnores, path: &Path) -> WatchPath {
    let Ok(rel) = path.strip_prefix(root) else { return WatchPath::Visible };
    if rel.components().any(|c| c.as_os_str() == ".git") {
        return WatchPath::Git;
    }
    // Dependencies: always ignored, and checked without asking the disk
    // whether it's a folder (installs and caches churn there).
    if rel.components().any(|c| c.as_os_str() == "node_modules") {
        return WatchPath::Ignored;
    }
    if ignores.is_ignored(path, path.is_dir()) {
        return WatchPath::Ignored;
    }
    WatchPath::Visible
}

/// More changes than this in one burst (a branch switch, an unpacked
/// archive, a dev server's rebuild) are reported as a single "re-check
/// everything" event: the frontend handles each event, and thousands at
/// once froze it.
const MAX_EVENTS_PER_FLUSH: usize = 300;
/// Raw events handled per pass, so a flood can't keep the loop from ever
/// reporting anything.
const MAX_RAW_EVENTS_PER_PASS: usize = 5_000;
/// Changes are reported once things are quiet for this long...
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// ...or at the latest this long after the first, while they keep coming.
const WATCH_MAX_WAIT: Duration = Duration::from_secs(2);

/// The watcher's changes waiting to be reported: each path once, when the
/// burst is quiet for WATCH_DEBOUNCE or WATCH_MAX_WAIT after it began; past
/// MAX_EVENTS_PER_FLUSH, one "re-check everything" instead.
#[derive(Default)]
struct WatchBatch {
    events: HashMap<FileWatchPayload, Instant>,
    started: Option<Instant>,
    last: Option<Instant>,
    overflowed: bool,
}

#[derive(Debug, PartialEq)]
enum WatchFlush {
    Nothing,
    Events(Vec<FileWatchPayload>),
    Everything,
}

impl WatchBatch {
    fn add(&mut self, payload: FileWatchPayload, now: Instant) {
        self.started.get_or_insert(now);
        self.last = Some(now);
        if self.overflowed {
            return;
        }
        self.events.insert(payload, now);
        if self.events.len() > MAX_EVENTS_PER_FLUSH {
            // Too many to report one by one: stop collecting them.
            self.events.clear();
            self.overflowed = true;
        }
    }

    fn take_due(&mut self, now: Instant) -> WatchFlush {
        let (Some(started), Some(last)) = (self.started, self.last) else { return WatchFlush::Nothing };
        let overdue = now.duration_since(started) >= WATCH_MAX_WAIT;
        if self.overflowed {
            if !overdue && now.duration_since(last) < WATCH_DEBOUNCE {
                return WatchFlush::Nothing;
            }
            *self = WatchBatch::default();
            return WatchFlush::Everything;
        }
        let mut ready = Vec::new();
        self.events.retain(|payload, seen| {
            let due = overdue || now.duration_since(*seen) >= WATCH_DEBOUNCE;
            if due {
                ready.push(payload.clone());
            }
            !due
        });
        if self.events.is_empty() {
            *self = WatchBatch::default();
        }
        if ready.is_empty() { WatchFlush::Nothing } else { WatchFlush::Events(ready) }
    }
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
    let watch_root = root.clone();

    // Use Tauri's built-in async runtime to handle async sleeping & debounce intervals smoothly
    tauri::async_runtime::spawn(async move {
        // The project's .gitignore files: the root's at once, all of them
        // when found (and again whenever one changes).
        let mut ignores = Arc::new(WatchIgnores::root_only(&watch_root));
        let (ignores_tx, ignores_rx) = std::sync::mpsc::channel::<WatchIgnores>();
        let find_ignores = |root: PathBuf, tx: std::sync::mpsc::Sender<WatchIgnores>| {
            tauri::async_runtime::spawn_blocking(move || {
                let _ = tx.send(WatchIgnores::all(&root));
            });
        };
        find_ignores(watch_root.clone(), ignores_tx.clone());

        let mut batch = WatchBatch::default();
        // Last activity inside .git/ (a commit writes hundreds of files there).
        let mut git_touched: Option<Instant> = None;

        loop {
            if let Ok(found) = ignores_rx.try_recv() {
                ignores = Arc::new(found);
            }
            // Step 1: Drain the native channel, a bounded number of events per pass.
            let mut handled = 0;
            while handled < MAX_RAW_EVENTS_PER_PASS {
                let Ok(res) = rx.try_recv() else { break };
                handled += 1;
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
                        // Git's own files and ignored paths (target/, node_modules/)
                        // aren't the user's files: reporting each one flooded the app.
                        match watch_path_kind(&watch_root, &ignores, &path) {
                            WatchPath::Git => {
                                git_touched = Some(Instant::now());
                                continue;
                            }
                            WatchPath::Ignored => continue,
                            WatchPath::Visible => {}
                        }
                        if path.file_name().is_some_and(|n| n == ".gitignore") {
                            find_ignores(watch_root.clone(), ignores_tx.clone());
                            if let Ok(mut cached) = app_handle.state::<WatcherState>().gitignore.lock() {
                                *cached = None; // the tree's listing re-reads it too
                            }
                        }
                        let is_dir = path.is_dir();
                        let payload = FileWatchPayload {
                            path: path.to_string_lossy().into_owned(),
                            operation: operation.clone(),
                            is_dir,
                        };
                        batch.add(payload, Instant::now());
                    }
                }
            }

            // Step 2: what's due, reported. A flood is one "re-check
            // everything" (operation "unknown", the project root).
            let now = Instant::now();
            let flush = batch.take_due(now);
            // A visible file changed: git's status of the working tree did too.
            let tree_changed = flush != WatchFlush::Nothing;
            match flush {
                WatchFlush::Nothing => {}
                WatchFlush::Everything => {
                    let _ = app_handle.emit(
                        "fs-changes",
                        FileWatchPayload {
                            path: watch_root.to_string_lossy().into_owned(),
                            operation: WatchOp::Unknown,
                            is_dir: true,
                        },
                    );
                }
                WatchFlush::Events(events) => {
                    for payload in events {
                        let _ = app_handle.emit("fs-changes", payload);
                    }
                }
            }
            // Git activity once it has settled, or a change to the working tree:
            // the Git tab refreshes.
            let git_settled = git_touched.is_some_and(|t| now.duration_since(t) >= WATCH_DEBOUNCE);
            if git_settled {
                git_touched = None;
            }
            if git_settled || tree_changed {
                let _ = app_handle.emit("git-changed", ());
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

/// Writes a file, creating any missing parent directories first — saving a
/// tab whose file (or whole folder) was deleted puts it back. Plain
/// `fs::write` rather than an atomic rename on purpose: it keeps the inode,
/// and with it the file's stable id.
#[tauri::command]
pub fn write_file_content(path: String, content: String) -> Result<(), String> {
    let p = Path::new(&path);
    if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(p, content).map_err(|e| format!("{path}: {e}"))
}

/// How many entries a folder has directly in it (hidden ones and `.git`
/// included, unlike the tree's listing), counting up to `limit`; None when
/// the path isn't a folder. For the delete confirmation: an empty folder is
/// deleted without asking, and a folder whose only entry is `.git` must not
/// look empty.
#[tauri::command]
pub fn dir_entry_count(path: String, limit: Option<usize>) -> Result<Option<usize>, String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Ok(None);
    }
    let limit = limit.unwrap_or(10_000);
    let entries = fs::read_dir(p).map_err(|e| format!("{path}: {e}"))?;
    Ok(Some(entries.take(limit).count()))
}

/// Whether a path currently exists. Used to tell an open tab's file was
/// deleted: watcher events can arrive out of order (a remove and a re-create
/// in the same debounce window), so the frontend checks the disk instead of
/// trusting the event kind.
#[tauri::command]
pub fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
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
/// Creates an empty file; returns its path (the tree opens it).
pub fn create_file(
    state: State<'_, WatcherState>,
    dir_id: String,
    name: String,
) -> Result<String, String> {
    let dir = resolve_id(&state, &dir_id)?;
    let p = dir.join(&name);
    if p.exists() {
        return Err(format!("{name}: already exists"));
    }
    fs::File::create(&p).map_err(|e| format!("{name}: {e}"))?;
    remember_id(&state, &stable_file_id(&p), &p);
    Ok(p.to_string_lossy().into_owned())
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

/// Starts the error of a delete that couldn't use the trash, so the frontend
/// can offer to delete permanently instead (lib/commands.ts).
const TRASH_UNAVAILABLE: &str = "trash-unavailable: ";

/// Deletes a file or folder by moving it to the system trash (Windows'
/// Recycle Bin, the macOS Trash, the freedesktop.org trash on Linux), so it
/// can be restored from there. Where there's no trash (some network drives,
/// or a Linux drive without a trash folder) this fails rather than delete
/// for good; `permanent` then deletes outright, once the user has agreed.
#[tauri::command]
pub fn delete_path(state: State<'_, WatcherState>, id: String, permanent: Option<bool>) -> Result<(), String> {
    let p = resolve_id(&state, &id)?;
    let meta = fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    if permanent.unwrap_or(false) {
        let result = if meta.is_dir() {
            fs::remove_dir_all(&p)
        } else {
            fs::remove_file(&p)
        };
        result.map_err(|e| format!("{}: {e}", p.display()))?;
    } else {
        trash::delete(&p).map_err(|e| format!("{TRASH_UNAVAILABLE}{}: {e}", p.display()))?;
    }
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
    crate::atomic_file::write_atomic(&path, json.as_bytes()).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastProjectData {
    path: Option<String>,
    opened_files: Option<Vec<OpenFile>>,
    expanded_paths: Option<Vec<String>>,
    terminal_collapsed: bool,
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
        let defaults: LastProjectData = LastProjectData {
            path: None,
            opened_files: None,
            expanded_paths: None,
            terminal_collapsed: true,
        };
        return Ok(
            serde_json::to_string(&defaults).map_err(|e| format!("Failed to serialize project data defaults: {e}"))?
        );
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
    terminal_collapsed: bool
) -> Result<(), String> {
    let project_path = last_project_path(&app)?;
    let data = LastProjectData {
        path: Some(path),
        opened_files,
        expanded_paths,
        terminal_collapsed
    };
    let json = serde_json::to_string(&data)
        .map_err(|e| format!("Failed to serialize last project: {e}"))?;
    crate::atomic_file::write_atomic(&project_path, json.as_bytes())
        .map_err(|e| format!("Failed to save last project: {e}"))
}


#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn git_files_and_ignored_paths_are_kept_out() {
        let dir = std::env::temp_dir().join(format!("watch-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        let m = WatchIgnores::root_only(&dir);
        let kind = |p: &str| watch_path_kind(&dir, &m, &dir.join(p));
        assert_eq!(kind(".git/objects/ab/cdef0123"), WatchPath::Git);
        assert_eq!(kind(".git/index"), WatchPath::Git);
        assert_eq!(kind("sub/.git/HEAD"), WatchPath::Git);
        assert_eq!(kind("target/debug/app"), WatchPath::Ignored);
        assert_eq!(kind("build.log"), WatchPath::Ignored);
        assert_eq!(kind("src/main.rs"), WatchPath::Visible);
        assert_eq!(kind(".gitignore"), WatchPath::Visible);
        assert_eq!(kind(".github/workflows/ci.yml"), WatchPath::Visible);
        assert_eq!(watch_path_kind(&dir, &m, Path::new("/elsewhere/x")), WatchPath::Visible);
        assert_eq!(kind("packages/a/node_modules/.vite/deps/x.js"), WatchPath::Ignored);
    }

    fn change(path: &str) -> FileWatchPayload {
        FileWatchPayload { path: path.into(), operation: WatchOp::Modify, is_dir: false }
    }

    #[test]
    fn changes_are_reported_once_quiet_or_at_the_latest_after_the_max_wait() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut b = WatchBatch::default();
        assert_eq!(b.take_due(t0), WatchFlush::Nothing);
        b.add(change("a"), t0);
        b.add(change("a"), ms(100));
        assert_eq!(b.take_due(ms(200)), WatchFlush::Nothing, "still changing");
        assert_eq!(b.take_due(ms(400)), WatchFlush::Events(vec![change("a")]), "once, when quiet");
        assert_eq!(b.take_due(ms(500)), WatchFlush::Nothing);

        // A dev server writing every 200ms: reported anyway after WATCH_MAX_WAIT.
        let mut b = WatchBatch::default();
        let mut reported = None;
        for i in 0..20u64 {
            b.add(change("out.js"), ms(i * 200));
            if let WatchFlush::Events(e) = b.take_due(ms(i * 200 + 50)) {
                reported = Some((i, e));
                break;
            }
        }
        let (i, events) = reported.expect("reported while still changing");
        assert_eq!((i * 200 + 50 >= 2_000, events), (true, vec![change("out.js")]));
    }

    #[test]
    fn a_flood_becomes_one_recheck_of_everything() {
        let t0 = Instant::now();
        let mut b = WatchBatch::default();
        for i in 0..=MAX_EVENTS_PER_FLUSH {
            b.add(change(&format!("f{i}")), t0);
        }
        assert!(b.events.is_empty(), "no longer collected one by one");
        b.add(change("more"), t0 + Duration::from_millis(250));
        assert_eq!(b.take_due(t0 + Duration::from_millis(400)), WatchFlush::Nothing, "still flooding");
        assert_eq!(b.take_due(t0 + Duration::from_millis(600)), WatchFlush::Everything);
        assert_eq!(b.take_due(t0 + Duration::from_millis(700)), WatchFlush::Nothing);
    }

    #[test]
    fn a_packages_own_gitignore_counts_under_that_package_only() {
        let dir = std::env::temp_dir().join(format!("watch-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["packages/web/src", "packages/web/.turbo", "packages/api/src", "node_modules/x"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        // Anchored: packages/web/dist, not a dist anywhere else.
        std::fs::write(dir.join("packages/web/.gitignore"), "/dist\n.turbo/\n*.timestamp-*.mjs\n").unwrap();
        // In node_modules: never looked at.
        std::fs::write(dir.join("node_modules/x/.gitignore"), "*\n").unwrap();
        let m = WatchIgnores::all(&dir);
        assert_eq!(m.matchers.len(), 2, "the root's and packages/web's");
        let kind = |p: &str| watch_path_kind(&dir, &m, &dir.join(p));
        assert_eq!(kind("packages/web/dist/index.js"), WatchPath::Ignored);
        assert_eq!(kind("packages/web/.turbo/cache.json"), WatchPath::Ignored);
        assert_eq!(kind("packages/web/vite.config.ts.timestamp-1700000000-ab12.mjs"), WatchPath::Ignored);
        assert_eq!(kind("packages/web/src/app.ts"), WatchPath::Visible);
        assert_eq!(kind("packages/api/dist/index.js"), WatchPath::Visible, "another package's dist");
        assert_eq!(kind("dist/index.js"), WatchPath::Visible);
        assert_eq!(kind("server.log"), WatchPath::Ignored);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

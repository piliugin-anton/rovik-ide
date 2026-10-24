//! The project's files as the file tree and the editor see them: the
//! folders' entries, each with a stable id, a file's text, the images a
//! preview shows, saving, and the settings kept beside them. What changes
//! the tree is in `entries`, and what reports the disk's changes to it in
//! `watch`.

mod entries;
mod watch;

pub use entries::*;
pub use watch::*;

use crate::util::ErrText;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rovik_client::provider_keys::ProviderKeys;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// Stable identity for a directory entry, robust to case-only path
/// differences (case-insensitive filesystems). Prefers the OS's real file
/// identity; falls back to a lowercased path when that's unavailable
/// (broken symlinks, filesystems without file-index support, etc).
fn file_identity(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let meta = fs::symlink_metadata(path).ok()?;
        Some(format!("u:{}:{}", meta.dev(), meta.ino()))
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

/// What the file tree's commands share: the project's watcher, its
/// .gitignore, and the path each id was last seen at.
#[derive(Default)]
pub struct WatcherState {
    /// Dropping the watcher stops its watch loop.
    active_watcher: Mutex<Option<notify::RecommendedWatcher>>,
    gitignore: Mutex<Option<(PathBuf, Arc<Gitignore>)>>,
    /// Maps a stable_file_id() back to the last path we saw it at, with the
    /// identity of what was there then. Populated whenever we hand an id to
    /// the frontend (read_dir_entries, get_path_id) and updated on
    /// rename/move, so mutating commands can always resolve an id to a real
    /// path instead of trusting a path string the frontend may be holding
    /// onto with stale or differently-cased casing. After a cross-device
    /// move the old id maps to the copy, whose identity is the copy's own.
    id_to_path: Mutex<HashMap<String, (PathBuf, Option<String>)>>,
}

fn resolve_id(state: &WatcherState, id: &str) -> Result<PathBuf, String> {
    let (path, was) = state
        .id_to_path
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("{id}: unknown id (try refreshing the folder view)"))?;
    // Another entry at the path now (renamed outside the app, the tree
    // not refreshed yet): not the id's, and not acted on as it.
    if let (Some(was), Some(now)) = (was, file_identity(&path)) {
        if now != was {
            return Err(format!(
                "{}: another file is at this path now (refresh the folder view)",
                path.display()
            ));
        }
    }
    Ok(path)
}

fn remember_id(state: &WatcherState, id: &str, path: &Path) {
    state
        .id_to_path
        .lock()
        .unwrap()
        .insert(id.to_string(), (path.to_path_buf(), file_identity(path)));
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

/// Whether `matcher` ignores `path`. Only a path under the matcher's root
/// is asked: the `ignore` crate asserts that, and the assertion would end
/// the app (a folder listed by a path outside the project's root, through
/// a link or a stale root).
fn ignored_by(matcher: &Gitignore, path: &Path, is_dir: bool) -> bool {
    path.starts_with(matcher.path())
        && matcher
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
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
/// On a worker thread: a big folder, or one on a slow disk or share, would
/// hold the window's main thread as long.
#[tauri::command]
pub async fn read_dir_entries(
    app: tauri::AppHandle,
    root_path: String,
    path: String,
) -> Result<Vec<DirEntryInfo>, String> {
    use tauri::Manager;
    crate::util::blocking(move || dir_entries(&app.state::<WatcherState>(), &root_path, &path))
        .await
}

fn dir_entries(
    state: &WatcherState,
    root_path: &str,
    path: &str,
) -> Result<Vec<DirEntryInfo>, String> {
    let matcher = get_or_build_gitignore(state, Path::new(root_path))?;

    let dir = Path::new(&path);
    let read = fs::read_dir(dir).map_err(|e| format!("{path}: {e}"))?;

    // Remember the directory's own id too, so "new file/folder in this
    // directory" can resolve it even before any of its children are cached.
    remember_id(state, &stable_file_id(dir), dir);

    let mut entries: Vec<DirEntryInfo> = Vec::new();
    for entry in read {
        let entry = entry.err_text()?;
        let file_type = entry.file_type().err_text()?;
        let name = entry.file_name().to_string_lossy().to_string();

        if name == ".git" {
            continue;
        }

        let entry_path = entry.path();
        // A link to a folder opens like one (its contents load only when
        // it's expanded, so a link to a parent can't loop).
        let is_dir = file_type.is_dir() || (file_type.is_symlink() && entry_path.is_dir());
        let ignored = ignored_by(&matcher, &entry_path, is_dir);

        let id = stable_file_id(&entry_path);
        remember_id(state, &id, &entry_path);

        entries.push(DirEntryInfo {
            id,
            name,
            path: entry_path.to_string_lossy().to_string(),
            is_dir,
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
    let mut file = fs::File::open(path).err_text()?;
    let mut buf = vec![0u8; BINARY_SNIFF_BYTES];
    let n = file.read(&mut buf).err_text()?;
    Ok(buf[..n].contains(&0))
}

/// Largest file `read_file_content` opens.
const MAX_EDITOR_FILE: u64 = 50 << 20;

/// A text file for the editor, read on a worker thread: up to 50 MB of it
/// would hold the window's main thread as long.
#[tauri::command]
pub async fn read_file_content(path: String) -> Result<String, String> {
    crate::util::blocking(move || read_text_for_editor(&path)).await
}

fn read_text_for_editor(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    // A huge log or dump would be copied whole into the webview and stall
    // (or kill) both sides; the editor can't usefully show it anyway.
    if fs::metadata(p).is_ok_and(|m| m.len() > MAX_EDITOR_FILE) {
        return Err(format!(
            "{path}: larger than {} MB, too large to open in the editor",
            MAX_EDITOR_FILE >> 20
        ));
    }
    if looks_binary(p)? {
        return Err(format!("{path}: binary file, cannot open in editor"));
    }
    fs::read_to_string(p).map_err(|e| format!("{path}: {e}"))
}

/// Largest image the Markdown preview shows.
const MAX_PREVIEW_IMAGE: u64 = 20 << 20;

/// What the Markdown preview shows as images (IMAGE_TYPES in
/// src/lib/markdownPreview.ts).
const PREVIEW_IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "bmp", "ico",
];

/// An image in a Markdown file, for its preview: the bytes as they are (an
/// ArrayBuffer in the webview; JSON would send each byte as a number).
#[tauri::command]
pub async fn read_image_file(path: String) -> Result<tauri::ipc::Response, String> {
    let bytes = crate::util::blocking(move || read_preview_image(Path::new(&path))).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Only image files: the preview asks for whatever a document's image
/// links name.
fn read_preview_image(p: &Path) -> Result<Vec<u8>, String> {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !PREVIEW_IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("{}: not an image", p.display()));
    }
    let meta = fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", p.display()));
    }
    if meta.len() > MAX_PREVIEW_IMAGE {
        return Err(format!(
            "{}: larger than {} MB",
            p.display(),
            MAX_PREVIEW_IMAGE >> 20
        ));
    }
    fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Starts the error of a save refused because the file changed on disk;
/// the rest is who changed it. The editor offers Overwrite / Compare.
pub const CHANGED_ON_DISK: &str = "changed-on-disk:";

/// Writes a file, creating any missing parent directories first — saving a
/// tab whose file (or whole folder) was deleted puts it back. Plain
/// `fs::write` rather than an atomic rename on purpose: it keeps the inode,
/// and with it the file's stable id.
///
/// `expected`: the content the tab loaded. If the file on disk is no longer
/// that (an agent, another chat or a program changed it since), nothing is
/// written and the error starts with [`CHANGED_ON_DISK`], so a save can't
/// silently erase that change (file_guard.rs). Without it, the file is
/// written regardless (the user chose to overwrite).
#[tauri::command]
pub fn write_file_content(
    path: String,
    content: String,
    expected: Option<String>,
) -> Result<(), String> {
    use rovik_core::file_guard::{self, WriteError, Writer};
    let expected = expected.map(|e| file_guard::hash(e.as_bytes()));
    match file_guard::write_for_editor(Path::new(&path), content.as_bytes(), expected) {
        Ok(()) => Ok(()),
        Err(WriteError::Changed(changed)) => Err(format!(
            "{CHANGED_ON_DISK}{}",
            match changed.by {
                Some(Writer::Chat(_)) => "by an agent chat",
                Some(Writer::Editor) => "by another save in the editor",
                None => "outside the editor: by a command or another program",
            }
        )),
        Err(WriteError::Io(e)) => Err(format!("{path}: {e}")),
    }
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

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct EditorConfigFile {
    dir: String,
    content: String,
}

/// Bigger isn't a real .editorconfig.
const MAX_EDITORCONFIG: u64 = 256 * 1024;

/// The `.editorconfig` files that can apply to a file: one per folder from
/// the file's own up to the filesystem root, nearest first. Which of them
/// count (`root = true` stops the search) is decided by the frontend's
/// parser (src/lib/editorconfig.ts), the only place that reads them.
#[tauri::command]
pub async fn read_editorconfigs(path: String) -> Vec<EditorConfigFile> {
    // Off the main thread: a folder up the way may be a slow network share.
    rovik_core::rt::spawn_blocking(move || editorconfigs_for(&path))
        .await
        .unwrap_or_default()
}

fn editorconfigs_for(path: &str) -> Vec<EditorConfigFile> {
    let Some(parent) = Path::new(path).parent() else {
        return Vec::new();
    };
    parent
        .ancestors()
        .filter_map(|dir| {
            let file = dir.join(".editorconfig");
            let meta = fs::metadata(&file).ok()?;
            if !meta.is_file() || meta.len() > MAX_EDITORCONFIG {
                return None;
            }
            Some(EditorConfigFile {
                dir: dir.to_string_lossy().into_owned(),
                content: fs::read_to_string(&file).ok()?,
            })
        })
        .collect()
}

// --- AI settings and the last project --------------------------------------

// `ai-settings.json`, its API keys in the keychain (H06): off the main
// thread, as the keychain may take a moment, or ask the user.
#[tauri::command]
pub async fn load_ai_settings(keys: State<'_, Arc<ProviderKeys>>) -> Result<String, String> {
    let keys = Arc::clone(&keys);
    crate::util::blocking(move || keys.load()).await
}

/// When the AI settings were last saved, by any window (ProviderKeys::stamp).
#[tauri::command]
pub fn ai_settings_stamp(keys: State<'_, Arc<ProviderKeys>>) -> u64 {
    keys.stamp()
}

/// `base`: the settings the frontend made `json` of, as it loaded them:
/// saved onto the settings as they are now, which another window may have
/// saved meanwhile (ProviderKeys::save_onto).
#[tauri::command]
pub async fn save_ai_settings(
    keys: State<'_, Arc<ProviderKeys>>,
    json: String,
    base: Option<String>,
) -> Result<(), String> {
    let keys = Arc::clone(&keys);
    crate::util::blocking(move || match base {
        Some(base) => keys.save_onto(&json, &base),
        None => keys.save(&json),
    })
    .await
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
        let defaults = LastProjectData {
            path: None,
            opened_files: None,
            expanded_paths: None,
            terminal_collapsed: true,
        };
        return serde_json::to_string(&defaults)
            .map_err(|e| format!("Failed to serialize project data defaults: {e}"));
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
    terminal_collapsed: bool,
) -> Result<(), String> {
    let project_path = last_project_path(&app)?;
    let data = LastProjectData {
        path: Some(path),
        opened_files,
        expanded_paths,
        terminal_collapsed,
    };
    let json = serde_json::to_string(&data)
        .map_err(|e| format!("Failed to serialize last project: {e}"))?;
    rovik_core::atomic_file::write_atomic(&project_path, json.as_bytes())
        .map_err(|e| format!("Failed to save last project: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path outside the matcher's root (a folder listed through a link,
    /// or a root that changed) is never ignored, and never asked: the
    /// `ignore` crate's assertion would end the app.
    #[test]
    fn a_path_outside_the_root_is_not_ignored_and_doesnt_panic() {
        let dir = std::env::temp_dir().join(format!("ignored-by-{}", std::process::id()));
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        let matcher = build_gitignore(&root).unwrap();
        assert!(ignored_by(&matcher, &root.join("build"), true));
        assert!(!ignored_by(&matcher, &root.join("src"), true));
        assert!(!ignored_by(&matcher, &dir.join("elsewhere/build"), true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_over_a_change_on_disk_is_refused_until_overwritten() {
        let dir = std::env::temp_dir().join(format!("editor-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, "loaded\n").unwrap();
        let p = path.to_string_lossy().to_string();

        // Nothing changed: saved.
        write_file_content(p.clone(), "mine\n".into(), Some("loaded\n".into())).unwrap();
        // Changed on disk since the tab loaded it (a command, here).
        fs::write(&path, "theirs\n").unwrap();
        let err = write_file_content(p.clone(), "mine again\n".into(), Some("mine\n".into()))
            .unwrap_err();
        assert_eq!(
            err,
            format!("{CHANGED_ON_DISK}outside the editor: by a command or another program")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        // Overwrite: no expectation.
        write_file_content(p, "mine again\n".into(), None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine again\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_image_files_are_read() {
        let dir = std::env::temp_dir().join(format!("preview-images-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("folder.png")).unwrap();
        let png = [0x89, b'P', b'N', b'G', 0, 1, 2];
        fs::write(dir.join("shot.PNG"), png).unwrap();
        fs::write(dir.join("notes.txt"), "not for the preview").unwrap();

        assert_eq!(read_preview_image(&dir.join("shot.PNG")).unwrap(), png);
        let err = read_preview_image(&dir.join("notes.txt")).unwrap_err();
        assert!(err.ends_with("not an image"), "{err}");
        let err = read_preview_image(&dir.join("folder.png")).unwrap_err();
        assert!(err.ends_with("not a file"), "{err}");
        assert!(read_preview_image(&dir.join("missing.svg")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn editorconfigs_are_read_nearest_first() {
        let dir = std::env::temp_dir().join(format!("editorconfigs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join(".editorconfig"), "root = true\n").unwrap();
        fs::write(dir.join("a/b/.editorconfig"), "[*]\nindent_size = 4\n").unwrap();
        // A folder by that name isn't a config file.
        fs::create_dir_all(dir.join("a/.editorconfig")).unwrap();

        let found = editorconfigs_for(&dir.join("a/b/main.rs").to_string_lossy());
        let ours: Vec<_> = found
            .iter()
            .filter(|f| Path::new(&f.dir).starts_with(&dir))
            .collect();
        assert_eq!(ours.len(), 2);
        assert_eq!(Path::new(&ours[0].dir), dir.join("a/b"));
        assert_eq!(ours[0].content, "[*]\nindent_size = 4\n");
        assert_eq!(Path::new(&ours[1].dir), dir);
        let _ = fs::remove_dir_all(&dir);
    }
}

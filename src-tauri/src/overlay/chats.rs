//! Each chat's overlay: opened when one of its runs starts, mounted as a
//! folder the run uses as its project root, kept until its changes land or
//! are discarded.
//!
//! The overlay's own folder is `<app data>/overlays/<chat id>/` (changes and
//! the list of them, so a chat's work survives a restart); the view is
//! mounted at a short path of its own (`$XDG_RUNTIME_DIR/rovik-ide/chats/…`
//! on Linux), so commands can use it unquoted.
//!
//! Isolation needs the virtual-files driver (FUSE, macFUSE or WinFsp). If
//! it can't mount, the chat works in the project directly, as before
//! isolation, and the run says so.

use super::fs::{OverlayChange, OverlayFs, DEFAULT_LIMIT};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

pub struct ChatOverlay {
    pub fs: Arc<OverlayFs>,
    /// The project folder underneath.
    pub project: PathBuf,
    /// Where the chat sees it: the mount.
    pub view: PathBuf,
    /// The overlay's own folder (changes, build output of its commands).
    pub store: PathBuf,
    mount: Mutex<Option<crate::vfs::Mount>>,
}

impl ChatOverlay {
    /// Variables for the chat's commands: build output goes to the
    /// overlay's own folder on disk instead of through the mount, and isn't
    /// shared with other chats or the project.
    pub fn command_env(&self) -> Vec<(String, String)> {
        let out = self.store.join("out");
        vec![("CARGO_TARGET_DIR".into(), out.join("target").to_string_lossy().into_owned())]
    }

    /// Why a command can't run in this chat as written, if it can't.
    /// Changes reach the project by landing, so git commands that change
    /// the repository are refused (reading it works: status, diff, log,
    /// show, blame), and so is the project's own folder named by its full
    /// path, which would reach past the chat's view.
    pub fn command_problem(&self, command: &str) -> Option<String> {
        const CHANGES_REPO: &[&str] = &[
            "checkout", "switch", "reset", "commit", "stash", "push", "pull", "merge", "rebase", "clean", "restore",
            "cherry-pick", "revert", "tag", "am", "apply", "rm", "mv", "fetch", "gc", "worktree", "init", "clone",
        ];
        let parsed = crate::agent_run::permissions::parse_command(command);
        for segment in &parsed.segments {
            let mut words = segment.iter().map(String::as_str);
            if words.next().is_some_and(|p| p == "git" || p.ends_with("/git") || p.eq_ignore_ascii_case("git.exe")) {
                let sub = segment.iter().skip(1).find(|w| !w.starts_with('-')).map(String::as_str);
                if let Some(sub) = sub.filter(|s| CHANGES_REPO.contains(s)) {
                    return Some(format!(
                        "Error: `git {sub}` changes the repository, and this chat is isolated: its changes reach the project when they land, not through git. Read-only git (status, diff, log, show, blame) works. Leave committing to the user."
                    ));
                }
            }
        }
        let project = self.project.to_string_lossy();
        if names_path(command, &project) {
            return Some(format!(
                "Error: this chat works in its own view of the project (the current folder, {}), not in {project} itself. Use paths relative to the current folder.",
                self.view.display()
            ));
        }
        None
    }

    fn unmount(&self) {
        self.fs.flush();
        if let Some(m) = self.mount.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = m.unmount();
        }
    }
}

/// Whether `text` names `path` itself or something inside it (not just a
/// longer name that starts the same, like `/p2` for `/p`).
fn names_path(text: &str, path: &str) -> bool {
    !path.is_empty()
        && text.match_indices(path).any(|(i, _)| {
            let after = text[i + path.len()..].chars().next();
            after.is_none_or(|c| matches!(c, '/' | '\\' | ' ' | '"' | '\'' | ';' | '&' | '|' | ')'))
        })
}

static OPEN: LazyLock<Mutex<HashMap<String, Arc<ChatOverlay>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where overlays are kept: the app's data folder. Landings a crash
/// interrupted are finished first.
pub fn init(data_dir: &Path) {
    *DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(data_dir.to_path_buf());
    let finished = super::land::recover(data_dir);
    if !finished.is_empty() {
        eprintln!("overlay: finished an interrupted landing: {}", finished.join(", "));
    }
}

/// Whether chats work in overlays. A development setting until isolation is
/// switched on for every chat (milestone 8 of the plan): set
/// `ROVIK_ISOLATE_CHATS=1`.
pub fn enabled() -> bool {
    std::env::var("ROVIK_ISOLATE_CHATS").is_ok_and(|v| v == "1")
}

fn data_dir() -> Option<PathBuf> {
    DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn store_dir(dialog_id: &str) -> Option<PathBuf> {
    Some(data_dir()?.join("overlays").join(dialog_id))
}

fn mountpoint(dialog_id: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_dir()) {
        Some(dir) => PathBuf::from(dir).join("rovik-ide").join("chats"),
        None => std::env::temp_dir().join(format!("rovik-ide-{}", unsafe { libc::getuid() })).join("chats"),
    };
    #[cfg(not(target_os = "linux"))]
    let base = std::env::temp_dir().join("rovik-chats");
    base.join(dialog_id)
}

/// The chat's overlay, opened (and mounted) if it isn't yet. `on_warn`:
/// told once when its changes pass 80% of its limit (used, limit).
pub fn open(
    dialog_id: &str,
    project: &Path,
    on_warn: Box<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<Arc<ChatOverlay>, String> {
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = open.get(dialog_id) {
        if o.project == project {
            return Ok(o.clone());
        }
        return Err("this chat's isolated changes belong to another project folder".into());
    }
    let store = store_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    // A chat stays with the project its changes were made in.
    let marker = store.join("project.txt");
    match std::fs::read_to_string(&marker) {
        Ok(saved) if Path::new(saved.trim()) != project => {
            return Err(format!("this chat's isolated changes belong to {}", saved.trim()));
        }
        Ok(_) => {}
        Err(_) => std::fs::write(&marker, project.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?,
    }
    let fs = Arc::new(OverlayFs::open(project, &store, DEFAULT_LIMIT).map_err(|e| e.to_string())?);
    fs.on_warn(on_warn);
    let at = mountpoint(dialog_id);
    let (mount, view) = crate::vfs::platform_mount(fs.clone(), Some(&at))
        .map_err(|e| format!("couldn't mount the chat's view: {}", e.message()))?;
    let overlay = Arc::new(ChatOverlay {
        fs,
        project: project.to_path_buf(),
        view,
        store,
        mount: Mutex::new(Some(mount)),
    });
    open.insert(dialog_id.to_string(), overlay.clone());
    Ok(overlay)
}

/// The chat's overlay if it's open.
pub fn get(dialog_id: &str) -> Option<Arc<ChatOverlay>> {
    OPEN.lock().unwrap_or_else(|e| e.into_inner()).get(dialog_id).cloned()
}

/// Whether the chat has an overlay, open or saved.
pub fn exists(dialog_id: &str) -> bool {
    get(dialog_id).is_some() || store_dir(dialog_id).is_some_and(|s| s.join("manifest.json").is_file())
}

/// Unmounts the chat's view; its changes stay saved.
pub fn close(dialog_id: &str) {
    let removed = OPEN.lock().unwrap_or_else(|e| e.into_inner()).remove(dialog_id);
    if let Some(o) = removed {
        o.unmount();
    }
}

/// Throws the chat's changes away.
pub fn discard(dialog_id: &str) -> Result<(), String> {
    close(dialog_id);
    match store_dir(dialog_id) {
        Some(store) if store.exists() => std::fs::remove_dir_all(&store).map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// At exit: every view unmounted, or its folder stays a dead mount.
pub fn close_all() {
    let all: Vec<Arc<ChatOverlay>> = OPEN.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, o)| o).collect();
    for o in all {
        o.unmount();
    }
}

/// What the chat changed, for the UI: landable changes only.
pub fn changes(dialog_id: &str) -> Vec<OverlayChange> {
    let fs = match get(dialog_id) {
        Some(o) => o.fs.clone(),
        None => {
            let (Some(store), Some(project)) = (store_dir(dialog_id), store_dir(dialog_id).and_then(|s| std::fs::read_to_string(s.join("project.txt")).ok())) else {
                return Vec::new();
            };
            match OverlayFs::open(Path::new(project.trim()), &store, DEFAULT_LIMIT) {
                Ok(fs) => Arc::new(fs),
                Err(_) => return Vec::new(),
            }
        }
    };
    fs.changes().into_iter().filter(|c| !c.by_product()).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn naming_the_project_path() {
        assert!(super::names_path("cat /home/u/p/src/a.rs", "/home/u/p"));
        assert!(super::names_path("cd /home/u/p && ls", "/home/u/p"));
        assert!(super::names_path("ls /home/u/p", "/home/u/p"));
        assert!(!super::names_path("cat /home/u/p2/x", "/home/u/p"));
        assert!(!super::names_path("ls src", "/home/u/p"));
    }
}

// ---------------------------------------------------------------------------
// For the UI
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    /// added, changed, deleted, moved.
    pub change: &'static str,
    /// Moved: from where in the project.
    pub from: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayStatus {
    /// The chat has an overlay (open or saved).
    pub isolated: bool,
    /// Why its changes didn't land, if they didn't.
    pub pending: Option<Pending>,
    /// Its latest landing, for Undo.
    pub last_landing: Option<LastLanding>,
    /// Its view, while it's mounted.
    pub view: Option<String>,
    pub files: Vec<ChangedFile>,
    pub used: u64,
    pub limit: u64,
}

fn summarize(changes: &[OverlayChange]) -> Vec<ChangedFile> {
    let mut out = Vec::new();
    for c in changes {
        match c {
            OverlayChange::Delete { path, .. } => {
                // A delete followed by a write at the same path is a replacement.
                if !changes.iter().any(|w| matches!(w, OverlayChange::Write { path: p, .. } if p == path)) {
                    out.push(ChangedFile { path: path.clone(), change: "deleted", from: None });
                }
            }
            OverlayChange::Write { path, kind, origin, upper, perm_changed, .. } => {
                if *kind == crate::vfs::store::Kind::Dir {
                    continue;
                }
                let change = match origin {
                    None => "added",
                    Some(o) if o != path => "moved",
                    Some(_) if upper.is_some() || *perm_changed => "changed",
                    Some(_) => continue,
                };
                let from = origin.clone().filter(|o| o != path);
                out.push(ChangedFile { path: path.clone(), change, from });
            }
        }
    }
    out
}

#[tauri::command]
pub fn overlay_status(dialog_id: String) -> OverlayStatus {
    let isolated = exists(&dialog_id);
    let open = get(&dialog_id);
    let (used, limit) = open.as_ref().map(|o| o.fs.used()).unwrap_or((0, DEFAULT_LIMIT));
    OverlayStatus {
        isolated,
        pending: if isolated { pending(&dialog_id) } else { None },
        last_landing: last_landing(&dialog_id),
        view: open.map(|o| o.view.to_string_lossy().into_owned()),
        files: if isolated { summarize(&changes(&dialog_id)) } else { Vec::new() },
        used,
        limit,
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVersions {
    /// The project's file now (None: not there).
    pub project: Option<String>,
    /// The chat's version (None: the chat deleted it).
    pub chat: Option<String>,
}

/// A changed file as the project has it and as the chat has it, for a diff.
#[tauri::command]
pub fn overlay_file(dialog_id: String, path: String) -> Result<FileVersions, String> {
    if path.split(['/', '\\']).any(|p| p == "..") {
        return Err("path must not contain '..'".into());
    }
    let changes = changes(&dialog_id);
    let store_project = store_dir(&dialog_id).and_then(|s| std::fs::read_to_string(s.join("project.txt")).ok());
    let project_dir = get(&dialog_id).map(|o| o.project.clone()).or(store_project.map(|p| PathBuf::from(p.trim())));
    let project_dir = project_dir.ok_or("this chat has no isolated changes")?;
    let in_project = |rel: &str| std::fs::read_to_string(project_dir.join(rel)).ok();
    let chat = changes.iter().rev().find(|c| c.path() == path).and_then(|c| match c {
        OverlayChange::Write { upper: Some(file), .. } => std::fs::read_to_string(file).ok(),
        OverlayChange::Write { origin: Some(o), .. } => in_project(o),
        _ => None,
    });
    Ok(FileVersions { project: in_project(&path), chat })
}

/// Throws away the chat's changes (all of them).
#[tauri::command]
pub fn overlay_discard(dialog_id: String) -> Result<(), String> {
    discard(&dialog_id)
}

// ---------------------------------------------------------------------------
// Landing
// ---------------------------------------------------------------------------

/// Why a chat's changes didn't land, kept until they do (or are discarded).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub reason: String,
    pub conflicts: Vec<super::land::Conflict>,
}

pub fn set_pending(dialog_id: &str, pending: Option<&Pending>) {
    let Some(store) = store_dir(dialog_id) else { return };
    let file = store.join("landing.json");
    match pending {
        Some(p) if store.is_dir() => {
            if let Ok(bytes) = serde_json::to_vec(p) {
                let _ = crate::atomic_file::write_atomic(&file, &bytes);
            }
        }
        _ => {
            let _ = std::fs::remove_file(file);
        }
    }
}

pub fn pending(dialog_id: &str) -> Option<Pending> {
    let bytes = std::fs::read(store_dir(dialog_id)?.join("landing.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn project_of(dialog_id: &str) -> Option<PathBuf> {
    get(dialog_id)
        .map(|o| o.project.clone())
        .or_else(|| store_dir(dialog_id).and_then(|s| std::fs::read_to_string(s.join("project.txt")).ok()).map(|p| PathBuf::from(p.trim())))
}

/// Lands the chat's changes now. `keep_chat`: on conflicts, the chat's
/// version wins ("Land anyway"). Landed (or nothing to land): the overlay
/// is gone, and the chat's next run starts from the project as it is.
pub fn land_now(dialog_id: &str, keep_chat: bool) -> Result<Option<super::land::Landed>, super::land::LandError> {
    use super::land::LandError;
    let data = data_dir().ok_or_else(|| LandError::Failed("the app's data folder isn't known yet".into()))?;
    let project = project_of(dialog_id).ok_or_else(|| LandError::Failed("this chat has no isolated changes".into()))?;
    if let Some(o) = get(dialog_id) {
        o.fs.flush();
    }
    let changes = changes(dialog_id);
    match super::land::land(&data, dialog_id, &project, &changes, keep_chat) {
        Ok(landed) => {
            let _ = discard(dialog_id);
            Ok(landed)
        }
        Err(e) => {
            let conflicts = match &e {
                LandError::Conflicts(c) => c.clone(),
                _ => Vec::new(),
            };
            set_pending(dialog_id, Some(&Pending { reason: e.to_string(), conflicts }));
            Err(e)
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastLanding {
    pub id: String,
    pub paths: Vec<String>,
    /// Seconds since 1970.
    pub at: u64,
}

pub fn last_landing(dialog_id: &str) -> Option<LastLanding> {
    let (id, paths, at) = super::land::latest(&data_dir()?, dialog_id)?;
    Some(LastLanding { id, paths, at })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandResult {
    pub landed: Option<super::land::Landed>,
    pub conflicts: Vec<super::land::Conflict>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn overlay_land(dialog_id: String, keep_chat: bool) -> LandResult {
    let result = tauri::async_runtime::spawn_blocking(move || land_now(&dialog_id, keep_chat)).await;
    match result {
        Ok(Ok(landed)) => LandResult { landed, conflicts: Vec::new(), error: None },
        Ok(Err(super::land::LandError::Conflicts(c))) => LandResult { landed: None, conflicts: c, error: None },
        Ok(Err(e)) => LandResult { landed: None, conflicts: Vec::new(), error: Some(e.to_string()) },
        Err(e) => LandResult { landed: None, conflicts: Vec::new(), error: Some(e.to_string()) },
    }
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since.
#[tauri::command]
pub async fn overlay_undo(dialog_id: String, id: String) -> Result<Vec<super::land::Conflict>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let data = data_dir().ok_or("the app's data folder isn't known yet")?;
        let project = super::land::project_of(&data, &dialog_id).ok_or("no landing to undo")?;
        super::land::undo(&data, &dialog_id, &project, &id)
    })
    .await
    .map_err(|e| e.to_string())?
}

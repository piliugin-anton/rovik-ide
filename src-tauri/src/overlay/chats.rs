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
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

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
    /// Variables for the chat's commands. Rust's build output goes to a
    /// folder on disk outside the mount, shared by the project's isolated
    /// chats (so only the first chat builds from cold) but not with the
    /// project's own `target/` (so the user never runs a chat's build).
    /// Cargo locks it, and the queue for heavy work runs one build at a
    /// time anyway.
    pub fn command_env(&self) -> Vec<(String, String)> {
        let shared = data_dir().map(|d| d.join("overlays").join("targets").join(project_key(&self.project)));
        let dir = shared.unwrap_or_else(|| self.store.join("out").join("target"));
        vec![("CARGO_TARGET_DIR".into(), dir.to_string_lossy().into_owned())]
    }

    /// How the chat's commands are kept out of the project: the project's
    /// path shows the chat's view to them (confine.rs). None where that
    /// isn't possible.
    pub fn confine(&self) -> Option<super::confine::Confine> {
        let c = super::confine::Confine { view: self.view.clone(), project: self.project.clone() };
        super::confine::available(&c).then_some(c)
    }

    /// Why a command can't run in this chat as written, if it can't.
    /// Changes reach the project by landing, so git commands that change
    /// the repository are refused (reading it works: status, diff, log,
    /// show, blame). Where commands can't be confined, so is the project's
    /// own folder named by its full path, which would reach past the
    /// chat's view.
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
        if names_path(command, &project) && self.confine().is_none() {
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

/// Whether chats work in overlays: always (the user's decision), unless
/// `ROVIK_ISOLATE_CHATS=0` turns it off for troubleshooting.
pub fn enabled() -> bool {
    std::env::var("ROVIK_ISOLATE_CHATS").map_or(true, |v| v != "0")
}

/// A short, stable name for a project folder.
fn project_key(project: &Path) -> String {
    let hash = crate::file_guard::hash(project.to_string_lossy().as_bytes());
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Chats already told this session that they can't be isolated.
static TOLD: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);

/// Whether to tell this chat it isn't isolated: once per session.
pub fn tell_not_isolated(dialog_id: &str) -> bool {
    TOLD.lock().unwrap_or_else(|e| e.into_inner()).insert(dialog_id.to_string())
}

fn data_dir() -> Option<PathBuf> {
    DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A chat id from the frontend names one folder of ours, nothing else.
pub fn valid_id(dialog_id: &str) -> bool {
    !dialog_id.is_empty()
        && dialog_id.len() <= 200
        && dialog_id != "."
        && dialog_id != ".."
        && !dialog_id.contains(['/', '\\', '\0', ':'])
}

fn store_dir(dialog_id: &str) -> Option<PathBuf> {
    valid_id(dialog_id).then(|| Some(data_dir()?.join("overlays").join(dialog_id))).flatten()
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

/// A deleted chat: its overlay and its landings' journals go.
pub fn forget(dialog_id: &str) {
    if !valid_id(dialog_id) {
        return;
    }
    let _ = discard(dialog_id);
    if let Some(data) = data_dir() {
        let _ = std::fs::remove_dir_all(data.join("landings").join(dialog_id));
    }
}

/// At exit: every view unmounted, or its folder stays a dead mount.
pub fn close_all() {
    let all: Vec<Arc<ChatOverlay>> = OPEN.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, o)| o).collect();
    for o in all {
        o.unmount();
    }
}

// ---------------------------------------------------------------------------
// The project changing under open chats
// ---------------------------------------------------------------------------

/// Notes for each chat's model, waiting for its next tool result.
static NOTES: LazyLock<Mutex<HashMap<String, Notes>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct Notes {
    changed: std::collections::BTreeSet<String>,
    conflicts: std::collections::BTreeSet<String>,
    many: bool,
}

/// More files than this in one go is "many files changed" (a branch
/// switch, a pull), not a list.
const MANY: usize = 200;

/// The project changed: `abs` paths in it, or (`everything`) too many to
/// list. Every open chat on that project sees untouched files as they are
/// now; its model is told about files it had looked at; a file it changed
/// itself is merged with the new version right away, and a conflict is
/// reported now instead of when the chat tries to land.
pub fn project_changed(project: &Path, abs: &[PathBuf], everything: bool) -> Vec<(String, String)> {
    let overlays: Vec<Arc<ChatOverlay>> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|o| same_folder(&o.project, project))
        .cloned()
        .collect();
    if everything || abs.len() > MANY {
        project_busy(project);
    }
    let mut conflicts_found = Vec::new();
    for o in overlays {
        let id = o.store.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if everything || abs.len() > MANY {
            o.fs.project_changed_everywhere();
            NOTES.lock().unwrap_or_else(|e| e.into_inner()).entry(id).or_default().many = true;
            continue;
        }
        let rels: Vec<String> = abs
            .iter()
            .filter_map(|p| p.strip_prefix(&o.project).ok())
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .filter(|r| !r.is_empty() && !r.starts_with(".git/") && r != ".git")
            .collect();
        let (seen, touched) = o.fs.project_changed(&rels);
        let mut conflicts = Vec::new();
        for rel in &touched {
            if conflicts_now(&o, rel) {
                conflicts.push(rel.clone());
            }
        }
        let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
        let entry = notes.entry(id.clone()).or_default();
        entry.changed.extend(seen);
        entry.conflicts.extend(conflicts.iter().cloned());
        drop(notes);
        if !conflicts.is_empty() {
            let pending = Pending {
                reason: "Won't land on its own: the project changed the same lines meanwhile.".into(),
                conflicts: conflicts
                    .iter()
                    .map(|p| super::land::Conflict { path: p.clone(), reason: "the chat and the project changed the same lines".into() })
                    .collect(),
            };
            set_pending(&id, Some(&pending));
            conflicts_found.extend(conflicts.into_iter().map(|p| (id.clone(), p)));
        }
    }
    conflicts_found
}

// ---------------------------------------------------------------------------
// Chats that aren't isolated
// ---------------------------------------------------------------------------

/// Without the virtual-files driver, chats work in the project itself. Their
/// file tools are guarded (file_guard.rs), but commands write unchecked, so
/// only one such chat per project at a time runs commands that write: the
/// first, until its run ends.
static DIRECT: LazyLock<Mutex<HashMap<PathBuf, String>>> = LazyLock::new(Default::default);

/// Lets a chat working in the project directly run a command that writes,
/// or says which chat already does.
pub fn claim_direct_writes(project: &Path, dialog_id: &str) -> Result<(), String> {
    let mut direct = DIRECT.lock().unwrap_or_else(|e| e.into_inner());
    let holder = direct.entry(busy_key(project)).or_insert_with(|| dialog_id.to_string());
    if holder == dialog_id {
        return Ok(());
    }
    Err(format!(
        "Error: another chat that isn't isolated (chat {}) is running commands that change files in this project, and this chat isn't isolated either, so the two could overwrite each other. Read-only commands work; use the write and edit tools for changes, or wait until that chat's run ends.",
        &holder[..holder.len().min(8)]
    ))
}

/// The chat's run ended: another chat may run commands that write.
pub fn release_direct_writes(dialog_id: &str) {
    DIRECT.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, holder| holder != dialog_id);
}

// ---------------------------------------------------------------------------
// Many files changing at once
// ---------------------------------------------------------------------------

/// Projects where many files are changing at once (a branch switch, a pull,
/// a stash): when that was last seen, and git operations of the app's own
/// still going.
static BUSY: LazyLock<Mutex<HashMap<PathBuf, (Instant, usize)>>> = LazyLock::new(Default::default);

/// How long the project has to stay still before isolated chats go on.
const QUIET: Duration = Duration::from_secs(2);

fn busy_key(project: &Path) -> PathBuf {
    project.canonicalize().unwrap_or_else(|_| project.to_path_buf())
}

/// Many files are changing in the project right now.
pub fn project_busy(project: &Path) {
    let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    busy.entry(busy_key(project)).or_insert((Instant::now(), 0)).0 = Instant::now();
}

/// Whether isolated chats on the project should wait: many files changed
/// in the last moments, or a git operation is still going.
pub fn is_busy(project: &Path) -> bool {
    let busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    busy.get(&busy_key(project)).is_some_and(|(at, held)| *held > 0 || at.elapsed() < QUIET)
}

/// Held while one of the app's git operations rewrites the working tree.
pub struct BusyGuard(PathBuf);

pub fn hold_busy(project: &Path) -> BusyGuard {
    let key = busy_key(project);
    let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let entry = busy.entry(key.clone()).or_insert((Instant::now(), 0));
    entry.1 += 1;
    BusyGuard(key)
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = busy.get_mut(&self.0) {
            entry.1 = entry.1.saturating_sub(1);
            entry.0 = Instant::now();
        }
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || (a.canonicalize().ok().is_some() && a.canonicalize().ok() == b.canonicalize().ok())
}

/// Whether the chat's change to `rel` now conflicts with the project's file.
fn conflicts_now(o: &ChatOverlay, rel: &str) -> bool {
    let Some((ours, base)) = o.fs.versions(rel) else { return false };
    let (Ok(ours), Ok(base)) = (std::fs::read(ours), std::fs::read(base)) else { return false };
    let theirs = std::fs::read(o.project.join(rel)).ok();
    match theirs {
        None => true,
        Some(t) if t == base || t == ours => false,
        Some(t) => {
            if crate::merge::is_binary(&t) || crate::merge::is_binary(&ours) || crate::merge::is_binary(&base) {
                return true;
            }
            let merged = crate::merge::merge3(
                &String::from_utf8_lossy(&base),
                &String::from_utf8_lossy(&ours),
                &String::from_utf8_lossy(&t),
            );
            !matches!(merged, Ok(crate::merge::Merged::Clean(_)))
        }
    }
}

/// What the chat's model should know about the project changing under it,
/// once: added to its next tool result.
pub fn take_note(dialog_id: &str) -> Option<String> {
    let notes = NOTES.lock().unwrap_or_else(|e| e.into_inner()).remove(dialog_id)?;
    let mut parts = Vec::new();
    if notes.many {
        parts.push("Many files changed in the project meanwhile (a branch switch or a pull?). Files you haven't changed now show their new content; read again what you rely on.".to_string());
    }
    if !notes.changed.is_empty() {
        let list = notes.changed.iter().take(20).cloned().collect::<Vec<_>>().join(", ");
        parts.push(format!(
            "Changed in the project since you looked at them (another chat landed, or the user edited them): {list}. Your view shows the new content; read them again before relying on them."
        ));
    }
    if !notes.conflicts.is_empty() {
        let list = notes.conflicts.iter().cloned().collect::<Vec<_>>().join(", ");
        parts.push(format!(
            "Also changed in the project, on lines you changed too: {list}. Your version won't land without the user deciding; say so in your summary."
        ));
    }
    (!parts.is_empty()).then(|| format!("\n\nNote: {}", parts.join(" ")))
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
    /// Files the project's newer changes were merged into (Undo the merge).
    pub merged_in: Vec<String>,
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
        merged_in: if isolated { merged_in(&dialog_id) } else { Vec::new() },
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
    check_rel(&path)?;
    let changes = changes(&dialog_id);
    // Only the chat's own changes: not a way to read any file.
    if !changes.iter().any(|c| c.path() == path) {
        return Err(format!("{path}: this chat didn't change it"));
    }
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
    // Conflicts handed to the chat land once it has resolved them.
    let left = markers_left(dialog_id);
    if !left.is_empty() {
        let e = LandError::Failed(format!("conflict markers are still in {}", left.join(", ")));
        set_pending(dialog_id, Some(&Pending { reason: format!("Not landed: {e}."), conflicts: Vec::new() }));
        return Err(e);
    }
    let changes = changes(dialog_id);
    match super::land::land(&data, dialog_id, &project, &changes, keep_chat) {
        Ok(landed) => {
            let _ = discard(dialog_id);
            // Other chats on the project hear about it now, not only when
            // the project's watcher (if it's open in the editor) does.
            if let Some(l) = &landed {
                let abs: Vec<PathBuf> = l.paths.iter().map(|p| project.join(p)).collect();
                project_changed(&project, &abs, false);
            }
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
        if !valid_id(&dialog_id) {
            return Err("bad chat id".to_string());
        }
        let data = data_dir().ok_or("the app's data folder isn't known yet")?;
        let project = super::land::project_of(&data, &dialog_id).ok_or("no landing to undo")?;
        let paths = super::land::latest(&data, &dialog_id).filter(|(l, _, _)| *l == id).map(|(_, p, _)| p).unwrap_or_default();
        let kept = super::land::undo(&data, &dialog_id, &project, &id)?;
        // Other chats on the project see the files as they are again.
        let abs: Vec<PathBuf> = paths.iter().map(|p| project.join(p)).collect();
        project_changed(&project, &abs, false);
        Ok(kept)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Conflicts: resolved by the user, or handed to the chat
// ---------------------------------------------------------------------------

/// The chat's overlay, open or (between runs, after a restart) opened from
/// its saved changes for this one call.
fn with_fs<T>(dialog_id: &str, f: impl FnOnce(&OverlayFs) -> T) -> Result<T, String> {
    if let Some(o) = get(dialog_id) {
        return Ok(f(&o.fs));
    }
    let store = store_dir(dialog_id).filter(|s| s.join("manifest.json").is_file()).ok_or("this chat has no isolated changes")?;
    let project = project_of(dialog_id).ok_or("this chat has no isolated changes")?;
    let fs = OverlayFs::open(&project, &store, DEFAULT_LIMIT).map_err(|e| e.to_string())?;
    let out = f(&fs);
    fs.flush();
    Ok(out)
}

fn check_rel(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.split(['/', '\\']).any(|p| p == "..") {
        return Err(format!("{path}: not a path in the project"));
    }
    Ok(())
}

/// A file's three versions: what the chat changed it from, the chat's
/// version, the project's now (None: not there).
struct Sides {
    base: Option<Vec<u8>>,
    chat: Option<Vec<u8>>,
    project: Option<Vec<u8>>,
}

fn sides(dialog_id: &str, path: &str) -> Result<Sides, String> {
    check_rel(path)?;
    let project_dir = project_of(dialog_id).ok_or("this chat has no isolated changes")?;
    let change = changes(dialog_id).into_iter().rev().find(|c| c.path() == path).ok_or_else(|| format!("{path}: this chat didn't change it"))?;
    let (chat, base) = match &change {
        OverlayChange::Write { upper: Some(file), base_copy, .. } => {
            (std::fs::read(file).ok(), base_copy.as_ref().and_then(|b| std::fs::read(b).ok()))
        }
        OverlayChange::Write { origin: Some(o), .. } => (std::fs::read(project_dir.join(o)).ok(), None),
        _ => (None, None),
    };
    Ok(Sides { base, chat, project: std::fs::read(project_dir.join(path)).ok() })
}

/// A conflicted file for the conflict resolver: the project's version on the
/// left ("ours", it's where the chat's changes go), the chat's on the right.
pub fn conflict_file(dialog_id: &str, path: &str) -> Result<crate::git_sync::ConflictFile, String> {
    use crate::merge::{is_binary, merge3_raw, parse_merged, MergeSegment};
    let s = sides(dialog_id, path)?;
    let binary = [&s.base, &s.chat, &s.project].iter().any(|b| b.as_deref().is_some_and(is_binary));
    let text = |b: &Option<Vec<u8>>| b.as_deref().map(|b| String::from_utf8_lossy(b).into_owned());
    let segments = if binary {
        Vec::new()
    } else if let (Some(p), Some(c)) = (&s.project, &s.chat) {
        let merged = merge3_raw(s.base.as_deref().unwrap_or(&[]), p, c)?;
        parse_merged(&String::from_utf8_lossy(&merged.content))
    } else {
        vec![MergeSegment::Conflict {
            ours: text(&s.project).unwrap_or_default(),
            base: text(&s.base).unwrap_or_default(),
            theirs: text(&s.chat).unwrap_or_default(),
        }]
    };
    Ok(crate::git_sync::ConflictFile {
        path: path.to_string(),
        ours: text(&s.project),
        theirs: text(&s.chat),
        base: text(&s.base),
        binary,
        segments,
        resolved: false,
        current: None,
        ours_label: "Project (now)".into(),
        theirs_label: "This chat".into(),
    })
}

/// Settles a conflicted file: the chat's version becomes `content` (None:
/// deleted), or the whole of one `side` ("ours": the project's, "theirs":
/// the chat's), and it's merged against the project's file as it is now, so
/// it lands as it is. Returns the conflicts left.
pub fn resolve_conflict(dialog_id: &str, path: &str, content: Option<Vec<u8>>, side: Option<&str>) -> Result<Vec<super::land::Conflict>, String> {
    let s = sides(dialog_id, path)?;
    let content = match side {
        Some("ours") => s.project.clone(),
        Some("theirs") => s.chat.clone(),
        Some(other) => return Err(format!("unknown side: {other}")),
        None => content,
    };
    with_fs(dialog_id, |fs| fs.rebase_file(path, content.as_deref(), s.project.as_deref()))?
        .map_err(|e| format!("{path}: {e:?}"))?;
    let mut left = Vec::new();
    if let Some(mut p) = pending(dialog_id) {
        p.conflicts.retain(|c| c.path != path);
        left = p.conflicts.clone();
        if p.conflicts.is_empty() {
            set_pending(dialog_id, None);
        } else {
            set_pending(dialog_id, Some(&p));
        }
    }
    Ok(left)
}

/// The markers put in the chat's files for it to resolve, labelled so they
/// can be found again (and aren't mistaken for markers of its own).
const MARK_PROJECT: &str = "<<<<<<< project (now)";
const MARK_BASE: &str = "||||||| before";
const MARK_SPLIT: &str = "=======";
const MARK_CHAT: &str = ">>>>>>> this chat";

fn markers_file(dialog_id: &str) -> Option<PathBuf> {
    Some(store_dir(dialog_id)?.join("markers.json"))
}

/// Files the chat was asked to resolve markers in.
fn marked(dialog_id: &str) -> Vec<String> {
    markers_file(dialog_id).and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Hands the conflicts to the chat: each text file the project and the chat
/// both changed gets git-style conflict markers in the chat's version, and
/// is merged against the project's file as it is now. Returns the files
/// marked; the rest (binary, or deleted on one side) stay for the user.
pub fn mark_conflicts(dialog_id: &str) -> Result<Vec<String>, String> {
    let Some(mut p) = pending(dialog_id) else { return Ok(Vec::new()) };
    let mut done = Vec::new();
    for c in &p.conflicts {
        let file = conflict_file(dialog_id, &c.path)?;
        if file.binary || file.ours.is_none() || file.theirs.is_none() {
            continue;
        }
        let mut text = String::new();
        for seg in &file.segments {
            match seg {
                crate::merge::MergeSegment::Common { text: t } => text.push_str(t),
                crate::merge::MergeSegment::Conflict { ours, base, theirs } => {
                    for (mark, part) in [(MARK_PROJECT, ours), (MARK_BASE, base), (MARK_SPLIT, theirs)] {
                        text.push_str(mark);
                        text.push('\n');
                        text.push_str(part);
                        if !part.is_empty() && !part.ends_with('\n') {
                            text.push('\n');
                        }
                    }
                    text.push_str(MARK_CHAT);
                    text.push('\n');
                }
            }
        }
        let project = std::fs::read(project_of(dialog_id).unwrap_or_default().join(&c.path)).ok();
        with_fs(dialog_id, |fs| fs.rebase_file(&c.path, Some(text.as_bytes()), project.as_deref()))?
            .map_err(|e| format!("{}: {e:?}", c.path))?;
        done.push(c.path.clone());
    }
    let mut all = marked(dialog_id);
    all.extend(done.iter().cloned());
    all.sort();
    all.dedup();
    if let Some(f) = markers_file(dialog_id) {
        let _ = crate::atomic_file::write_atomic(&f, &serde_json::to_vec(&all).unwrap_or_default());
    }
    p.conflicts.retain(|c| !done.contains(&c.path));
    p.reason = "Waiting for the chat to resolve the conflicts.".into();
    set_pending(dialog_id, Some(&p));
    Ok(done)
}

/// Files the chat was asked to resolve that still have its markers. Ones
/// resolved are forgotten.
pub fn markers_left(dialog_id: &str) -> Vec<String> {
    let all = marked(dialog_id);
    if all.is_empty() {
        return all;
    }
    let left: Vec<String> = all
        .into_iter()
        .filter(|path| {
            sides(dialog_id, path).ok().and_then(|s| s.chat).is_some_and(|c| {
                String::from_utf8_lossy(&c).lines().any(|l| l.starts_with(MARK_PROJECT) || l.starts_with(MARK_CHAT))
            })
        })
        .collect();
    if let Some(f) = markers_file(dialog_id) {
        if left.is_empty() {
            let _ = std::fs::remove_file(f);
        } else {
            let _ = crate::atomic_file::write_atomic(&f, &serde_json::to_vec(&left).unwrap_or_default());
        }
    }
    left
}

/// What the chat is asked, with the conflicts marked in its files.
pub fn resolve_request(files: &[String]) -> String {
    format!(
        "Your changes conflict with changes made in the project meanwhile, in: {}.\n\nEach of these files now has conflict markers: `{MARK_PROJECT}` starts the project's version, `{MARK_BASE}` the lines before either change, `{MARK_SPLIT}` your version, and `{MARK_CHAT}` ends the conflict. Read each file, combine both changes so neither is lost (keep the project's change and redo yours on top of it), remove every marker, and make sure the result still works. Your changes land once no markers are left.",
        files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
    )
}

/// The chat's own versions from before the project's changes were merged
/// in, kept until its changes land (or the user takes them back).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct PreMerge {
    /// path → (the chat's version, what it was merged against), as files
    /// in the pre-merge folder; None: not there.
    files: BTreeMap<String, (Option<String>, Option<String>)>,
}

fn pre_merge_dir(dialog_id: &str) -> Option<PathBuf> {
    Some(store_dir(dialog_id)?.join("pre-merge"))
}

fn load_pre_merge(dialog_id: &str) -> PreMerge {
    pre_merge_dir(dialog_id)
        .and_then(|d| std::fs::read(d.join("list.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// What changed from `old` to `new`, as a unified diff of a few dozen lines
/// at most, for the chat's model.
fn short_diff(old: &[u8], new: &[u8]) -> String {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(1);
    let Ok(mut patch) = git2::Patch::from_buffers(old, None, new, None, Some(&mut opts)) else { return String::new() };
    let Ok(buf) = patch.to_buf() else { return String::new() };
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with("diff --git") && !l.starts_with("--- ") && !l.starts_with("+++ ")).collect();
    let mut out = lines.iter().take(60).copied().collect::<Vec<_>>().join("\n");
    if lines.len() > 60 {
        out.push_str(&format!("\n… {} more lines", lines.len() - 60));
    }
    out
}

/// The project's newer changes merged into the chat's files: each file
/// both changed (cleanly, in `plan`) takes the merged result in the chat's
/// view, and is merged against the project's file as it is now. The chat's
/// own version is kept first (see [`undo_merge`]). Returns each file with
/// what the project changed in it.
pub fn take_merged(dialog_id: &str, plan: &super::land::Plan) -> Result<Vec<(String, String)>, String> {
    let files = plan.merged_files();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let dir = pre_merge_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut kept = load_pre_merge(dialog_id);
    let mut out = Vec::new();
    for (path, _, project) in &files {
        let s = sides(dialog_id, path)?;
        // The earliest version wins: undoing goes back to the chat's own.
        if !kept.files.contains_key(path) {
            let n = kept.files.len();
            let save = |what: &str, bytes: &Option<Vec<u8>>| -> Result<Option<String>, String> {
                let Some(b) = bytes else { return Ok(None) };
                let name = format!("{n}-{what}");
                std::fs::write(dir.join(&name), b).map_err(|e| e.to_string())?;
                Ok(Some(name))
            };
            let entry = (save("chat", &s.chat)?, save("base", &s.base)?);
            kept.files.insert(path.clone(), entry);
        }
        out.push((path.clone(), short_diff(s.base.as_deref().unwrap_or(&[]), project)));
    }
    let list = serde_json::to_vec(&kept).map_err(|e| e.to_string())?;
    crate::atomic_file::write_atomic(&dir.join("list.json"), &list).map_err(|e| e.to_string())?;
    with_fs(dialog_id, |fs| {
        for (path, merged, project) in &files {
            fs.rebase_file(path, Some(merged), Some(project)).map_err(|e| format!("{path}: {e:?}"))?;
        }
        Ok::<_, String>(())
    })??;
    Ok(out)
}

/// Files the project's changes were merged into, which can be taken back.
pub fn merged_in(dialog_id: &str) -> Vec<String> {
    load_pre_merge(dialog_id).files.into_keys().collect()
}

/// Takes the project's merged changes back out: each file is the chat's own
/// version again, merged against what it was before.
pub fn undo_merge(dialog_id: &str) -> Result<Vec<String>, String> {
    let kept = load_pre_merge(dialog_id);
    let dir = pre_merge_dir(dialog_id).ok_or("this chat has no isolated changes")?;
    let read = |name: &Option<String>| name.as_ref().and_then(|n| std::fs::read(dir.join(n)).ok());
    with_fs(dialog_id, |fs| {
        for (path, (chat, base)) in &kept.files {
            fs.rebase_file(path, read(chat).as_deref(), read(base).as_deref()).map_err(|e| format!("{path}: {e:?}"))?;
        }
        Ok::<_, String>(())
    })??;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(kept.files.into_keys().collect())
}

#[tauri::command]
pub async fn overlay_undo_merge(dialog_id: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || undo_merge(&dialog_id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn overlay_conflict_file(dialog_id: String, path: String) -> Result<crate::git_sync::ConflictFile, String> {
    tauri::async_runtime::spawn_blocking(move || conflict_file(&dialog_id, &path)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn overlay_conflict_resolve(
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<super::land::Conflict>, String> {
    tauri::async_runtime::spawn_blocking(move || resolve_conflict(&dialog_id, &path, content.map(String::into_bytes), side.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandedOver {
    pub files: Vec<String>,
    /// The message to send the chat (empty: nothing could be handed over).
    pub request: String,
}

/// Puts conflict markers in the chat's files for it to resolve; returns
/// the message that asks it to.
#[tauri::command]
pub async fn overlay_conflicts_to_chat(dialog_id: String) -> Result<HandedOver, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = mark_conflicts(&dialog_id)?;
        let request = if files.is_empty() { String::new() } else { resolve_request(&files) };
        Ok(HandedOver { files, request })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod conflict_tests {
    use super::*;
    use crate::vfs::backend::Backend;
    use crate::vfs::store::SetAttr;

    /// A chat whose change to a.txt conflicts with the project's, saved
    /// (not mounted): as between runs.
    fn conflicted(name: &str) -> (PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!("conflicts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        init(&root.join("data"));
        let id = format!("conflict-chat-{name}");
        let store = store_dir(&id).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("project.txt"), project.to_string_lossy().as_bytes()).unwrap();
        let fs = OverlayFs::open(&project, &store, DEFAULT_LIMIT).unwrap();
        let ino = fs.resolve("a.txt").unwrap();
        fs.setattr(ino, &SetAttr { size: Some(0), ..Default::default() }).unwrap();
        fs.write(ino, 0, b"CHAT\n2\n3\n4\n5\n").unwrap();
        fs.save();
        drop(fs);
        std::fs::write(project.join("a.txt"), "PROJECT\n2\n3\n4\n5\n").unwrap();
        let plan = super::super::land::plan(&project, &changes(&id), false);
        set_pending(&id, Some(&Pending { reason: "conflicts".into(), conflicts: plan.conflicts }));
        (root, project, id)
    }

    // One at a time: the data folder is the app's, one for all.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn a_conflict_resolved_in_the_resolver_lands() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("resolver");
        let file = conflict_file(&id, "a.txt").unwrap();
        assert_eq!(file.ours.as_deref(), Some("PROJECT\n2\n3\n4\n5\n"));
        assert_eq!(file.theirs.as_deref(), Some("CHAT\n2\n3\n4\n5\n"));
        assert!(file.segments.iter().any(|s| matches!(s, crate::merge::MergeSegment::Conflict { .. })));
        let left = resolve_conflict(&id, "a.txt", Some(b"PROJECT CHAT\n2\n3\n4\n5\n".to_vec()), None).unwrap();
        assert!(left.is_empty());
        assert!(pending(&id).is_none());
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "PROJECT CHAT\n2\n3\n4\n5\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_conflict_handed_to_the_chat_lands_once_its_markers_are_gone() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("chat");
        assert_eq!(mark_conflicts(&id).unwrap(), vec!["a.txt"]);
        let marked = String::from_utf8(sides(&id, "a.txt").unwrap().chat.unwrap()).unwrap();
        assert!(marked.starts_with(&format!("{MARK_PROJECT}\nPROJECT\n{MARK_BASE}\n1\n{MARK_SPLIT}\nCHAT\n{MARK_CHAT}\n2\n")), "{marked}");
        assert!(resolve_request(&["a.txt".into()]).contains("`a.txt`"));
        // Markers left: nothing lands, not even anyway.
        assert!(matches!(land_now(&id, true), Err(super::super::land::LandError::Failed(_))));
        assert_eq!(markers_left(&id), vec!["a.txt"]);
        // The chat resolves them.
        with_fs(&id, |fs| fs.rebase_file("a.txt", Some(b"PROJECT\nCHAT\n2\n3\n4\n5\n"), Some(b"PROJECT\n2\n3\n4\n5\n")))
            .unwrap()
            .unwrap();
        assert!(markers_left(&id).is_empty());
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "PROJECT\nCHAT\n2\n3\n4\n5\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merging_the_project_in_can_be_taken_back() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("undo-merge");
        // Not a conflict this time: the project changed another line.
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\nFIVE\n").unwrap();
        set_pending(&id, None);
        let plan = super::super::land::plan(&project, &changes(&id), false);
        assert!(plan.merged && plan.conflicts.is_empty());
        let taken = take_merged(&id, &plan).unwrap();
        assert_eq!(taken.len(), 1);
        assert!(taken[0].1.contains("+FIVE"), "the note shows what the project changed: {}", taken[0].1);
        assert_eq!(String::from_utf8(sides(&id, "a.txt").unwrap().chat.unwrap()).unwrap(), "CHAT\n2\n3\n4\nFIVE\n");
        assert_eq!(merged_in(&id), vec!["a.txt"]);
        undo_merge(&id).unwrap();
        let s = sides(&id, "a.txt").unwrap();
        assert_eq!(String::from_utf8(s.chat.unwrap()).unwrap(), "CHAT\n2\n3\n4\n5\n");
        assert_eq!(String::from_utf8(s.base.unwrap()).unwrap(), "1\n2\n3\n4\n5\n");
        assert!(merged_in(&id).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn chat_ids_name_one_folder() {
        assert!(valid_id("8f1c2d3e-aaaa"));
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "c:x"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(overlay_file("../..".into(), "a".into()).is_err());
        assert!(overlay_file("chat".into(), "/etc/passwd".into()).is_err());
    }

    #[test]
    fn one_chat_without_isolation_writes_at_a_time() {
        let dir = std::env::temp_dir().join(format!("direct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        claim_direct_writes(&dir, "chat-one").unwrap();
        claim_direct_writes(&dir, "chat-one").unwrap();
        let refused = claim_direct_writes(&dir, "chat-two").unwrap_err();
        assert!(refused.contains("chat-one"), "{refused}");
        release_direct_writes("chat-one");
        claim_direct_writes(&dir, "chat-two").unwrap();
        release_direct_writes("chat-two");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn busy_while_held_and_for_a_moment_after() {
        let dir = std::env::temp_dir().join(format!("busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_busy(&dir));
        let guard = hold_busy(&dir);
        assert!(is_busy(&dir));
        drop(guard);
        assert!(is_busy(&dir), "settling");
        BUSY.lock().unwrap().get_mut(&busy_key(&dir)).unwrap().0 = Instant::now() - QUIET;
        assert!(!is_busy(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod hub_tests {
    use super::*;
    use crate::vfs::backend::Backend;
    use crate::vfs::store::SetAttr;

    fn write(o: &ChatOverlay, rel: &str, content: &str) {
        let ino = o.fs.resolve(rel).unwrap();
        o.fs.setattr(ino, &SetAttr { size: Some(0), ..Default::default() }).unwrap();
        o.fs.write(ino, 0, content.as_bytes()).unwrap();
    }

    /// Two isolated chats on one project; one lands. Needs FUSE.
    #[test]
    #[ignore]
    fn a_landing_reaches_other_chats_at_once() {
        let root = std::env::temp_dir().join(format!("hub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        std::fs::write(project.join("b.txt"), "b\n").unwrap();
        init(&root.join("data"));
        let a = open("hub-chat-a", &project, Box::new(|_, _| {})).unwrap();
        let b = open("hub-chat-b", &project, Box::new(|_, _| {})).unwrap();
        // B looks at b.txt and changes line 1 of a.txt; A changes line 1 of
        // a.txt too, and b.txt.
        assert_eq!(std::fs::read_to_string(b.view.join("b.txt")).unwrap(), "b\n");
        write(&b, "a.txt", "B\n2\n3\n4\n5\n");
        write(&a, "a.txt", "A\n2\n3\n4\n5\n");
        write(&a, "b.txt", "changed by a\n");
        land_now("hub-chat-a", false).unwrap();
        // B sees A's b.txt at once, and knows its a.txt now conflicts.
        assert_eq!(std::fs::read_to_string(b.view.join("b.txt")).unwrap(), "changed by a\n");
        let pending = pending("hub-chat-b").unwrap();
        assert_eq!(pending.conflicts[0].path, "a.txt");
        let note = take_note("hub-chat-b").unwrap();
        assert!(note.contains("b.txt") && note.contains("on lines you changed too: a.txt"), "{note}");
        assert!(take_note("hub-chat-b").is_none(), "told once");
        discard("hub-chat-b").unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod bench {
    /// Rough timing through a chat's mount: `cargo test --release --lib
    /// overlay::chats::bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reading_through_the_mount() {
        let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let data = std::env::temp_dir().join(format!("bench-{}", std::process::id()));
        super::init(&data);
        let o = super::open("bench-chat", &project, Box::new(|_, _| {})).unwrap();
        let time = |what: &str, dir: &std::path::Path, cmd: &str| {
            let t = std::time::Instant::now();
            let out = std::process::Command::new("sh").args(["-c", cmd]).current_dir(dir).output().unwrap();
            eprintln!("{what:>8}: {:>6} ms  {}", t.elapsed().as_millis(), String::from_utf8_lossy(&out.stdout).trim());
        };
        for (name, cmd) in [
            ("grep", "grep -rc 'fn ' src-tauri/src | wc -l"),
            ("cat", "cat src-tauri/src/agent_run/*.rs | wc -c"),
            ("find", "find src src-tauri/src -type f | wc -l"),
            ("git", "git status --short | wc -l"),
        ] {
            time(&format!("{name} dir"), &project, cmd);
            time(&format!("{name} view"), &o.view, cmd);
        }
        super::discard("bench-chat").unwrap();
        let _ = std::fs::remove_dir_all(&data);
    }
}

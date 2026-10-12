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
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
        let shared = data_dir().map(|d| {
            d.join("overlays")
                .join("targets")
                .join(project_key(&self.project))
        });
        let dir = shared.unwrap_or_else(|| self.store.join("out").join("target"));
        vec![(
            "CARGO_TARGET_DIR".into(),
            dir.to_string_lossy().into_owned(),
        )]
    }

    /// How the chat's commands are kept out of the project: the project's
    /// path shows the chat's view to them (confine.rs). None where that
    /// isn't possible.
    pub fn confine(&self) -> Option<super::confine::Confine> {
        let c = super::confine::Confine {
            view: self.view.clone(),
            project: self.project.clone(),
        };
        super::confine::available(&c).then_some(c)
    }

    /// Why a command can't run in this chat as written, if it can't.
    /// Changes reach the project by landing, so git commands that change
    /// the repository are refused (reading it works: status, diff, log,
    /// show, blame). Where commands can't be confined, so is the project's
    /// own folder named by its full path, which would reach past the
    /// chat's view.
    pub fn command_problem(&self, command: &str) -> Option<String> {
        let parsed = crate::agent_run::permissions::parse_command(command);
        for segment in &parsed.segments {
            if let Some(what) = git_changes_repo(segment) {
                return Some(format!(
                    "Error: `git {what}` changes the repository, and this chat is isolated: its changes reach the project when they land, not through git. Read-only git (status, diff, log, show, blame, branch listing) works. Leave committing to the user."
                ));
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
        self.fs.close_handles();
        crate::file_guard::forget_under(&self.view);
        if let Some(m) = self.mount.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = m.unmount();
        }
    }
}

/// The git command a command segment runs, if it changes the repository
/// (and so has no place in an isolated chat). Git's own options before the
/// command (`-C dir`, `-c key=value`) are skipped; ones that point it at
/// another repository are refused outright.
fn git_changes_repo(segment: &[String]) -> Option<String> {
    const CHANGES_REPO: &[&str] = &[
        "checkout",
        "switch",
        "reset",
        "commit",
        "push",
        "pull",
        "merge",
        "rebase",
        "clean",
        "restore",
        "cherry-pick",
        "revert",
        "am",
        "apply",
        "rm",
        "mv",
        "fetch",
        "gc",
        "worktree",
        "init",
        "clone",
        "update-ref",
        "symbolic-ref",
        "notes",
        "submodule",
        "replace",
        "filter-branch",
        "filter-repo",
        "prune",
        "update-index",
        "commit-tree",
        "read-tree",
        "write-tree",
        "pack-refs",
        "repack",
        "bisect",
    ];
    let mut words = segment.iter().map(String::as_str);
    let program = words.next()?;
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    if name != "git" && name != "git.exe" {
        return None;
    }
    // Git's global options.
    let sub = loop {
        let w = words.next()?;
        match w {
            "-C" | "-c" | "--namespace" | "--config-env" => {
                words.next();
            }
            _ if w.starts_with("--git-dir") || w.starts_with("--work-tree") => {
                return Some(w.to_string())
            }
            _ if w.starts_with('-') => {}
            _ => break w,
        }
    };
    let args: Vec<&str> = words.collect();
    let has = |flags: &[&str]| {
        args.iter().any(|a| {
            flags
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
        })
    };
    let changes = match sub {
        s if CHANGES_REPO.contains(&s) => true,
        // Listing is fine; creating, deleting, moving or forcing isn't.
        "branch" => {
            has(&[
                "-d",
                "-D",
                "-f",
                "-m",
                "-M",
                "-c",
                "-C",
                "--delete",
                "--force",
                "--move",
                "--copy",
                "--set-upstream-to",
                "-u",
                "--unset-upstream",
                "--edit-description",
            ]) || args.iter().any(|a| !a.starts_with('-'))
                && !has(&[
                    "--list",
                    "-l",
                    "--contains",
                    "--no-contains",
                    "--merged",
                    "--no-merged",
                    "--points-at",
                ])
        }
        "tag" => {
            !(args.is_empty()
                || has(&[
                    "-l",
                    "--list",
                    "-n",
                    "--contains",
                    "--merged",
                    "--points-at",
                    "-v",
                    "--verify",
                ]))
        }
        "stash" => !matches!(args.first().copied(), Some("list") | Some("show")),
        "config" => !has(&[
            "--get",
            "--get-all",
            "--get-regexp",
            "--list",
            "-l",
            "--show-origin",
            "--show-scope",
        ]),
        "remote" => matches!(
            args.first().copied(),
            Some(
                "add"
                    | "remove"
                    | "rm"
                    | "rename"
                    | "set-url"
                    | "set-head"
                    | "set-branches"
                    | "prune"
                    | "update"
            )
        ),
        "reflog" => matches!(args.first().copied(), Some("expire" | "delete")),
        _ => false,
    };
    changes.then(|| sub.to_string())
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

static OPEN: LazyLock<Mutex<HashMap<String, Arc<ChatOverlay>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where overlays are kept: the app's data folder. Landings a crash
/// interrupted are finished first.
pub fn init(data_dir: &Path) {
    *DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(data_dir.to_path_buf());
    let finished = super::land::recover(data_dir);
    if !finished.is_empty() {
        eprintln!(
            "overlay: finished an interrupted landing: {}",
            finished.join(", ")
        );
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
    TOLD.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dialog_id.to_string())
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
    valid_id(dialog_id)
        .then(|| Some(data_dir()?.join("overlays").join(dialog_id)))
        .flatten()
}

fn mountpoint(dialog_id: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_dir()) {
        Some(dir) => PathBuf::from(dir).join("rovik-ide").join("chats"),
        None => std::env::temp_dir()
            .join(format!("rovik-ide-{}", unsafe { libc::getuid() }))
            .join("chats"),
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
    // One spelling of the folder (symlinks, `/private` on macOS, case on
    // Windows): the watcher and later runs may use another.
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let project = canonical.as_path();
    // Only this chat waits while its view is opened and mounted; the open
    // overlays stay free for everyone else.
    let gate = gate(dialog_id);
    let opening = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        if o.project == project {
            return Ok(o);
        }
        return Err("this chat's isolated changes belong to another project folder".into());
    }
    let store = store_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    // A chat stays with the project its changes were made in.
    let marker = store.join("project.txt");
    match std::fs::read_to_string(&marker) {
        Ok(saved) if !same_folder(Path::new(saved.trim()), project) => {
            return Err(format!(
                "this chat's isolated changes belong to {}",
                saved.trim()
            ));
        }
        Ok(_) => {}
        Err(_) => std::fs::write(&marker, project.to_string_lossy().as_bytes())
            .map_err(|e| e.to_string())?,
    }
    let fs = Arc::new(OverlayFs::open(project, &store, DEFAULT_LIMIT).map_err(|e| e.to_string())?);
    // Files a crash left behind: only here, where nothing else has the
    // chat's changes open (its gate is held).
    fs.sweep_orphans();
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
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dialog_id.to_string(), overlay.clone());
    // Before the check: it may mark conflicts, which takes the gate.
    drop(opening);
    // Closed until now (a restart): the project may have changed meanwhile.
    check_all(&overlay, dialog_id);
    Ok(overlay)
}

/// One lock per chat, held while its overlay is opened from its saved
/// changes (to mount it, or for one call): never two at once on the same
/// files. The open overlays' lock is only held to look one up.
static GATES: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

fn gate(dialog_id: &str) -> Arc<Mutex<()>> {
    GATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(dialog_id.to_string())
        .or_default()
        .clone()
}

/// The project a folder belongs to: the project of a chat's view, or the
/// folder itself.
pub fn project_for(path: &Path) -> PathBuf {
    let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.values()
        .find(|o| o.view == path)
        .map(|o| o.project.clone())
        .unwrap_or_else(|| path.to_path_buf())
}

/// The chat's overlay if it's open.
pub fn get(dialog_id: &str) -> Option<Arc<ChatOverlay>> {
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dialog_id)
        .cloned()
}

/// Whether the chat has an overlay, open or saved.
pub fn exists(dialog_id: &str) -> bool {
    get(dialog_id).is_some()
        || store_dir(dialog_id).is_some_and(|s| s.join("manifest.json").is_file())
}

/// Unmounts the chat's view; its changes stay saved.
pub fn close(dialog_id: &str) {
    let removed = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(dialog_id);
    if let Some(o) = removed {
        o.unmount();
    }
}

/// Throws the chat's changes away.
pub fn discard(dialog_id: &str) -> Result<(), String> {
    // The project, so the chats left open on it stop warning about this one.
    let project = project_of(dialog_id);
    close(dialog_id);
    drop_peer_state(dialog_id);
    forget_shown(dialog_id, None);
    if let Some(project) = project {
        refresh_project_peers(&project);
    }
    match store_dir(dialog_id) {
        // Gone already (discarded twice at once): nothing to do.
        Some(store) => match std::fs::remove_dir_all(&store) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
        None => Ok(()),
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
    let all: Vec<Arc<ChatOverlay>> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(_, o)| o)
        .collect();
    for o in all {
        o.unmount();
    }
}

// ---------------------------------------------------------------------------
// The project changing under open chats
// ---------------------------------------------------------------------------

/// Notes for each chat's model, waiting for its next tool result.
static NOTES: LazyLock<Mutex<HashMap<String, Notes>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct Notes {
    changed: std::collections::BTreeSet<String>,
    conflicts: std::collections::BTreeSet<String>,
    /// Conflicts handed to the chat: markers in these files.
    marked: std::collections::BTreeSet<String>,
    many: bool,
}

/// Chats with a run going: a conflict found for one goes to it.
static RUNNING: LazyLock<Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(Default::default);
/// Whether running chats resolve their conflicts (the user's setting).
static CHAT_RESOLVES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_running(dialog_id: &str, running: bool) {
    let mut set = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if running {
        set.insert(dialog_id.to_string());
    } else {
        set.remove(dialog_id);
    }
}

pub fn set_chat_resolves(on: bool) {
    CHAT_RESOLVES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A conflict found for a running chat goes to it: markers in its files,
/// and a note with its next tool result. Returns the files marked.
fn hand_to_running_chat(dialog_id: &str) -> Vec<String> {
    let running = RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(dialog_id);
    if !running || !CHAT_RESOLVES.load(std::sync::atomic::Ordering::Relaxed) {
        return Vec::new();
    }
    let marked = mark_conflicts(dialog_id).unwrap_or_default();
    if !marked.is_empty() {
        let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
        notes
            .entry(dialog_id.to_string())
            .or_default()
            .marked
            .extend(marked.iter().cloned());
    }
    marked
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
    // Open overlays keep the canonical spelling (`open`): one look at the
    // disk per event, not two per overlay.
    let canonical = project.canonicalize().ok();
    let overlays: Vec<Arc<ChatOverlay>> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|o| o.project == project || canonical.as_deref() == Some(o.project.as_path()))
        .cloned()
        .collect();
    if everything || abs.len() > MANY {
        project_busy(project);
        check_after_quiet(project);
    }
    let mut conflicts_found = Vec::new();
    for o in overlays {
        let id = o
            .store
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if everything || abs.len() > MANY {
            o.fs.project_changed_everywhere();
            NOTES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(id)
                .or_default()
                .many = true;
            continue;
        }
        let rels: Vec<String> = abs
            .iter()
            .filter_map(|p| {
                p.strip_prefix(project)
                    .or_else(|_| p.strip_prefix(&o.project))
                    .ok()
            })
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .filter(|r| !r.is_empty() && !r.starts_with(".git/") && r != ".git")
            .collect();
        let (seen, touched) = o.fs.project_changed(&rels);
        NOTES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.clone())
            .or_default()
            .changed
            .extend(seen);
        conflicts_found.extend(
            record_conflicts(&o, &id, &touched)
                .into_iter()
                .map(|p| (id.clone(), p)),
        );
    }
    conflicts_found
}

/// Of the files the chat changed (`candidates`), those that now conflict
/// with the project: added to its pending conflicts (kept with any found
/// before), told to its model, or handed to it if it's running. Returns
/// the ones left for the user.
fn record_conflicts(o: &ChatOverlay, id: &str, candidates: &[String]) -> Vec<String> {
    let conflicts: Vec<String> = candidates
        .iter()
        .filter(|rel| conflicts_now(o, rel))
        .cloned()
        .collect();
    if conflicts.is_empty() {
        return Vec::new();
    }
    NOTES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(id.to_string())
        .or_default()
        .conflicts
        .extend(conflicts.iter().cloned());
    {
        let lock = state_lock(id);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = pending(id).unwrap_or(Pending {
            reason: String::new(),
            conflicts: Vec::new(),
        });
        for p in &conflicts {
            if !pending.conflicts.iter().any(|c| &c.path == p) {
                pending.conflicts.push(super::land::Conflict {
                    path: p.clone(),
                    reason: "the chat and the project changed the same lines".into(),
                });
            }
        }
        let n = pending.conflicts.len();
        pending.reason = format!(
            "Won't land on its own: the project changed the same lines meanwhile ({n} file{}).",
            if n == 1 { "" } else { "s" }
        );
        set_pending(id, Some(&pending));
    }
    // A running chat takes its conflicts itself (unless the user said to
    // ask them); the rest are the user's.
    let marked = hand_to_running_chat(id);
    if !marked.is_empty() {
        let mut notes = NOTES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = notes.get_mut(id) {
            n.conflicts.retain(|p| !marked.contains(p));
        }
    }
    conflicts
        .into_iter()
        .filter(|p| !marked.contains(p))
        .collect()
}

/// Every file the chat changed, checked against the project as it is now:
/// after a branch switch settles, or when the chat's overlay opens again.
fn check_all(o: &ChatOverlay, id: &str) -> Vec<String> {
    let changed: Vec<String> =
        o.fs.changes()
            .into_iter()
            .filter(|c| {
                !c.by_product()
                    && matches!(
                        c,
                        OverlayChange::Write {
                            upper: Some(_),
                            base_copy: Some(_),
                            ..
                        }
                    )
            })
            .map(|c| c.path().to_string())
            .collect();
    record_conflicts(o, id, &changed)
}

/// Many files changed (a branch switch, a pull): once the project is still,
/// every open chat on it is checked for conflicts.
fn check_after_quiet(project: &Path) {
    static WAITING: LazyLock<Mutex<std::collections::HashSet<PathBuf>>> =
        LazyLock::new(Default::default);
    let key = busy_key(project);
    if !WAITING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone())
    {
        return;
    }
    let project = project.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("overlay-recheck".into())
        .spawn(move || {
            let started = Instant::now();
            while is_busy(&project) && started.elapsed() < Duration::from_secs(600) {
                std::thread::sleep(Duration::from_millis(250));
            }
            WAITING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            let overlays: Vec<(String, Arc<ChatOverlay>)> = OPEN
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, o)| same_folder(&o.project, &project))
                .map(|(id, o)| (id.clone(), o.clone()))
                .collect();
            for (id, o) in overlays {
                check_all(&o, &id);
            }
        });
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
    let holder = direct
        .entry(busy_key(project))
        .or_insert_with(|| dialog_id.to_string());
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
    DIRECT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, holder| holder != dialog_id);
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
    project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf())
}

/// Many files are changing in the project right now.
pub fn project_busy(project: &Path) {
    let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    busy.entry(busy_key(project))
        .or_insert((Instant::now(), 0))
        .0 = Instant::now();
}

/// Whether isolated chats on the project should wait: many files changed
/// in the last moments, or a git operation is still going.
pub fn is_busy(project: &Path) -> bool {
    let busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    busy.get(&busy_key(project))
        .is_some_and(|(at, held)| *held > 0 || at.elapsed() < QUIET)
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
    let Some((ours, base)) = o.fs.versions(rel) else {
        return false;
    };
    let (Ok(ours), Ok(base)) = (std::fs::read(ours), std::fs::read(base)) else {
        return false;
    };
    let theirs = std::fs::read(o.project.join(rel)).ok();
    match theirs {
        None => true,
        Some(t) if t == base || t == ours => false,
        Some(t) => !matches!(crate::merge::merge_bytes(&base, &ours, &t), Ok(Some(_))),
    }
}

/// What the chat's model should know about the project changing under it,
/// once: added to its next tool result.
pub fn take_note(dialog_id: &str) -> Option<String> {
    let notes = NOTES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(dialog_id)
        .unwrap_or_default();
    let mut parts = Vec::new();
    if notes.many {
        parts.push("Many files changed in the project meanwhile (a branch switch or a pull?). Files you haven't changed now show their new content; read again what you rely on.".to_string());
    }
    if !notes.changed.is_empty() {
        let list = notes
            .changed
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Changed in the project since you looked at them (another chat landed, or the user edited them): {list}. Your view shows the new content; read them again before relying on them."
        ));
    }
    if !notes.conflicts.is_empty() {
        let list = notes
            .conflicts
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Also changed in the project, on lines you changed too: {list}. Your version won't land without the user deciding; say so in your summary."
        ));
    }
    if !notes.marked.is_empty() {
        let files: Vec<String> = notes.marked.iter().cloned().collect();
        parts.push(resolve_request(&files).replace("\n\n", " "));
    }
    if let Some(peer) = fresh_peer_note(dialog_id) {
        parts.push(peer);
    }
    (!parts.is_empty()).then(|| format!("\n\nNote: {}", parts.join(" ")))
}

// ---------------------------------------------------------------------------
// Chats changing the same files as each other
// ---------------------------------------------------------------------------
//
// Landing serializes chats against the project: the second chat to land
// finds the first one's changes there and merges or conflicts then. That's
// late: both may have built on incompatible versions of the same lines. So
// after every tool call, the open overlays on a project are compared with
// each other, pair by pair, on the files both changed, with the same
// three-way merge landing uses. A clash warns both chats' models (once, with
// their next tool result) and shows in their bars. It never blocks landing:
// landing still decides against the project as it is then.

/// A file this chat and other open chats on the same project all changed.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerConflict {
    pub path: String,
    /// The other chats' ids.
    pub peers: Vec<String>,
    /// They changed the same lines (false: they disagree, but their bases
    /// had already diverged, so lines can't be told apart).
    pub same_lines: bool,
}

/// Live clashes: chat → path → (other chats, same lines).
type PeerRows = HashMap<String, BTreeMap<String, (BTreeSet<String>, bool)>>;
static PEERS: LazyLock<Mutex<PeerRows>> = LazyLock::new(Default::default);

/// Per project, each open chat's generation when its chats were last
/// compared.
static LAST_COMPARED: LazyLock<Mutex<HashMap<PathBuf, Vec<(String, u64)>>>> =
    LazyLock::new(Default::default);

/// Which (path, other chat) pairs a chat's model already heard about.
static TOLD_PEERS: LazyLock<Mutex<HashMap<String, BTreeSet<String>>>> =
    LazyLock::new(Default::default);

fn told_key(path: &str, peer: &str) -> String {
    format!("{path}\0{peer}")
}

/// One side of a comparison: what a chat has at a path.
struct Side {
    /// False: the chat deleted it.
    present: bool,
    content: Vec<u8>,
    /// What the project had when the chat first changed it (None: created).
    base: Option<Vec<u8>>,
}

/// What a chat with `changes` has at `rel`: None when it never touched it
/// (or its content can't be read: never warn on guesses).
fn side_of(changes: &[OverlayChange], project: &Path, rel: &str) -> Option<Side> {
    match changes.iter().rev().find(|c| c.path() == rel)? {
        OverlayChange::Delete { .. } => Some(Side {
            present: false,
            content: Vec::new(),
            base: None,
        }),
        OverlayChange::Write {
            kind,
            origin,
            upper,
            base_copy,
            target,
            ..
        } => match kind {
            crate::vfs::store::Kind::Dir => None,
            // A link is one value: same target, no clash.
            crate::vfs::store::Kind::Symlink => Some(Side {
                present: true,
                content: target.clone().unwrap_or_default().into_bytes(),
                base: None,
            }),
            crate::vfs::store::Kind::File => match (upper, origin) {
                (Some(file), _) => Some(Side {
                    present: true,
                    content: std::fs::read(file).ok()?,
                    base: base_copy.as_ref().and_then(|b| std::fs::read(b).ok()),
                }),
                // Moved or permissions only: the project's file it came from.
                (None, Some(o)) => Some(Side {
                    present: true,
                    content: std::fs::read(project.join(o)).ok()?,
                    base: None,
                }),
                (None, None) => None,
            },
        },
    }
}

/// Whether two chats' versions of one file clash: `Some(true)` on the same
/// lines, `Some(false)` when they disagree but lines can't be told apart,
/// `None` when both can land (same content, or a clean merge).
fn clash(a: &Side, b: &Side) -> Option<bool> {
    if !a.present && !b.present {
        return None;
    }
    if !a.present || !b.present {
        // Deleted on one side and changed on the other (created-then-
        // deleted is no clash).
        let other = if a.present { a } else { b };
        return other.base.is_some().then_some(true);
    }
    if a.content == b.content {
        return None;
    }
    match (&a.base, &b.base) {
        // Both created it, differently: only one can land.
        (None, None) => Some(true),
        (Some(x), Some(y)) if x == y => {
            match crate::merge::merge_bytes(x, &a.content, &b.content) {
                Ok(Some(_)) => None,
                Ok(None) => Some(true),
                Err(_) => Some(false),
            }
        }
        // The bases diverged (one chat took in a landing): landing still
        // decides, but lines can't be told apart here.
        _ => {
            let clean_against = |base: &[u8]| {
                matches!(
                    crate::merge::merge_bytes(base, &a.content, &b.content),
                    Ok(Some(_))
                )
            };
            let clean = a.base.as_deref().is_some_and(clean_against)
                && b.base.as_deref().is_some_and(clean_against);
            (!clean).then_some(false)
        }
    }
}

/// This chat's current clashes with other open chats.
pub fn peer_conflicts(dialog_id: &str) -> Vec<PeerConflict> {
    PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dialog_id)
        .map(|row| {
            row.iter()
                .map(|(path, (peers, same_lines))| PeerConflict {
                    path: path.clone(),
                    peers: peers.iter().cloned().collect(),
                    same_lines: *same_lines,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Compares every open overlay on `project` with the others and keeps the
/// result. Only files two chats both changed are merged.
fn refresh_project_peers(project: &Path) {
    let on_project: Vec<(String, Arc<ChatOverlay>)> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, o)| same_folder(&o.project, project))
        .map(|(id, o)| (id.clone(), o.clone()))
        .collect();
    // Nothing changed in any of its chats since the last comparison (a
    // read, a search): the clashes are as they were.
    let mut seen: Vec<(String, u64)> = on_project
        .iter()
        .map(|(id, o)| (id.clone(), o.fs.generation()))
        .collect();
    seen.sort();
    {
        let mut last = LAST_COMPARED.lock().unwrap_or_else(|e| e.into_inner());
        if last.get(project).is_some_and(|l| *l == seen) {
            return;
        }
        last.insert(project.to_path_buf(), seen);
    }
    let snapshots: Vec<(String, Arc<ChatOverlay>, Vec<OverlayChange>)> = on_project
        .into_iter()
        .map(|(id, o)| {
            let changed =
                o.fs.changes()
                    .into_iter()
                    .filter(|c| !c.by_product())
                    .collect();
            (id, o, changed)
        })
        .collect();
    let mut rows: PeerRows = snapshots
        .iter()
        .map(|(id, _, _)| (id.clone(), BTreeMap::new()))
        .collect();
    for i in 0..snapshots.len() {
        for j in (i + 1)..snapshots.len() {
            let (a_id, a_o, a_changes) = &snapshots[i];
            let (b_id, b_o, b_changes) = &snapshots[j];
            let b_paths: BTreeSet<&str> = b_changes.iter().map(|c| c.path()).collect();
            let shared: BTreeSet<&str> = a_changes
                .iter()
                .map(|c| c.path())
                .filter(|p| b_paths.contains(p))
                .collect();
            for rel in shared {
                let (Some(a), Some(b)) = (
                    side_of(a_changes, &a_o.project, rel),
                    side_of(b_changes, &b_o.project, rel),
                ) else {
                    continue;
                };
                if let Some(same_lines) = clash(&a, &b) {
                    for (me, other) in [(a_id, b_id), (b_id, a_id)] {
                        let cell = rows
                            .entry(me.clone())
                            .or_default()
                            .entry(rel.to_string())
                            .or_default();
                        cell.0.insert(other.clone());
                        cell.1 |= same_lines;
                    }
                }
            }
        }
    }
    let mut peers = PEERS.lock().unwrap_or_else(|e| e.into_inner());
    for (id, row) in &rows {
        peers.insert(id.clone(), row.clone());
    }
    drop(peers);
    // Forget what's no longer true, so a new clash is told again.
    let mut told = TOLD_PEERS.lock().unwrap_or_else(|e| e.into_inner());
    for (id, row) in &rows {
        let live: BTreeSet<String> = row
            .iter()
            .flat_map(|(path, (others, _))| others.iter().map(|o| told_key(path, o)))
            .collect();
        if let Some(entry) = told.get_mut(id) {
            entry.retain(|k| live.contains(k));
        }
    }
}

/// Drops a chat's clash state (discarded, or landed and gone).
fn drop_peer_state(dialog_id: &str) {
    PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(dialog_id);
    TOLD_PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(dialog_id);
    // The next comparison runs whatever the generations say.
    LAST_COMPARED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Compares this chat with the other open chats on its project (after each
/// of its tool calls); returns its clashes.
pub fn check_peer_conflicts(dialog_id: &str) -> Vec<PeerConflict> {
    if let Some(o) = get(dialog_id) {
        refresh_project_peers(&o.project);
    }
    peer_conflicts(dialog_id)
}

/// The clashes this chat's model hasn't heard about yet, as one note.
fn fresh_peer_note(dialog_id: &str) -> Option<String> {
    let current = PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dialog_id)
        .cloned()
        .unwrap_or_default();
    if current.is_empty() {
        return None;
    }
    let mut told = TOLD_PEERS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = told.entry(dialog_id.to_string()).or_default();
    let mut same = Vec::new();
    let mut file_level = Vec::new();
    for (path, (others, same_lines)) in &current {
        let new_here: Vec<&String> = others
            .iter()
            .filter(|o| !entry.contains(&told_key(path, o)))
            .collect();
        if new_here.is_empty() {
            continue;
        }
        for o in &new_here {
            entry.insert(told_key(path, o));
        }
        if *same_lines {
            same.push(format!("`{path}`"));
        } else {
            file_level.push(format!("`{path}`"));
        }
    }
    let mut parts = Vec::new();
    if !same.is_empty() {
        parts.push(format!(
            "Another isolated chat is changing the same lines you changed in: {}. Only one version can land as it is: keep your change to those lines as small as you can, and say in your summary that the user may have to pick.",
            same.join(", ")
        ));
    }
    if !file_level.is_empty() {
        parts.push(format!(
            "Another isolated chat also changed files you changed: {}. Check with the user before assuming yours wins.",
            file_level.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// What the chat changed, for the UI: landable changes only.
pub fn changes(dialog_id: &str) -> Vec<OverlayChange> {
    if let Some(o) = get(dialog_id) {
        return o
            .fs
            .changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect();
    }
    // Not open: read from its saved list, with its gate held so the chat's
    // own overlay isn't opened on the same files meanwhile.
    let gate = gate(dialog_id);
    let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        return o
            .fs
            .changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect();
    }
    let (Some(store), Some(project)) = (store_dir(dialog_id), saved_project(dialog_id)) else {
        return Vec::new();
    };
    match OverlayFs::open(&project, &store, DEFAULT_LIMIT) {
        Ok(fs) => fs
            .changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn git_commands_that_change_the_repository() {
        let check = |cmd: &str| {
            let parsed = crate::agent_run::permissions::parse_command(cmd);
            parsed
                .segments
                .iter()
                .find_map(|s| super::git_changes_repo(s))
        };
        for bad in [
            "git commit -am x",
            "git -C /p commit -m x",
            "git -c user.name=x commit",
            "git --git-dir=/p/.git log",
            "git branch -D main",
            "git branch new-feature",
            "git update-ref refs/heads/x HEAD",
            "git config user.name x",
            "git tag v1",
            "git stash",
            "git remote add o url",
            "/usr/bin/git checkout main",
        ] {
            assert!(check(bad).is_some(), "{bad}");
        }
        for ok in [
            "git status",
            "git -C sub log --oneline",
            "git branch",
            "git branch -a",
            "git branch --list 'feat*'",
            "git tag",
            "git tag -l",
            "git stash list",
            "git config --get user.name",
            "git remote -v",
            "git diff HEAD~1",
            "git show HEAD:src/a.rs",
        ] {
            assert!(check(ok).is_none(), "{ok}");
        }
    }

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
    /// Files other open chats on the project are changing too, on the
    /// same lines (as of their last tool calls).
    pub peer_conflicts: Vec<PeerConflict>,
    pub files: Vec<ChangedFile>,
    pub used: u64,
    pub limit: u64,
    /// Build output and dependencies in the chat's view (not landed).
    pub by_products: u64,
}

fn summarize(changes: &[OverlayChange]) -> Vec<ChangedFile> {
    let mut out = Vec::new();
    for c in changes {
        match c {
            OverlayChange::Delete { path, .. } => {
                // A delete followed by a write at the same path is a replacement.
                if !changes
                    .iter()
                    .any(|w| matches!(w, OverlayChange::Write { path: p, .. } if p == path))
                {
                    out.push(ChangedFile {
                        path: path.clone(),
                        change: "deleted",
                        from: None,
                    });
                }
            }
            OverlayChange::Write {
                path,
                kind,
                origin,
                upper,
                perm_changed,
                ..
            } => {
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
                out.push(ChangedFile {
                    path: path.clone(),
                    change,
                    from,
                });
            }
        }
    }
    out
}

/// Polled while a run goes: off the UI thread, it reads the overlay.
#[tauri::command]
pub async fn overlay_status(dialog_id: String) -> Result<OverlayStatus, String> {
    tauri::async_runtime::spawn_blocking(move || status(dialog_id))
        .await
        .map_err(|e| e.to_string())
}

fn status(dialog_id: String) -> OverlayStatus {
    let isolated = exists(&dialog_id);
    let open = get(&dialog_id);
    let (used, limit) = open
        .as_ref()
        .map(|o| o.fs.used())
        .unwrap_or((0, DEFAULT_LIMIT));
    let by_products = open.as_ref().map(|o| o.fs.by_product_bytes()).unwrap_or(0);
    OverlayStatus {
        isolated,
        pending: if isolated { pending(&dialog_id) } else { None },
        last_landing: last_landing(&dialog_id),
        view: open.map(|o| o.view.to_string_lossy().into_owned()),
        merged_in: if isolated {
            merged_in(&dialog_id)
        } else {
            Vec::new()
        },
        peer_conflicts: if isolated {
            peer_conflicts(&dialog_id)
        } else {
            Vec::new()
        },
        files: if isolated {
            summarize(&changes(&dialog_id))
        } else {
            Vec::new()
        },
        used,
        limit,
        by_products,
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
pub async fn overlay_file(dialog_id: String, path: String) -> Result<FileVersions, String> {
    tauri::async_runtime::spawn_blocking(move || file_versions(dialog_id, path))
        .await
        .map_err(|e| e.to_string())?
}

fn file_versions(dialog_id: String, path: String) -> Result<FileVersions, String> {
    check_rel(&path)?;
    let changes = changes(&dialog_id);
    // Only the chat's own changes: not a way to read any file.
    if !changes.iter().any(|c| c.path() == path) {
        return Err(format!("{path}: this chat didn't change it"));
    }
    let store_project =
        store_dir(&dialog_id).and_then(|s| std::fs::read_to_string(s.join("project.txt")).ok());
    let project_dir = get(&dialog_id)
        .map(|o| o.project.clone())
        .or(store_project.map(|p| PathBuf::from(p.trim())));
    let project_dir = project_dir.ok_or("this chat has no isolated changes")?;
    let in_project = |rel: &str| std::fs::read_to_string(project_dir.join(rel)).ok();
    let chat = changes
        .iter()
        .rev()
        .find(|c| c.path() == path)
        .and_then(|c| match c {
            OverlayChange::Write {
                upper: Some(file), ..
            } => std::fs::read_to_string(file).ok(),
            OverlayChange::Write {
                origin: Some(o), ..
            } => in_project(o),
            _ => None,
        });
    Ok(FileVersions {
        project: in_project(&path),
        chat,
    })
}

/// Throws away the chat's changes (all of them).
/// Refused while the chat's run is going: its files would go from under it.
fn not_running(agent: &crate::agent_run::AgentState, dialog_id: &str) -> Result<(), String> {
    if agent.is_running(dialog_id) {
        return Err("this chat's run is still going: stop it first".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn overlay_discard(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
) -> Result<(), String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || discard(&dialog_id))
        .await
        .map_err(|e| e.to_string())?
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
    let Some(store) = store_dir(dialog_id) else {
        return;
    };
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
    get(dialog_id).map(|o| o.project.clone()).or_else(|| {
        store_dir(dialog_id)
            .and_then(|s| std::fs::read_to_string(s.join("project.txt")).ok())
            .map(|p| PathBuf::from(p.trim()))
    })
}

/// Lands the chat's changes now. `keep_chat`: on conflicts, the chat's
/// version wins ("Land anyway"). Landed (or nothing to land): the overlay
/// is gone, and the chat's next run starts from the project as it is.
pub fn land_now(
    dialog_id: &str,
    keep_chat: bool,
) -> Result<Option<super::land::Landed>, super::land::LandError> {
    land_now_with(dialog_id, keep_chat, true)
}

/// [`land_now`]; `allow_merge` false: refused when the project changed a
/// file the chat changed since its checks ran (see `land::land_checked`).
pub fn land_now_with(
    dialog_id: &str,
    keep_chat: bool,
    allow_merge: bool,
) -> Result<Option<super::land::Landed>, super::land::LandError> {
    use super::land::LandError;
    let data = data_dir()
        .ok_or_else(|| LandError::Failed("the app's data folder isn't known yet".into()))?;
    let project = project_of(dialog_id)
        .ok_or_else(|| LandError::Failed("this chat has no isolated changes".into()))?;
    if let Some(o) = get(dialog_id) {
        o.fs.flush();
    }
    // Conflicts handed to the chat land once it has resolved them.
    let left = markers_left(dialog_id);
    if !left.is_empty() {
        let e = LandError::Failed(format!("conflict markers are still in {}", left.join(", ")));
        set_pending(
            dialog_id,
            Some(&Pending {
                reason: format!("Not landed: {e}."),
                conflicts: Vec::new(),
            }),
        );
        return Err(e);
    }
    let changes = changes(dialog_id);
    // Land anyway takes the chat's version over the conflicts the user was
    // shown (the bar's list, which the confirmation named), not over one
    // the project caused since: that one is reported instead.
    let known: Vec<super::land::Conflict> = if keep_chat {
        pending(dialog_id).map(|p| p.conflicts).unwrap_or_default()
    } else {
        Vec::new()
    };
    let known_paths: Vec<String> = known.iter().map(|c| c.path.clone()).collect();
    match super::land::land_checked(
        &data,
        dialog_id,
        &project,
        &changes,
        keep_chat,
        keep_chat.then_some(known_paths.as_slice()),
        allow_merge,
    ) {
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
            let mut conflicts = match &e {
                LandError::Conflicts(c) => c.clone(),
                _ => Vec::new(),
            };
            // The new conflicts are the error's; the bar keeps showing the
            // ones the user already knew about with them.
            if matches!(e, LandError::Conflicts(_)) {
                for k in known {
                    if !conflicts.iter().any(|c| c.path == k.path) {
                        conflicts.push(k);
                    }
                }
            }
            set_pending(
                dialog_id,
                Some(&Pending {
                    reason: e.to_string(),
                    conflicts,
                }),
            );
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
pub async fn overlay_land(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
    keep_chat: bool,
) -> Result<LandResult, String> {
    not_running(&agent, &dialog_id)?;
    Ok(land_command(dialog_id, keep_chat).await)
}

async fn land_command(dialog_id: String, keep_chat: bool) -> LandResult {
    let result =
        tauri::async_runtime::spawn_blocking(move || land_now(&dialog_id, keep_chat)).await;
    match result {
        Ok(Ok(landed)) => LandResult {
            landed,
            conflicts: Vec::new(),
            error: None,
        },
        Ok(Err(super::land::LandError::Conflicts(c))) => LandResult {
            landed: None,
            conflicts: c,
            error: None,
        },
        Ok(Err(e)) => LandResult {
            landed: None,
            conflicts: Vec::new(),
            error: Some(e.to_string()),
        },
        Err(e) => LandResult {
            landed: None,
            conflicts: Vec::new(),
            error: Some(e.to_string()),
        },
    }
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since. Refused while the chat's run is going, like landing.
#[tauri::command]
pub async fn overlay_undo(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
    id: String,
) -> Result<Vec<super::land::Conflict>, String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        if !valid_id(&dialog_id) {
            return Err("bad chat id".to_string());
        }
        let data = data_dir().ok_or("the app's data folder isn't known yet")?;
        let project = super::land::project_of(&data, &dialog_id).ok_or("no landing to undo")?;
        let paths = super::land::latest(&data, &dialog_id)
            .filter(|(l, _, _)| *l == id)
            .map(|(_, p, _)| p)
            .unwrap_or_default();
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

/// One lock per chat around its conflict state (pending conflicts, markers,
/// kept versions): the project's watcher, a landing in another chat and the
/// user's commands all change it, from different threads.
static STATE_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn state_lock(dialog_id: &str) -> Arc<Mutex<()>> {
    STATE_LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(dialog_id.to_string())
        .or_default()
        .clone()
}

/// The project a chat's saved changes belong to, from its folder alone
/// (without looking at the open overlays).
fn saved_project(dialog_id: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(store_dir(dialog_id)?.join("project.txt")).ok()?;
    Some(PathBuf::from(text.trim()))
}

/// The chat's overlay, open or (between runs, after a restart) opened from
/// its saved changes for this one call. Its gate stays held while a saved
/// one is used, so the chat's own overlay isn't opened on the same files
/// meanwhile: `f` mustn't open this chat's overlay.
fn with_fs<T>(dialog_id: &str, f: impl FnOnce(&OverlayFs) -> T) -> Result<T, String> {
    if let Some(o) = get(dialog_id) {
        return Ok(f(&o.fs));
    }
    let gate = gate(dialog_id);
    let held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        drop(held);
        return Ok(f(&o.fs));
    }
    let store = store_dir(dialog_id)
        .filter(|s| s.join("manifest.json").is_file())
        .ok_or("this chat has no isolated changes")?;
    let project = saved_project(dialog_id).ok_or("this chat has no isolated changes")?;
    let fs = OverlayFs::open(&project, &store, DEFAULT_LIMIT).map_err(|e| e.to_string())?;
    let out = f(&fs);
    fs.flush();
    drop(held);
    Ok(out)
}

fn check_rel(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.split(['/', '\\']).any(|p| p == "..") {
        return Err(format!("{path}: not a path in the project"));
    }
    Ok(())
}

fn hex_hash(bytes: &[u8]) -> String {
    crate::file_guard::hash(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    // Handed to the chat with markers: the conflict is between the chat's
    // own version (kept) and the project's.
    if marked(dialog_id).iter().any(|m| m == path) {
        if let Some((chat, base)) = kept_versions(dialog_id, path) {
            return Ok(Sides {
                base,
                chat,
                project: std::fs::read(project_dir.join(path)).ok(),
            });
        }
    }
    let change = changes(dialog_id)
        .into_iter()
        .rev()
        .find(|c| c.path() == path)
        .ok_or_else(|| format!("{path}: this chat didn't change it"))?;
    let (chat, base) = match &change {
        OverlayChange::Write {
            upper: Some(file),
            base_copy,
            ..
        } => (
            std::fs::read(file).ok(),
            base_copy.as_ref().and_then(|b| std::fs::read(b).ok()),
        ),
        OverlayChange::Write {
            origin: Some(o), ..
        } => (std::fs::read(project_dir.join(o)).ok(), None),
        _ => (None, None),
    };
    Ok(Sides {
        base,
        chat,
        project: std::fs::read(project_dir.join(path)).ok(),
    })
}

/// The chat's version of `path` as it is now in its view.
fn current_version(dialog_id: &str, path: &str) -> Option<Vec<u8>> {
    let project_dir = project_of(dialog_id)?;
    match changes(dialog_id)
        .into_iter()
        .rev()
        .find(|c| c.path() == path)?
    {
        OverlayChange::Write {
            upper: Some(file), ..
        } => std::fs::read(file).ok(),
        OverlayChange::Write {
            origin: Some(o), ..
        } => std::fs::read(project_dir.join(o)).ok(),
        _ => None,
    }
}

/// The project's version of a conflicted file as the resolver last showed
/// it, per chat and path (None: not in the project then). A resolution is
/// made against it, so it's what the resolved file is merged against when
/// landing: if the project changed the file again meanwhile, that change
/// is merged in or reported as a new conflict, not dropped.
static SHOWN: LazyLock<Mutex<HashMap<String, HashMap<String, Option<Vec<u8>>>>>> =
    LazyLock::new(Default::default);

fn note_shown(dialog_id: &str, path: &str, project: Option<&[u8]>) {
    SHOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(dialog_id.to_string())
        .or_default()
        .insert(path.to_string(), project.map(<[u8]>::to_vec));
}

/// The project version last shown for `path`, forgotten once taken.
fn take_shown(dialog_id: &str, path: &str) -> Option<Option<Vec<u8>>> {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    let chat = shown.get_mut(dialog_id)?;
    let taken = chat.remove(path);
    if chat.is_empty() {
        shown.remove(dialog_id);
    }
    taken
}

/// Forgets what was shown for `path` (None: for every file of the chat).
fn forget_shown(dialog_id: &str, path: Option<&str>) {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    match path {
        Some(p) => {
            if let Some(chat) = shown.get_mut(dialog_id) {
                chat.remove(p);
                if chat.is_empty() {
                    shown.remove(dialog_id);
                }
            }
        }
        None => {
            shown.remove(dialog_id);
        }
    }
}

/// A conflicted file for the conflict resolver: the project's version on the
/// left ("ours", it's where the chat's changes go), the chat's on the right.
/// The project's version shown is what a resolution of the file is then
/// merged against ([`resolve_conflict`]).
pub fn conflict_file(dialog_id: &str, path: &str) -> Result<crate::git_sync::ConflictFile, String> {
    let s = sides(dialog_id, path)?;
    note_shown(dialog_id, path, s.project.as_deref());
    Ok(conflict_file_of(path, &s))
}

/// [`conflict_file`], from the three versions as read.
fn conflict_file_of(path: &str, s: &Sides) -> crate::git_sync::ConflictFile {
    use crate::merge::{is_binary, merge3_raw, parse_merged, MergeSegment};
    let binary = [&s.base, &s.chat, &s.project]
        .iter()
        .any(|b| b.as_deref().is_some_and(is_binary));
    let text = |b: &Option<Vec<u8>>| {
        b.as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    };
    let segments = if binary {
        Vec::new()
    } else if let (Some(p), Some(c)) = (&s.project, &s.chat) {
        // Refused by libgit2 (not for text, but just in case): the whole
        // file as one conflict.
        match merge3_raw(s.base.as_deref().unwrap_or(&[]), p, c) {
            Ok(merged) => parse_merged(&String::from_utf8_lossy(&merged.content)),
            Err(_) => vec![MergeSegment::Conflict {
                ours: text(&s.project).unwrap_or_default(),
                base: text(&s.base).unwrap_or_default(),
                theirs: text(&s.chat).unwrap_or_default(),
            }],
        }
    } else {
        vec![MergeSegment::Conflict {
            ours: text(&s.project).unwrap_or_default(),
            base: text(&s.base).unwrap_or_default(),
            theirs: text(&s.chat).unwrap_or_default(),
        }]
    };
    crate::git_sync::ConflictFile {
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
    }
}

/// Settles a conflicted file: the chat's version becomes `content` (None:
/// deleted), or the whole of one `side` ("ours": the project's, "theirs":
/// the chat's). It's merged against the project's version the resolver
/// showed (the one the resolution was made from), so it lands as it is;
/// if the project changed the file again since, the landing merges that
/// change in, or reports a new conflict, rather than drop it. Taking the
/// project's side takes its file as it is now. Returns the conflicts left.
pub fn resolve_conflict(
    dialog_id: &str,
    path: &str,
    content: Option<Vec<u8>>,
    side: Option<&str>,
) -> Result<Vec<super::land::Conflict>, String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let s = sides(dialog_id, path)?;
    // Not shown first (nothing to have resolved against): the project now.
    let shown = take_shown(dialog_id, path).unwrap_or_else(|| s.project.clone());
    let (content, base) = match side {
        Some("ours") => (s.project.clone(), s.project.clone()),
        Some("theirs") => (s.chat.clone(), shown),
        Some(other) => return Err(format!("unknown side: {other}")),
        None => (content, shown),
    };
    with_fs(dialog_id, |fs| {
        fs.rebase_file(path, content.as_deref(), base.as_deref())
    })?
    .map_err(|e| format!("{path}: {e:?}"))?;
    // Settled: no markers to wait for, no older version to go back to.
    set_marked(
        dialog_id,
        marked(dialog_id)
            .into_iter()
            .filter(|m| m != path)
            .collect(),
    );
    forget_kept(dialog_id, path);
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
    markers_file(dialog_id)
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn set_marked(dialog_id: &str, mut files: Vec<String>) {
    let Some(f) = markers_file(dialog_id) else {
        return;
    };
    files.sort();
    files.dedup();
    if files.is_empty() {
        let _ = std::fs::remove_file(f);
    } else if f.parent().is_some_and(|d| d.is_dir()) {
        let _ =
            crate::atomic_file::write_atomic(&f, &serde_json::to_vec(&files).unwrap_or_default());
    }
}

/// Hands the conflicts to the chat: each text file the project and the chat
/// both changed gets git-style conflict markers in the chat's version, and
/// is merged against the project's file as it is now. Files already handed
/// over are left as they are (the chat may be resolving them). Returns the
/// files newly marked; the rest (binary, or deleted on one side) stay for
/// the user.
pub fn mark_conflicts(dialog_id: &str) -> Result<Vec<String>, String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut p) = pending(dialog_id) else {
        return Ok(Vec::new());
    };
    let mut all = marked(dialog_id);
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for c in &p.conflicts {
        if all.contains(&c.path) {
            continue;
        }
        match mark_one(dialog_id, &c.path) {
            Ok(true) => {
                done.push(c.path.clone());
                // Tracked at once: markers nobody knows about could land.
                all.push(c.path.clone());
                set_marked(dialog_id, all.clone());
            }
            Ok(false) => {}
            Err(e) => failed.push(format!("{}: {e}", c.path)),
        }
    }
    if done.is_empty() && !failed.is_empty() {
        return Err(failed.join("; "));
    }
    if !failed.is_empty() {
        eprintln!(
            "overlay: couldn't mark some conflicts: {}",
            failed.join("; ")
        );
    }
    if !all.is_empty() {
        p.reason = format!(
            "With the chat: it's resolving the conflict markers in {}. Resolve… settles them here instead; Undo the merge takes them out.",
            all.join(", ")
        );
        set_pending(dialog_id, Some(&p));
    }
    Ok(done)
}

/// Puts conflict markers in one file; false when it can't have them.
fn mark_one(dialog_id: &str, path: &str) -> Result<bool, String> {
    // The three versions read once: the markers show the project's version
    // the file is then merged against. Whatever the resolver showed before
    // is superseded.
    let s = sides(dialog_id, path)?;
    forget_shown(dialog_id, Some(path));
    let file = conflict_file_of(path, &s);
    if file.binary || file.ours.is_none() || file.theirs.is_none() {
        return Ok(false);
    }
    let mut text = String::new();
    for seg in &file.segments {
        match seg {
            crate::merge::MergeSegment::Common { text: t } => text.push_str(t),
            crate::merge::MergeSegment::Conflict { ours, base, theirs } => {
                for (mark, part) in [
                    (MARK_PROJECT, ours),
                    (MARK_BASE, base),
                    (MARK_SPLIT, theirs),
                ] {
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
    // The chat's own version is kept: Resolve… and Undo the merge use it.
    keep_versions(dialog_id, path, &s)?;
    with_fs(dialog_id, |fs| {
        fs.rebase_file(path, Some(text.as_bytes()), s.project.as_deref())
    })?
    .map_err(|e| format!("{e:?}"))?;
    note_written(dialog_id, path, text.as_bytes());
    Ok(true)
}

/// Files the chat was asked to resolve that still have its markers. Ones
/// it resolved are forgotten, with their kept versions: undoing would
/// throw its resolution away.
pub fn markers_left(dialog_id: &str) -> Vec<String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let all = marked(dialog_id);
    if all.is_empty() {
        return all;
    }
    let (left, resolved): (Vec<String>, Vec<String>) = all.into_iter().partition(|path| {
        current_version(dialog_id, path).is_some_and(|c| {
            String::from_utf8_lossy(&c)
                .lines()
                .any(|l| l.starts_with(MARK_PROJECT) || l.starts_with(MARK_CHAT))
        })
    });
    for path in &resolved {
        forget_kept(dialog_id, path);
    }
    set_marked(dialog_id, left.clone());
    left
}

/// What the chat is asked, with the conflicts marked in its files.
pub fn resolve_request(files: &[String]) -> String {
    format!(
        "Your changes conflict with changes made in the project meanwhile, in: {}.\n\nEach of these files now has conflict markers: `{MARK_PROJECT}` starts the project's version, `{MARK_BASE}` the lines before either change, `{MARK_SPLIT}` your version, and `{MARK_CHAT}` ends the conflict. Read each file, combine both changes so neither is lost (keep the project's change and redo yours on top of it), remove every marker, and make sure the result still works. Your changes land once no markers are left.",
        files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
    )
}

/// A file's version kept before a merge or markers replaced it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct Kept {
    /// The chat's version and what it was merged against, as files in the
    /// pre-merge folder; None: not there.
    chat: Option<String>,
    base: Option<String>,
    /// The hash of what replaced it (the merged result, or the markers):
    /// a file that no longer has it was changed since, and stays.
    #[serde(default)]
    written: Option<String>,
}

/// The chat's own versions from before the project's changes were merged
/// in, kept until its changes land (or the user takes them back).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct PreMerge {
    files: BTreeMap<String, Kept>,
    /// Names files uniquely: never reused while the list exists.
    #[serde(default)]
    next: u64,
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

/// Saves the list; an empty one goes with its folder.
fn save_pre_merge(dialog_id: &str, kept: &PreMerge) -> Result<(), String> {
    let dir = pre_merge_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    if kept.files.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let list = serde_json::to_vec(kept).map_err(|e| e.to_string())?;
    crate::atomic_file::write_atomic(&dir.join("list.json"), &list).map_err(|e| e.to_string())
}

/// What changed from `old` to `new`, as a unified diff of a few dozen lines
/// at most, for the chat's model.
fn short_diff(old: &[u8], new: &[u8]) -> String {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(1);
    let Ok(mut patch) = git2::Patch::from_buffers(old, None, new, None, Some(&mut opts)) else {
        return String::new();
    };
    let Ok(buf) = patch.to_buf() else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            !l.starts_with("diff --git") && !l.starts_with("--- ") && !l.starts_with("+++ ")
        })
        .collect();
    let mut out = lines
        .iter()
        .take(60)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
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
pub fn take_merged(
    dialog_id: &str,
    plan: &super::land::Plan,
) -> Result<Vec<(String, String)>, String> {
    let files = plan.merged_files();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (path, merged, project) in &files {
        let s = sides(dialog_id, path)?;
        keep_versions(dialog_id, path, &s)?;
        with_fs(dialog_id, |fs| {
            fs.rebase_file(path, Some(merged), Some(project))
        })?
        .map_err(|e| format!("{path}: {e:?}"))?;
        note_written(dialog_id, path, merged);
        out.push((
            path.clone(),
            short_diff(s.base.as_deref().unwrap_or(&[]), project),
        ));
    }
    Ok(out)
}

/// Keeps the chat's version of `path` (and its base) before it's replaced
/// by a merge or by conflict markers; the earliest kept version stays.
/// The chat's state lock is held.
fn keep_versions(dialog_id: &str, path: &str, s: &Sides) -> Result<(), String> {
    let dir = pre_merge_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut kept = load_pre_merge(dialog_id);
    if kept.files.contains_key(path) {
        return Ok(());
    }
    let n = kept.next;
    kept.next += 1;
    let save = |what: &str, bytes: &Option<Vec<u8>>| -> Result<Option<String>, String> {
        let Some(b) = bytes else { return Ok(None) };
        let name = format!("{n}-{what}");
        std::fs::write(dir.join(&name), b).map_err(|e| e.to_string())?;
        Ok(Some(name))
    };
    let entry = Kept {
        chat: save("chat", &s.chat)?,
        base: save("base", &s.base)?,
        written: None,
    };
    kept.files.insert(path.to_string(), entry);
    save_pre_merge(dialog_id, &kept)
}

/// Records what replaced a kept file (see [`Kept::written`]).
fn note_written(dialog_id: &str, path: &str, bytes: &[u8]) {
    let mut kept = load_pre_merge(dialog_id);
    if let Some(entry) = kept.files.get_mut(path) {
        entry.written = Some(hex_hash(bytes));
        let _ = save_pre_merge(dialog_id, &kept);
    }
}

/// The kept version of `path`, if there is one: (the chat's, its base).
fn kept_versions(dialog_id: &str, path: &str) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    let dir = pre_merge_dir(dialog_id)?;
    let entry = load_pre_merge(dialog_id).files.remove(path)?;
    let read = |name: Option<String>| name.and_then(|n| std::fs::read(dir.join(n)).ok());
    Some((read(entry.chat), read(entry.base)))
}

/// Forgets the kept version of `path` (settled for good), with its files.
fn forget_kept(dialog_id: &str, path: &str) {
    let Some(dir) = pre_merge_dir(dialog_id) else {
        return;
    };
    let mut kept = load_pre_merge(dialog_id);
    if let Some(entry) = kept.files.remove(path) {
        for name in [entry.chat, entry.base].into_iter().flatten() {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = save_pre_merge(dialog_id, &kept);
    }
}

/// Files the project's changes were merged into, which can be taken back.
pub fn merged_in(dialog_id: &str) -> Vec<String> {
    load_pre_merge(dialog_id).files.into_keys().collect()
}

#[derive(serde::Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct UndoneMerge {
    /// Back to the chat's own version.
    pub restored: Vec<String>,
    /// Changed since the merge (by the chat, or the user): left as they are.
    pub changed_since: Vec<String>,
}

/// Takes the project's merged changes (and conflict markers) back out of
/// the chat's files (`only`: of these files): each is the chat's own
/// version again, merged against what it was before. A file changed since
/// the merge stays as it is: the work done on it isn't thrown away.
pub fn undo_merge(dialog_id: &str, only: Option<&[String]>) -> Result<UndoneMerge, String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept = load_pre_merge(dialog_id);
    let dir = pre_merge_dir(dialog_id).ok_or("this chat has no isolated changes")?;
    let read = |name: &Option<String>| name.as_ref().and_then(|n| std::fs::read(dir.join(n)).ok());
    let targets: Vec<String> = kept
        .files
        .keys()
        .filter(|p| only.is_none_or(|o| o.iter().any(|x| x == *p)))
        .cloned()
        .collect();
    let mut done = UndoneMerge::default();
    for path in targets {
        let entry = kept.files[&path].clone();
        let unchanged = match &entry.written {
            Some(w) => current_version(dialog_id, &path).is_some_and(|c| hex_hash(&c) == *w),
            None => true,
        };
        if !unchanged {
            done.changed_since.push(path);
            continue;
        }
        with_fs(dialog_id, |fs| {
            fs.rebase_file(
                &path,
                read(&entry.chat).as_deref(),
                read(&entry.base).as_deref(),
            )
        })?
        .map_err(|e| format!("{path}: {e:?}"))?;
        for name in [&entry.chat, &entry.base].into_iter().flatten() {
            let _ = std::fs::remove_file(dir.join(name));
        }
        kept.files.remove(&path);
        done.restored.push(path);
    }
    save_pre_merge(dialog_id, &kept)?;
    // Markers taken out with them: those conflicts are back.
    set_marked(
        dialog_id,
        marked(dialog_id)
            .into_iter()
            .filter(|m| !done.restored.contains(m))
            .collect(),
    );
    if let Some(project) = project_of(dialog_id) {
        let plan = super::land::plan(&project, &changes(dialog_id), false);
        if plan.conflicts.is_empty() {
            set_pending(dialog_id, None);
        } else {
            let n = plan.conflicts.len();
            let reason = format!(
                "Not landed: {n} file{} changed in the project too, on the same lines.",
                if n == 1 { "" } else { "s" }
            );
            set_pending(
                dialog_id,
                Some(&Pending {
                    reason,
                    conflicts: plan.conflicts,
                }),
            );
        }
    }
    Ok(done)
}

#[tauri::command]
pub async fn overlay_undo_merge(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
) -> Result<UndoneMerge, String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || undo_merge(&dialog_id, None))
        .await
        .map_err(|e| e.to_string())?
}

/// Takes markers just handed to the chat back out (it couldn't take them):
/// only those files, nothing merged before.
#[tauri::command]
pub async fn overlay_unmark(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
    files: Vec<String>,
) -> Result<UndoneMerge, String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || undo_merge(&dialog_id, Some(&files)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn overlay_conflict_file(
    dialog_id: String,
    path: String,
) -> Result<crate::git_sync::ConflictFile, String> {
    tauri::async_runtime::spawn_blocking(move || conflict_file(&dialog_id, &path))
        .await
        .map_err(|e| e.to_string())?
}

/// Refused while the chat's run goes: it may be changing the same file.
#[tauri::command]
pub async fn overlay_conflict_resolve(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<super::land::Conflict>, String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        resolve_conflict(
            &dialog_id,
            &path,
            content.map(String::into_bytes),
            side.as_deref(),
        )
    })
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
pub async fn overlay_conflicts_to_chat(
    agent: tauri::State<'_, crate::agent_run::AgentState>,
    dialog_id: String,
) -> Result<HandedOver, String> {
    not_running(&agent, &dialog_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let files = mark_conflicts(&dialog_id)?;
        let request = if files.is_empty() {
            String::new()
        } else {
            resolve_request(&files)
        };
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
        std::fs::write(
            store.join("project.txt"),
            project.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let fs = OverlayFs::open(&project, &store, DEFAULT_LIMIT).unwrap();
        let ino = fs.resolve("a.txt").unwrap();
        fs.setattr(
            ino,
            &SetAttr {
                size: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        fs.write(ino, 0, b"CHAT\n2\n3\n4\n5\n").unwrap();
        fs.save();
        drop(fs);
        std::fs::write(project.join("a.txt"), "PROJECT\n2\n3\n4\n5\n").unwrap();
        let plan = super::super::land::plan(&project, &changes(&id), false);
        set_pending(
            &id,
            Some(&Pending {
                reason: "conflicts".into(),
                conflicts: plan.conflicts,
            }),
        );
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
        assert!(file
            .segments
            .iter()
            .any(|s| matches!(s, crate::merge::MergeSegment::Conflict { .. })));
        let left = resolve_conflict(
            &id,
            "a.txt",
            Some(b"PROJECT CHAT\n2\n3\n4\n5\n".to_vec()),
            None,
        )
        .unwrap();
        assert!(left.is_empty());
        assert!(pending(&id).is_none());
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "PROJECT CHAT\n2\n3\n4\n5\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn read(project: &Path, name: &str) -> String {
        std::fs::read_to_string(project.join(name)).unwrap()
    }

    /// The project changes the file again (another chat lands, the user
    /// saves) while the resolver shows it: the resolution, made against the
    /// older version, is merged with the newer change when landing instead
    /// of replacing it.
    #[test]
    fn a_resolution_made_against_an_older_project_version_is_merged_with_the_newer_one() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("stale");
        let file = conflict_file(&id, "a.txt").unwrap();
        assert_eq!(file.ours.as_deref(), Some("PROJECT\n2\n3\n4\n5\n"));
        std::fs::write(project.join("a.txt"), "PROJECT\n2\n3\n4\nFIVE\n").unwrap();
        resolve_conflict(
            &id,
            "a.txt",
            Some(b"PROJECT CHAT\n2\n3\n4\n5\n".to_vec()),
            None,
        )
        .unwrap();
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(read(&project, "a.txt"), "PROJECT CHAT\n2\n3\n4\nFIVE\n");

        // Taking the chat's side: the same, with its version.
        let (root2, project, id) = conflicted("stale-theirs");
        conflict_file(&id, "a.txt").unwrap();
        std::fs::write(project.join("a.txt"), "PROJECT\n2\n3\n4\nFIVE\n").unwrap();
        resolve_conflict(&id, "a.txt", None, Some("theirs")).unwrap();
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(read(&project, "a.txt"), "CHAT\n2\n3\n4\nFIVE\n");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(root2);
    }

    /// The newer change is to the lines the resolution changed: a conflict
    /// again, not the resolution over it. Taking the project's side takes
    /// its file as it is now.
    #[test]
    fn a_resolution_made_against_an_older_project_version_conflicts_with_a_newer_change_to_the_same_lines(
    ) {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("stale-conflict");
        conflict_file(&id, "a.txt").unwrap();
        std::fs::write(project.join("a.txt"), "PROJECT AGAIN\n2\n3\n4\n5\n").unwrap();
        assert!(resolve_conflict(
            &id,
            "a.txt",
            Some(b"PROJECT CHAT\n2\n3\n4\n5\n".to_vec()),
            None,
        )
        .unwrap()
        .is_empty());
        let err = land_now(&id, false).unwrap_err();
        assert!(
            matches!(&err, super::super::land::LandError::Conflicts(c) if c.len() == 1 && c[0].path == "a.txt"),
            "{err:?}"
        );
        assert_eq!(read(&project, "a.txt"), "PROJECT AGAIN\n2\n3\n4\n5\n");
        assert_eq!(pending(&id).unwrap().conflicts.len(), 1);
        conflict_file(&id, "a.txt").unwrap();
        resolve_conflict(&id, "a.txt", None, Some("ours")).unwrap();
        // The project has it already: nothing to write.
        assert!(land_now(&id, false).unwrap().is_none());
        assert_eq!(read(&project, "a.txt"), "PROJECT AGAIN\n2\n3\n4\n5\n");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Land anyway takes the chat's version over the conflicts the user was
    /// shown. One that came up since (the project changed another file the
    /// chat changed too) stops it and joins the bar's list instead.
    #[test]
    fn land_anyway_takes_over_only_the_conflicts_the_user_saw() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("anyway");
        std::fs::write(project.join("b.txt"), "b1\nb2\n").unwrap();
        with_fs(&id, |fs| {
            let ino = fs.resolve("b.txt").unwrap();
            fs.setattr(
                ino,
                &SetAttr {
                    size: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
            fs.write(ino, 0, b"CHAT B\nb2\n").unwrap();
        })
        .unwrap();
        std::fs::write(project.join("b.txt"), "PROJECT B\nb2\n").unwrap();
        // The bar names a.txt only.
        assert_eq!(pending(&id).unwrap().conflicts.len(), 1);
        let err = land_now(&id, true).unwrap_err();
        let super::super::land::LandError::Conflicts(new) = err else {
            panic!("{err:?}")
        };
        assert_eq!(
            new.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            ["b.txt"]
        );
        assert_eq!(read(&project, "a.txt"), "PROJECT\n2\n3\n4\n5\n");
        assert_eq!(read(&project, "b.txt"), "PROJECT B\nb2\n");
        let mut shown: Vec<String> = pending(&id)
            .unwrap()
            .conflicts
            .into_iter()
            .map(|c| c.path)
            .collect();
        shown.sort();
        assert_eq!(shown, ["a.txt", "b.txt"]);
        // Both seen now: the chat's version of both.
        land_now(&id, true).unwrap().unwrap();
        assert_eq!(read(&project, "a.txt"), "CHAT\n2\n3\n4\n5\n");
        assert_eq!(read(&project, "b.txt"), "CHAT B\nb2\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_conflict_handed_to_the_chat_lands_once_its_markers_are_gone() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("chat");
        assert_eq!(mark_conflicts(&id).unwrap(), vec!["a.txt"]);
        let marked = String::from_utf8(current_version(&id, "a.txt").unwrap()).unwrap();
        // The resolver still sees the real conflict (the chat's own version).
        assert_eq!(
            conflict_file(&id, "a.txt").unwrap().theirs.as_deref(),
            Some("CHAT\n2\n3\n4\n5\n")
        );
        assert!(
            marked.starts_with(&format!(
                "{MARK_PROJECT}\nPROJECT\n{MARK_BASE}\n1\n{MARK_SPLIT}\nCHAT\n{MARK_CHAT}\n2\n"
            )),
            "{marked}"
        );
        assert!(resolve_request(&["a.txt".into()]).contains("`a.txt`"));
        // Markers left: nothing lands, not even anyway.
        assert!(matches!(
            land_now(&id, true),
            Err(super::super::land::LandError::Failed(_))
        ));
        assert_eq!(markers_left(&id), vec!["a.txt"]);
        // The chat resolves them.
        with_fs(&id, |fs| {
            fs.rebase_file(
                "a.txt",
                Some(b"PROJECT\nCHAT\n2\n3\n4\n5\n"),
                Some(b"PROJECT\n2\n3\n4\n5\n"),
            )
        })
        .unwrap()
        .unwrap();
        assert!(markers_left(&id).is_empty());
        land_now(&id, false).unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "PROJECT\nCHAT\n2\n3\n4\n5\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn markers_handed_to_the_chat_can_be_taken_back() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, _project, id) = conflicted("unmark");
        mark_conflicts(&id).unwrap();
        assert!(!markers_left(&id).is_empty());
        // Marking again leaves files already handed over as they are.
        assert!(mark_conflicts(&id).unwrap().is_empty());
        let undone = undo_merge(&id, Some(&["a.txt".to_string()])).unwrap();
        assert_eq!(undone.restored, vec!["a.txt"]);
        assert_eq!(
            String::from_utf8(current_version(&id, "a.txt").unwrap()).unwrap(),
            "CHAT\n2\n3\n4\n5\n"
        );
        assert!(markers_left(&id).is_empty());
        assert_eq!(
            pending(&id).unwrap().conflicts.len(),
            1,
            "the conflict is back for the user"
        );
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
        assert!(
            taken[0].1.contains("+FIVE"),
            "the note shows what the project changed: {}",
            taken[0].1
        );
        assert_eq!(
            String::from_utf8(sides(&id, "a.txt").unwrap().chat.unwrap()).unwrap(),
            "CHAT\n2\n3\n4\nFIVE\n"
        );
        assert_eq!(merged_in(&id), vec!["a.txt"]);
        undo_merge(&id, None).unwrap();
        let s = sides(&id, "a.txt").unwrap();
        assert_eq!(
            String::from_utf8(s.chat.unwrap()).unwrap(),
            "CHAT\n2\n3\n4\n5\n"
        );
        assert_eq!(
            String::from_utf8(s.base.unwrap()).unwrap(),
            "1\n2\n3\n4\n5\n"
        );
        assert!(merged_in(&id).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn undoing_a_merge_keeps_what_the_chat_did_since() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id) = conflicted("changed-since");
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\nFIVE\n").unwrap();
        set_pending(&id, None);
        let plan = super::super::land::plan(&project, &changes(&id), false);
        take_merged(&id, &plan).unwrap();
        // The chat goes on editing the merged file.
        with_fs(&id, |fs| {
            fs.rebase_file(
                "a.txt",
                Some(b"CHAT\n2\n3\nMORE\nFIVE\n"),
                Some(b"1\n2\n3\n4\nFIVE\n"),
            )
        })
        .unwrap()
        .unwrap();
        let undone = undo_merge(&id, None).unwrap();
        assert_eq!(undone.changed_since, vec!["a.txt"]);
        assert_eq!(
            String::from_utf8(current_version(&id, "a.txt").unwrap()).unwrap(),
            "CHAT\n2\n3\nMORE\nFIVE\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn kept_versions_never_share_a_file() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, _project, id) = conflicted("kept-names");
        let s = |chat: &str| Sides {
            base: Some(b"base".to_vec()),
            chat: Some(chat.as_bytes().to_vec()),
            project: None,
        };
        let lock = state_lock(&id);
        let held = lock.lock().unwrap();
        keep_versions(&id, "a", &s("A")).unwrap();
        keep_versions(&id, "b", &s("B")).unwrap();
        forget_kept(&id, "a");
        keep_versions(&id, "c", &s("C")).unwrap();
        drop(held);
        assert_eq!(kept_versions(&id, "b").unwrap().0.unwrap(), b"B");
        assert_eq!(kept_versions(&id, "c").unwrap().0.unwrap(), b"C");
        let _ = std::fs::remove_dir_all(root);
    }

    fn side(content: &str, base: Option<&str>) -> Side {
        Side {
            present: true,
            content: content.as_bytes().to_vec(),
            base: base.map(|b| b.as_bytes().to_vec()),
        }
    }

    fn gone() -> Side {
        Side {
            present: false,
            content: Vec::new(),
            base: None,
        }
    }

    #[test]
    fn chats_on_the_same_lines_clash_and_on_different_lines_do_not() {
        let base = "1\n2\n3\n4\n5\n";
        assert_eq!(
            clash(
                &side("A\n2\n3\n4\n5\n", Some(base)),
                &side("B\n2\n3\n4\n5\n", Some(base))
            ),
            Some(true)
        );
        assert_eq!(
            clash(
                &side("A\n2\n3\n4\n5\n", Some(base)),
                &side("1\n2\n3\n4\nB\n", Some(base))
            ),
            None
        );
        assert_eq!(
            clash(&side("x\n2\n", Some(base)), &side("x\n2\n", Some(base))),
            None
        );
        assert_eq!(clash(&gone(), &gone()), None);
        assert_eq!(clash(&gone(), &side("CHANGED\n", Some(base))), Some(true));
        assert_eq!(clash(&gone(), &side("new\n", None)), None);
        assert_eq!(clash(&side("a\n", None), &side("b\n", None)), Some(true));
        // Bases diverged: a file-level warning.
        assert_eq!(
            clash(
                &side("A\n2\n3\n", Some("1\n2\n3\n")),
                &side("1\nB\n3\n", Some("1\nX\n3\n"))
            ),
            Some(false)
        );
    }

    #[test]
    fn a_clash_is_told_once() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let id = "peer-note-chat";
        PEERS.lock().unwrap().insert(
            id.to_string(),
            BTreeMap::from([(
                "a.txt".to_string(),
                (BTreeSet::from(["other-chat".to_string()]), true),
            )]),
        );
        let note = take_note(id).unwrap();
        assert!(
            note.contains("`a.txt`") && note.contains("same lines"),
            "{note}"
        );
        assert!(take_note(id).is_none(), "told once");
        drop_peer_state(id);
    }

    #[test]
    fn chat_ids_name_one_folder() {
        assert!(valid_id("8f1c2d3e-aaaa"));
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "c:x"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(file_versions("../..".into(), "a".into()).is_err());
        assert!(file_versions("chat".into(), "/etc/passwd".into()).is_err());
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
        o.fs.setattr(
            ino,
            &SetAttr {
                size: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
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
        assert_eq!(
            std::fs::read_to_string(b.view.join("b.txt")).unwrap(),
            "b\n"
        );
        write(&b, "a.txt", "B\n2\n3\n4\n5\n");
        write(&a, "a.txt", "A\n2\n3\n4\n5\n");
        write(&a, "b.txt", "changed by a\n");
        land_now("hub-chat-a", false).unwrap();
        // B sees A's b.txt at once, and knows its a.txt now conflicts.
        assert_eq!(
            std::fs::read_to_string(b.view.join("b.txt")).unwrap(),
            "changed by a\n"
        );
        let pending = pending("hub-chat-b").unwrap();
        assert_eq!(pending.conflicts[0].path, "a.txt");
        let note = take_note("hub-chat-b").unwrap();
        assert!(
            note.contains("b.txt") && note.contains("on lines you changed too: a.txt"),
            "{note}"
        );
        assert!(take_note("hub-chat-b").is_none(), "told once");
        // Two running chats on the same line: both hear it before either lands.
        let c = open("hub-chat-c", &project, Box::new(|_, _| {})).unwrap();
        write(&c, "b.txt", "changed by c\n");
        // B now has b.txt as A landed it; C started from that too.
        write(&b, "b.txt", "changed by b\n");
        let clashes = check_peer_conflicts("hub-chat-c");
        assert_eq!(clashes.len(), 1, "{clashes:?}");
        assert_eq!(clashes[0].path, "b.txt");
        assert!(clashes[0].same_lines);
        assert!(peer_conflicts("hub-chat-b")
            .iter()
            .any(|p| p.peers == vec!["hub-chat-c".to_string()]));
        assert!(take_note("hub-chat-b").unwrap().contains("same lines"));
        discard("hub-chat-c").unwrap();
        assert!(
            peer_conflicts("hub-chat-b").is_empty(),
            "gone with the other chat"
        );
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
        let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let data = std::env::temp_dir().join(format!("bench-{}", std::process::id()));
        super::init(&data);
        let o = super::open("bench-chat", &project, Box::new(|_, _| {})).unwrap();
        let time = |what: &str, dir: &std::path::Path, cmd: &str| {
            let t = std::time::Instant::now();
            let out = std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(dir)
                .output()
                .unwrap();
            eprintln!(
                "{what:>8}: {:>6} ms  {}",
                t.elapsed().as_millis(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
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

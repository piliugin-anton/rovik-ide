//! Each chat's overlay: opened when one of its runs starts, mounted as a
//! folder the run uses as its project root, kept until its changes land or
//! are discarded.
//!
//! The overlay's own folder is `<app data>/overlays/<chat id>/` (changes and
//! the list of them, so a chat's work survives a restart); the view is
//! mounted at a short path of its own (`$XDG_RUNTIME_DIR/rovik-ide/chats/…`
//! on Linux), so commands can use it unquoted.
//!
//! A workspace chat's view (docs/design/workspaces-plan.md, 4.2) holds its
//! folders by name, each its own overlay, kept in `parts/<name>/` of the
//! chat's folder, with `folders.json` naming the workspace and the folders
//! at its last run; a folder chat's is its folder's one overlay, with
//! `project.txt`.
//!
//! Isolation needs the virtual-files driver (FUSE or macFUSE). If it can't
//! mount, the chat works in the project directly, as before isolation, and
//! the run says so. Windows has none: a chat of a project runs in WSL
//! (the client's `Remote::go_home`), whose daemon gives it a worktree.

use super::composite::CompositeFs;
use super::fs::{Budget, ChatFs, OverlayChange, OverlayFs, DEFAULT_LIMIT};
// The markers put in a chat's files for it to resolve.
use super::markers::{
    self, BASE as MARK_BASE, CHAT as MARK_CHAT, PROJECT as MARK_PROJECT, SPLIT as MARK_SPLIT,
};
use crate::folders::{Folder, Folders};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

pub struct ChatOverlay {
    /// The chat's files as its view shows them.
    pub fs: Arc<dyn ChatFs>,
    /// What the view is over.
    under: Under,
    /// Where the chat sees it: the mount.
    pub view: PathBuf,
    /// The overlay's own folder (changes, build output of its commands).
    pub store: PathBuf,
    mount: Mutex<Option<crate::vfs::Mount>>,
    /// The store's lock while it's open here (`hold_store`).
    held: Mutex<Option<crate::os_lock::Held>>,
}

/// What a chat's view is over.
enum Under {
    /// One folder's overlay: a folder chat's, a child's (over its parent's
    /// view), a scratch view of one folder.
    Folder(Arc<OverlayFs>),
    /// A workspace chat's folders (or a scratch view of them), each its own
    /// overlay in one view, and the folders the view was matched to last.
    Workspace {
        view: Arc<CompositeFs>,
        folders: RwLock<Folders>,
    },
}

impl Under {
    fn chat_fs(&self) -> Arc<dyn ChatFs> {
        match self {
            Under::Folder(fs) => fs.clone(),
            Under::Workspace { view, .. } => view.clone(),
        }
    }

    fn backend(&self) -> Arc<dyn crate::vfs::backend::Backend> {
        match self {
            Under::Folder(fs) => fs.clone(),
            Under::Workspace { view, .. } => view.clone(),
        }
    }
}

impl ChatOverlay {
    /// The folders under the view: a folder chat's one (a child's, its
    /// parent's view), or a workspace chat's, as its view has them now.
    pub fn folders(&self) -> Folders {
        match &self.under {
            Under::Folder(fs) => Folders::one(fs.base().to_path_buf()),
            Under::Workspace { folders, .. } => {
                folders.read().unwrap_or_else(|e| e.into_inner()).clone()
            }
        }
    }

    /// A folder chat's folder (a child's: its parent's view); None for a
    /// workspace chat's view.
    pub fn folder(&self) -> Option<&Path> {
        match &self.under {
            Under::Folder(fs) => Some(fs.base()),
            Under::Workspace { .. } => None,
        }
    }

    /// The overlay of the chat's folder at `project` (`canonical`: its
    /// path with links resolved), with its name in the chat ("" for a
    /// folder chat's folder).
    fn part_on(
        &self,
        project: &Path,
        canonical: Option<&Path>,
    ) -> Option<(String, Arc<OverlayFs>)> {
        let on = |base: &Path| base == project || canonical == Some(base);
        match &self.under {
            Under::Folder(fs) => on(fs.base()).then(|| (String::new(), fs.clone())),
            Under::Workspace { view, .. } => view
                .folders()
                .into_iter()
                .find(|(_, base)| on(base))
                .and_then(|(name, _)| Some((name.clone(), view.part(&name)?))),
        }
    }

    /// What the chat's builds and its commands' history are kept by: its
    /// folder, or its workspace, whose chats share them whichever of its
    /// folders each works in.
    fn shared_by(&self) -> String {
        match &self.under {
            Under::Folder(fs) => fs.base().to_string_lossy().into_owned(),
            Under::Workspace { folders, .. } => {
                let folders = folders.read().unwrap_or_else(|e| e.into_inner());
                format!("workspace:{}", folders.workspace().unwrap_or_default())
            }
        }
    }

    /// Variables for the chat's commands. Rust's build output goes to a
    /// folder on disk outside the mount, shared by the project's isolated
    /// chats (a workspace's, for a workspace chat), so only the first chat
    /// builds from cold, but not with the project's own `target/` (so the
    /// user never runs a chat's build). Cargo locks it, and the queue for
    /// heavy work runs one build at a time anyway.
    pub fn command_env(&self) -> Vec<(String, String)> {
        vec![(
            "CARGO_TARGET_DIR".into(),
            self.target_dir().to_string_lossy().into_owned(),
        )]
    }

    /// Where the chat's Rust builds go (`command_env`).
    fn target_dir(&self) -> PathBuf {
        let shared = data_dir().map(|d| {
            d.join("overlays")
                .join("targets")
                .join(shared_key(&self.shared_by()))
        });
        shared.unwrap_or_else(|| self.store.join("out").join("target"))
    }

    /// The sandbox the chat's commands run in: they may write to the view
    /// and the build folder, which is made here since they couldn't
    /// (confine.rs). A folder chat's project path shows its view to them; a
    /// workspace chat's folders' own paths show the folders as they are,
    /// read-only. None where that isn't possible.
    pub fn confine(&self) -> Option<super::confine::Confine> {
        let target = self.target_dir();
        let _ = std::fs::create_dir_all(&target);
        let c = match &self.under {
            Under::Folder(fs) => {
                super::confine::Confine::redirecting(self.view.clone(), fs.base().to_path_buf())
            }
            Under::Workspace { .. } => super::confine::Confine::hiding(self.view.clone()),
        }
        .also_writable(target);
        super::confine::available(&c).then_some(c)
    }

    /// Why a command can't run in this chat as written, if it can't.
    /// Changes reach the project by landing, so git commands that change
    /// the repository are refused (reading it works: status, diff, log,
    /// show, blame). So is a folder of the chat named by its full path
    /// where its commands don't see the view there, which would reach past
    /// the chat's view.
    pub fn command_problem(&self, command: &str) -> Option<String> {
        let parsed = crate::agent_run::permissions::parse_command(command);
        for segment in &parsed.segments {
            if let Some(what) = git_changes_repo(segment) {
                return Some(format!(
                    "Error: `git {what}` changes the repository, and this chat is isolated: its changes reach the project when they land, not through git. Read-only git (status, diff, log, show, blame, branch listing) works. Leave committing to the user."
                ));
            }
        }
        let confine = self.confine();
        let what = match self.under {
            Under::Folder(_) => "the project",
            Under::Workspace { .. } => "its folders",
        };
        for folder in self.folders().iter() {
            let path = folder.path.to_string_lossy();
            let redirected = confine
                .as_ref()
                .is_some_and(|c| c.redirects() && c.project == folder.path);
            if names_path(command, &path) && !redirected {
                return Some(format!(
                    "Error: this chat works in its own view of {what} (the current folder, {}), not in {path} itself. Use paths relative to the current folder.",
                    self.view.display()
                ));
            }
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
        // Last: the store is anyone's again.
        self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Git commands that change the repository whatever follows them.
pub(crate) const GIT_CHANGES_REPO: &[&str] = &[
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

/// `git branch` options that create, delete, move or force a branch
/// (listing is fine).
pub(crate) const GIT_BRANCH_CHANGES: &[&str] = &[
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
];

/// `git remote` subcommands that change the remotes.
pub(crate) const GIT_REMOTE_CHANGES: &[&str] = &[
    "add",
    "remove",
    "rm",
    "rename",
    "set-url",
    "set-head",
    "set-branches",
    "prune",
    "update",
];

/// `git reflog` subcommands that change the reflog.
pub(crate) const GIT_REFLOG_CHANGES: &[&str] = &["expire", "delete"];

/// The git command a command segment runs, if it changes the repository
/// (and so has no place in an isolated chat). Git's own options before the
/// command (`-C dir`, `-c key=value`) are skipped; ones that point it at
/// another repository are refused outright.
pub(crate) fn git_changes_repo(segment: &[String]) -> Option<String> {
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
        s if GIT_CHANGES_REPO.contains(&s) => true,
        // Listing is fine; creating, deleting, moving or forcing isn't.
        "branch" => {
            has(GIT_BRANCH_CHANGES)
                || args.iter().any(|a| !a.starts_with('-'))
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
        "remote" => args.first().is_some_and(|a| GIT_REMOTE_CHANGES.contains(a)),
        "reflog" => args.first().is_some_and(|a| GIT_REFLOG_CHANGES.contains(a)),
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

/// Tests that use the overlays' data folder (the process's) take this, so
/// they don't set it under each other.
#[cfg(any(test, feature = "testing"))]
pub static TEST_DATA: LazyLock<Arc<tokio::sync::Mutex<()>>> = LazyLock::new(Default::default);

/// A test's data folder, held until the guard goes.
#[cfg(any(test, feature = "testing"))]
pub fn test_data(data_dir: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let held = TEST_DATA.clone().blocking_lock_owned();
    init(data_dir);
    held
}

/// Where overlays are kept: the app's data folder. Landings a crash
/// interrupted are finished first.
pub fn init(data_dir: &Path) {
    *DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(data_dir.to_path_buf());
    sweep_scratch(data_dir);
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

/// A short, stable name for what chats share a build folder by: a project
/// folder, or a workspace (`ChatOverlay::shared_by`).
fn shared_key(owner: &str) -> String {
    let hash = crate::file_guard::hash(owner.as_bytes());
    crate::file_guard::hex(&hash[..8])
}

/// Chats already told this session that they can't be isolated.
static TOLD: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);

/// Whether to tell this chat it isn't isolated: once per session.
pub fn tell_not_isolated(dialog_id: &str) -> bool {
    TOLD.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dialog_id.to_string())
}

pub(crate) fn data_dir() -> Option<PathBuf> {
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

pub(super) fn store_dir(dialog_id: &str) -> Option<PathBuf> {
    valid_id(dialog_id)
        .then(|| Some(data_dir()?.join("overlays").join(dialog_id)))
        .flatten()
}

pub(crate) fn mountpoint(dialog_id: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_dir()) {
        Some(dir) => PathBuf::from(dir).join("rovik-ide").join("chats"),
        None => std::env::temp_dir()
            .join(format!("rovik-ide-{}", unsafe { libc::getuid() }))
            .join("chats"),
    };
    #[cfg(not(target_os = "linux"))]
    let base = std::env::temp_dir().join("rovik-chats");
    // macFUSE's FSKit backend mounts nowhere else (macOS 15.4+, macFUSE
    // 5.1+): a view under the temporary folder never mounted there, and
    // every isolated chat worked in the project instead.
    if crate::vfs::mounts_only_under_volumes() {
        return fskit_view(dialog_id);
    }
    // Tests: each process's views in a folder of its own. Tests running at
    // once in processes of their own (cargo-nextest) name their chats
    // alike (the daemon's tests' "chat-1"), and would mount on each other's
    // view.
    #[cfg(any(test, feature = "testing"))]
    let base = base.join(format!("test-{}", std::process::id()));
    base.join(dialog_id)
}

/// Where a chat's view goes with macFUSE's FSKit backend: right under
/// `/Volumes`, by a name of the characters FSKit takes.
const FSKIT_VIEW_PREFIX: &str = "/Volumes/RovikChat-";

/// The chat's view with macFUSE's FSKit backend ([`FSKIT_VIEW_PREFIX`]).
fn fskit_view(dialog_id: &str) -> PathBuf {
    let name: String = dialog_id
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || "-_.".contains(c) => c,
            _ => '-',
        })
        .collect();
    PathBuf::from(format!("{FSKIT_VIEW_PREFIX}{name}"))
}

/// Whether `path` is a chat's view (a mount made here), not a project.
pub fn is_chat_view(path: &Path) -> bool {
    let base = mountpoint("x");
    let fskit = path
        .to_str()
        .and_then(|p| p.strip_prefix(FSKIT_VIEW_PREFIX))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'));
    fskit || path.parent().is_some_and(|p| Some(p) == base.parent())
}

/// The chat's overlay over `folders`, opened (and mounted) if it isn't yet
/// (workspaces-plan.md, 4.2): a folder chat's folder, or a workspace chat's
/// folders, each its own overlay in one view. A workspace chat's view is
/// matched to `folders` each time, as each run's start finds them: a folder
/// that joined comes in, one that left goes with its store (not while the
/// chat has changes there that haven't landed), and one located elsewhere
/// is opened there with its changes. A store made for the other kind of
/// chat is refused, and left as it was. `on_warn`: told once when its
/// changes pass 80% of its limit (used, limit).
pub fn open(
    dialog_id: &str,
    folders: &Folders,
    on_warn: super::fs::OnWarn,
) -> Result<Arc<ChatOverlay>, String> {
    // One spelling of each folder (symlinks, `/private` on macOS, case on
    // Windows): the watcher and later runs may use another.
    let folders = canonical(folders)?;
    // Only this chat waits while its view is opened and mounted; the open
    // overlays stay free for everyone else.
    let gate = gate(dialog_id);
    let opening = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        return match (&o.under, folders.single()) {
            (Under::Folder(fs), Some(project)) if fs.base() == project => Ok(o),
            (Under::Folder(_), Some(_)) => {
                Err("this chat's isolated changes belong to another project folder".into())
            }
            (Under::Workspace { .. }, None) => {
                let changed = rematch(&o, dialog_id, &folders)?;
                // Before the check: it may mark conflicts, which takes the
                // gate.
                drop(opening);
                if changed {
                    check_all(&o, dialog_id);
                }
                Ok(o)
            }
            (Under::Folder(_), None) => Err(FOLDER_STORE.into()),
            (Under::Workspace { .. }, Some(_)) => Err(WORKSPACE_STORE.into()),
        };
    }
    let store = store_dir(dialog_id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    // A chat stays with the project, or the workspace, its changes were
    // made in.
    check_store(&store, &folders)?;
    let held = hold_store(&store)?;
    let budget = Budget::new(DEFAULT_LIMIT);
    budget.on_warn(on_warn);
    let overlay = mount_view(dialog_id, &store, &folders, budget, held)?;
    // A child's landing into this view that a crash interrupted (F01).
    if let Some(data) = data_dir() {
        let finished = super::land::recover_into(&data, dialog_id, &overlay.view);
        if !finished.is_empty() {
            eprintln!(
                "overlay: finished an interrupted landing into {dialog_id}: {}",
                finished.join(", ")
            );
        }
    }
    // Before the check: it may mark conflicts, which takes the gate.
    drop(opening);
    // Closed until now (a restart): the project may have changed meanwhile.
    check_all(&overlay, dialog_id);
    Ok(overlay)
}

/// Why a workspace chat's changes don't open as one folder's.
const WORKSPACE_STORE: &str =
    "this chat's isolated changes are across a workspace's folders, not in one folder";

/// Why a folder chat's changes don't open across a workspace's folders.
const FOLDER_STORE: &str =
    "this chat's isolated changes are in one folder, not across a workspace's folders";

/// A folder chat's store: the folder its changes are of.
const PROJECT_FILE: &str = "project.txt";

/// A workspace chat's store: its workspace, and its folders at its last
/// run ([`SavedView`]).
const FOLDERS_FILE: &str = "folders.json";

/// Where a workspace chat's store keeps each folder's overlay, by name.
const PARTS: &str = "parts";

/// What a workspace chat's view was over at its last run.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedView {
    workspace: String,
    folders: Vec<super::land::SavedFolder>,
}

/// The store of a workspace chat's folder `name`.
fn part_store(store: &Path, name: &str) -> PathBuf {
    store.join(PARTS).join(name)
}

/// `folders`, each spelled once: with links resolved, where it's there.
fn canonical(folders: &Folders) -> Result<Folders, String> {
    let spelled = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(folder) = folders.single() {
        return Ok(Folders::one(spelled(folder)));
    }
    let list = folders
        .iter()
        .map(|f| Folder {
            name: f.name.clone(),
            path: spelled(&f.path),
        })
        .collect();
    let named = Folders::named(list)?;
    Ok(match folders.workspace() {
        Some(id) => named.in_workspace(id),
        None => named,
    })
}

/// What a store's view is over, as [`record`] left it: a workspace chat's
/// folders, or a folder chat's folder.
fn recorded(store: &Path) -> Option<Folders> {
    match std::fs::read(store.join(FOLDERS_FILE)) {
        Ok(bytes) => {
            let view: SavedView = serde_json::from_slice(&bytes).ok()?;
            let list = view
                .folders
                .into_iter()
                .map(|f| Folder {
                    name: f.name,
                    path: f.path,
                })
                .collect();
            Some(Folders::named(list).ok()?.in_workspace(&view.workspace))
        }
        Err(_) => {
            let text = std::fs::read_to_string(store.join(PROJECT_FILE)).ok()?;
            Some(Folders::one(PathBuf::from(text.trim())))
        }
    }
}

/// Records what the store's view is over, for opening it again.
fn record(store: &Path, folders: &Folders) -> Result<(), String> {
    match folders.single() {
        Some(project) => std::fs::write(
            store.join(PROJECT_FILE),
            project.to_string_lossy().as_bytes(),
        )
        .map_err(|e| e.to_string()),
        None => {
            let view = SavedView {
                workspace: folders.workspace().unwrap_or_default().to_string(),
                folders: folders
                    .iter()
                    .map(|f| super::land::SavedFolder {
                        name: f.name.clone(),
                        path: f.path.clone(),
                    })
                    .collect(),
            };
            let bytes = serde_json::to_vec_pretty(&view).map_err(|e| e.to_string())?;
            crate::atomic_file::write_atomic(&store.join(FOLDERS_FILE), &bytes)
                .map_err(|e| e.to_string())
        }
    }
}

/// Whether the chat's store can be opened over `folders`: it's new, or a
/// folder chat's of the same folder, or a workspace chat's of the same
/// workspace. It's only read.
fn check_store(store: &Path, folders: &Folders) -> Result<(), String> {
    let saved = recorded(store);
    match folders.single() {
        Some(project) => match saved.as_ref().map(|s| s.single()) {
            Some(None) => Err(WORKSPACE_STORE.into()),
            Some(Some(saved)) if !same_folder(saved, project) => Err(format!(
                "this chat's isolated changes belong to {}",
                saved.display()
            )),
            _ if saved.is_none() && store.join(FOLDERS_FILE).exists() => {
                Err(WORKSPACE_STORE.into())
            }
            _ => Ok(()),
        },
        None => {
            // A folder chat's changes are its store's own list.
            let folder_store = store.join("manifest.json").is_file()
                || saved.as_ref().is_some_and(|s| s.single().is_some());
            if folder_store {
                return Err(FOLDER_STORE.into());
            }
            let workspace = folders
                .workspace()
                .ok_or("a workspace chat's view needs its workspace")?;
            match saved.as_ref().and_then(Folders::workspace) {
                Some(other) if other != workspace => {
                    Err("this chat's isolated changes belong to another workspace".into())
                }
                _ => Ok(()),
            }
        }
    }
}

/// An overlay of `folders` kept in `store`, for `id`: its folders' overlays
/// opened from their saved lists and swept (the store's lock is `held`,
/// so nothing else has them open), what it's over recorded, mounted and
/// registered as open.
fn mount_view(
    id: &str,
    store: &Path,
    folders: &Folders,
    budget: Arc<Budget>,
    held: Option<crate::os_lock::Held>,
) -> Result<Arc<ChatOverlay>, String> {
    let under = match folders.single() {
        Some(base) => {
            let fs = OverlayFs::open_in(base, store, budget).map_err(|e| e.to_string())?;
            // Files a crash left behind: only here, where nothing else has
            // the chat's changes open (its gate is held, and its store's
            // lock).
            fs.sweep_orphans();
            Under::Folder(Arc::new(fs))
        }
        None => {
            drop_left_parts(store, folders)?;
            let view = Arc::new(CompositeFs::new(budget));
            let budget = view.budget();
            view.matched(folders, |folder| open_part(store, folder, &budget))
                .map_err(|e| format!("couldn't open the chat's changes in {e}"))?;
            for folder in folders.iter() {
                if let Some(part) = view.part(&folder.name) {
                    part.sweep_orphans();
                }
            }
            Under::Workspace {
                view,
                folders: RwLock::new(folders.clone()),
            }
        }
    };
    let at = mountpoint(id);
    let (mount, view) = crate::vfs::platform_mount(under.backend(), Some(&at))
        .map_err(|e| format!("couldn't mount the chat's view: {}", e.message()))?;
    // Recorded once it's there: a view that couldn't be mounted leaves no
    // workspace chat's store behind (`exists`). An error unmounts it.
    record(store, folders)?;
    let overlay = Arc::new(ChatOverlay {
        fs: under.chat_fs(),
        under,
        view,
        store: store.to_path_buf(),
        mount: Mutex::new(Some(mount)),
        held: Mutex::new(held),
    });
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.to_string(), overlay.clone());
    keep_saving();
    Ok(overlay)
}

/// The overlay of a workspace chat's folder, kept in its part of `store`
/// and counted in the chat's `budget`.
fn open_part(
    store: &Path,
    folder: &Folder,
    budget: &Arc<Budget>,
) -> std::io::Result<Arc<OverlayFs>> {
    OverlayFs::open_in(
        &folder.path,
        &part_store(store, &folder.name),
        budget.clone(),
    )
    .map(Arc::new)
    .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", folder.name)))
}

/// What a chat would land of `changes`: the by-products left out, each
/// named from the view's root (`name`: its folder's, "" for a folder
/// chat's).
fn landable(changes: Vec<OverlayChange>, name: &str) -> Vec<OverlayChange> {
    changes
        .into_iter()
        .filter(|c| !c.by_product())
        .map(|c| match name.is_empty() {
            true => c,
            false => c.under(name),
        })
        .collect()
}

/// Why a workspace chat's view can't leave its folder `name` out.
fn left_with_changes(name: &str) -> String {
    format!(
        "this chat has changes in {name} that haven't landed, and {name} is no longer one of its folders: land or discard them, or check {name} for the chat again"
    )
}

/// The stores of folders no longer among a workspace chat's (`folders`),
/// left while its view was closed: removed, unless the chat has changes in
/// one of them that haven't landed, which leaves them all, and keeps its
/// view from opening without them.
fn drop_left_parts(store: &Path, folders: &Folders) -> Result<(), String> {
    let Ok(parts) = std::fs::read_dir(store.join(PARTS)) else {
        return Ok(());
    };
    let left: Vec<(String, PathBuf)> = parts
        .flatten()
        .map(|part| (part.file_name().to_string_lossy().into_owned(), part.path()))
        .filter(|(name, _)| !folders.iter().any(|f| f.name == *name))
        .collect();
    for (name, part) in &left {
        let saved = super::fs::saved_changes(part)
            .map_err(|e| format!("this chat's changes in {name} can't be read ({e})"))?;
        if !landable(saved, name).is_empty() {
            return Err(left_with_changes(name));
        }
    }
    for (name, part) in left {
        std::fs::remove_dir_all(part).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

/// Apply (D32): a workspace chat's folders are `folders` from now on. Its
/// open view takes them at once: a folder checked comes in, and one
/// unchecked goes, its store with it, refused while the chat has changes
/// there that haven't landed. A closed chat's store lets go of the folders
/// it no longer has, as its view would; the others come in at its next
/// run.
pub fn set_folders(dialog_id: &str, folders: &Folders) -> Result<(), String> {
    let folders = canonical(folders)?;
    let gate = gate(dialog_id);
    let held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        let changed = rematch(&o, dialog_id, &folders)?;
        // Before the check: it may mark conflicts, which takes the gate.
        drop(held);
        if changed {
            check_all(&o, dialog_id);
        }
        return Ok(());
    }
    // Nothing saved yet: its first run opens its view over them.
    let Some(store) = store_dir(dialog_id).filter(|s| s.join(FOLDERS_FILE).is_file()) else {
        return Ok(());
    };
    check_store(&store, &folders)?;
    let _lock = hold_store(&store)?;
    drop_left_parts(&store, &folders)?;
    record(&store, &folders)
}

/// Locate… (D19): a workspace chat's folder `name` is at `path` from now
/// on, as the same project: the chat's changes there go with it. Its open
/// view opens the folder there from the changes' saved list; a closed
/// one's store, and its landings, say where it is.
pub fn relocate(dialog_id: &str, name: &str, path: &Path) -> Result<(), String> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let moved = |folders: &Folders| -> Result<Folders, String> {
        let list = folders
            .iter()
            .map(|f| Folder {
                name: f.name.clone(),
                path: match f.name == name {
                    true => path.clone(),
                    false => f.path.clone(),
                },
            })
            .collect();
        let named = Folders::named(list)?;
        Ok(match folders.workspace() {
            Some(id) => named.in_workspace(id),
            None => named,
        })
    };
    let gate = gate(dialog_id);
    let held = gate.lock().unwrap_or_else(|e| e.into_inner());
    match get(dialog_id) {
        Some(o) => {
            if matches!(o.under, Under::Workspace { .. }) {
                let changed = rematch(&o, dialog_id, &moved(&o.folders())?)?;
                drop(held);
                if changed {
                    check_all(&o, dialog_id);
                }
            }
        }
        None => {
            let store = store_dir(dialog_id).filter(|s| s.join(FOLDERS_FILE).is_file());
            if let Some(store) = store {
                if let Some(folders) = recorded(&store).filter(|f| f.single().is_none()) {
                    let _lock = hold_store(&store)?;
                    record(&store, &moved(&folders)?)?;
                }
            }
        }
    }
    match data_dir() {
        Some(data) => super::land::relocate(&data, dialog_id, name, &path),
        None => Ok(()),
    }
}

/// A workspace chat's open view matched to `folders`, its gate held: a
/// folder that joined is opened, one located elsewhere is opened there from
/// its saved list, and one that left goes, its store with it, refused while
/// the chat has changes there that haven't landed. Whether anything
/// changed.
fn rematch(o: &ChatOverlay, dialog_id: &str, folders: &Folders) -> Result<bool, String> {
    let Under::Workspace {
        view,
        folders: current,
    } = &o.under
    else {
        return Err(FOLDER_STORE.into());
    };
    let workspace = current
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .workspace()
        .map(str::to_string);
    if workspace.as_deref() != folders.workspace() {
        return Err("this chat's isolated changes belong to another workspace".into());
    }
    let had = view.folders();
    let wanted: Vec<(String, PathBuf)> = folders
        .iter()
        .map(|f| (f.name.clone(), f.path.clone()))
        .collect();
    let recorded = *current.read().unwrap_or_else(|e| e.into_inner()) == *folders;
    if had == wanted && recorded {
        return Ok(false);
    }
    for (name, _) in had
        .iter()
        .filter(|(n, _)| !folders.iter().any(|f| f.name == *n))
    {
        let left = view.part(name).map(|p| p.changes()).unwrap_or_default();
        if !landable(left, name).is_empty() {
            return Err(left_with_changes(name));
        }
    }
    // Saved first: a folder located elsewhere is opened from its list.
    view.flush();
    let budget = view.budget();
    let left = view
        .matched(folders, |folder| open_part(&o.store, folder, &budget))
        .map_err(|e| format!("couldn't open the chat's changes in {e}"))?;
    for (name, path) in &wanted {
        if !had.iter().any(|(n, p)| n == name && p == path) {
            if let Some(part) = view.part(name) {
                part.sweep_orphans();
            }
        }
    }
    for (name, part) in left {
        part.close_handles();
        // Its clashes there were of the folder it was over.
        PEERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(dialog_id.to_string(), part.base().to_path_buf()));
        drop(part);
        crate::file_guard::forget_under(&o.view.join(&name));
        // What's left of a store that can't be removed goes the next time
        // the view opens (`drop_left_parts`).
        if !folders.iter().any(|f| f.name == name) {
            match std::fs::remove_dir_all(part_store(&o.store, &name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    log::warn!("overlay: {name}'s store in {}: {e}", o.store.display())
                }
                _ => {}
            }
        }
    }
    // Kept as the view's only once recorded: one that couldn't be is
    // recorded again at the next match.
    record(&o.store, folders)?;
    *current.write().unwrap_or_else(|e| e.into_inner()) = folders.clone();
    Ok(true)
}

/// One lock per chat, held while its overlay is opened from its saved
/// changes (to mount it, or for one call): never two at once on the same
/// files. The open overlays' lock is only held to look one up.
static GATES: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

pub(super) fn gate(dialog_id: &str) -> Arc<Mutex<()>> {
    GATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(dialog_id.to_string())
        .or_default()
        .clone()
}

/// What a command run in `path` is kept by in the workload's history: the
/// folder a chat's view is of, or its workspace (`workspace:<id>`), whose
/// chats share it; any other folder is its own.
pub fn project_for(path: &Path) -> String {
    let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.values()
        .find(|o| o.view == path)
        .map(|o| o.shared_by())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The chat's overlay if it's open.
pub fn get(dialog_id: &str) -> Option<Arc<ChatOverlay>> {
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dialog_id)
        .cloned()
}

/// Whether the chat has an overlay, open or saved: a folder chat's changes,
/// or a workspace chat's view.
pub fn exists(dialog_id: &str) -> bool {
    get(dialog_id).is_some()
        || store_dir(dialog_id)
            .is_some_and(|s| s.join("manifest.json").is_file() || s.join(FOLDERS_FILE).is_file())
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
    if open_elsewhere(dialog_id) {
        return Err(OPEN_ELSEWHERE.into());
    }
    let_go(dialog_id);
    match store_dir(dialog_id) {
        // Gone already (discarded twice at once): nothing to do.
        Some(store) => match std::fs::remove_dir_all(&store) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
        None => Ok(()),
    }
}

/// What goes with the chat's changes besides its store: the commands it
/// left running (in its view, maybe), its view, its checks' saved output,
/// its children's overlays and what it was compared with.
pub(super) fn let_go(dialog_id: &str) {
    crate::command_tool::background::stop_chat(dialog_id);
    // Its checks' saved output goes with it (landing discards too).
    crate::agent_run::check_output::clean(dialog_id);
    // And its children's overlays, which stood on its view (F02).
    for child in children_of(dialog_id) {
        discard_child(&child);
    }
    // Its folders, so the chats left open on them stop warning about this
    // one.
    let folders = folders_of(dialog_id);
    close(dialog_id);
    drop_peer_state(dialog_id);
    forget_shown(dialog_id, None);
    for folder in folders.iter().flat_map(Folders::iter) {
        refresh_project_peers(&folder.path);
    }
}

/// A deleted chat: its overlay and its landings' journals go, and the lock
/// it held while at work (chat_hold.rs).
pub fn forget(dialog_id: &str) {
    if !valid_id(dialog_id) {
        return;
    }
    crate::chat_hold::forget(dialog_id);
    let _ = discard(dialog_id);
    if let Some(data) = data_dir() {
        let _ = std::fs::remove_dir_all(data.join("landings").join(dialog_id));
    }
}

/// At exit: every view unmounted, or its folder stays a dead mount.
pub fn close_all() {
    let mut all: Vec<(String, Arc<ChatOverlay>)> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .collect();
    // Children first: they read from their parents' views.
    all.sort_by_key(|(id, _)| parent_of(id).is_none());
    for (_, o) in all {
        o.unmount();
    }
}

/*
 * ---------------------------------------------------------------------------
 * Child overlays (F01, F02): a parallel worker's view, stacked on its
 * parent chat's view and landed back into it
 * ---------------------------------------------------------------------------
 */

/// Child chat id -> its parent's.
static CHILDREN: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
/// The least room a child gets, whatever its parent has used.
const MIN_CHILD_LIMIT: u64 = 1 << 20;

/// The open children of a chat.
pub fn children_of(parent: &str) -> Vec<String> {
    let mut kids: Vec<String> = CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, p)| p.as_str() == parent)
        .map(|(c, _)| c.clone())
        .collect();
    // Saved ones a restart left behind, too.
    if let Some(data) = data_dir() {
        if let Ok(dirs) = std::fs::read_dir(data.join("overlays")) {
            for d in dirs.flatten() {
                let id = d.file_name().to_string_lossy().into_owned();
                let saved = std::fs::read_to_string(d.path().join("parent.txt")).ok();
                if saved.as_deref().map(str::trim) == Some(parent) && !kids.contains(&id) {
                    kids.push(id);
                }
            }
        }
    }
    kids
}

/// The parent of a child chat, while it's open.
pub fn parent_of(child: &str) -> Option<String> {
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(child)
        .cloned()
}

#[cfg(test)]
mod fskit_view_tests {
    use super::*;

    /// With macFUSE's FSKit backend a chat's view goes right under
    /// /Volumes by a name FSKit takes, and is known for a chat's view.
    #[test]
    fn an_fskit_view_is_a_plain_name_under_volumes() {
        let view = fskit_view("d1b2c3-4e5f_6.7");
        assert_eq!(view, Path::new("/Volumes/RovikChat-d1b2c3-4e5f_6.7"));
        let odd = fskit_view("a b/c#é");
        assert!(crate::vfs::problem::fskit_mountpoint_ok(
            &odd.to_string_lossy()
        ));
        assert!(is_chat_view(&view) && is_chat_view(&odd));
        assert!(!is_chat_view(Path::new("/Volumes/RovikChat-")));
        assert!(!is_chat_view(Path::new("/Volumes/RovikChat-x/inside")));
        assert!(!is_chat_view(Path::new("/Volumes/Other")));
    }
}

/// F02: opens (and mounts) a child overlay of `parent`'s view. Its changes
/// count inside the parent's limit: it gets what the parent and its other
/// open children leave of it.
#[cfg(test)]
pub fn open_child(parent: &str, child: &str) -> Result<Arc<ChatOverlay>, String> {
    open_child_shared(parent, child, 1)
}

/// [`open_child`] for one of `parts` children opened together (a parallel
/// group's workers): each gets an equal part of what's left. The other
/// open children's limits are taken as theirs, whatever they've used so
/// far, so children opened before any of them writes don't each get it all.
pub fn open_child_shared(
    parent: &str,
    child: &str,
    parts: usize,
) -> Result<Arc<ChatOverlay>, String> {
    if !valid_id(child) {
        return Err("not a chat id".into());
    }
    let p = get(parent).ok_or("the chat isn't isolated, so it can't have child overlays")?;
    if let Some(o) = get(child) {
        return Ok(o);
    }
    let (used, limit) = p.fs.used();
    let siblings: Vec<Arc<ChatOverlay>> =
        children_of(parent).iter().filter_map(|c| get(c)).collect();
    let reserved: u64 = siblings.iter().map(|o| o.fs.used().1).sum();
    // Of the parts, those not opened yet share what's left.
    let left = parts.saturating_sub(siblings.len()).max(1) as u64;
    let room = (limit.saturating_sub(used).saturating_sub(reserved) / left).max(MIN_CHILD_LIMIT);
    let overlay = mount_on(child, &Folders::one(p.view.clone()), room, Some(parent))?;
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(child.to_string(), parent.to_string());
    Ok(overlay)
}

/// E04: a throwaway overlay of `folders` (the project, the code before a
/// chat's change; a workspace chat's folders in one view), for running
/// something there without touching it. Removed with `discard_child`.
pub fn open_scratch(id: &str, folders: &Folders) -> Result<Arc<ChatOverlay>, String> {
    if !valid_id(id) {
        return Err("not a chat id".into());
    }
    // P6.8: marked, so one a crash left is swept at the next start; marked
    // before it's mounted, so a crash in between leaves it marked (round 2).
    if let Some(store) = store_dir(id) {
        std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
        std::fs::write(store.join(SCRATCH_MARK), "").map_err(|e| e.to_string())?;
    }
    mount_on(id, folders, DEFAULT_LIMIT, None)
}

/// Scratch overlays' ids start with this (the test writer's red check).
pub const SCRATCH_PREFIX: &str = "red-";

/// The file that marks a scratch overlay's store (never a chat's).
const SCRATCH_MARK: &str = "scratch.txt";

/// Scratch overlays a crash left: none is open at start, but in another of
/// Rovik's processes, whose stay.
fn sweep_scratch(data_dir: &Path) {
    let Ok(dirs) = std::fs::read_dir(data_dir.join("overlays")) else {
        return;
    };
    for d in dirs.flatten() {
        // Marked, or named as one (a chat's id is a bare uuid).
        let named = d.file_name().to_string_lossy().starts_with(SCRATCH_PREFIX);
        let scratch = named || d.path().join(SCRATCH_MARK).is_file();
        if scratch && !crate::os_lock::held_elsewhere(&d.path().join(MOUNTED)) {
            let _ = std::fs::remove_dir_all(d.path());
        }
    }
}

/// The lock in an overlay's store, held by the process that has it open:
/// its view mounted, or its changes open for a moment (`with_fs`).
const MOUNTED: &str = "mounted.lock";

/// Why a chat's changes can't be opened here.
pub const OPEN_ELSEWHERE: &str = "this chat's changes are open in another Rovik window";

/// The lock of the overlay store `store`, for opening it here: never in
/// two processes at once, which would each change its files unaware of
/// the other. None: the lock couldn't be made, and the store opens anyway.
fn hold_store(store: &Path) -> Result<Option<crate::os_lock::Held>, String> {
    match crate::os_lock::try_hold(&store.join(MOUNTED)) {
        Ok(Some(lock)) => Ok(Some(lock)),
        Ok(None) => Err(OPEN_ELSEWHERE.into()),
        Err(e) => {
            eprintln!("overlay: {}: {e}", store.display());
            Ok(None)
        }
    }
}

/// Whether another of Rovik's processes has the chat's changes open.
pub fn open_elsewhere(dialog_id: &str) -> bool {
    get(dialog_id).is_none()
        && store_dir(dialog_id).is_some_and(|s| crate::os_lock::held_elsewhere(&s.join(MOUNTED)))
}

/// An overlay of `folders`, mounted, registered as open: a child's, of its
/// `parent`'s view, or a scratch view.
fn mount_on(
    id: &str,
    folders: &Folders,
    limit: u64,
    parent: Option<&str>,
) -> Result<Arc<ChatOverlay>, String> {
    let store = store_dir(id).ok_or("the app's data folder isn't known yet")?;
    std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    if let Some(parent) = parent {
        std::fs::write(store.join("parent.txt"), parent).map_err(|e| e.to_string())?;
    }
    let held = hold_store(&store)?;
    mount_view(id, &store, folders, Budget::new(limit), held)
}

/// Saves each open overlay's list of changes once it changed, every
/// `SAVE_EVERY`, from a thread of its own, started once. A change saves the
/// list itself only when the last save is that old, so the changes of the
/// moment after a save waited for a later one; a crash before it lost them
/// (and the sweep at the next open their files).
fn keep_saving() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let saver = std::thread::Builder::new()
            .name("overlay-save".into())
            .spawn(|| loop {
                std::thread::sleep(super::fs::SAVE_EVERY);
                let open: Vec<Arc<ChatOverlay>> = OPEN
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .cloned()
                    .collect();
                for overlay in open {
                    overlay.fs.flush();
                }
            });
        if let Err(e) = saver {
            log::warn!("overlays: no thread to save their changes in the background: {e}");
        }
    });
}

/// F02: lands a child's changes into its parent's view (merged three ways,
/// journaled: land.rs), then removes the child. The app lands with retries
/// and keeps the copy on an error (`group::land_copy`).
#[cfg(test)]
pub fn land_child(child: &str) -> Result<Vec<String>, super::land::LandError> {
    let landed = land_child_paths(child, &[])?;
    discard_child(child);
    Ok(landed)
}

/// P1.2: lands a child's changes except at `except` (paths that conflict),
/// and keeps the child: what's left of it can land later, merged again.
pub fn land_child_except(
    child: &str,
    except: &[String],
) -> Result<Vec<String>, super::land::LandError> {
    land_child_paths(child, except)
}

fn land_child_paths(child: &str, except: &[String]) -> Result<Vec<String>, super::land::LandError> {
    use super::land::LandError;
    let o = get(child).ok_or_else(|| LandError::Failed("the worker's view isn't open".into()))?;
    let data = data_dir()
        .ok_or_else(|| LandError::Failed("the app's data folder isn't known yet".into()))?;
    // An earlier landing of it that stopped half way is finished first
    // (P1.3): landing again over it would leave the chat's view mixed.
    let parent = parent_of(child)
        .or_else(|| {
            store_dir(child)
                .and_then(|s| std::fs::read_to_string(s.join("parent.txt")).ok())
                .map(|p| p.trim().to_string())
        })
        .unwrap_or_default();
    // Its parent's view, as one folder: the composite routes a workspace
    // chat's paths to their folders.
    let into = o
        .folder()
        .ok_or_else(|| LandError::Failed("a worker's view is over one folder".into()))?
        .to_path_buf();
    if super::land::interrupted_into(&data, &parent, &into) {
        super::land::recover_into(&data, &parent, &into);
        if super::land::interrupted_into(&data, &parent, &into) {
            return Err(LandError::Failed(
                "an earlier landing into the chat's view isn't finished".into(),
            ));
        }
    }
    o.fs.flush();
    let changes: Vec<_> =
        o.fs.changes()
            .into_iter()
            .filter(|c| {
                let path = c.path().replace('\\', "/");
                !except
                    .iter()
                    .any(|e| crate::agent_run::paths::under(e, &path))
            })
            .collect();
    // P6.9: before anything is written: recovery finds it by the parent.
    if !parent.is_empty() {
        super::land::mark_into_chat(&data, child, &parent).map_err(LandError::Failed)?;
    }
    let landed = super::land::land(&data, child, &into, &changes, false)?;
    // The parent's overlay saves what came in.
    if let Some(parent) = parent_of(child).and_then(|p| get(&p)) {
        parent.fs.flush();
    }
    Ok(landed.map(|l| l.paths).unwrap_or_default())
}

/// Round 2, P1.3: a child's landing into this chat's view that stopped
/// half way is finished now, before anything removes its journal. Err: it
/// couldn't be, so nothing may land into the view or discard its copies.
pub fn finish_landing_into(dialog_id: &str) -> Result<(), String> {
    let Some(data) = data_dir() else {
        return Ok(());
    };
    if !super::land::interrupted_into(&data, dialog_id, &mountpoint(dialog_id)) {
        return Ok(());
    }
    if let Some(o) = get(dialog_id) {
        super::land::recover_into(&data, dialog_id, &o.view);
    }
    if super::land::interrupted_into(&data, dialog_id, &mountpoint(dialog_id)) {
        return Err(
            "a worker's landing into this chat's files stopped half way and couldn't be finished"
                .into(),
        );
    }
    Ok(())
}

/// Round 2, P1.2: a kept copy conflicted on `paths`: the chat's version of
/// each (in `view`) is noted by hash, to know later whether the lead has
/// merged it since.
pub fn note_conflicts(child: &str, view: &Path, paths: &[String]) {
    let Some(store) = store_dir(child) else {
        return;
    };
    let mut noted: HashMap<String, String> = std::fs::read(store.join(CONFLICTS))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for p in paths {
        noted.insert(p.clone(), content_hash(&view.join(p)));
    }
    if let Ok(json) = serde_json::to_vec(&noted) {
        let _ = std::fs::write(store.join(CONFLICTS), json);
    }
}

/// The paths a kept copy conflicted on whose version in `view` changed
/// since (the lead merged them).
pub fn merged_since(child: &str, view: &Path) -> Vec<String> {
    let noted: HashMap<String, String> = store_dir(child)
        .and_then(|s| std::fs::read(s.join(CONFLICTS)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut merged: Vec<String> = noted
        .into_iter()
        .filter(|(p, hash)| content_hash(&view.join(p)) != *hash)
        .map(|(p, _)| p)
        .collect();
    merged.sort();
    merged
}

/// Where a kept copy's conflicts are noted, in its store.
const CONFLICTS: &str = "conflicts.json";

fn content_hash(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => crate::file_guard::sha256_hex(&bytes),
        Err(_) => "absent".into(),
    }
}

/// Round 2, P1.2: a kept copy of `parent`'s, opened again after the app
/// closed (it was registered in memory only), so a follow-up goes on in it;
/// one of `parts` reopened together, which share the room (R03). None:
/// `child` has no copy kept for `parent`.
pub fn reopen_child(parent: &str, child: &str, parts: usize) -> Option<Arc<ChatOverlay>> {
    if let Some(o) = get(child) {
        return (parent_of(child).as_deref() == Some(parent)).then_some(o);
    }
    if !child_kept(parent, child) {
        return None;
    }
    open_child_shared(parent, child, parts).ok()
}

/// Whether `child`'s overlay of `parent` is still there: open, or its store
/// kept on disk (neither landed nor discarded).
pub fn child_kept(parent: &str, child: &str) -> bool {
    if get(child).is_some() {
        return parent_of(child).as_deref() == Some(parent);
    }
    store_dir(child)
        .and_then(|store| std::fs::read_to_string(store.join("parent.txt")).ok())
        .is_some_and(|saved| saved.trim() == parent)
}

/// What an app restart does to an open child: unmounted, and its parent
/// forgotten (only its store on disk is left).
#[cfg(test)]
pub fn forget_child_as_if_restarted(child: &str) {
    close(child);
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(child);
}

/// F02: a child's overlay, unmounted and removed with its landing journals.
pub fn discard_child(child: &str) {
    if !valid_id(child) {
        return;
    }
    close(child);
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(child);
    if let Some(store) = store_dir(child) {
        let _ = std::fs::remove_dir_all(store);
    }
    if let Some(data) = data_dir() {
        let _ = std::fs::remove_dir_all(data.join("landings").join(child));
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

/// The project folder `project` changed: `abs` paths in it, or
/// (`everything`) too many to list. Every open chat with that folder sees
/// untouched files as they are now; its model is told about files it had
/// looked at; a file it changed itself is merged with the new version
/// right away, and a conflict is reported now instead of when the chat
/// tries to land. A workspace chat hears of them by its own paths: the
/// folder's name, then the path in it.
pub fn project_changed(project: &Path, abs: &[PathBuf], everything: bool) -> Vec<(String, String)> {
    // Open overlays keep the canonical spelling (`open`): one look at the
    // disk per event, not two per overlay.
    let canonical = project.canonicalize().ok();
    let parts: Vec<(String, Arc<ChatOverlay>, String, Arc<OverlayFs>)> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(|(id, o)| {
            let (name, part) = o.part_on(project, canonical.as_deref())?;
            Some((id.clone(), o.clone(), name, part))
        })
        .collect();
    if everything || abs.len() > MANY {
        project_busy(project);
        check_after_quiet(project);
    }
    let mut conflicts_found = Vec::new();
    for (id, o, name, part) in parts {
        if everything || abs.len() > MANY {
            part.project_changed_everywhere();
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
                    .or_else(|_| p.strip_prefix(part.base()))
                    .ok()
            })
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .filter(|r| !r.is_empty() && !r.starts_with(".git/") && r != ".git")
            .collect();
        let (seen, touched) = part.project_changed(&rels);
        NOTES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.clone())
            .or_default()
            .changed
            .extend(seen.iter().map(|rel| in_chat(&name, rel)));
        let touched: Vec<String> = touched.iter().map(|rel| in_chat(&name, rel)).collect();
        conflicts_found.extend(
            record_conflicts(&o, &id, &touched)
                .into_iter()
                .map(|p| (id.clone(), p)),
        );
    }
    conflicts_found
}

/// A chat's path for `rel` in its folder `name` ("" for a folder chat's,
/// whose paths are its folder's own).
fn in_chat(name: &str, rel: &str) -> String {
    match name.is_empty() {
        true => rel.to_string(),
        false => format!("{name}/{rel}"),
    }
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
            let canonical = project.canonicalize().ok();
            let overlays: Vec<(String, Arc<ChatOverlay>)> = OPEN
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, o)| o.part_on(&project, canonical.as_deref()).is_some())
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
        holder.chars().take(8).collect::<String>()
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
/// in the last moments, or a git operation is still going. A git operation
/// holds the repository's working folder, and a checkout there rewrites every
/// folder in it: a hold counts for the folder itself, for one above it and for
/// one below it (D10).
pub fn is_busy(project: &Path) -> bool {
    let key = busy_key(project);
    let busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    busy.iter().any(|(held, (at, count))| {
        (*count > 0 || at.elapsed() < QUIET) && (key.starts_with(held) || held.starts_with(&key))
    })
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
    let theirs = read_in(&o.folders(), rel);
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
        parts.push(markers_note(&files));
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

/// One chat's clashes in one folder: by path in the folder, the other
/// chats and whether on the same lines.
type FolderClashes = BTreeMap<String, (BTreeSet<String>, bool)>;
/// Live clashes, by chat and folder (its canonical path): the folder's name
/// in the chat ("" for a folder chat's) and the chat's clashes there.
type PeerRows = HashMap<(String, PathBuf), (String, FolderClashes)>;
static PEERS: LazyLock<Mutex<PeerRows>> = LazyLock::new(Default::default);

/// Each open chat's generation, by its id.
type Generations = Vec<(String, u64)>;

/// Per folder, each open chat's generation there when its chats were last
/// compared.
static LAST_COMPARED: LazyLock<Mutex<HashMap<PathBuf, Generations>>> =
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

/// This chat's current clashes with other open chats, by its paths.
pub fn peer_conflicts(dialog_id: &str) -> Vec<PeerConflict> {
    let peers = PEERS.lock().unwrap_or_else(|e| e.into_inner());
    let mut all: BTreeMap<String, PeerConflict> = BTreeMap::new();
    for ((id, _), (name, row)) in peers.iter() {
        if id != dialog_id {
            continue;
        }
        for (path, (others, same_lines)) in row {
            let path = in_chat(name, path);
            all.insert(
                path.clone(),
                PeerConflict {
                    path,
                    peers: others.iter().cloned().collect(),
                    same_lines: *same_lines,
                },
            );
        }
    }
    all.into_values().collect()
}

/// Compares the open chats that have `project` among their folders with
/// each other, by path in it, and keeps the result for that folder: a
/// workspace chat's other folders keep theirs. Only files two chats both
/// changed are merged.
fn refresh_project_peers(project: &Path) {
    let key = busy_key(project);
    let on_project: Vec<(String, String, Arc<OverlayFs>)> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(|(id, o)| {
            let (name, part) = o.part_on(project, Some(&key))?;
            Some((id.clone(), name, part))
        })
        .collect();
    // Nothing changed in any of its chats there since the last comparison
    // (a read, a search): the clashes are as they were.
    let mut seen: Vec<(String, u64)> = on_project
        .iter()
        .map(|(id, _, part)| (id.clone(), part.generation()))
        .collect();
    seen.sort();
    {
        let mut last = LAST_COMPARED.lock().unwrap_or_else(|e| e.into_inner());
        if last.get(&key).is_some_and(|l| *l == seen) {
            return;
        }
        last.insert(key.clone(), seen);
    }
    let snapshots: Vec<(String, Arc<OverlayFs>, Vec<OverlayChange>)> = on_project
        .iter()
        .map(|(id, _, part)| (id.clone(), part.clone(), landable(part.changes(), "")))
        .collect();
    let mut rows: HashMap<String, (String, FolderClashes)> = on_project
        .iter()
        .map(|(id, name, _)| (id.clone(), (name.clone(), BTreeMap::new())))
        .collect();
    for i in 0..snapshots.len() {
        for j in (i + 1)..snapshots.len() {
            let (a_id, a_part, a_changes) = &snapshots[i];
            let (b_id, b_part, b_changes) = &snapshots[j];
            let b_paths: BTreeSet<&str> = b_changes.iter().map(|c| c.path()).collect();
            let shared: BTreeSet<&str> = a_changes
                .iter()
                .map(|c| c.path())
                .filter(|p| b_paths.contains(p))
                .collect();
            for rel in shared {
                let (Some(a), Some(b)) = (
                    side_of(a_changes, a_part.base(), rel),
                    side_of(b_changes, b_part.base(), rel),
                ) else {
                    continue;
                };
                if let Some(same_lines) = clash(&a, &b) {
                    for (me, other) in [(a_id, b_id), (b_id, a_id)] {
                        if let Some((_, row)) = rows.get_mut(me) {
                            let cell = row.entry(rel.to_string()).or_default();
                            cell.0.insert(other.clone());
                            cell.1 |= same_lines;
                        }
                    }
                }
            }
        }
    }
    let mut peers = PEERS.lock().unwrap_or_else(|e| e.into_inner());
    // A chat no longer open on the folder has no clashes there.
    peers.retain(|(id, at), _| *at != key || rows.contains_key(id));
    for (id, row) in &rows {
        peers.insert((id.clone(), key.clone()), row.clone());
    }
    drop(peers);
    // Forget what's no longer true in this folder, so a new clash is told
    // again; what was told of the chat's other folders stays.
    let mut told = TOLD_PEERS.lock().unwrap_or_else(|e| e.into_inner());
    for (id, (name, row)) in &rows {
        let live: BTreeSet<String> = row
            .iter()
            .flat_map(|(path, (others, _))| {
                others.iter().map(|o| told_key(&in_chat(name, path), o))
            })
            .collect();
        let here = |k: &str| name.is_empty() || k.starts_with(&format!("{name}/"));
        if let Some(entry) = told.get_mut(id) {
            entry.retain(|k| live.contains(k) || !here(k));
        }
    }
}

/// Drops a chat's clash state (discarded, or landed and gone).
fn drop_peer_state(dialog_id: &str) {
    PEERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(id, _), _| id != dialog_id);
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

/// Compares this chat with the other open chats on each of its folders
/// (after each of its tool calls); returns its clashes.
pub fn check_peer_conflicts(dialog_id: &str) -> Vec<PeerConflict> {
    if let Some(o) = get(dialog_id) {
        for folder in o.folders().iter() {
            refresh_project_peers(&folder.path);
        }
    }
    peer_conflicts(dialog_id)
}

/// The clashes this chat's model hasn't heard about yet, as one note.
fn fresh_peer_note(dialog_id: &str) -> Option<String> {
    let current = peer_conflicts(dialog_id);
    if current.is_empty() {
        return None;
    }
    let mut told = TOLD_PEERS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = told.entry(dialog_id.to_string()).or_default();
    let mut same = Vec::new();
    let mut file_level = Vec::new();
    for clash in &current {
        let path = &clash.path;
        let new_here: Vec<&String> = clash
            .peers
            .iter()
            .filter(|o| !entry.contains(&told_key(path, o)))
            .collect();
        if new_here.is_empty() {
            continue;
        }
        for o in &new_here {
            entry.insert(told_key(path, o));
        }
        if clash.same_lines {
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
        return landable(o.fs.changes(), "");
    }
    // Not open: read from its saved list, with its gate held so the chat's
    // own overlay isn't opened on the same files meanwhile.
    let gate = gate(dialog_id);
    let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        return landable(o.fs.changes(), "");
    }
    let Some(store) = store_dir(dialog_id) else {
        return Vec::new();
    };
    stored_changes(&store)
}

/// A closed chat's landable changes, from its store: each folder's
/// overlay opened from its saved list, or, for a workspace chat's folder
/// that isn't there, the list as it was saved.
fn stored_changes(store: &Path) -> Vec<OverlayChange> {
    let Some(folders) = recorded(store) else {
        return Vec::new();
    };
    if let Some(project) = folders.single() {
        return match OverlayFs::open(project, store, DEFAULT_LIMIT) {
            Ok(fs) => landable(fs.changes(), ""),
            Err(_) => Vec::new(),
        };
    }
    folders
        .iter()
        .flat_map(|f| {
            let part = part_store(store, &f.name);
            let changes = match f.path.is_dir() {
                true => OverlayFs::open(&f.path, &part, DEFAULT_LIMIT).map(|fs| fs.changes()),
                false => super::fs::saved_changes(&part),
            };
            landable(changes.unwrap_or_default(), &f.name)
        })
        .collect()
}

/// The chat paths of a workspace chat's changes in its folder `name` that
/// haven't landed (D19, D32): from its view while it's open, else from that
/// folder's saved list, which needs nothing of the folder, so it answers
/// while the folder is missing. Empty for a chat with nothing there; an
/// error when the list can't be read.
pub fn changes_in_folder(dialog_id: &str, name: &str) -> Result<Vec<String>, String> {
    if !valid_id(dialog_id) || !valid_id(name) {
        return Err(format!("{name}: not a folder of a chat"));
    }
    let gate = gate(dialog_id);
    let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
    changes_there(dialog_id, name)
}

/// [`changes_in_folder`], the chat's gate held.
fn changes_there(dialog_id: &str, name: &str) -> Result<Vec<String>, String> {
    let changes =
        match get(dialog_id) {
            Some(o) => match &o.under {
                Under::Workspace { view, .. } => view.part(name).map(|p| p.changes()),
                Under::Folder(_) => None,
            },
            None => {
                let part = store_dir(dialog_id).map(|s| part_store(&s, name));
                match part.filter(|p| p.is_dir()) {
                    Some(part) => Some(super::fs::saved_changes(&part).map_err(|e| {
                        format!("this chat's changes in {name} can't be read ({e})")
                    })?),
                    None => None,
                }
            }
        };
    Ok(landable(changes.unwrap_or_default(), name)
        .iter()
        .map(|c| c.path().to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    /// A test process's views are its own: another's, of a chat named the
    /// same, mounts elsewhere.
    #[test]
    fn a_test_processs_views_go_in_a_folder_of_its_own() {
        if crate::vfs::mounts_only_under_volumes() {
            return;
        }
        let view = super::mountpoint("chat-1");
        let own = format!("test-{}", std::process::id());
        let folder = view.parent().and_then(|p| p.file_name());
        assert_eq!(folder, Some(std::ffi::OsStr::new(&own)), "{view:?}");
        assert!(super::is_chat_view(&view));
    }

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
    /// Parallel groups whose merge can be thrown away and redone one step
    /// at a time (R06), while the chat's changes wait to land.
    pub redo_groups: Vec<String>,
    /// A remote chat's last sync with the project (6.7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<rovik_protocol::work::ChatSync>,
    /// A remote chat's newer result, held back while its conflicts are
    /// resolved (6.5): the bar offers it in their place.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub newer_result: bool,
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
pub async fn overlay_status(dialog_id: String) -> Result<OverlayStatus, String> {
    crate::rt::spawn_blocking(move || status(dialog_id))
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
        redo_groups: if isolated && pending(&dialog_id).is_some() {
            saved_groups(&dialog_id)
                .into_iter()
                .filter(|g| !g.redone && g.unavailable.is_none())
                .map(|g| g.name)
                .collect()
        } else {
            Vec::new()
        },
        sync: None,
        newer_result: false,
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
pub async fn overlay_file(dialog_id: String, path: String) -> Result<FileVersions, String> {
    crate::rt::spawn_blocking(move || file_versions(dialog_id, path))
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
    let folders = folders_of(&dialog_id).ok_or("this chat has no isolated changes")?;
    let in_project = |rel: &str| {
        read_in(&folders, rel).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
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
/// Its subagents run inside its run (agent_run/subagent_run.rs), so they
/// count: Stop ends them with it (S15).
fn not_running(agent: &crate::agent_run::AgentState, dialog_id: &str) -> Result<(), String> {
    if agent.is_running(dialog_id) {
        let helpers = agent.running_children(dialog_id);
        return Err(if helpers == 0 {
            "this chat's run is still going: stop it first".into()
        } else {
            format!(
                "this chat's run is still going, with {helpers} subagent{} at work: stop it first",
                if helpers == 1 { "" } else { "s" }
            )
        });
    }
    Ok(())
}

pub async fn overlay_discard(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
) -> Result<(), String> {
    not_running(agent, &dialog_id)?;
    agent.drop_carry(&dialog_id);
    crate::rt::spawn_blocking(move || discard(&dialog_id))
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

/// The folders a chat's changes are in: its open view's, else as its store
/// recorded them.
fn folders_of(dialog_id: &str) -> Option<Folders> {
    get(dialog_id)
        .map(|o| o.folders())
        .or_else(|| recorded(&store_dir(dialog_id)?))
}

/// The project's version of a chat path: its file in its folder now (None:
/// not there, or in none of the chat's folders).
fn read_in(folders: &Folders, path: &str) -> Option<Vec<u8>> {
    std::fs::read(folders.abs(path)?).ok()
}

/// The other open chats on each of `folders` hear that `paths` (chat
/// paths) changed there.
fn tell_others(folders: &Folders, paths: &[String]) {
    for folder in folders.iter() {
        let abs: Vec<PathBuf> = paths
            .iter()
            .filter(|p| folders.split(p).is_some_and(|(f, _)| f.name == folder.name))
            .filter_map(|p| folders.abs(p))
            .collect();
        if !abs.is_empty() {
            project_changed(&folder.path, &abs, false);
        }
    }
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
    let folders = folders_of(dialog_id)
        .ok_or_else(|| LandError::Failed("this chat has no isolated changes".into()))?;
    // A child's landing into this chat's view that a crash interrupted is
    // finished first, which needs the view: landing the half of it that
    // got in would put half a worker's step in the project.
    // P1.3: whether the view is open or not (a group's landing can fail
    // half way while the chat runs).
    if super::land::interrupted_into(&data, dialog_id, &mountpoint(dialog_id)) {
        match get(dialog_id) {
            Some(o) => {
                super::land::recover_into(&data, dialog_id, &o.view);
            }
            None => {
                open(dialog_id, &folders, Box::new(|_, _| {})).map_err(|e| {
                    LandError::Failed(format!(
                        "a step that was landing into this chat when the app closed isn't finished, and the chat's view couldn't be opened to finish it ({e})"
                    ))
                })?;
            }
        }
        if super::land::interrupted_into(&data, dialog_id, &mountpoint(dialog_id)) {
            return Err(LandError::Failed(
                "a step that was landing into this chat when the app closed couldn't be finished"
                    .into(),
            ));
        }
    }
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
    match super::land::land_checked_in(
        &data,
        dialog_id,
        &folders,
        &changes,
        keep_chat,
        keep_chat.then_some(known_paths.as_slice()),
        allow_merge,
    ) {
        Ok(landed) => {
            let _ = discard(dialog_id);
            // Other chats on its folders hear about it now, not only when
            // a folder's watcher (if it's open in the editor) does.
            if let Some(l) = &landed {
                tell_others(&folders, &l.paths);
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

pub async fn overlay_land(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
    keep_chat: bool,
) -> Result<LandResult, String> {
    not_running(agent, &dialog_id)?;
    Ok(land_command(dialog_id, keep_chat).await)
}

async fn land_command(dialog_id: String, keep_chat: bool) -> LandResult {
    let result = crate::rt::spawn_blocking(move || land_now(&dialog_id, keep_chat)).await;
    let result = result.unwrap_or_else(|e| Err(super::land::LandError::Failed(e.to_string())));
    LandResult::from(result)
}

impl From<Result<Option<super::land::Landed>, super::land::LandError>> for LandResult {
    /// A landing as the bar shows it.
    fn from(result: Result<Option<super::land::Landed>, super::land::LandError>) -> LandResult {
        match result {
            Ok(landed) => LandResult {
                landed,
                conflicts: Vec::new(),
                error: None,
            },
            Err(super::land::LandError::Conflicts(c)) => LandResult {
                landed: None,
                conflicts: c,
                error: None,
            },
            Err(e) => LandResult {
                landed: None,
                conflicts: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }
}

/// Undoes a landing. Returns the files left as they are because they
/// changed since. Refused while the chat's run is going, like landing.
pub async fn overlay_undo(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
    id: String,
) -> Result<Vec<super::land::Conflict>, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || {
        if !valid_id(&dialog_id) {
            return Err("bad chat id".to_string());
        }
        let data = data_dir().ok_or("the app's data folder isn't known yet")?;
        let folders = super::land::folders_of(&data, &dialog_id).ok_or("no landing to undo")?;
        let paths = super::land::latest(&data, &dialog_id)
            .filter(|(l, _, _)| *l == id)
            .map(|(_, p, _)| p)
            .unwrap_or_default();
        let kept = super::land::undo_in(&data, &dialog_id, &folders, &id)?;
        // Other chats on each folder see its files as they are again.
        tell_others(&folders, &paths);
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

/// The chat's overlay, open or (between runs, after a restart) opened from
/// its saved changes for this one call. Its gate stays held while a saved
/// one is used, so the chat's own overlay isn't opened on the same files
/// meanwhile: `f` mustn't open this chat's overlay.
fn with_fs<T>(dialog_id: &str, f: impl FnOnce(&dyn ChatFs) -> T) -> Result<T, String> {
    if let Some(o) = get(dialog_id) {
        return Ok(f(&*o.fs));
    }
    let gate = gate(dialog_id);
    let held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = get(dialog_id) {
        drop(held);
        return Ok(f(&*o.fs));
    }
    let store = store_dir(dialog_id)
        .filter(|s| s.join("manifest.json").is_file() || s.join(FOLDERS_FILE).is_file())
        .ok_or("this chat has no isolated changes")?;
    let folders = recorded(&store).ok_or("this chat has no isolated changes")?;
    let lock = hold_store(&store)?;
    let fs = open_saved(&store, &folders)?;
    let out = f(&*fs);
    fs.flush();
    drop(fs);
    drop(lock);
    drop(held);
    Ok(out)
}

/// A closed chat's changes opened from its store for one call, unmounted:
/// a folder chat's overlay, or a workspace chat's view of those of its
/// folders that are there.
fn open_saved(store: &Path, folders: &Folders) -> Result<Arc<dyn ChatFs>, String> {
    if let Some(project) = folders.single() {
        let fs = OverlayFs::open(project, store, DEFAULT_LIMIT).map_err(|e| e.to_string())?;
        return Ok(Arc::new(fs));
    }
    let there: Vec<Folder> = folders
        .iter()
        .filter(|f| f.path.is_dir())
        .cloned()
        .collect();
    let there = Folders::named(there).map_err(|_| "none of this chat's folders is there")?;
    let view = Arc::new(CompositeFs::new(Budget::new(DEFAULT_LIMIT)));
    let budget = view.budget();
    view.matched(&there, |folder| open_part(store, folder, &budget))
        .map_err(|e| format!("couldn't open the chat's changes in {e}"))?;
    Ok(view)
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
    let folders = folders_of(dialog_id).ok_or("this chat has no isolated changes")?;
    // Handed to the chat with markers: the conflict is between the chat's
    // own version (kept) and the project's.
    if marked(dialog_id).iter().any(|m| m == path) {
        if let Some((chat, base)) = kept_versions(dialog_id, path) {
            return Ok(Sides {
                base,
                chat,
                project: read_in(&folders, path),
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
        } => (read_in(&folders, o), None),
        _ => (None, None),
    };
    Ok(Sides {
        base,
        chat,
        project: read_in(&folders, path),
    })
}

/// The chat's version of `path` as it is now in its view.
fn current_version(dialog_id: &str, path: &str) -> Option<Vec<u8>> {
    let folders = folders_of(dialog_id)?;
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
        } => read_in(&folders, &o),
        _ => None,
    }
}

/// A chat's conflicted files as the resolver showed them: the project's
/// version of each, by path (None: not in the project then).
type ShownFiles = HashMap<String, Option<Vec<u8>>>;

/// The project's version of a conflicted file as the resolver last showed
/// it, per chat and path (None: not in the project then). A resolution is
/// made against it, so it's what the resolved file is merged against when
/// landing: if the project changed the file again meanwhile, that change
/// is merged in or reported as a new conflict, not dropped.
static SHOWN: LazyLock<Mutex<HashMap<String, ShownFiles>>> = LazyLock::new(Default::default);

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
pub fn conflict_file(dialog_id: &str, path: &str) -> Result<crate::merge::ConflictFile, String> {
    let s = sides(dialog_id, path)?;
    note_shown(dialog_id, path, s.project.as_deref());
    Ok(conflict_file_of(path, &s))
}

/// [`conflict_file`], from the three versions as read.
fn conflict_file_of(path: &str, s: &Sides) -> crate::merge::ConflictFile {
    let binary = [&s.base, &s.chat, &s.project]
        .iter()
        .any(|b| b.as_deref().is_some_and(crate::merge::is_binary));
    let text = |b: &Option<Vec<u8>>| {
        b.as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    };
    let segments = if binary {
        Vec::new()
    } else {
        markers::view(s.base.as_deref(), s.project.as_deref(), s.chat.as_deref())
    };
    crate::merge::ConflictFile {
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

pub(super) fn set_marked(dialog_id: &str, mut files: Vec<String>) {
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
    // The chat's markers show the lines before either change too.
    let text = markers::text(&markers::segments(
        s.base.as_deref(),
        s.project.as_deref(),
        s.chat.as_deref(),
    ));
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
    let (left, resolved): (Vec<String>, Vec<String>) = all
        .into_iter()
        .partition(|path| current_version(dialog_id, path).is_some_and(|c| markers::left_in(&c)));
    for path in &resolved {
        forget_kept(dialog_id, path);
    }
    set_marked(dialog_id, left.clone());
    left
}

/// What the chat's model is told of files marked for it, in a note.
pub fn markers_note(files: &[String]) -> String {
    resolve_request(files).replace("\n\n", " ")
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
/// at most, for the chat's model: what the project changed in a file its
/// merge took, here or on a server (6.6).
pub fn short_diff(old: &[u8], new: &[u8]) -> String {
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
        entry.written = Some(crate::file_guard::sha256_hex(bytes));
        let _ = save_pre_merge(dialog_id, &kept);
    }
}

/// A file's two kept versions: the chat's, and its base (None: none).
type KeptVersions = (Option<Vec<u8>>, Option<Vec<u8>>);

/// The kept version of `path`, if there is one: (the chat's, its base).
fn kept_versions(dialog_id: &str, path: &str) -> Option<KeptVersions> {
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
            Some(w) => current_version(dialog_id, &path)
                .is_some_and(|c| crate::file_guard::sha256_hex(&c) == *w),
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
    if let Some(folders) = folders_of(dialog_id) {
        let plan = super::land::plan_in(&folders, &changes(dialog_id), false);
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

pub async fn overlay_undo_merge(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
) -> Result<UndoneMerge, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || undo_merge(&dialog_id, None))
        .await
        .map_err(|e| e.to_string())?
}

/// Takes markers just handed to the chat back out (it couldn't take them):
/// only those files, nothing merged before.
pub async fn overlay_unmark(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
    files: Vec<String>,
) -> Result<UndoneMerge, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || undo_merge(&dialog_id, Some(&files)))
        .await
        .map_err(|e| e.to_string())?
}

pub async fn overlay_conflict_file(
    dialog_id: String,
    path: String,
) -> Result<crate::merge::ConflictFile, String> {
    crate::rt::spawn_blocking(move || conflict_file(&dialog_id, &path))
        .await
        .map_err(|e| e.to_string())?
}

/// Refused while the chat's run goes: it may be changing the same file.
pub async fn overlay_conflict_resolve(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
    path: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<Vec<super::land::Conflict>, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || {
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
pub async fn overlay_conflicts_to_chat(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
) -> Result<HandedOver, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || {
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

// ---------------------------------------------------------------------------
// Redo sequentially (R06)
// ---------------------------------------------------------------------------

/// What a path was in the chat just before a group's join, so the join can
/// be undone exactly: bytes, not text, and the base the chat's own version
/// is merged against when it lands.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "was", rename_all = "camelCase")]
pub enum Before {
    /// The chat hadn't changed it: the project's version (or nothing) shows.
    Untouched,
    /// The chat's own file. `content` and `base` name files in the group's
    /// folder; `base`: what it's merged against (None: the chat created it).
    File {
        content: String,
        base: Option<String>,
    },
    /// The chat's own folder (created, or moved there).
    Dir,
    /// Not there: the chat had deleted it.
    Absent,
    /// Something a join doesn't change (a symlink): left as it is.
    Other,
}

/// A parallel group joined into the chat: what its paths were before the
/// join, so the merge can be thrown away and its steps redone one at a time.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedGroup {
    pub name: String,
    pub plan_path: String,
    pub steps: Vec<usize>,
    /// Each path the join wrote (files and folders), as it was before.
    pub before: Vec<(String, Before)>,
    /// Redo sequentially was used: the group's steps run one at a time now.
    pub redone: bool,
    /// Why Redo sequentially can't be offered (P1.4): what a path was before
    /// couldn't be read. Never recorded as untouched instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

fn groups_file(dialog_id: &str) -> Option<PathBuf> {
    store_dir(dialog_id).map(|s| s.join("groups.json"))
}

/// Where the snapshots' bytes are kept.
fn group_blobs(dialog_id: &str) -> Option<PathBuf> {
    store_dir(dialog_id).map(|s| s.join("groups"))
}

/// How many saved versions the chat's groups keep (tests).
#[cfg(test)]
pub fn group_blob_count(dialog_id: &str) -> usize {
    group_blobs(dialog_id)
        .and_then(|d| std::fs::read_dir(d).ok())
        .map_or(0, |d| d.count())
}

pub fn saved_groups(dialog_id: &str) -> Vec<SavedGroup> {
    groups_file(dialog_id)
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_groups(dialog_id: &str, groups: &[SavedGroup]) -> Result<(), String> {
    let file = groups_file(dialog_id).ok_or("this chat has no isolated changes")?;
    let bytes = serde_json::to_vec(groups).map_err(|e| e.to_string())?;
    crate::atomic_file::write_atomic(&file, &bytes).map_err(|e| e.to_string())
}

/// Keeps bytes in the chat's group folder; returns their file's name.
fn keep_blob(dialog_id: &str, bytes: &[u8]) -> Result<String, String> {
    let dir = group_blobs(dialog_id).ok_or("this chat has no isolated changes")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = uuid::Uuid::new_v4().to_string();
    std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string())?;
    Ok(name)
}

fn read_blob(dialog_id: &str, name: &str) -> Result<Vec<u8>, String> {
    if !valid_id(name) {
        return Err(format!("{name}: not a saved version"));
    }
    let dir = group_blobs(dialog_id).ok_or("this chat has no isolated changes")?;
    std::fs::read(dir.join(name)).map_err(|e| format!("a saved version is missing ({e})"))
}

/// What `paths` are in the chat now, before a join writes them (R06).
/// Call it before each worker's copy lands.
pub fn snapshot(dialog_id: &str, paths: &[String]) -> Result<Vec<(String, Before)>, String> {
    snapshot_each(dialog_id, paths)?
        .into_iter()
        .map(|(p, b)| b.map(|b| (p, b)))
        .collect()
}

/// Each path with what it was, or why that couldn't be read.
pub type PathSnapshots = Vec<(String, Result<Before, String>)>;

/// `snapshot`, path by path (P1.4): one that can't be read is its own
/// error, not the whole snapshot's.
pub fn snapshot_each(dialog_id: &str, paths: &[String]) -> Result<PathSnapshots, String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let changes = changes(dialog_id);
    let folders = folders_of(dialog_id).ok_or("this chat has no isolated changes")?;
    let mut out = Vec::new();
    for path in paths {
        let before = (|| -> Result<Before, String> {
            check_rel(path)?;
            let own = changes.iter().rev().find(|c| c.path() == path);
            // Inside a folder the chat deleted (and not written since): gone.
            let under_deleted = || {
                changes.iter().any(|c| {
                    matches!(c, OverlayChange::Delete { path: d, .. } if path.starts_with(&format!("{d}/")))
                })
            };
            Ok(match own {
                Some(OverlayChange::Delete { .. }) => Before::Absent,
                Some(OverlayChange::Write {
                    kind,
                    upper,
                    origin,
                    ..
                }) => match kind {
                    crate::vfs::store::Kind::Dir => Before::Dir,
                    crate::vfs::store::Kind::Symlink => Before::Other,
                    crate::vfs::store::Kind::File => {
                        let bytes = match (upper, origin) {
                            (Some(file), _) => {
                                std::fs::read(file).map_err(|e| format!("{path}: {e}"))?
                            }
                            (None, Some(o)) => {
                                let from = folders.abs(o).ok_or_else(|| {
                                    format!("{path}: {o} is in none of the chat's folders")
                                })?;
                                std::fs::read(from).map_err(|e| format!("{path}: {e}"))?
                            }
                            (None, None) => Vec::new(),
                        };
                        let base = with_fs(dialog_id, |fs| fs.versions(path))?
                            .map(|(_, base)| {
                                std::fs::read(base).map_err(|e| format!("{path}: {e}"))
                            })
                            .transpose()?;
                        Before::File {
                            content: keep_blob(dialog_id, &bytes)?,
                            base: base.map(|b| keep_blob(dialog_id, &b)).transpose()?,
                        }
                    }
                },
                None if under_deleted() => Before::Absent,
                None => Before::Untouched,
            })
        })();
        out.push((path.clone(), before));
    }
    Ok(out)
}

/// A snapshot's saved versions that weren't kept for a group: they go.
pub fn forget_snapshot(dialog_id: &str, unused: &[Before]) {
    let Some(dir) = group_blobs(dialog_id) else {
        return;
    };
    for b in unused {
        if let Before::File { content, base } = b {
            for name in std::iter::once(content).chain(base.iter()) {
                if valid_id(name) {
                    let _ = std::fs::remove_file(dir.join(name));
                }
            }
        }
    }
}

/// P1.4: Redo sequentially can't put the group back as the chat had it
/// (what a path was couldn't be read): it isn't offered, and says why.
pub fn mark_redo_unavailable(dialog_id: &str, name: &str, reason: &str) -> Result<(), String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = saved_groups(dialog_id);
    let Some(group) = all.iter_mut().find(|g| g.name == name) else {
        return Ok(());
    };
    if group.unavailable.is_none() {
        group.unavailable = Some(reason.to_string());
    }
    write_groups(dialog_id, &all)
}

/// Keeps a joined group. A later join of the same name adds the paths it
/// wrote that the first didn't; what a path was before the first stays.
pub fn save_group(dialog_id: &str, group: SavedGroup) -> Result<(), String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = saved_groups(dialog_id);
    match all.iter_mut().find(|g| g.name == group.name) {
        Some(g) => {
            let mut before = g.before.clone();
            let mut superseded = Vec::new();
            for (path, was) in group.before {
                if !before.iter().any(|(p, _)| *p == path) {
                    before.push((path, was));
                } else {
                    superseded.push(was);
                }
            }
            *g = SavedGroup { before, ..group };
            write_groups(dialog_id, &all)?;
            // P6.8: versions of a path the group already had aren't kept.
            forget_snapshot(dialog_id, &superseded);
            return Ok(());
        }
        None => all.push(group),
    }
    write_groups(dialog_id, &all)
}

/// Adds paths written after a group's join (by the repair loop's fixers)
/// to what Redo sequentially puts back; a path it has already stays as
/// it was before the group.
pub fn add_group_paths(
    dialog_id: &str,
    name: &str,
    entries: Vec<(String, Before)>,
) -> Result<(), String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = saved_groups(dialog_id);
    let Some(group) = all.iter_mut().find(|g| g.name == name) else {
        return Ok(());
    };
    let mut superseded = Vec::new();
    for (path, was) in entries {
        if !group.before.iter().any(|(p, _)| *p == path) {
            group.before.push((path, was));
        } else {
            superseded.push(was);
        }
    }
    write_groups(dialog_id, &all)?;
    forget_snapshot(dialog_id, &superseded);
    Ok(())
}

/// Whether the group is being redone one step at a time.
pub fn group_redone(dialog_id: &str, name: &str) -> bool {
    saved_groups(dialog_id)
        .iter()
        .any(|g| g.name == name && g.redone)
}

/// Makes the folders up to `rel`'s parent in the chat's view.
fn make_parents(fs: &dyn ChatFs, rel: &str) -> Result<(), String> {
    use crate::vfs::backend::Backend;
    let mut at = String::new();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        let parent = Backend::resolve(fs, &at).map_err(|e| format!("{rel}: {e:?}"))?;
        at = if at.is_empty() {
            part.to_string()
        } else {
            format!("{at}/{part}")
        };
        match Backend::resolve(fs, &at) {
            Ok(_) => {}
            Err(crate::vfs::store::VfsError::NotFound) => {
                Backend::create(
                    fs,
                    parent,
                    part,
                    crate::vfs::store::Kind::Dir,
                    0o755,
                    0,
                    0,
                    None,
                )
                .map_err(|e| format!("{at}: {e:?}"))?;
            }
            Err(e) => return Err(format!("{at}: {e:?}")),
        }
    }
    Ok(())
}

/// Puts one path back as it was before the join; a note when it couldn't
/// be, entirely.
fn restore_path(
    dialog_id: &str,
    fs: &dyn ChatFs,
    folders: &Folders,
    path: &str,
    was: &Before,
) -> Result<Option<String>, String> {
    use crate::vfs::backend::Backend;
    use crate::vfs::store::VfsError;
    let in_project = folders.abs(path);
    let now = Backend::resolve(fs, path)
        .ok()
        .and_then(|ino| Backend::getattr(fs, ino).ok())
        .map(|a| a.kind);
    let remove_dir = |path: &str| -> Result<Option<String>, String> {
        let (parent, name) =
            Backend::resolve_parent(fs, path).map_err(|e| format!("{path}: {e:?}"))?;
        match Backend::rmdir(fs, parent, name) {
            Ok(()) | Err(VfsError::NotFound) => Ok(None),
            Err(VfsError::NotEmpty) => Ok(Some(format!(
                "`{path}/` was kept: it has files the group didn't write"
            ))),
            Err(e) => Err(format!("{path}: {e:?}")),
        }
    };
    match was {
        Before::Other => Ok(None),
        Before::Dir => {
            if now != Some(crate::vfs::store::Kind::Dir) {
                make_parents(fs, &format!("{path}/x"))?;
            }
            Ok(None)
        }
        Before::File { content, base } => {
            if now == Some(crate::vfs::store::Kind::Dir) {
                if let Some(note) = remove_dir(path)? {
                    return Ok(Some(note));
                }
            }
            make_parents(fs, path)?;
            let content = read_blob(dialog_id, content)?;
            let base = base
                .as_deref()
                .map(|b| read_blob(dialog_id, b))
                .transpose()?;
            fs.rebase_file(path, Some(&content), base.as_deref())
                .map_err(|e| format!("{path}: {e:?}"))?;
            Ok(None)
        }
        Before::Untouched | Before::Absent => {
            let project_file = in_project
                .as_ref()
                .filter(|p| p.is_file())
                .and_then(|p| std::fs::read(p).ok());
            let project_dir = in_project.as_ref().is_some_and(|p| p.is_dir());
            match now {
                Some(crate::vfs::store::Kind::Dir) => {
                    if project_dir && matches!(was, Before::Untouched) {
                        Ok(None)
                    } else {
                        remove_dir(path)
                    }
                }
                None if matches!(was, Before::Untouched) && project_file.is_none() => Ok(None),
                _ => {
                    match (was, project_file) {
                        // The project's file as it is now, and nothing to
                        // land: the chat leaves it alone, as before.
                        (Before::Untouched, Some(bytes)) => {
                            make_parents(fs, path)
                                .and_then(|_| {
                                    if fs.revert_file(path).map_err(|e| format!("{e:?}"))? {
                                        return Ok(());
                                    }
                                    // Deleted or replaced by the group: back
                                    // first, then the project's again.
                                    fs.rebase_file(path, Some(&bytes), Some(&bytes))
                                        .and_then(|_| fs.revert_file(path).map(|_| ()))
                                        .map_err(|e| format!("{e:?}"))
                                })
                                .map_err(|e| format!("{path}: {e}"))?;
                            return Ok(None);
                        }
                        // The chat had deleted it: deleted again, against
                        // the project's file as it is now.
                        (Before::Absent, Some(bytes)) => fs.rebase_file(path, None, Some(&bytes)),
                        _ => fs.rebase_file(path, None, None),
                    }
                    .map_err(|e| format!("{path}: {e:?}"))?;
                    Ok(None)
                }
            }
        }
    }
}

/// Throws the group's merge away (each path it wrote goes back to what the
/// chat had before the join, with the base it lands against) and returns
/// the request that has its steps redone by step workers, one at a time.
/// Nothing is marked redone unless every path went back.
pub fn redo_group(dialog_id: &str, name: &str) -> Result<String, String> {
    let lock = state_lock(dialog_id);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = saved_groups(dialog_id);
    let group = all
        .iter_mut()
        .find(|g| g.name == name)
        .ok_or_else(|| format!("no parallel group \"{name}\" in this chat"))?;
    if let Some(why) = &group.unavailable {
        return Err(format!(
            "the group \"{name}\" can't be redone: what the chat had before it isn't known ({why})"
        ));
    }
    if group.redone {
        return Err(format!("the group \"{name}\" was redone already"));
    }
    let folders = folders_of(dialog_id).ok_or("this chat has no isolated changes")?;
    // Files before folders, deepest first: a folder the group made is
    // empty by the time it's removed.
    let mut order: Vec<&(String, Before)> = group.before.iter().collect();
    order.sort_by_key(|(p, was)| {
        (
            matches!(was, Before::Untouched | Before::Absent)
                && !folders.abs(p).is_some_and(|a| a.is_file()),
            std::cmp::Reverse(p.matches('/').count()),
        )
    });
    let mut notes = Vec::new();
    let mut failed = Vec::new();
    for (path, _) in &order {
        check_rel(path)?;
    }
    // P6.8: the chat's overlay opened once for every path.
    let restored = with_fs(dialog_id, |fs| {
        order
            .iter()
            .map(|(path, was)| restore_path(dialog_id, fs, &folders, path, was))
            .collect::<Vec<_>>()
    })?;
    for r in restored {
        match r {
            Ok(Some(note)) => notes.push(note),
            Ok(None) => {}
            Err(e) => failed.push(e),
        }
    }
    if !failed.is_empty() {
        return Err(format!(
            "the group's changes couldn't all be undone: {}. Try again, or Discard this chat's changes.",
            failed.join("; ")
        ));
    }
    group.redone = true;
    let steps = group
        .steps
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let files = group
        .before
        .iter()
        .map(|(p, _)| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut request = format!(
        "Redo sequentially: the parallel group \"{name}\" didn't pass its checks, so its merged changes were thrown away \
         ({files} are back as they were before it). Carry out its steps ({steps}) of the plan in `{}` again, one at a time, \
         each with a step worker (task with kind \"worker\" and its step), not as a parallel group.",
        group.plan_path
    );
    if !notes.is_empty() {
        request.push_str(&format!(" Note: {}.", notes.join("; ")));
    }
    // P6.8: what the group's paths were is spent: its saved versions go.
    let spent: Vec<Before> = all
        .iter()
        .find(|g| g.name == name)
        .map(|g| g.before.iter().map(|(_, b)| b.clone()).collect())
        .unwrap_or_default();
    write_groups(dialog_id, &all)?;
    forget_snapshot(dialog_id, &spent);
    set_pending(dialog_id, None);
    Ok(request)
}

/// The Redo sequentially button: returns the message to send the chat.
pub async fn overlay_redo_group(
    agent: &crate::agent_run::AgentState,
    dialog_id: String,
    group: String,
) -> Result<String, String> {
    not_running(agent, &dialog_id)?;
    crate::rt::spawn_blocking(move || redo_group(&dialog_id, &group))
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
    fn conflicted(name: &str) -> (PathBuf, PathBuf, String, tokio::sync::OwnedMutexGuard<()>) {
        let root = std::env::temp_dir().join(format!("conflicts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        let held = test_data(&root.join("data"));
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
        (root, project, id, held)
    }

    // One at a time: the data folder is the app's, one for all.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn a_conflict_resolved_in_the_resolver_lands() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (root, project, id, _data) = conflicted("resolver");
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
        let (root, project, id, data) = conflicted("stale");
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

        // Taking the chat's side: the same, with its version (the data
        // folder's lock is taken again, so the first hold goes first).
        drop(data);
        let (root2, project, id, _data) = conflicted("stale-theirs");
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
        let (root, project, id, _data) = conflicted("stale-conflict");
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
        let (root, project, id, _data) = conflicted("anyway");
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
        let (root, project, id, _data) = conflicted("chat");
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
        let (root, _project, id, _data) = conflicted("unmark");
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
        let (root, project, id, _data) = conflicted("undo-merge");
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
        let (root, project, id, _data) = conflicted("changed-since");
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
        let (root, _project, id, _data) = conflicted("kept-names");
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
            (id.to_string(), PathBuf::from("/p")),
            (
                String::new(),
                BTreeMap::from([(
                    "a.txt".to_string(),
                    (BTreeSet::from(["other-chat".to_string()]), true),
                )]),
            ),
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
    fn a_hold_counts_above_and_below_its_folder() {
        let repo = std::env::temp_dir().join(format!("busy-repo-{}", std::process::id()));
        let beside = std::env::temp_dir().join(format!("busy-beside-{}", std::process::id()));
        for dir in [repo.join("app"), repo.join("web"), beside.clone()] {
            std::fs::create_dir_all(&dir).unwrap();
        }
        // A git operation holds the repository's working folder: every folder
        // in it waits, and a folder beside it doesn't.
        let held = hold_busy(&repo);
        assert!(is_busy(&repo));
        assert!(is_busy(&repo.join("app")), "a folder inside it");
        assert!(is_busy(&repo.join("web")));
        assert!(!is_busy(&beside), "another folder isn't in the repository");
        drop(held);
        // The hold's aftermath is the repository's alone.
        BUSY.lock().unwrap().remove(&busy_key(&repo));
        assert!(!is_busy(&beside));
        // A hold on a folder in a repository covers the folders above it too:
        // what's rewritten there may be beside it.
        let held = hold_busy(&repo.join("app"));
        assert!(is_busy(&repo.join("app")));
        assert!(is_busy(&repo), "a hold below the folder counts");
        assert!(!is_busy(&repo.join("web")), "a folder beside it doesn't");
        drop(held);
        BUSY.lock().unwrap().remove(&busy_key(&repo.join("app")));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&beside);
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

    /// A chat with saved (not mounted) changes on a project of `files`,
    /// made by `change` on its overlay; returns the project's folder.
    fn closed_chat(
        id: &str,
        root: &Path,
        files: &[(&str, &str)],
        change: impl FnOnce(&OverlayFs),
    ) -> PathBuf {
        let project = root.join("project");
        for (p, c) in files {
            let path = project.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        let store = store_dir(id).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("project.txt"),
            project.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let fs = OverlayFs::open(&project, &store, DEFAULT_LIMIT).unwrap();
        change(&fs);
        fs.flush();
        project
    }

    fn put(fs: &dyn ChatFs, rel: &str, content: &str) {
        let ino = match fs.resolve(rel) {
            Ok(ino) => ino,
            Err(_) => {
                let (parent, name) = fs.resolve_parent(rel).unwrap();
                fs.create(
                    parent,
                    name,
                    crate::vfs::store::Kind::File,
                    0o644,
                    0,
                    0,
                    None,
                )
                .unwrap()
                .ino
            }
        };
        fs.setattr(
            ino,
            &SetAttr {
                size: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        fs.write(ino, 0, content.as_bytes()).unwrap();
    }

    fn text(id: &str, rel: &str) -> Option<String> {
        with_fs(id, |fs| {
            let ino = fs.resolve(rel).ok()?;
            let size = fs.getattr(ino).ok()?.size as usize;
            fs.read(ino, 0, size)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        })
        .ok()
        .flatten()
    }

    /// Round 2 (section 4.2): a path whose saved version can't be read is
    /// an error of its own, never "untouched"; the all-or-nothing snapshot
    /// fails with it.
    #[test]
    fn an_unreadable_path_marks_redo_unavailable_not_untouched() {
        let root = std::env::temp_dir().join(format!("redo-unreadable-{}", uuid::Uuid::new_v4()));
        let _data = test_data(&root.join("data"));
        let id = "redo-unreadable-chat";
        closed_chat(id, &root, &[("a.rs", "a\n"), ("b.rs", "b\n")], |fs| {
            put(fs, "a.rs", "a by the chat\n");
        });
        // Its saved copy goes missing behind the overlay's back.
        let upper = changes(id)
            .into_iter()
            .find_map(|c| match c {
                OverlayChange::Write {
                    path,
                    upper: Some(u),
                    ..
                } if path == "a.rs" => Some(u),
                _ => None,
            })
            .unwrap();
        std::fs::remove_file(&upper).unwrap();
        let each = snapshot_each(id, &["a.rs".into(), "b.rs".into()]).unwrap();
        assert!(each[0].1.is_err(), "{:?}", each[0]);
        assert!(matches!(each[1].1, Ok(Before::Untouched)), "{:?}", each[1]);
        assert!(snapshot(id, &["a.rs".into(), "b.rs".into()]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round 2 (section 4.2): Redo on a chat whose overlay is closed puts
    /// a file the group moved back where it was, removes where it went,
    /// and spends the saved versions.
    #[test]
    fn redo_on_a_closed_overlay_and_of_a_moved_file() {
        let root = std::env::temp_dir().join(format!("redo-moved-{}", uuid::Uuid::new_v4()));
        let _data = test_data(&root.join("data"));
        let id = "redo-moved-chat";
        closed_chat(id, &root, &[("a.rs", "a\n")], |fs| {
            put(fs, "a.rs", "a by the chat\n");
        });
        let before = snapshot(id, &["a.rs".into(), "b.rs".into()]).unwrap();
        assert!(matches!(before[0].1, Before::File { .. }));
        assert!(matches!(before[1].1, Before::Untouched));
        // The group moves a.rs to b.rs.
        with_fs(id, |fs| {
            let root_ino = fs.resolve("").unwrap();
            fs.rename(
                root_ino,
                "a.rs",
                root_ino,
                "b.rs",
                crate::vfs::store::RenameMode::Replace,
            )
            .unwrap();
        })
        .unwrap();
        save_group(
            id,
            SavedGroup {
                name: "g".into(),
                plan_path: "p.md".into(),
                steps: vec![1, 2],
                before,
                redone: false,
                unavailable: None,
            },
        )
        .unwrap();
        assert!(group_blob_count(id) > 0);
        redo_group(id, "g").unwrap();
        assert_eq!(text(id, "a.rs").as_deref(), Some("a by the chat\n"));
        assert_eq!(text(id, "b.rs"), None);
        assert_eq!(group_blob_count(id), 0, "spent");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round 2 (section 4.2): a path inside a folder the chat deleted was
    /// absent before the group (pinned: Absent, so Redo removes it).
    #[test]
    fn a_fixer_file_under_a_folder_the_chat_deleted() {
        let root = std::env::temp_dir().join(format!("redo-deleted-{}", uuid::Uuid::new_v4()));
        let _data = test_data(&root.join("data"));
        let id = "redo-deleted-chat";
        closed_chat(
            id,
            &root,
            &[("old/x.rs", "x\n"), ("keep.rs", "k\n")],
            |fs| {
                let root_ino = fs.resolve("").unwrap();
                let old = fs.resolve("old").unwrap();
                fs.unlink(old, "x.rs").unwrap();
                fs.rmdir(root_ino, "old").unwrap();
            },
        );
        let each = snapshot_each(id, &["old/x.rs".into(), "old/new.rs".into()]).unwrap();
        assert!(matches!(each[0].1, Ok(Before::Absent)), "{:?}", each[0]);
        assert!(matches!(each[1].1, Ok(Before::Absent)), "{:?}", each[1]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P1.4: a group whose snapshot failed isn't offered for Redo, and
    /// says why, rather than putting paths back as if untouched.
    #[test]
    fn a_group_whose_snapshot_failed_isnt_redone() {
        let root = std::env::temp_dir().join(format!("redo-unknown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _data = test_data(&root.join("data"));
        let id = "redo-unknown-chat";
        std::fs::create_dir_all(store_dir(id).unwrap()).unwrap();
        save_group(
            id,
            SavedGroup {
                name: "g".into(),
                plan_path: "p.md".into(),
                steps: vec![1, 2],
                before: Vec::new(),
                redone: false,
                unavailable: None,
            },
        )
        .unwrap();
        mark_redo_unavailable(id, "g", "src/a.rs: permission denied").unwrap();
        let e = redo_group(id, "g").unwrap_err();
        assert!(
            e.contains("can't be redone") && e.contains("permission denied"),
            "{e}"
        );
        assert!(saved_groups(id)[0].unavailable.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P6.8: a version a group already has isn't kept twice; a scratch
    /// overlay a crash left is swept at the next start.
    #[test]
    fn superseded_versions_and_left_scratch_overlays_go() {
        let root = std::env::temp_dir().join(format!("group-blobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        let _data = test_data(&data);
        let id = "group-blobs-chat";
        std::fs::create_dir_all(store_dir(id).unwrap()).unwrap();
        let file = |text: &str| Before::File {
            content: keep_blob(id, text.as_bytes()).unwrap(),
            base: None,
        };
        let blobs = || std::fs::read_dir(group_blobs(id).unwrap()).unwrap().count();
        let group = |before| SavedGroup {
            name: "g".into(),
            plan_path: "p.md".into(),
            steps: vec![1, 2],
            before,
            redone: false,
            unavailable: None,
        };
        save_group(id, group(vec![("a.rs".into(), file("first"))])).unwrap();
        assert_eq!(blobs(), 1);
        // A second join of the group: a.rs keeps its first version.
        save_group(
            id,
            group(vec![
                ("a.rs".into(), file("again")),
                ("b.rs".into(), file("b")),
            ]),
        )
        .unwrap();
        assert_eq!(blobs(), 2, "the second a.rs version went");
        add_group_paths(id, "g", vec![("b.rs".into(), file("b again"))]).unwrap();
        assert_eq!(blobs(), 2);

        let scratch = data.join("overlays").join("red-left");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(SCRATCH_MARK), "").unwrap();
        init(&data);
        assert!(!scratch.exists(), "swept");
        assert!(store_dir(id).unwrap().exists(), "a chat's store stays");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A change made right after a save is saved within moments, with no
    /// further change to bring it: a crash then doesn't lose it. Needs FUSE.
    #[test]
    fn a_change_right_after_a_save_is_saved_without_another() {
        if !crate::vfs::mount_works() {
            return;
        }
        let root = std::env::temp_dir().join(format!("saver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let _data = test_data(&root.join("data"));
        let o = open(
            "saver-chat",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
        put(&*o.fs, "first.txt", "1\n");
        o.fs.flush();
        put(&*o.fs, "second.txt", "2\n");
        let manifest = store_dir("saver-chat").unwrap().join("manifest.json");
        let saved = || std::fs::read_to_string(&manifest).unwrap_or_default();
        for _ in 0..100 {
            if saved().contains("second.txt") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(saved().contains("second.txt"), "{}", saved());
        close("saver-chat");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two isolated chats on one project; one lands. Needs FUSE.
    #[test]
    fn a_landing_reaches_other_chats_at_once() {
        if !crate::vfs::mount_works() {
            return;
        }
        let root = std::env::temp_dir().join(format!("hub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        std::fs::write(project.join("b.txt"), "b\n").unwrap();
        let _data = test_data(&root.join("data"));
        let a = open(
            "hub-chat-a",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
        let b = open(
            "hub-chat-b",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
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
        let c = open(
            "hub-chat-c",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
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
        // The repository, three folders up from src-tauri/crates/rovik-core.
        let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_path_buf();
        let data = std::env::temp_dir().join(format!("bench-{}", std::process::id()));
        super::init(&data);
        let o = super::open(
            "bench-chat",
            &crate::folders::Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
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

/// F01–F02: child overlays stacked on a chat's view. Needs FUSE (skipped
/// where mounts don't work).
#[cfg(test)]
mod child_tests {
    use super::*;

    /// Round 2 (section 4.2): a child that renames, deletes and creates
    /// lands all three; what the parent had before lets Redo put them back.
    #[test]
    fn a_worker_that_deletes_renames_and_creates_lands_all_three() {
        if !crate::vfs::mount_works() {
            return;
        }
        let root = std::env::temp_dir().join(format!("kids3-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        for f in ["a", "c"] {
            std::fs::write(project.join(f), format!("{f}\n")).unwrap();
        }
        let _data = test_data(&root.join("data"));
        let parent = open(
            "kids3-parent",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
        let w = open_child("kids3-parent", "kids3-w").unwrap();
        std::fs::rename(w.view.join("a"), w.view.join("b")).unwrap();
        std::fs::remove_file(w.view.join("c")).unwrap();
        std::fs::write(w.view.join("d"), "x\n").unwrap();
        w.fs.flush();
        let paths = ["a", "b", "c", "d"].map(String::from);
        let before = snapshot("kids3-parent", &paths).unwrap();
        land_child("kids3-w").unwrap();
        let has = |f: &str| parent.view.join(f).exists();
        assert!(has("b") && has("d") && !has("a") && !has("c"));
        save_group(
            "kids3-parent",
            SavedGroup {
                name: "g".into(),
                plan_path: "p.md".into(),
                steps: vec![1, 2],
                before,
                redone: false,
                unavailable: None,
            },
        )
        .unwrap();
        redo_group("kids3-parent", "g").unwrap();
        assert!(has("a") && has("c") && !has("b") && !has("d"));
        assert_eq!(read(&parent.view.join("a")).as_deref(), Some("a\n"));
        close("kids3-parent");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round 2 (section 4.2): a child's landing records its parent before
    /// it writes anything, so one that fails at the first file is still
    /// found by its parent.
    #[test]
    fn land_child_paths_marks_the_parent_before_any_write() {
        if !crate::vfs::mount_works() {
            return;
        }
        use super::super::land;
        let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("kidsmark-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.rs"), "a\n").unwrap();
        let data = root.join("data");
        let _data = test_data(&data);
        let parent = open(
            "kidsmark-parent",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
        let w = open_child("kidsmark-parent", "kidsmark-w").unwrap();
        std::fs::write(w.view.join("a.rs"), "a from w\n").unwrap();
        w.fs.flush();
        *land::FAIL_WRITING.lock().unwrap() = Some((parent.view.clone(), "a.rs".into()));
        land::FAIL_WRITING_ALWAYS.store(true, std::sync::atomic::Ordering::SeqCst);
        let landed = land_child_except("kidsmark-w", &[]);
        land::FAIL_WRITING_ALWAYS.store(false, std::sync::atomic::Ordering::SeqCst);
        *land::FAIL_WRITING.lock().unwrap() = None;
        assert!(landed.is_err(), "{landed:?}");
        assert_eq!(
            std::fs::read_to_string(
                data.join("landings")
                    .join("kidsmark-w")
                    .join("into-chat.txt")
            )
            .ok()
            .as_deref(),
            Some("kidsmark-parent")
        );
        assert!(land::interrupted_into(
            &data,
            "kidsmark-parent",
            &parent.view
        ));
        discard_child("kidsmark-w");
        close("kidsmark-parent");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round 2 (section 4.2): scratch overlays a crash left are swept at
    /// start, marked or only named as one; a chat's store stays.
    #[test]
    fn a_scratch_overlay_left_by_a_crash_is_swept_even_without_its_mark() {
        let root = std::env::temp_dir().join(format!("sweep-{}", uuid::Uuid::new_v4()));
        let overlays = root.join("data").join("overlays");
        let unmarked = overlays.join(format!("{SCRATCH_PREFIX}{}", uuid::Uuid::new_v4()));
        let marked = overlays.join(uuid::Uuid::new_v4().to_string());
        let chat = overlays.join(uuid::Uuid::new_v4().to_string());
        for (dir, files) in [
            (&unmarked, vec!["project.txt"]),
            (&marked, vec!["project.txt", SCRATCH_MARK]),
            (&chat, vec!["project.txt"]),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            for f in files {
                std::fs::write(dir.join(f), "x").unwrap();
            }
        }
        let _data = test_data(&root.join("data"));
        assert!(!unmarked.exists() && !marked.exists());
        assert!(chat.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Another of Rovik's processes has a scratch overlay open (its red
    /// check runs there): the sweep at this one's start leaves it.
    #[test]
    fn a_scratch_overlay_open_in_another_process_isnt_swept() {
        let root = std::env::temp_dir().join(format!("sweep-{}", uuid::Uuid::new_v4()));
        let overlays = root.join("data").join("overlays");
        let live = overlays.join(format!("{SCRATCH_PREFIX}{}", uuid::Uuid::new_v4()));
        let left = overlays.join(format!("{SCRATCH_PREFIX}{}", uuid::Uuid::new_v4()));
        for dir in [&live, &left] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(SCRATCH_MARK), "").unwrap();
        }
        let theirs = crate::os_lock::try_hold(&live.join(MOUNTED))
            .unwrap()
            .unwrap();
        let _data = test_data(&root.join("data"));
        assert!(live.join(SCRATCH_MARK).is_file());
        assert!(!left.exists());
        drop(theirs);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A chat's changes open in another of Rovik's processes aren't opened
    /// here as well, nor thrown away: each would change its files unaware
    /// of the other.
    #[test]
    fn a_chats_changes_open_in_another_process_stay_its_own() {
        let root = std::env::temp_dir().join(format!("elsewhere-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let data = root.join("data");
        let _data = test_data(&data);
        let chat = uuid::Uuid::new_v4().to_string();
        let store = data.join("overlays").join(&chat);
        std::fs::create_dir_all(&store).unwrap();
        let theirs = crate::os_lock::try_hold(&store.join(MOUNTED))
            .unwrap()
            .unwrap();
        assert!(open_elsewhere(&chat));
        let opened = open(&chat, &Folders::one(project.clone()), Box::new(|_, _| {}));
        assert_eq!(opened.err().as_deref(), Some(OPEN_ELSEWHERE));
        assert_eq!(discard(&chat).err().as_deref(), Some(OPEN_ELSEWHERE));
        assert!(store.is_dir());
        drop(theirs);
        assert!(!open_elsewhere(&chat));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn read(p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    #[test]
    fn child_overlays_land_into_their_parent_not_the_project() {
        if !crate::vfs::mount_works() {
            return;
        }
        let root = std::env::temp_dir().join(format!("kids-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/a.rs"), "a\n").unwrap();
        std::fs::write(project.join("src/b.rs"), "b\n").unwrap();
        let _data = test_data(&root.join("data"));
        let parent = open(
            "kids-parent",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();
        // The parent changed a file before the workers start.
        std::fs::write(parent.view.join("src/a.rs"), "a from the lead\n").unwrap();

        // Two children of one parent: each sees the parent's view.
        let w1 = open_child("kids-parent", "kids-w1").unwrap();
        assert_eq!(
            read(&w1.view.join("src/a.rs")).as_deref(),
            Some("a from the lead\n")
        );
        std::fs::write(w1.view.join("src/a.rs"), "a from w1\n").unwrap();
        std::fs::write(w1.view.join("src/new.rs"), "new\n").unwrap();
        w1.fs.flush();
        let w2 = open_child("kids-parent", "kids-w2").unwrap();
        std::fs::write(w2.view.join("src/b.rs"), "b from w2\n").unwrap();
        // Their space counts inside the parent's limit.
        let (parent_used, parent_limit) = parent.fs.used();
        assert!(
            w1.fs.used().1 <= parent_limit - parent_used,
            "{:?}",
            w1.fs.used()
        );
        assert!(w2.fs.used().1 <= parent_limit - parent_used - w1.fs.used().0);
        // Neither sees the other before landing.
        assert_eq!(
            read(&w2.view.join("src/a.rs")).as_deref(),
            Some("a from the lead\n")
        );

        // One after another, into the parent.
        let landed = land_child("kids-w1").unwrap();
        assert_eq!(landed.len(), 2, "{landed:?}");
        assert!(land_child("kids-w2").is_ok());
        assert_eq!(
            read(&parent.view.join("src/a.rs")).as_deref(),
            Some("a from w1\n")
        );
        assert_eq!(
            read(&parent.view.join("src/new.rs")).as_deref(),
            Some("new\n")
        );
        assert_eq!(
            read(&parent.view.join("src/b.rs")).as_deref(),
            Some("b from w2\n")
        );
        // The project is unchanged; the children are gone.
        assert_eq!(read(&project.join("src/a.rs")).as_deref(), Some("a\n"));
        assert!(!project.join("src/new.rs").exists());
        assert!(get("kids-w1").is_none() && children_of("kids-parent").is_empty());
        // The parent's changes include theirs, for its own landing.
        let mut paths: Vec<String> = changes("kids-parent")
            .iter()
            .map(|c| c.path().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, ["src/a.rs", "src/b.rs", "src/new.rs"]);

        // A conflict: a child changed what the parent changed meanwhile.
        let w3 = open_child("kids-parent", "kids-w3").unwrap();
        std::fs::write(w3.view.join("src/b.rs"), "b from w3\n").unwrap();
        std::fs::write(parent.view.join("src/b.rs"), "b from the lead again\n").unwrap();
        match land_child("kids-w3") {
            Err(super::super::land::LandError::Conflicts(c)) => assert_eq!(c[0].path, "src/b.rs"),
            other => panic!("expected a conflict: {other:?}"),
        }
        assert!(get("kids-w3").is_some(), "kept for the lead");
        assert_eq!(
            read(&parent.view.join("src/b.rs")).as_deref(),
            Some("b from the lead again\n")
        );
        discard_child("kids-w3");

        // A crash during a child's landing: its journal waits for the
        // parent's view, then finishes there.
        let w4 = open_child("kids-parent", "kids-w4").unwrap();
        std::fs::write(w4.view.join("src/new.rs"), "new from w4\n").unwrap();
        w4.fs.flush();
        let data = root.join("data");
        let under = w4.folder().unwrap();
        super::super::land::land(&data, "kids-w4", under, &w4.fs.changes(), false).unwrap();
        // As if the app died mid-write: the journal says "applying" and
        // the parent's file is still the old one.
        let journals = data.join("landings").join("kids-w4");
        let landing = std::fs::read_dir(&journals)
            .unwrap()
            .flatten()
            .find(|d| d.path().is_dir())
            .unwrap()
            .path();
        let journal = landing.join("journal.json");
        let text = std::fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        std::fs::write(&journal, text).unwrap();
        std::fs::write(parent.view.join("src/new.rs"), "new\n").unwrap();
        assert!(
            super::super::land::recover(&data).is_empty(),
            "not into an unmounted view"
        );
        let finished = super::super::land::recover_into(&data, "kids-parent", &parent.view);
        assert_eq!(finished, ["src/new.rs"]);
        assert_eq!(
            read(&parent.view.join("src/new.rs")).as_deref(),
            Some("new from w4\n")
        );
        discard_child("kids-w4");

        // The same with the parent's view closed, then Land: the
        // interrupted landing is finished first, so the whole of the
        // worker's change lands, not the half that got in.
        let w6 = open_child("kids-parent", "kids-w6").unwrap();
        std::fs::write(w6.view.join("src/new.rs"), "new from w6\n").unwrap();
        std::fs::write(w6.view.join("src/b.rs"), "b from w6\n").unwrap();
        w6.fs.flush();
        let under = w6.folder().unwrap();
        super::super::land::land(&data, "kids-w6", under, &w6.fs.changes(), false).unwrap();
        let journals = data.join("landings").join("kids-w6");
        let landing = std::fs::read_dir(&journals)
            .unwrap()
            .flatten()
            .find(|d| d.path().is_dir())
            .unwrap()
            .path();
        let journal = landing.join("journal.json");
        let text = std::fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        std::fs::write(&journal, text).unwrap();
        // Half of it got in: b.rs did, new.rs didn't.
        std::fs::write(parent.view.join("src/new.rs"), "new from w4\n").unwrap();
        close("kids-w6");
        close("kids-parent");
        assert!(super::super::land::interrupted_into(
            &data,
            "kids-parent",
            &mountpoint("kids-parent")
        ));
        land_now("kids-parent", false).unwrap();
        assert_eq!(
            read(&project.join("src/new.rs")).as_deref(),
            Some("new from w6\n")
        );
        assert_eq!(
            read(&project.join("src/b.rs")).as_deref(),
            Some("b from w6\n")
        );
        assert!(!super::super::land::interrupted_into(
            &data,
            "kids-parent",
            &mountpoint("kids-parent")
        ));
        discard_child("kids-w6");
        let _parent = open(
            "kids-parent",
            &Folders::one(project.clone()),
            Box::new(|_, _| {}),
        )
        .unwrap();

        // Two children opened together, before either writes, share
        // what's left: together they can't pass the parent's limit.
        let p = get("kids-parent").unwrap();
        let (used, limit) = p.fs.used();
        let s1 = open_child_shared("kids-parent", "kids-s1", 2).unwrap();
        let s2 = open_child_shared("kids-parent", "kids-s2", 2).unwrap();
        let (l1, l2) = (s1.fs.used().1, s2.fs.used().1);
        assert!(l1 + l2 <= limit - used, "{l1} + {l2} > {}", limit - used);
        assert!(l1.abs_diff(l2) <= 1, "{l1} vs {l2}");
        // Reopened together after a restart (R03), they share it again: the
        // first one reopened doesn't take it all.
        forget_child_as_if_restarted("kids-s1");
        forget_child_as_if_restarted("kids-s2");
        assert!(child_kept("kids-parent", "kids-s1"));
        assert!(!child_kept("kids-other", "kids-s1"), "another's");
        let r1 = reopen_child("kids-parent", "kids-s1", 2).unwrap();
        let r2 = reopen_child("kids-parent", "kids-s2", 2).unwrap();
        let (l1, l2) = (r1.fs.used().1, r2.fs.used().1);
        assert!(l1.abs_diff(l2) <= 1, "{l1} vs {l2}");
        discard_child("kids-s1");
        discard_child("kids-s2");
        assert!(!child_kept("kids-parent", "kids-s1"), "gone");

        // Discarding the parent takes any child with it.
        open_child("kids-parent", "kids-w5").unwrap();
        discard("kids-parent").unwrap();
        assert!(get("kids-w5").is_none());
        assert!(!data.join("overlays").join("kids-w5").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
#[path = "workspace_chat_tests.rs"]
mod workspace_tests;

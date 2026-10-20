//! Landing: a chat's changes into the project.
//!
//! Each changed file is merged three ways: what it was when the chat first
//! changed it (the overlay's base copy), the chat's version, and the
//! project's file now. A file nobody else changed takes the chat's version;
//! changes to different lines merge; changes to the same lines are a
//! conflict, and nothing lands (unless the user lands anyway, keeping the
//! chat's version).
//!
//! Before anything is written, a journal keeps every touched path's old and
//! new content (`<app data>/landings/<chat>/<id>/`). It makes a landing
//! complete after a crash ([`recover`]) and lets the user undo it
//! ([`undo`]). The writes happen under each file's lock (file_guard.rs),
//! after checking that no file changed since the merge.
//!
//! Only one landing per project at a time. A landing being written holds
//! a lock in its folder (`APPLYING`), so another of Rovik's processes,
//! starting meanwhile, doesn't take it for one a crash interrupted.

use super::fs::{check_name, DeletedBase, OverlayChange};
use crate::vfs::store::Kind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// What a path becomes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Content {
    Absent,
    Dir,
    File { file: PathBuf, perm: Option<u16> },
    Symlink { target: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    old: Content,
    new: Content,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    version: u32,
    /// "applying" until every entry is written; then "done"; "undone" after
    /// an undo.
    state: String,
    at: u64,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub path: String,
    /// What collided, in words.
    pub reason: String,
}

/// What landing would do: the changes, merged; conflicts if any.
#[derive(Clone)]
pub struct Plan {
    actions: Vec<Action>,
    pub conflicts: Vec<Conflict>,
    /// Some file also changed in the project, so the result differs from
    /// both versions: worth checking before it lands.
    pub merged: bool,
}

#[derive(Clone, Debug)]
struct Action {
    path: String,
    new: NewContent,
    /// The project's entry when planned, which must still be there when
    /// the landing writes.
    expected: Snapshot,
    /// The project changed the file too: `new` is both changes merged.
    merged: bool,
    /// The project's file the merge used: `expected`, or for a moved file
    /// the project's file where it came from.
    source: Snapshot,
}

#[derive(Clone, Debug)]
enum NewContent {
    Absent,
    Dir,
    File { bytes: Vec<u8>, perm: Option<u16> },
    Symlink(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Snapshot {
    Absent,
    Dir,
    File(Vec<u8>),
    Symlink(String),
}

fn snapshot(path: &Path) -> Snapshot {
    match fs::symlink_metadata(path) {
        Err(_) => Snapshot::Absent,
        Ok(m) if m.file_type().is_symlink() => Snapshot::Symlink(
            fs::read_link(path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        Ok(m) if m.is_dir() => Snapshot::Dir,
        Ok(_) => Snapshot::File(fs::read(path).unwrap_or_default()),
    }
}

fn abs(project: &Path, rel: &str) -> PathBuf {
    let mut p = project.to_path_buf();
    p.extend(rel.split('/').filter(|s| !s.is_empty()));
    p
}

/// Plans landing `changes` (by-products already left out) in `project`.
/// `keep_chat`: on a conflict, take the chat's version instead of stopping.
pub fn plan(project: &Path, changes: &[OverlayChange], keep_chat: bool) -> Plan {
    let mut actions = Vec::new();
    let mut conflicts = Vec::new();
    let mut merged = false;
    // Where moved files came from: their content is merged at the new place
    // against the project's file here, so the delete here isn't checked
    // twice.
    let carried: std::collections::HashSet<&str> = changes
        .iter()
        .filter_map(|c| match c {
            OverlayChange::Write {
                kind: Kind::File,
                path,
                origin: Some(o),
                ..
            } if o != path => Some(o.as_str()),
            _ => None,
        })
        .collect();
    for change in changes.iter().filter(|c| !c.by_product()) {
        let path = change.path().to_string();
        // A change's path is the chat's own: one that isn't (a saved list
        // or an imported tree written by hand) never reaches outside.
        if !path.split('/').all(|part| check_name(part).is_ok()) {
            conflicts.push(Conflict {
                path,
                reason: "it isn't a path inside the project".into(),
            });
            continue;
        }
        let target = abs(project, &path);
        let now = snapshot(&target);
        match change {
            OverlayChange::Delete { base, .. } => {
                // Already gone: nothing to do.
                if now == Snapshot::Absent {
                    continue;
                }
                // Changed in the project since the chat deleted it: deleting
                // would lose that change.
                let changed = base.as_ref().is_some_and(|b| !b.still(&target));
                if changed && !keep_chat && !carried.contains(path.as_str()) {
                    let reason = match base {
                        Some(DeletedBase::Dir { complete: false, .. }) => {
                            "the chat deleted this folder, and it's too large to check for changes in the project".to_string()
                        }
                        Some(DeletedBase::Dir { .. }) => "the chat deleted this folder, and its contents changed in the project".into(),
                        _ => "the chat deleted it, and it changed in the project".into(),
                    };
                    conflicts.push(Conflict { path, reason });
                    continue;
                }
                actions.push(Action {
                    path,
                    new: NewContent::Absent,
                    expected: now,
                    merged: false,
                    source: Snapshot::Absent,
                })
            }
            OverlayChange::Write {
                kind: Kind::Dir, ..
            } => {
                if now != Snapshot::Dir {
                    actions.push(Action {
                        path,
                        new: NewContent::Dir,
                        expected: now,
                        merged: false,
                        source: Snapshot::Absent,
                    });
                }
            }
            OverlayChange::Write {
                kind: Kind::Symlink,
                target: link,
                ..
            } => {
                let link = link.clone().unwrap_or_default();
                if now != Snapshot::Symlink(link.clone()) {
                    actions.push(Action {
                        path,
                        new: NewContent::Symlink(link),
                        expected: now,
                        merged: false,
                        source: Snapshot::Absent,
                    });
                }
            }
            OverlayChange::Write {
                kind: Kind::File,
                origin,
                upper,
                base_copy,
                perm,
                perm_changed,
                ..
            } => {
                let ours = match (upper, origin) {
                    (Some(file), _) => fs::read(file).ok(),
                    (None, Some(o)) => fs::read(abs(project, o)).ok(),
                    (None, None) => Some(Vec::new()),
                };
                let Some(ours) = ours else {
                    conflicts.push(Conflict {
                        path,
                        reason: "the chat's version can't be read".into(),
                    });
                    continue;
                };
                let perm = perm_changed.then_some(*perm);
                let base = base_copy.as_ref().and_then(|b| fs::read(b).ok());
                // Moved: the project's side is its file where the chat's
                // came from (the new place is checked by its own delete),
                // unless the new place holds the file as it was or as the
                // chat has it: the user moved it too, and that's the
                // project's side. Another file there is theirs.
                let moved_from = origin.as_ref().filter(|o| **o != path && base.is_some());
                let user_moved_it = match &now {
                    Snapshot::File(b) => Some(b) == base.as_ref() || *b == ours,
                    _ => false,
                };
                let source = match moved_from {
                    Some(_) if user_moved_it => now.clone(),
                    Some(_) if now != Snapshot::Absent && !keep_chat => {
                        conflicts.push(Conflict {
                            path,
                            reason: "the project has another file there now".into(),
                        });
                        continue;
                    }
                    Some(o) => snapshot(&abs(project, o)),
                    None => now.clone(),
                };
                let theirs = match &source {
                    Snapshot::File(bytes) => Some(bytes.clone()),
                    Snapshot::Absent => None,
                    _ => {
                        conflicts.push(Conflict {
                            path,
                            reason: "the project has a folder or link there now".into(),
                        });
                        continue;
                    }
                };
                let result = resolve(base.as_deref(), &ours, theirs.as_deref());
                let was_merged = matches!(result, Resolved::Merged(_));
                let bytes = match result {
                    Resolved::Take(bytes) => bytes,
                    Resolved::Merged(bytes) => {
                        merged = true;
                        bytes
                    }
                    // The project already has it: nothing to write, unless it
                    // moved (the project has it at the old place only), or
                    // its old place is its new one but for case, which a
                    // file system that doesn't tell them apart takes for one
                    // file: the old place's deletion would take it (R04).
                    Resolved::Same
                        if moved_from.is_some_and(|o| {
                            now != Snapshot::File(ours.clone())
                                || o.to_lowercase() == path.to_lowercase()
                        }) =>
                    {
                        ours
                    }
                    Resolved::Same => {
                        if perm.is_none() {
                            continue;
                        }
                        ours
                    }
                    Resolved::Conflict(reason) if keep_chat => {
                        let _ = reason;
                        ours
                    }
                    Resolved::Conflict(reason) => {
                        conflicts.push(Conflict { path, reason });
                        continue;
                    }
                };
                actions.push(Action {
                    path,
                    new: NewContent::File { bytes, perm },
                    expected: now,
                    merged: was_merged,
                    source,
                });
            }
        }
    }
    Plan {
        actions,
        conflicts,
        merged,
    }
}

enum Resolved {
    /// The chat's version as it is.
    Take(Vec<u8>),
    /// Both sides' changes, merged.
    Merged(Vec<u8>),
    /// Nothing to write: the project already has it.
    Same,
    Conflict(String),
}

fn resolve(base: Option<&[u8]>, ours: &[u8], theirs: Option<&[u8]>) -> Resolved {
    match (base, theirs) {
        (_, Some(t)) if t == ours => Resolved::Same,
        // Nobody else changed it (or it's new and still absent).
        (Some(b), Some(t)) if t == b => Resolved::Take(ours.to_vec()),
        (None, None) => Resolved::Take(ours.to_vec()),
        (Some(_), None) => {
            Resolved::Conflict("the chat changed it, and it was deleted in the project".into())
        }
        (None, Some(_)) => {
            Resolved::Conflict("the chat and the project both created it, differently".into())
        }
        (Some(b), Some(t)) => match crate::merge::merge_bytes(b, ours, t) {
            Ok(Some(bytes)) => Resolved::Merged(bytes),
            Ok(None) => {
                Resolved::Conflict("the chat and the project changed the same lines".into())
            }
            Err(_) => {
                Resolved::Conflict("a binary file changed in the chat and in the project".into())
            }
        },
    }
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// The paths it writes or deletes.
    pub fn paths(&self) -> Vec<String> {
        self.actions.iter().map(|a| a.path.clone()).collect()
    }

    /// The files both the chat and the project changed: (path, both changes
    /// merged, the project's file now).
    pub fn merged_files(&self) -> Vec<(String, Vec<u8>, Vec<u8>)> {
        self.actions
            .iter()
            .filter(|a| a.merged)
            .filter_map(|a| match (&a.new, &a.source) {
                (NewContent::File { bytes, .. }, Snapshot::File(project)) => {
                    Some((a.path.clone(), bytes.clone(), project.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

static PROJECT_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn project_lock(project: &Path) -> Arc<Mutex<()>> {
    PROJECT_LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project.to_path_buf())
        .or_default()
        .clone()
}

fn landings_dir(data_dir: &Path, dialog_id: &str) -> PathBuf {
    data_dir.join("landings").join(dialog_id)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a landing did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Landed {
    pub id: String,
    pub paths: Vec<String>,
}

impl Landed {
    /// What its notice says: how many changes are in the project, then
    /// `more`, then where Undo is.
    pub fn notice(&self, more: &str) -> String {
        let n = self.paths.len();
        let (s, are) = if n == 1 { ("", "is") } else { ("s", "are") };
        format!(
            "{n} change{s} from this chat {are} in the project.{more} Undo is above the message box."
        )
    }
}

#[derive(Debug)]
pub enum LandError {
    /// The same lines changed in the chat and the project.
    Conflicts(Vec<Conflict>),
    /// The project kept changing while the landing tried to write.
    Busy,
    /// The project changed these files again after the merged result was
    /// checked: what would land now wasn't.
    Unchecked(Vec<String>),
    Failed(String),
}

impl std::fmt::Display for LandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandError::Conflicts(c) => write!(
                f,
                "{} file(s) changed in both the chat and the project",
                c.len()
            ),
            LandError::Busy => write!(
                f,
                "the project kept changing while the changes were landing"
            ),
            LandError::Unchecked(p) => {
                write!(
                    f,
                    "the project changed {} again after the checks ran on the merged result",
                    p.join(", ")
                )
            }
            LandError::Failed(e) => f.write_str(e),
        }
    }
}

/// Lands the chat's changes in `project` (planning, journaling, writing).
#[cfg_attr(not(test), allow(dead_code))]
/// Replans when a file changes between planning and writing, up to three
/// times. `keep_chat`: the chat's version wins every conflict.
pub fn land(
    data_dir: &Path,
    dialog_id: &str,
    project: &Path,
    changes: &[OverlayChange],
    keep_chat: bool,
) -> Result<Option<Landed>, LandError> {
    land_checked(data_dir, dialog_id, project, changes, keep_chat, None, true)
}

/// Tests: landings into this folder find it changing under them this many
/// more times (Busy); and how many were tried.
#[cfg(test)]
pub(crate) static BUSY_TIMES: std::sync::LazyLock<Mutex<HashMap<PathBuf, (usize, usize)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`land`]; with `allow_merge` false, refuses when any file would land
/// merged with project changes (they came after the checks ran).
///
/// `known`, with `keep_chat`: the conflicts the user was shown and chose
/// to override ("Land anyway"). A conflict in any other file came up since
/// (the project changed it meanwhile): the chat's version isn't taken over
/// it unseen, and the landing stops with those conflicts. `None`: every
/// conflict is overridden.
pub fn land_checked(
    data_dir: &Path,
    dialog_id: &str,
    project: &Path,
    changes: &[OverlayChange],
    keep_chat: bool,
    known: Option<&[String]>,
    allow_merge: bool,
) -> Result<Option<Landed>, LandError> {
    #[cfg(test)]
    if let Some((left, calls)) = BUSY_TIMES.lock().unwrap().get_mut(project) {
        *calls += 1;
        if *left > 0 {
            *left -= 1;
            return Err(LandError::Busy);
        }
    }
    let lock = project_lock(project);
    let _landing = lock.lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..3 {
        if let (true, Some(known)) = (keep_chat, known) {
            // Under the project's lock, so nothing lands in between: what
            // conflicts now is what Land anyway would take over.
            let unseen: Vec<Conflict> = plan(project, changes, false)
                .conflicts
                .into_iter()
                .filter(|c| !known.contains(&c.path))
                .collect();
            if !unseen.is_empty() {
                return Err(LandError::Conflicts(unseen));
            }
        }
        let plan = plan(project, changes, keep_chat);
        if !plan.conflicts.is_empty() {
            return Err(LandError::Conflicts(plan.conflicts));
        }
        if !allow_merge && plan.merged {
            return Err(LandError::Unchecked(
                plan.merged_files().into_iter().map(|(p, _, _)| p).collect(),
            ));
        }
        if plan.is_empty() {
            return Ok(None);
        }
        // Every touched file locked (each once, in order: no deadlock), then
        // checked.
        let paths: Vec<PathBuf> = plan.actions.iter().map(|a| abs(project, &a.path)).collect();
        let _locks = crate::file_guard::lock_all(paths.iter().map(PathBuf::as_path));
        let unchanged = plan
            .actions
            .iter()
            .all(|a| snapshot(&abs(project, &a.path)) == a.expected);
        if !unchanged {
            continue;
        }
        return apply(data_dir, dialog_id, project, &plan)
            .map(Some)
            .map_err(LandError::Failed);
    }
    Err(LandError::Busy)
}

fn save_journal(dir: &Path, journal: &Journal) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|e| e.to_string())?;
    crate::atomic_file::write_atomic(&dir.join("journal.json"), &bytes).map_err(|e| e.to_string())
}

/// Copies a project entry into the journal, for undo.
fn keep_old(dir: &Path, n: usize, path: &Path) -> Result<Content, String> {
    Ok(match snapshot(path) {
        Snapshot::Absent => Content::Absent,
        Snapshot::Dir => {
            // A folder that goes: everything in it is kept.
            let copy = dir.join("old").join(n.to_string());
            copy_tree(path, &copy).map_err(|e| e.to_string())?;
            Content::File {
                file: copy,
                perm: None,
            }
        }
        Snapshot::File(bytes) => {
            let copy = dir.join("old").join(n.to_string());
            write_durable(&copy, &bytes)?;
            Content::File {
                file: copy,
                perm: perm_of(path),
            }
        }
        Snapshot::Symlink(t) => Content::Symlink { target: t },
    })
}

/// Writes a journal copy and flushes it to disk: after a power cut the
/// journal must not say "applying" over empty copies.
fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// A folder copied with what's in it: files (with their permissions),
/// symlinks and subfolders.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    if let Some(perm) = perm_of(from) {
        set_perm(to, perm);
    }
    for e in fs::read_dir(from)? {
        let e = e?;
        let ft = e.file_type()?;
        let dest = to.join(e.file_name());
        if ft.is_symlink() {
            let target = fs::read_link(e.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dest)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&target, &dest)?;
        } else if ft.is_dir() {
            copy_tree(&e.path(), &dest)?;
        } else if ft.is_file() {
            // fs::copy keeps the permission bits.
            fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn perm_of(path: &Path) -> Option<u16> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|m| (m.permissions().mode() & 0o7777) as u16)
}

#[cfg(not(unix))]
fn perm_of(_path: &Path) -> Option<u16> {
    None
}

#[cfg(unix)]
fn set_perm(path: &Path, perm: u16) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(perm as u32));
}

#[cfg(not(unix))]
fn set_perm(_path: &Path, _perm: u16) {}

/// Tests: the path whose write fails, to check what a failed landing does.
#[cfg(test)]
/// Fails once, for that project's path.
pub(crate) static FAIL_WRITING: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);
/// Tests that inject failures hold this: the hooks are process-wide.
#[cfg(test)]
pub(crate) static FAIL_LOCK: Mutex<()> = Mutex::new(());
/// Tests: FAIL_WRITING fails every time (putting it back too), not once.
#[cfg(test)]
pub(crate) static FAIL_WRITING_ALWAYS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn write_content(
    project: &Path,
    rel: &str,
    content: &Content,
    dialog_id: &str,
) -> Result<(), String> {
    #[cfg(test)]
    {
        let mut fail = FAIL_WRITING.lock().unwrap();
        if fail.as_ref().is_some_and(|(p, r)| p == project && r == rel) {
            if !FAIL_WRITING_ALWAYS.load(std::sync::atomic::Ordering::SeqCst) {
                *fail = None;
            }
            return Err(format!("{rel}: injected failure"));
        }
        let _ = content;
    }
    let path = abs(project, rel);
    let err = |e: std::io::Error| format!("{rel}: {e}");
    // Whatever is there goes first, unless it's what comes.
    let replace = |path: &Path| -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                fs::remove_dir_all(path).map_err(err)
            }
            Ok(_) => fs::remove_file(path).map_err(err),
            Err(_) => Ok(()),
        }
    };
    match content {
        Content::Absent => replace(&path),
        Content::Dir => {
            if !path.is_dir() {
                replace(&path)?;
            }
            fs::create_dir_all(&path).map_err(err)
        }
        Content::File { file, perm } => {
            if file.is_dir() {
                // A folder kept by the journal (undoing a delete).
                replace(&path)?;
                return copy_tree(file, &path).map_err(err);
            }
            let bytes = fs::read(file).map_err(err)?;
            if path.is_dir() {
                replace(&path)?;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(err)?;
            }
            // In place, like every other write: keeps the editor's file id.
            fs::write(&path, &bytes).map_err(err)?;
            if let Some(perm) = perm {
                set_perm(&path, *perm);
            }
            crate::file_guard::note_landing(&path, dialog_id, &bytes);
            Ok(())
        }
        Content::Symlink { target } => {
            replace(&path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(err)?;
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, &path).map_err(err)
            }
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_file(target, &path).map_err(err)
            }
        }
    }
}

/// Landing ids: time, process and a counter, so two landings of one chat in
/// the same second don't share a folder.
fn landing_id() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!(
        "{}-{}-{}",
        now_secs(),
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Whether the project's entry at `rel` is what `content` says.
fn is_content(project: &Path, rel: &str, content: &Content) -> bool {
    let now = snapshot(&abs(project, rel));
    match (content, now) {
        (Content::Absent, Snapshot::Absent) | (Content::Dir, Snapshot::Dir) => true,
        (Content::File { file, .. }, Snapshot::File(now)) if !file.is_dir() => {
            fs::read(file).is_ok_and(|b| b == now)
        }
        // A folder kept whole (a deleted folder's journal copy).
        (Content::File { file, .. }, Snapshot::Dir) if file.is_dir() => true,
        (Content::Symlink { target }, Snapshot::Symlink(t)) => *target == t,
        _ => false,
    }
}

/// The lock a landing holds while it's written (`apply`, `recover_where`).
pub(super) const APPLYING: &str = "applying.lock";

fn apply(data_dir: &Path, dialog_id: &str, project: &Path, plan: &Plan) -> Result<Landed, String> {
    let id = landing_id();
    let dir = landings_dir(data_dir, dialog_id).join(&id);
    fs::create_dir_all(dir.join("old")).map_err(|e| e.to_string())?;
    // A new folder: free. Without it (a lock that couldn't be made), the
    // landing goes on as before.
    let _applying = crate::os_lock::try_hold(&dir.join(APPLYING)).ok().flatten();
    // Which project, for recovery and undo after the overlay is gone.
    fs::write(
        landings_dir(data_dir, dialog_id).join("project.txt"),
        project.to_string_lossy().as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    fs::create_dir_all(dir.join("new")).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for (n, a) in plan.actions.iter().enumerate() {
        let old = keep_old(&dir, n, &abs(project, &a.path))?;
        let new = match &a.new {
            NewContent::Absent => Content::Absent,
            NewContent::Dir => Content::Dir,
            NewContent::Symlink(t) => Content::Symlink { target: t.clone() },
            NewContent::File { bytes, perm } => {
                let file = dir.join("new").join(n.to_string());
                write_durable(&file, bytes)?;
                Content::File { file, perm: *perm }
            }
        };
        entries.push(Entry {
            path: a.path.clone(),
            old,
            new,
        });
    }
    sync_dir(&dir.join("old"));
    sync_dir(&dir.join("new"));
    let mut journal = Journal {
        version: 1,
        state: "applying".into(),
        at: now_secs(),
        entries,
    };
    save_journal(&dir, &journal)?;
    for (i, e) in journal.entries.iter().enumerate() {
        if let Err(err) = write_content(project, &e.path, &e.new, dialog_id) {
            // Back to how the project was: what was written goes back, in
            // reverse, and the journal says the landing failed.
            let mut left = Vec::new();
            for done in journal.entries[..=i].iter().rev() {
                if write_content(project, &done.path, &done.old, dialog_id).is_err() {
                    left.push(done.path.clone());
                }
            }
            // P1.3: put back whole, it failed; half put back, it's still
            // applying: recovery finishes it, and Land waits for that.
            journal.state = if left.is_empty() {
                "failed"
            } else {
                "applying"
            }
            .into();
            let _ = save_journal(&dir, &journal);
            return Err(if left.is_empty() {
                format!("{err} (nothing landed)")
            } else {
                format!("{err}; these couldn't be put back: {}", left.join(", "))
            });
        }
    }
    journal.state = "done".into();
    save_journal(&dir, &journal)?;
    Ok(Landed {
        id,
        paths: plan.paths(),
    })
}

/// The project a chat's landings went to.
pub fn project_of(data_dir: &Path, dialog_id: &str) -> Option<PathBuf> {
    fs::read_to_string(landings_dir(data_dir, dialog_id).join("project.txt"))
        .ok()
        .map(|p| PathBuf::from(p.trim()))
}

/// P6.9: a child chat's landings go into its parent's view: the parent's
/// id is recorded, so they're found by it, not by the view's path (which
/// can change between sessions).
pub fn mark_into_chat(data_dir: &Path, child: &str, parent: &str) -> Result<(), String> {
    let dir = landings_dir(data_dir, child);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(dir.join(INTO_CHAT), parent).map_err(|e| e.to_string())
}

const INTO_CHAT: &str = "into-chat.txt";

/// The chat whose view a chat's landings went into, if they did.
fn into_chat(data_dir: &Path, dialog_id: &str) -> Option<String> {
    fs::read_to_string(landings_dir(data_dir, dialog_id).join(INTO_CHAT))
        .ok()
        .map(|p| p.trim().to_string())
}

/// Finishes landings a crash interrupted: their journals say "applying".
/// Returns the paths written.
pub fn recover(data_dir: &Path) -> Vec<String> {
    // A child chat's landing went into its parent's view, which isn't
    // mounted yet: it's finished when the parent's view is (F01). Older
    // records without the parent's id: by the path's look.
    recover_where(data_dir, |id, project| {
        (into_chat(data_dir, id).is_none() && !super::chats::is_chat_view(project))
            .then(|| project.to_path_buf())
    })
}

/// Finishes the interrupted landings into one chat's view (its children's),
/// once that view (`view`) is mounted: into it, wherever it's mounted now.
pub fn recover_into(data_dir: &Path, parent: &str, view: &Path) -> Vec<String> {
    recover_where(data_dir, |id, project| {
        into_chat(data_dir, id)
            .map_or(project == view, |p| p == parent)
            .then(|| view.to_path_buf())
    })
}

/// Whether a landing into `parent`'s view was interrupted and is still to
/// be finished.
pub fn interrupted_into(data_dir: &Path, parent: &str, view: &Path) -> bool {
    let Ok(chats) = fs::read_dir(data_dir.join("landings")) else {
        return false;
    };
    chats.flatten().any(|chat| {
        let id = chat.file_name().to_string_lossy().into_owned();
        let into = match into_chat(data_dir, &id) {
            Some(p) => p == parent,
            None => project_of(data_dir, &id).is_some_and(|p| p == view),
        };
        into && fs::read_dir(chat.path()).is_ok_and(|landings| {
            landings.flatten().any(|l| {
                fs::read(l.path().join("journal.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
                    .is_some_and(|j| j.state == "applying")
            })
        })
    })
}

/// `wanted`: whether a chat's landings are finished here, and into where.
fn recover_where(data_dir: &Path, wanted: impl Fn(&str, &Path) -> Option<PathBuf>) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(chats) = fs::read_dir(data_dir.join("landings")) else {
        return done;
    };
    for chat in chats.flatten() {
        let dialog_id = chat.file_name().to_string_lossy().into_owned();
        let Some(recorded) = project_of(data_dir, &dialog_id) else {
            continue;
        };
        let Some(project) = wanted(&dialog_id, &recorded) else {
            continue;
        };
        let Ok(landings) = fs::read_dir(chat.path()) else {
            continue;
        };
        for landing in landings.flatten() {
            let dir = landing.path();
            let Some(mut journal) = fs::read(dir.join("journal.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
            else {
                continue;
            };
            if journal.state != "applying" {
                continue;
            }
            // Still being written, by another of Rovik's processes: not
            // interrupted. Held while it's finished here.
            let _finishing = match crate::os_lock::try_hold(&dir.join(APPLYING)) {
                Ok(None) => continue,
                Ok(Some(lock)) => Some(lock),
                Err(_) => None,
            };
            // Only paths still as the landing found them, or already as it
            // left them: anything else changed after the crash and stays.
            // A path can have two entries (a file replaced by a folder: its
            // delete, then the folder), each starting where the one before
            // left it.
            let mut order: Vec<&str> = Vec::new();
            let mut by_path: HashMap<&str, Vec<&Entry>> = HashMap::new();
            for e in &journal.entries {
                if !by_path.contains_key(e.path.as_str()) {
                    order.push(&e.path);
                }
                by_path.entry(&e.path).or_default().push(e);
            }
            let mut skipped = Vec::new();
            // A write that failed (not a path changed since): the landing
            // stays to finish, tried again next time (P1.3).
            let mut unwritten = Vec::new();
            for path in order {
                let group = &by_path[path];
                if group
                    .last()
                    .is_some_and(|e| is_content(&project, path, &e.new))
                {
                    continue;
                }
                let mut ok = true;
                for (k, e) in group.iter().enumerate() {
                    if is_content(&project, path, &e.new) {
                        continue;
                    }
                    let before = if k == 0 { &e.old } else { &group[k - 1].new };
                    if !is_content(&project, path, before) {
                        ok = false;
                        break;
                    }
                    if write_content(&project, path, &e.new, &dialog_id).is_err() {
                        unwritten.push(path.to_string());
                        ok = false;
                        break;
                    }
                }
                if ok {
                    done.push(path.to_string());
                } else if !unwritten.iter().any(|u| u == path) {
                    skipped.push(path.to_string());
                }
            }
            if !unwritten.is_empty() {
                for p in unwritten {
                    done.push(format!("{p} (not finished: it couldn't be written)"));
                }
                continue;
            }
            journal.state = if skipped.is_empty() {
                "done".into()
            } else {
                "partial".into()
            };
            let _ = save_journal(&dir, &journal);
            for p in skipped {
                done.push(format!("{p} (not finished: it changed since)"));
            }
        }
    }
    done
}

/// The chat's latest landing that can be undone.
pub fn latest(data_dir: &Path, dialog_id: &str) -> Option<(String, Vec<String>, u64)> {
    let dir = landings_dir(data_dir, dialog_id);
    let mut found: Vec<(u64, String, Vec<String>)> = fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let j: Journal =
                serde_json::from_slice(&fs::read(e.path().join("journal.json")).ok()?).ok()?;
            (j.state == "done").then(|| {
                (
                    j.at,
                    e.file_name().to_string_lossy().into_owned(),
                    j.entries.iter().map(|x| x.path.clone()).collect(),
                )
            })
        })
        .collect();
    found.sort();
    found.pop().map(|(at, id, paths)| (id, paths, at))
}

/// Undoes a landing: each path goes back to what it was, unless it changed
/// since (then it's left as it is and named in the result).
pub fn undo(
    data_dir: &Path,
    dialog_id: &str,
    project: &Path,
    id: &str,
) -> Result<Vec<Conflict>, String> {
    if id.contains(['/', '\\']) || id.contains("..") {
        return Err("bad landing id".into());
    }
    let lock = project_lock(project);
    let _landing = lock.lock().unwrap_or_else(|e| e.into_inner());
    let dir = landings_dir(data_dir, dialog_id).join(id);
    let mut journal: Journal =
        serde_json::from_slice(&fs::read(dir.join("journal.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if journal.state != "done" {
        return Err("that landing can't be undone".into());
    }
    let mut kept = Vec::new();
    let mut restored = std::collections::HashSet::new();
    for e in journal.entries.iter().rev() {
        // A path with two entries (deleted, then written anew) goes back once.
        if !restored.insert(e.path.clone()) {
            continue;
        }
        let path = abs(project, &e.path);
        let _lock = crate::file_guard::lock(&path);
        // Already back (an earlier undo that stopped partway): done, so a
        // retry finishes the rest instead of calling these conflicts.
        if is_content(project, &e.path, &e.old) {
            continue;
        }
        let still = match (&e.new, snapshot(&path)) {
            (Content::Absent, Snapshot::Absent) | (Content::Dir, Snapshot::Dir) => true,
            (Content::File { file, .. }, Snapshot::File(now)) => {
                fs::read(file).is_ok_and(|b| b == now)
            }
            (Content::Symlink { target }, Snapshot::Symlink(t)) => *target == t,
            _ => false,
        };
        if !still {
            kept.push(Conflict {
                path: e.path.clone(),
                reason: "changed since it landed".into(),
            });
            continue;
        }
        // A folder the landing made (where there was nothing, or a file or
        // link) goes only if nothing else is in it now: files added since,
        // or files kept above, stay with it.
        if matches!(e.new, Content::Dir) && !matches!(e.old, Content::Dir) {
            if fs::remove_dir(&path).is_err() {
                kept.push(Conflict {
                    path: e.path.clone(),
                    reason: "files were added to this folder since it landed".into(),
                });
                continue;
            }
            if matches!(e.old, Content::Absent) {
                continue;
            }
        }
        write_content(project, &e.path, &e.old, dialog_id)?;
    }
    journal.state = "undone".into();
    save_journal(&dir, &journal)?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R04: a rename of case alone writes the new name even when the
    /// project's file there reads as the chat's, as it does where the file
    /// system takes both names for one file (shown here by two files): the
    /// old name's deletion goes first, and would take it along.
    #[test]
    fn a_rename_of_case_alone_writes_the_new_name() {
        let dir = crate::testing::temp_dir();
        let project = dir.0.join("project");
        let text = b"# The project\n";
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("readme.md"), text).unwrap();
        fs::write(project.join("README.md"), text).unwrap();
        let kept = dir.0.join("kept");
        fs::write(&kept, text).unwrap();
        let changes = [
            OverlayChange::Delete {
                path: "readme.md".into(),
                by_product: false,
                base: Some(DeletedBase::file(text)),
            },
            OverlayChange::Write {
                path: "README.md".into(),
                kind: Kind::File,
                origin: Some("readme.md".into()),
                upper: Some(kept.clone()),
                base_copy: Some(kept),
                perm: 0o644,
                perm_changed: false,
                target: None,
                by_product: false,
            },
        ];

        let plan = plan(&project, &changes, false);

        let paths: Vec<&str> = plan.actions.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["readme.md", "README.md"]);
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn a_landings_notice_says_how_many_changes_are_in_the_project() {
        let landed = |n: usize| Landed {
            id: "l1".into(),
            paths: (0..n).map(|i| format!("src/{i}.rs")).collect(),
        };
        assert_eq!(
            landed(1).notice(""),
            "1 change from this chat is in the project. Undo is above the message box."
        );
        assert_eq!(
            landed(3).notice(" No checks ran."),
            "3 changes from this chat are in the project. No checks ran. Undo is above the message box."
        );
    }
}

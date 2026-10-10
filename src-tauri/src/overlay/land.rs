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
//! Only one landing per project at a time.

use super::fs::OverlayChange;
use crate::merge::{is_binary, merge3, Merged};
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
pub struct Plan {
    actions: Vec<Action>,
    pub conflicts: Vec<Conflict>,
    /// Some file also changed in the project, so the result differs from
    /// both versions: worth checking before it lands.
    pub merged: bool,
}

#[derive(Debug)]
struct Action {
    path: String,
    new: NewContent,
    /// The project's entry when planned, which must still be there when
    /// the landing writes.
    expected: Snapshot,
}

#[derive(Debug)]
enum NewContent {
    Absent,
    Dir,
    File { bytes: Vec<u8>, perm: Option<u16> },
    Symlink(String),
}

#[derive(Debug, PartialEq)]
enum Snapshot {
    Absent,
    Dir,
    File(Vec<u8>),
    Symlink(String),
}

fn snapshot(path: &Path) -> Snapshot {
    match fs::symlink_metadata(path) {
        Err(_) => Snapshot::Absent,
        Ok(m) if m.file_type().is_symlink() => {
            Snapshot::Symlink(fs::read_link(path).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default())
        }
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
    for change in changes.iter().filter(|c| !c.by_product()) {
        let path = change.path().to_string();
        let target = abs(project, &path);
        let now = snapshot(&target);
        match change {
            OverlayChange::Delete { .. } => actions.push(Action { path, new: NewContent::Absent, expected: now }),
            OverlayChange::Write { kind: Kind::Dir, .. } => {
                if now != Snapshot::Dir {
                    actions.push(Action { path, new: NewContent::Dir, expected: now });
                }
            }
            OverlayChange::Write { kind: Kind::Symlink, target: link, .. } => {
                let link = link.clone().unwrap_or_default();
                if now != Snapshot::Symlink(link.clone()) {
                    actions.push(Action { path, new: NewContent::Symlink(link), expected: now });
                }
            }
            OverlayChange::Write { kind: Kind::File, origin, upper, base_copy, perm, perm_changed, .. } => {
                let ours = match (upper, origin) {
                    (Some(file), _) => fs::read(file).ok(),
                    (None, Some(o)) => fs::read(abs(project, o)).ok(),
                    (None, None) => Some(Vec::new()),
                };
                let Some(ours) = ours else {
                    conflicts.push(Conflict { path, reason: "the chat's version can't be read".into() });
                    continue;
                };
                let perm = perm_changed.then_some(*perm);
                let base = base_copy.as_ref().and_then(|b| fs::read(b).ok());
                let theirs = match &now {
                    Snapshot::File(bytes) => Some(bytes.clone()),
                    Snapshot::Absent => None,
                    _ => {
                        conflicts.push(Conflict { path, reason: "the project has a folder or link there now".into() });
                        continue;
                    }
                };
                let result = resolve(base.as_deref(), &ours, theirs.as_deref());
                let bytes = match result {
                    Resolved::Take(bytes) => bytes,
                    Resolved::Merged(bytes) => {
                        merged = true;
                        bytes
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
                actions.push(Action { path, new: NewContent::File { bytes, perm }, expected: now });
            }
        }
    }
    Plan { actions, conflicts, merged }
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
        (Some(_), None) => Resolved::Conflict("the chat changed it, and it was deleted in the project".into()),
        (None, Some(_)) => Resolved::Conflict("the chat and the project both created it, differently".into()),
        (Some(b), Some(t)) => {
            if is_binary(b) || is_binary(ours) || is_binary(t) {
                return Resolved::Conflict("a binary file changed in the chat and in the project".into());
            }
            let (b, o, t) = (String::from_utf8_lossy(b), String::from_utf8_lossy(ours), String::from_utf8_lossy(t));
            match merge3(&b, &o, &t) {
                Ok(Merged::Clean(text)) => Resolved::Merged(text.into_bytes()),
                Ok(Merged::Conflict(_)) => {
                    Resolved::Conflict("the chat and the project changed the same lines".into())
                }
                Err(e) => Resolved::Conflict(format!("couldn't merge: {e}")),
            }
        }
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

    /// Writes the merged results into the chat's view, so its checks run on
    /// what would land.
    pub fn write_into(&self, view: &Path) -> Result<(), String> {
        for a in &self.actions {
            if let NewContent::File { bytes, .. } = &a.new {
                fs::write(abs(view, &a.path), bytes).map_err(|e| format!("{}: {e}", a.path))?;
            }
        }
        Ok(())
    }
}

static PROJECT_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn project_lock(project: &Path) -> Arc<Mutex<()>> {
    PROJECT_LOCKS.lock().unwrap_or_else(|e| e.into_inner()).entry(project.to_path_buf()).or_default().clone()
}

fn landings_dir(data_dir: &Path, dialog_id: &str) -> PathBuf {
    data_dir.join("landings").join(dialog_id)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What a landing did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Landed {
    pub id: String,
    pub paths: Vec<String>,
}

#[derive(Debug)]
pub enum LandError {
    /// The same lines changed in the chat and the project.
    Conflicts(Vec<Conflict>),
    /// The project kept changing while the landing tried to write.
    Busy,
    Failed(String),
}

impl std::fmt::Display for LandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandError::Conflicts(c) => write!(f, "{} file(s) changed in both the chat and the project", c.len()),
            LandError::Busy => write!(f, "the project kept changing while the changes were landing"),
            LandError::Failed(e) => f.write_str(e),
        }
    }
}

/// Lands the chat's changes in `project` (planning, journaling, writing).
/// Replans when a file changes between planning and writing, up to three
/// times.
pub fn land(
    data_dir: &Path,
    dialog_id: &str,
    project: &Path,
    changes: &[OverlayChange],
    keep_chat: bool,
) -> Result<Option<Landed>, LandError> {
    let lock = project_lock(project);
    let _landing = lock.lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..3 {
        let plan = plan(project, changes, keep_chat);
        if !plan.conflicts.is_empty() {
            return Err(LandError::Conflicts(plan.conflicts));
        }
        if plan.is_empty() {
            return Ok(None);
        }
        // Every touched file locked (in order: no deadlock), then checked.
        let mut paths: Vec<PathBuf> = plan.actions.iter().map(|a| abs(project, &a.path)).collect();
        paths.sort();
        paths.dedup();
        let _locks: Vec<_> = paths.iter().map(|p| crate::file_guard::lock(p)).collect();
        let unchanged = plan.actions.iter().all(|a| snapshot(&abs(project, &a.path)) == a.expected);
        if !unchanged {
            continue;
        }
        return apply(data_dir, dialog_id, project, &plan).map(Some).map_err(LandError::Failed);
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
            Content::File { file: copy, perm: None }
        }
        Snapshot::File(bytes) => {
            let copy = dir.join("old").join(n.to_string());
            fs::write(&copy, bytes).map_err(|e| e.to_string())?;
            Content::File { file: copy, perm: perm_of(path) }
        }
        Snapshot::Symlink(t) => Content::Symlink { target: t },
    })
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let ft = e.file_type()?;
        let dest = to.join(e.file_name());
        if ft.is_dir() {
            copy_tree(&e.path(), &dest)?;
        } else if ft.is_file() {
            fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn perm_of(path: &Path) -> Option<u16> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).ok().map(|m| (m.permissions().mode() & 0o7777) as u16)
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

fn write_content(project: &Path, rel: &str, content: &Content, dialog_id: &str) -> Result<(), String> {
    let path = abs(project, rel);
    let err = |e: std::io::Error| format!("{rel}: {e}");
    // Whatever is there goes first, unless it's what comes.
    let replace = |path: &Path| -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => fs::remove_dir_all(path).map_err(err),
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

fn apply(data_dir: &Path, dialog_id: &str, project: &Path, plan: &Plan) -> Result<Landed, String> {
    let id = format!("{}-{}", now_secs(), std::process::id());
    let dir = landings_dir(data_dir, dialog_id).join(&id);
    fs::create_dir_all(dir.join("old")).map_err(|e| e.to_string())?;
    // Which project, for recovery and undo after the overlay is gone.
    fs::write(landings_dir(data_dir, dialog_id).join("project.txt"), project.to_string_lossy().as_bytes())
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
                fs::write(&file, bytes).map_err(|e| e.to_string())?;
                Content::File { file, perm: *perm }
            }
        };
        entries.push(Entry { path: a.path.clone(), old, new });
    }
    let mut journal = Journal { version: 1, state: "applying".into(), at: now_secs(), entries };
    save_journal(&dir, &journal)?;
    for e in &journal.entries {
        write_content(project, &e.path, &e.new, dialog_id)?;
    }
    journal.state = "done".into();
    save_journal(&dir, &journal)?;
    Ok(Landed { id, paths: plan.paths() })
}

/// The project a chat's landings went to.
pub fn project_of(data_dir: &Path, dialog_id: &str) -> Option<PathBuf> {
    fs::read_to_string(landings_dir(data_dir, dialog_id).join("project.txt")).ok().map(|p| PathBuf::from(p.trim()))
}

/// Finishes landings a crash interrupted: their journals say "applying".
/// Returns the paths written.
pub fn recover(data_dir: &Path) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(chats) = fs::read_dir(data_dir.join("landings")) else { return done };
    for chat in chats.flatten() {
        let dialog_id = chat.file_name().to_string_lossy().into_owned();
        let Some(project) = project_of(data_dir, &dialog_id) else { continue };
        let Ok(landings) = fs::read_dir(chat.path()) else { continue };
        for landing in landings.flatten() {
            let dir = landing.path();
            let Some(mut journal) =
                fs::read(dir.join("journal.json")).ok().and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
            else {
                continue;
            };
            if journal.state != "applying" {
                continue;
            }
            for e in &journal.entries {
                if write_content(&project, &e.path, &e.new, &dialog_id).is_ok() {
                    done.push(e.path.clone());
                }
            }
            journal.state = "done".into();
            let _ = save_journal(&dir, &journal);
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
            let j: Journal = serde_json::from_slice(&fs::read(e.path().join("journal.json")).ok()?).ok()?;
            (j.state == "done").then(|| (j.at, e.file_name().to_string_lossy().into_owned(), j.entries.iter().map(|x| x.path.clone()).collect()))
        })
        .collect();
    found.sort();
    found.pop().map(|(at, id, paths)| (id, paths, at))
}

/// Undoes a landing: each path goes back to what it was, unless it changed
/// since (then it's left as it is and named in the result).
pub fn undo(data_dir: &Path, dialog_id: &str, project: &Path, id: &str) -> Result<Vec<Conflict>, String> {
    if id.contains(['/', '\\']) || id.contains("..") {
        return Err("bad landing id".into());
    }
    let lock = project_lock(project);
    let _landing = lock.lock().unwrap_or_else(|e| e.into_inner());
    let dir = landings_dir(data_dir, dialog_id).join(id);
    let mut journal: Journal = serde_json::from_slice(&fs::read(dir.join("journal.json")).map_err(|e| e.to_string())?)
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
        let still = match (&e.new, snapshot(&path)) {
            (Content::Absent, Snapshot::Absent) | (Content::Dir, Snapshot::Dir) => true,
            (Content::File { file, .. }, Snapshot::File(now)) => fs::read(file).is_ok_and(|b| b == now),
            (Content::Symlink { target }, Snapshot::Symlink(t)) => *target == t,
            _ => false,
        };
        if !still {
            kept.push(Conflict { path: e.path.clone(), reason: "changed since it landed".into() });
            continue;
        }
        write_content(project, &e.path, &e.old, dialog_id)?;
    }
    journal.state = "undone".into();
    save_journal(&dir, &journal)?;
    Ok(kept)
}

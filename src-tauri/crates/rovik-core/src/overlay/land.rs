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
//!
//! A workspace chat's changes land in its folders all at once or not at all
//! (workspaces-plan.md, 4.6, D24): its paths start with a folder's name
//! (`api/src/x.ts`), one journal holds every folder's entries, and a
//! conflict in any folder lands nothing. A folder chat's paths are its
//! folder's own, as `Folders::one` has them.

use super::fs::{DeletedBase, OverlayChange};
use crate::folders::{Folder, Folders};
use crate::vfs::store::{check_name, Kind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    /// A workspace chat's landing: its folders' names, which its entries'
    /// paths start with. Empty for a folder chat's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folders: Vec<String>,
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
    /// The chat's path.
    path: String,
    /// The folder it's in, and its path in that folder.
    root: PathBuf,
    rel: String,
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

/// Whether a folder on the way from `project` to `rel` (not `rel` itself)
/// is a link: writing there would write wherever the link leads, outside
/// the project perhaps, rather than into the project's folder.
fn through_link(project: &Path, rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let mut at = project.to_path_buf();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        at.push(part);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

fn abs(project: &Path, rel: &str) -> PathBuf {
    let mut p = project.to_path_buf();
    p.extend(rel.split('/').filter(|s| !s.is_empty()));
    p
}

/// Plans landing `changes` (by-products already left out) in `project`.
/// `keep_chat`: on a conflict, take the chat's version instead of stopping.
pub fn plan(project: &Path, changes: &[OverlayChange], keep_chat: bool) -> Plan {
    plan_in(&Folders::one(project.to_path_buf()), changes, keep_chat)
}

/// The real place of a chat path in `folders`: the folder it's in, and its
/// path there.
fn place(folders: &Folders, path: &str) -> Option<(PathBuf, String)> {
    let (folder, rel) = folders.split(path)?;
    Some((folder.path.clone(), rel.to_string()))
}

/// [`plan`] for a chat's folders: each change's path, and where a moved one
/// came from, resolved by its first part, merged against that folder.
pub fn plan_in(folders: &Folders, changes: &[OverlayChange], keep_chat: bool) -> Plan {
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
    // Paths whose project entry this landing deletes (each checked against
    // what the chat deleted): a folder or a link written there may replace
    // it.
    let mut deleting: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        // In one of the chat's folders, not the view's top: nothing is made
        // there.
        let (root, rel) = match place(folders, &path) {
            Some((root, rel)) if !rel.is_empty() => (root, rel),
            _ => {
                conflicts.push(Conflict {
                    path,
                    reason: "it isn't inside one of the chat's folders".into(),
                });
                continue;
            }
        };
        // A folder on its way that became a link in the project: what the
        // chat wrote there isn't where it would land.
        if through_link(&root, &rel) {
            conflicts.push(Conflict {
                path,
                reason: "a folder on its way is a link in the project now".into(),
            });
            continue;
        }
        let target = abs(&root, &rel);
        let now = snapshot(&target);
        match change {
            OverlayChange::Delete { base, .. } => {
                // Already gone: nothing to do.
                if now == Snapshot::Absent {
                    continue;
                }
                // Changed in the project since the chat deleted it: deleting
                // would lose that change. Without a record of what was
                // deleted (the project had nothing there then, or a list
                // saved before records were kept), what's there now can't be
                // shown to be what the chat deleted, so it isn't deleted
                // unasked.
                let changed = base.as_ref().is_none_or(|b| !b.still(&target));
                if changed && !keep_chat && !carried.contains(path.as_str()) {
                    let reason = match base {
                        None => "the chat deleted this, and Rovik can't tell whether what the project has here now is what it deleted".to_string(),
                        Some(DeletedBase::Dir { complete: false, .. }) => {
                            "the chat deleted this folder, and it's too large to check for changes in the project".into()
                        }
                        Some(DeletedBase::Dir { .. }) => "the chat deleted this folder, and its contents changed in the project".into(),
                        _ => "the chat deleted it, and it changed in the project".into(),
                    };
                    conflicts.push(Conflict { path, reason });
                    continue;
                }
                deleting.insert(path.clone());
                actions.push(Action {
                    path,
                    root: root.clone(),
                    rel: rel.clone(),
                    new: NewContent::Absent,
                    expected: now,
                    merged: false,
                    source: Snapshot::Absent,
                })
            }
            OverlayChange::Write {
                kind: Kind::Dir, ..
            } => {
                if now == Snapshot::Dir {
                    continue;
                }
                if let Some(conflict) = occupied(&path, &now, &deleting, &conflicts, keep_chat) {
                    conflicts.extend(conflict);
                    continue;
                }
                actions.push(Action {
                    path,
                    root: root.clone(),
                    rel: rel.clone(),
                    new: NewContent::Dir,
                    expected: now,
                    merged: false,
                    source: Snapshot::Absent,
                });
            }
            OverlayChange::Write {
                kind: Kind::Symlink,
                target: link,
                ..
            } => {
                let link = link.clone().unwrap_or_default();
                if now == Snapshot::Symlink(link.clone()) {
                    continue;
                }
                if let Some(conflict) = occupied(&path, &now, &deleting, &conflicts, keep_chat) {
                    conflicts.extend(conflict);
                    continue;
                }
                actions.push(Action {
                    path,
                    root: root.clone(),
                    rel: rel.clone(),
                    new: NewContent::Symlink(link),
                    expected: now,
                    merged: false,
                    source: Snapshot::Absent,
                });
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
                let from = |o: &str| place(folders, o).map(|(root, rel)| abs(&root, &rel));
                let ours = match (upper, origin) {
                    (Some(file), _) => fs::read(file).ok(),
                    (None, Some(o)) => from(o).and_then(|o| fs::read(o).ok()),
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
                    Some(o) => from(o).map_or(Snapshot::Absent, |o| snapshot(&o)),
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
                    root: root.clone(),
                    rel: rel.clone(),
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

/// Whether a folder or link the chat made at `path` would replace an entry
/// the project has there now (`now`) that the chat never saw: one it made
/// meanwhile (`ln -s cfg.dev config` landing over a `config/` the user
/// filled). Not when nothing's there, or this landing deletes what's there
/// (`deleting`, checked against what the chat deleted), or the user lands
/// anyway. Some(None): stopped already by that path's own conflict.
fn occupied(
    path: &str,
    now: &Snapshot,
    deleting: &std::collections::HashSet<String>,
    conflicts: &[Conflict],
    keep_chat: bool,
) -> Option<Option<Conflict>> {
    if *now == Snapshot::Absent || deleting.contains(path) || keep_chat {
        return None;
    }
    if conflicts.iter().any(|c| c.path == path) {
        return Some(None);
    }
    let what = match now {
        Snapshot::Dir => "a folder",
        Snapshot::Symlink(_) => "a link",
        _ => "a file",
    };
    Some(Some(Conflict {
        path: path.to_string(),
        reason: format!("the project has {what} there now, which the chat didn't replace"),
    }))
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

/// Each of the chat's folders' landing locks, by the folder's key
/// (file_guard.rs: one per folder, whichever spelling of its path a chat
/// has), in the keys' order: a folder chat and a workspace chat landing in
/// one folder wait for each other, and two landings taking several always
/// take them in the same order, so neither waits for the other forever.
fn folder_locks(folders: &Folders) -> Vec<Arc<Mutex<()>>> {
    let mut keys: Vec<PathBuf> = folders
        .iter()
        .map(|f| crate::file_guard::key(&f.path))
        .collect();
    keys.sort();
    keys.dedup();
    let mut all = PROJECT_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    keys.into_iter()
        .map(|key| all.entry(key).or_default().clone())
        .collect()
}

/// Why a chat's changes can't land or be undone now: one of its folders
/// isn't there (D20), which landing would make anew with only the chat's
/// files in it. None when each one is.
fn missing(folders: &Folders) -> Option<String> {
    let gone: Vec<&Folder> = folders.iter().filter(|f| !f.path.is_dir()).collect();
    let (last, rest) = gone.split_last()?;
    if folders.single().is_some() {
        return Some(format!(
            "The project's folder {} isn't there: bring it back to land or undo this chat's changes.",
            last.path.display()
        ));
    }
    let names: Vec<&str> = rest.iter().map(|f| f.name.as_str()).collect();
    Some(match names.is_empty() {
        true => format!(
            "{} isn't there: locate it, or remove it from the workspace, to land or undo this chat's changes.",
            last.name
        ),
        false => format!(
            "{} and {} aren't there: locate them, or remove them from the workspace, to land or undo this chat's changes.",
            names.join(", "),
            last.name
        ),
    })
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

/// [`land`] in a chat's folders, all or nothing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn land_in(
    data_dir: &Path,
    dialog_id: &str,
    folders: &Folders,
    changes: &[OverlayChange],
    keep_chat: bool,
) -> Result<Option<Landed>, LandError> {
    land_checked_in(data_dir, dialog_id, folders, changes, keep_chat, None, true)
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
    let folders = Folders::one(project.to_path_buf());
    land_checked_in(
        data_dir,
        dialog_id,
        &folders,
        changes,
        keep_chat,
        known,
        allow_merge,
    )
}

/// [`land_checked`] in a chat's folders: refused while one of them is
/// missing, naming it, and written under each folder's lock.
pub fn land_checked_in(
    data_dir: &Path,
    dialog_id: &str,
    folders: &Folders,
    changes: &[OverlayChange],
    keep_chat: bool,
    known: Option<&[String]>,
    allow_merge: bool,
) -> Result<Option<Landed>, LandError> {
    #[cfg(test)]
    if let Some((left, calls)) = BUSY_TIMES.lock().unwrap().get_mut(&folders.first().path) {
        *calls += 1;
        if *left > 0 {
            *left -= 1;
            return Err(LandError::Busy);
        }
    }
    if let Some(why) = missing(folders) {
        return Err(LandError::Failed(why));
    }
    let locks = folder_locks(folders);
    let _landing: Vec<_> = locks
        .iter()
        .map(|lock| lock.lock().unwrap_or_else(|e| e.into_inner()))
        .collect();
    for _ in 0..3 {
        if let (true, Some(known)) = (keep_chat, known) {
            // Under the folders' locks, so nothing lands in between: what
            // conflicts now is what Land anyway would take over.
            let unseen: Vec<Conflict> = plan_in(folders, changes, false)
                .conflicts
                .into_iter()
                .filter(|c| !known.contains(&c.path))
                .collect();
            if !unseen.is_empty() {
                return Err(LandError::Conflicts(unseen));
            }
        }
        let plan = plan_in(folders, changes, keep_chat);
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
        let paths: Vec<PathBuf> = plan.actions.iter().map(|a| abs(&a.root, &a.rel)).collect();
        let _locks = crate::file_guard::lock_all(paths.iter().map(PathBuf::as_path));
        let unchanged = plan
            .actions
            .iter()
            .all(|a| snapshot(&abs(&a.root, &a.rel)) == a.expected);
        if !unchanged {
            continue;
        }
        return apply(data_dir, dialog_id, folders, &plan)
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
    // Checked again as it's written: a folder on the way may have become a
    // link since the plan.
    if through_link(project, rel) {
        return Err(format!(
            "{rel}: a folder on its way is a link in the project now"
        ));
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
        // A folder kept whole (a deleted folder's journal copy): the same
        // folder when it holds the same, not any folder by that name.
        (Content::File { file, .. }, Snapshot::Dir) if file.is_dir() => {
            same_tree(file, &abs(project, rel))
        }
        (Content::Symlink { target }, Snapshot::Symlink(t)) => *target == t,
        _ => false,
    }
}

/// Whether the folder at `b` holds what the folder at `a` holds: the same
/// names, each the same kind with the same content, nothing more or less.
fn same_tree(a: &Path, b: &Path) -> bool {
    let entries = |dir: &Path| -> Option<BTreeMap<std::ffi::OsString, fs::FileType>> {
        fs::read_dir(dir)
            .ok()?
            .map(|e| {
                let e = e.ok()?;
                Some((e.file_name(), e.file_type().ok()?))
            })
            .collect()
    };
    let (Some(in_a), Some(in_b)) = (entries(a), entries(b)) else {
        return false;
    };
    if !in_a.keys().eq(in_b.keys()) {
        return false;
    }
    in_a.iter().all(|(name, kind)| {
        let (pa, pb) = (a.join(name), b.join(name));
        let kind_b = in_b[name];
        if kind.is_dir() {
            kind_b.is_dir() && same_tree(&pa, &pb)
        } else if kind.is_symlink() {
            kind_b.is_symlink() && fs::read_link(&pa).ok() == fs::read_link(&pb).ok()
        } else {
            kind_b.is_file() && fs::read(&pa).ok() == fs::read(&pb).ok()
        }
    })
}

/// The lock a landing holds while it's written (`apply`, `recover_where`).
pub(super) const APPLYING: &str = "applying.lock";

/// A workspace chat's folder as its landings keep it
/// (`landings/<chat>/folders.json`): its name, and where it was.
#[derive(Serialize, Deserialize)]
struct SavedFolder {
    name: String,
    path: PathBuf,
}

const FOLDERS_FILE: &str = "folders.json";

/// Which folders, for recovery and undo after the overlay is gone: a folder
/// chat's in `project.txt`, as always, a workspace chat's in `folders.json`,
/// which an older Rovik's recovery doesn't read, so it leaves such a
/// landing alone.
fn keep_folders(data_dir: &Path, dialog_id: &str, folders: &Folders) -> Result<(), String> {
    let dir = landings_dir(data_dir, dialog_id);
    let (file, bytes) = match folders.single() {
        Some(project) => (
            "project.txt",
            project.to_string_lossy().into_owned().into_bytes(),
        ),
        None => {
            let saved: Vec<SavedFolder> = folders
                .iter()
                .map(|f| SavedFolder {
                    name: f.name.clone(),
                    path: f.path.clone(),
                })
                .collect();
            let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?;
            (FOLDERS_FILE, bytes)
        }
    };
    crate::atomic_file::write_atomic(&dir.join(file), &bytes).map_err(|e| e.to_string())
}

fn apply(
    data_dir: &Path,
    dialog_id: &str,
    folders: &Folders,
    plan: &Plan,
) -> Result<Landed, String> {
    let id = landing_id();
    let dir = landings_dir(data_dir, dialog_id).join(&id);
    fs::create_dir_all(dir.join("old")).map_err(|e| e.to_string())?;
    // A new folder: free. Without it (a lock that couldn't be made), the
    // landing goes on as before.
    let _applying = crate::os_lock::try_hold(&dir.join(APPLYING)).ok().flatten();
    keep_folders(data_dir, dialog_id, folders)?;
    fs::create_dir_all(dir.join("new")).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for (n, a) in plan.actions.iter().enumerate() {
        let old = keep_old(&dir, n, &abs(&a.root, &a.rel))?;
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
        folders: match folders.single() {
            Some(_) => Vec::new(),
            None => folders.iter().map(|f| f.name.clone()).collect(),
        },
        entries,
    };
    save_journal(&dir, &journal)?;
    for (i, (e, a)) in journal.entries.iter().zip(&plan.actions).enumerate() {
        if let Err(err) = write_content(&a.root, &a.rel, &e.new, dialog_id) {
            // Back to how every folder was: what was written goes back, in
            // reverse, and the journal says the landing failed.
            let mut left = Vec::new();
            let written = journal.entries[..=i].iter().zip(&plan.actions[..=i]);
            for (done, a) in written.rev() {
                if write_content(&a.root, &a.rel, &done.old, dialog_id).is_err() {
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

/// The folders a chat's landings went to: a workspace chat's from
/// `folders.json`, a folder chat's from `project.txt`.
pub fn folders_of(data_dir: &Path, dialog_id: &str) -> Option<Folders> {
    let dir = landings_dir(data_dir, dialog_id);
    if let Ok(bytes) = fs::read(dir.join(FOLDERS_FILE)) {
        let saved: Vec<SavedFolder> = serde_json::from_slice(&bytes).ok()?;
        let named = saved
            .into_iter()
            .map(|f| Folder {
                name: f.name,
                path: f.path,
            })
            .collect();
        return Folders::named(named).ok();
    }
    fs::read_to_string(dir.join("project.txt"))
        .ok()
        .map(|p| Folders::one(PathBuf::from(p.trim())))
}

/// A workspace's folder named `name` was located at `path`: its chats'
/// landings are finished and undone there from now on.
pub fn relocate(data_dir: &Path, dialog_id: &str, name: &str, path: &Path) -> Result<(), String> {
    let Some(folders) = folders_of(data_dir, dialog_id).filter(|f| f.single().is_none()) else {
        return Ok(());
    };
    let moved: Vec<Folder> = folders
        .iter()
        .map(|f| Folder {
            name: f.name.clone(),
            path: match f.name == name {
                true => path.to_path_buf(),
                false => f.path.clone(),
            },
        })
        .collect();
    keep_folders(data_dir, dialog_id, &Folders::named(moved)?)
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
    recover_where(data_dir, |id, folders| {
        let into_view = folders.iter().any(|f| super::chats::is_chat_view(&f.path));
        (into_chat(data_dir, id).is_none() && !into_view).then(|| folders.clone())
    })
}

/// Finishes the interrupted landings into one chat's view (its children's),
/// once that view (`view`) is mounted: into it, wherever it's mounted now.
pub fn recover_into(data_dir: &Path, parent: &str, view: &Path) -> Vec<String> {
    recover_where(data_dir, |id, folders| {
        into_chat(data_dir, id)
            .map_or(folders.single() == Some(view), |p| p == parent)
            .then(|| Folders::one(view.to_path_buf()))
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
            None => folders_of(data_dir, &id).is_some_and(|f| f.single() == Some(view)),
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

/// `wanted`: whether a chat's landings are finished here, and into which
/// folders. A folder that isn't there has nothing written in it, and isn't
/// made anew: its entries wait, the landing still applying, until it's back.
fn recover_where(
    data_dir: &Path,
    wanted: impl Fn(&str, &Folders) -> Option<Folders>,
) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(chats) = fs::read_dir(data_dir.join("landings")) else {
        return done;
    };
    for chat in chats.flatten() {
        let dialog_id = chat.file_name().to_string_lossy().into_owned();
        let Some(recorded) = folders_of(data_dir, &dialog_id) else {
            continue;
        };
        let Some(folders) = wanted(&dialog_id, &recorded) else {
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
            // A path whose folder isn't there: likewise, until it's back.
            let mut waiting = Vec::new();
            for path in order {
                let Some((root, rel)) = place(&folders, path).filter(|(root, _)| root.is_dir())
                else {
                    waiting.push(path.to_string());
                    continue;
                };
                let group = &by_path[path];
                if group
                    .last()
                    .is_some_and(|e| is_content(&root, &rel, &e.new))
                {
                    continue;
                }
                let mut ok = true;
                for (k, e) in group.iter().enumerate() {
                    if is_content(&root, &rel, &e.new) {
                        continue;
                    }
                    let before = if k == 0 { &e.old } else { &group[k - 1].new };
                    if !is_content(&root, &rel, before) {
                        ok = false;
                        break;
                    }
                    if write_content(&root, &rel, &e.new, &dialog_id).is_err() {
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
            if !unwritten.is_empty() || !waiting.is_empty() {
                for p in unwritten {
                    done.push(format!("{p} (not finished: it couldn't be written)"));
                }
                for p in waiting {
                    done.push(format!("{p} (not finished: its folder isn't there)"));
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
    undo_in(
        data_dir,
        dialog_id,
        &Folders::one(project.to_path_buf()),
        id,
    )
}

/// [`undo`] in a chat's folders: every one is put back, refused while one of
/// them is missing, naming it.
pub fn undo_in(
    data_dir: &Path,
    dialog_id: &str,
    folders: &Folders,
    id: &str,
) -> Result<Vec<Conflict>, String> {
    if id.contains(['/', '\\']) || id.contains("..") {
        return Err("bad landing id".into());
    }
    if let Some(why) = missing(folders) {
        return Err(why);
    }
    let locks = folder_locks(folders);
    let _landing: Vec<_> = locks
        .iter()
        .map(|lock| lock.lock().unwrap_or_else(|e| e.into_inner()))
        .collect();
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
        let Some((root, rel)) = place(folders, &e.path) else {
            kept.push(Conflict {
                path: e.path.clone(),
                reason: "its folder isn't one of the chat's any more".into(),
            });
            continue;
        };
        let path = abs(&root, &rel);
        let _lock = crate::file_guard::lock(&path);
        // Already back (an earlier undo that stopped partway): done, so a
        // retry finishes the rest instead of calling these conflicts.
        if is_content(&root, &rel, &e.old) {
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
        write_content(&root, &rel, &e.old, dialog_id)?;
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

    fn link_write(path: &str, target: &str) -> OverlayChange {
        OverlayChange::Write {
            path: path.into(),
            kind: Kind::Symlink,
            origin: None,
            upper: None,
            base_copy: None,
            perm: 0o777,
            perm_changed: false,
            target: Some(target.into()),
            by_product: false,
        }
    }

    /// A link the chat made doesn't land over a folder the project made
    /// there meanwhile, nor a folder over a file; over nothing, or over
    /// the entry this landing deletes, it does. Landing anyway writes it.
    #[test]
    fn a_folder_or_link_doesnt_land_over_an_entry_the_chat_never_saw() {
        let dir = crate::testing::temp_dir();
        let project = dir.0.join("project");
        fs::create_dir_all(project.join("config")).unwrap();
        fs::write(project.join("config/app.toml"), "the user's\n").unwrap();
        fs::write(project.join("notes"), "a file\n").unwrap();
        let dir_write = OverlayChange::Write {
            path: "notes".into(),
            kind: Kind::Dir,
            origin: None,
            upper: None,
            base_copy: None,
            perm: 0o755,
            perm_changed: false,
            target: None,
            by_product: false,
        };
        let changes = [
            link_write("config", "cfg.dev"),
            dir_write,
            link_write("fresh", "x"),
        ];

        let stopped = plan(&project, &changes, false);
        let mut paths: Vec<&str> = stopped.conflicts.iter().map(|c| c.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["config", "notes"]);
        assert_eq!(stopped.paths(), ["fresh"]);

        assert_eq!(plan(&project, &changes, true).paths().len(), 3);

        // The chat replaced the project's folder: its delete goes first,
        // checked against what it deleted, and the link takes its place.
        let replaced = [
            OverlayChange::Delete {
                path: "config".into(),
                by_product: false,
                base: crate::overlay::fs::DeletedBase::of(&project.join("config")),
            },
            link_write("config", "cfg.dev"),
        ];
        let p = plan(&project, &replaced, false);
        assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
        assert_eq!(p.paths(), ["config", "config"]);
    }

    /// A delete with no record of what was deleted can't be shown to be
    /// of what the project has there now: a conflict, unless the user
    /// lands anyway.
    #[test]
    fn a_delete_with_no_recorded_base_is_a_conflict() {
        let dir = crate::testing::temp_dir();
        let project = dir.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("notes.md"), "the user's\n").unwrap();
        let changes = [OverlayChange::Delete {
            path: "notes.md".into(),
            by_product: false,
            base: None,
        }];

        let stopped = plan(&project, &changes, false);
        assert!(stopped.actions.is_empty());
        assert_eq!(stopped.conflicts.len(), 1);
        assert_eq!(stopped.conflicts[0].path, "notes.md");

        let anyway = plan(&project, &changes, true);
        assert!(anyway.conflicts.is_empty());
        assert_eq!(anyway.paths(), ["notes.md"]);
    }

    /// A folder of the project that became a link is never written
    /// through: what the chat wrote under it is a conflict, and the file
    /// where the link leads stays as it was.
    #[cfg(unix)]
    #[test]
    fn a_landing_never_writes_through_a_folder_that_became_a_link() {
        let dir = crate::testing::temp_dir();
        let (project, elsewhere) = (dir.0.join("project"), dir.0.join("elsewhere"));
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("a.txt"), "not the project's\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("src")).unwrap();
        let ours = dir.0.join("ours");
        fs::write(&ours, "the chat's\n").unwrap();
        let changes = [OverlayChange::Write {
            path: "src/a.txt".into(),
            kind: Kind::File,
            origin: None,
            upper: Some(ours),
            base_copy: None,
            perm: 0o644,
            perm_changed: false,
            target: None,
            by_product: false,
        }];
        let stopped = plan(&project, &changes, true);
        assert!(stopped.actions.is_empty());
        assert_eq!(stopped.conflicts[0].path, "src/a.txt");
        let content = Content::File {
            file: dir.0.join("ours"),
            perm: None,
        };
        assert!(write_content(&project, "src/a.txt", &content, "chat").is_err());
        assert_eq!(
            fs::read_to_string(elsewhere.join("a.txt")).unwrap(),
            "not the project's\n"
        );
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

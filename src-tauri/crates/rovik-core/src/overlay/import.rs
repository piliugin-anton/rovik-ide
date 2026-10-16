//! A remote chat's result as its local overlay (6.5): what the chat changed
//! on its server, from its landing base's tree to its result's, kept as a
//! local isolated chat's changes are, so that landing, conflicts, Resolve
//! and Undo work on it as they do on theirs (land.rs, chats.rs). Nothing is
//! mounted: the chat runs on its server.
//!
//! - **Files** come from a libgit2 checkout of each tree into a temporary
//!   folder, through the repository that has both trees (the project's
//!   shadow), so line endings and the other filters follow the project's
//!   settings and attributes, as the user's own checkout would write them
//!   (K40).
//! - **Changes** are the overlay's own: a changed file's upper file and base
//!   copy, a new one's upper file, a deleted one by its base content's
//!   hash, and the folders the project lacks as new folders. A file whose
//!   kind changed (to or from a link) is deleted and written again. Build
//!   output and dependencies stay out, as they do locally (tiers.rs).
//! - **A moved file**, found as git finds renames (similar content), is
//!   a local overlay's move: written at its new place from where it was,
//!   with its base copy from there, and deleted there. Landing merges the
//!   project's changes to the old place into it (land.rs).
//! - **A subfolder of a repository** (K31) takes the changes under it, from
//!   its own root. The others are listed and never land.
//! - **Names that differ only in case** are one file on a file system that
//!   doesn't tell them apart (Windows', and macOS's unless formatted
//!   otherwise), as they aren't on the server's. A path the result writes
//!   that would be one file here with another stays on the server, listed
//!   (R04). A rename of case alone lands, as its deletion goes first.
//! - **An import replaces the chat's store**, unless the user is resolving
//!   conflicts in it. The new store is built beside it and swapped in.
//! - **Landing by itself** follows landing.rs's rules, but lands a result
//!   merged with the project's newer changes only when no checks would run
//!   on the merge: otherwise they run on the server first (6.6).

use super::chats::{self, Pending};
use super::fs::{write_manifest, DeletedBase, OverlayChange};
use super::land::{LandError, Landed};
use super::tiers::Tiers;
use crate::vfs::store::Kind;
use git2::build::CheckoutBuilder;
use git2::{DiffFile, DiffFindOptions, FileMode, Oid, Repository, Tree};
use rovik_protocol::work::ChatResult;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

/// The reason a result waits while its project isn't the open one: it
/// lands by itself once the project is open, or when the user lands it.
pub const READY: &str = "Ready to land: it lands once its project is open.";

/// What a store was imported from, kept in it (`import.json`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    /// The result's commit.
    pub commit: String,
    /// The paths it brought, from the root of the result's tree: what a
    /// landing reports (`WorkLanded`).
    pub changed: Vec<String>,
    /// The paths it changed outside the project's folder in its
    /// repository (K31): listed, never landed.
    pub outside: Vec<String>,
    /// No checks would run on it merged with the project's newer changes:
    /// such a merge lands by itself (`ChatResult::unchecked`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unchecked: bool,
    /// The paths it wrote that this computer's file system would take for
    /// another's, differing only in case: listed, never landed (R04).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_clashes: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum ImportError {
    /// The user is resolving conflicts in the chat's store, which stays.
    Resolving,
    Failed(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Resolving => f.write_str(
                "a newer result is ready, and waits while this chat's conflicts are resolved",
            ),
            ImportError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for ImportError {
    fn from(e: String) -> ImportError {
        ImportError::Failed(e)
    }
}

const RECORD: &str = "import.json";

/// Builds its chat's store for `project` from `result`: the changes of its
/// commit on its parent, the landing base, both of which `repo` has, with
/// the files its server put conflict markers in. `subpath` is the
/// project's folder in the trees ("" at their root).
pub fn import(
    repo: &Repository,
    project: &Path,
    subpath: &str,
    result: &ChatResult,
) -> Result<Imported, ImportError> {
    import_as(repo, project, subpath, result, case_insensitive(project))
}

/// `import`, on a file system that tells names apart by case or doesn't
/// (`fold`).
fn import_as(
    repo: &Repository,
    project: &Path,
    subpath: &str,
    result: &ChatResult,
    fold: bool,
) -> Result<Imported, ImportError> {
    let dialog_id = result.dialog_id.as_str();
    let store = chats::store_dir(dialog_id)
        .ok_or_else(|| ImportError::Failed("the app's data folder isn't known yet".into()))?;
    if resolving(dialog_id) {
        return Err(ImportError::Resolving);
    }
    let (marked, unchecked) = (&result.marked, result.unchecked);
    let result = Oid::from_str(&result.commit).map_err(|e| e.message().to_string())?;
    let project = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let staging = store.with_file_name(format!(".import-{dialog_id}"));
    remove(&staging)?;
    let built = build(
        repo, &project, subpath, result, unchecked, fold, &store, &staging,
    );
    let swapped = built
        .map_err(ImportError::Failed)
        .and_then(|(changes, imported)| {
            swap(dialog_id, &store, &staging, changes)?;
            Ok(imported)
        });
    let _ = remove(&staging);
    let imported = swapped?;
    let prefix = prefix(subpath);
    let marked = marked
        .iter()
        .filter(|path| imported.changed.contains(*path))
        .filter_map(|path| path.strip_prefix(prefix.as_str()))
        .map(String::from)
        .collect();
    chats::set_marked(dialog_id, marked);
    Ok(imported)
}

/// What the chat's store was imported from, when it was.
pub fn imported(dialog_id: &str) -> Option<Imported> {
    let bytes = fs::read(chats::store_dir(dialog_id)?.join(RECORD)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The chat's imported result waits, for `reason`.
pub fn wait(dialog_id: &str, reason: &str) {
    let pending = Pending {
        reason: reason.to_string(),
        conflicts: Vec::new(),
    };
    chats::set_pending(dialog_id, Some(&pending));
}

/// Lands the chat's imported result by itself: merged with the project's
/// newer changes only when no checks would run on the merge, which its
/// server checks first otherwise (6.6).
pub fn land_by_itself(dialog_id: &str) -> Result<Option<Landed>, LandError> {
    let unchecked = imported(dialog_id).is_some_and(|i| i.unchecked);
    chats::land_now_with(dialog_id, false, unchecked)
}

/// Whether the user is resolving conflicts in the chat's store.
pub fn resolving(dialog_id: &str) -> bool {
    chats::pending(dialog_id).is_some_and(|p| !p.conflicts.is_empty())
}

/// One side of a change: a file or a link. Folders and submodules aren't
/// changes of their own.
#[derive(Clone, Copy)]
struct Side {
    id: Oid,
    link: bool,
    exec: bool,
}

fn side(file: &DiffFile) -> Option<Side> {
    if !file.exists() {
        return None;
    }
    let (link, exec) = match file.mode() {
        FileMode::Blob => (false, false),
        FileMode::BlobExecutable => (false, true),
        FileMode::Link => (true, false),
        _ => return None,
    };
    Some(Side {
        id: file.id(),
        link,
        exec,
    })
}

/// A path's change, by its path in the trees.
struct Entry {
    tree_path: String,
    old: Option<Side>,
    new: Option<Side>,
    /// Moved here from this path, in the project's folder and in the trees:
    /// `old` is the file there.
    from: Option<(String, String)>,
}

/// Builds the store in `staging`, its files named as they will be once it
/// is moved to `store`: its changes, in the order to replay them, and what
/// it was imported from. `fold`: the project's file system doesn't tell
/// names apart by case.
#[allow(clippy::too_many_arguments)]
fn build(
    repo: &Repository,
    project: &Path,
    subpath: &str,
    result: Oid,
    unchecked: bool,
    fold: bool,
    store: &Path,
    staging: &Path,
) -> Result<(Vec<OverlayChange>, Imported), String> {
    let commit = repo.find_commit(result).map_err(git)?;
    let base = commit
        .parent(0)
        .map_err(|_| "the result has no landing base".to_string())?;
    let (base_tree, result_tree) = (base.tree().map_err(git)?, commit.tree().map_err(git)?);
    // Renames found as git finds them. A change of kind comes as a
    // deletion and an addition.
    let mut diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&result_tree), None)
        .map_err(git)?;
    let mut renames = DiffFindOptions::new();
    renames.renames(true);
    diff.find_similar(Some(&mut renames)).map_err(git)?;
    let (prefix, tiers) = (prefix(subpath), Tiers::new(project));
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut outside = BTreeSet::new();
    let in_project = |tree_path: &str| tree_path.strip_prefix(&prefix).map(String::from);
    let landed =
        |tree_path: &str| in_project(tree_path).filter(|rel| !tiers.is_by_product(rel, false));
    for delta in diff.deltas() {
        let (old, new) = (side(&delta.old_file()), side(&delta.new_file()));
        let path = |file: DiffFile| file.path().map(|p| p.to_string_lossy().replace('\\', "/"));
        let (Some(was), Some(now)) = (path(delta.old_file()), path(delta.new_file())) else {
            continue;
        };
        // A move is its old place deleted and its new one written from it.
        // One of a link, or across the project's folder's edge, is a
        // deletion and an addition.
        let files = old.zip(new).is_some_and(|(o, n)| !o.link && !n.link);
        let from = landed(&was).filter(|_| was != now && files && landed(&now).is_some());
        let sides = if was == now {
            vec![(now, old, new, None)]
        } else {
            let moved = from.map(|from| (from, was.clone()));
            let carried = moved.as_ref().and(old);
            vec![(was, old, None, None), (now, carried, new, moved)]
        };
        for (tree_path, old, new, from) in sides {
            let Some(rel) = in_project(&tree_path) else {
                outside.insert(tree_path);
                continue;
            };
            if (old.is_none() && new.is_none()) || tiers.is_by_product(&rel, false) {
                continue;
            }
            let entry = entries.entry(rel).or_insert(Entry {
                tree_path,
                old: None,
                new: None,
                from: None,
            });
            entry.old = entry.old.or(old);
            entry.new = entry.new.or(new);
            entry.from = entry.from.take().or(from);
        }
    }
    let case_clashes = if fold {
        hold_back_case_clashes(project, &mut entries)
    } else {
        Vec::new()
    };

    let out = staging.join("checkout");
    for dir in ["upper", "base", "checkout/base", "checkout/result"] {
        fs::create_dir_all(staging.join(dir)).map_err(io)?;
    }
    let files = |pick: fn(&Entry) -> Option<Side>| -> Vec<&str> {
        let entries = entries.values();
        let files = entries.filter(|e| pick(e).is_some_and(|s| !s.link));
        files.map(|e| e.tree_path.as_str()).collect()
    };
    // A moved file's base is its old place's, which that place's deletion
    // checks out.
    let old = |e: &Entry| e.old.filter(|_| e.from.is_none());
    checkout(repo, &base_tree, &files(old), &out.join("base"))?;
    checkout(repo, &result_tree, &files(|e| e.new), &out.join("result"))?;

    let mut deletes = Vec::new();
    let mut writes = BTreeMap::new();
    let mut next_file = 1;
    for (rel, entry) in &entries {
        let kept = entry
            .old
            .zip(entry.new)
            .is_some_and(|(old, new)| !old.link && !new.link);
        if let Some(old) = entry.old.filter(|_| !kept) {
            let base = if old.link {
                DeletedBase::Link {
                    target: link_target(repo, old.id)?,
                }
            } else {
                let file = out.join("base").join(&entry.tree_path);
                DeletedBase::file(&fs::read(&file).map_err(io)?)
            };
            deletes.push(OverlayChange::Delete {
                path: rel.clone(),
                by_product: false,
                base: Some(base),
            });
        }
        let Some(new) = entry.new else { continue };
        for dir in ancestors(rel) {
            if !project.join(&dir).is_dir() {
                writes.entry(dir.clone()).or_insert_with(|| folder(dir));
            }
        }
        if new.link {
            let target = Some(link_target(repo, new.id)?);
            writes.insert(rel.clone(), link(rel, target));
            continue;
        }
        let n = next_file.to_string();
        next_file += 1;
        let take = |from: &str, to: &str| {
            let from = out.join(from).join(&entry.tree_path);
            fs::rename(from, staging.join(to).join(&n)).map_err(io)
        };
        take("result", "upper")?;
        let base_copy = match (&entry.from, kept) {
            (Some((_, was)), _) => {
                let base = out.join("base").join(was);
                fs::copy(base, staging.join("base").join(&n)).map_err(io)?;
                Some(store.join("base").join(&n))
            }
            (None, true) => {
                take("base", "base")?;
                Some(store.join("base").join(&n))
            }
            (None, false) => None,
        };
        let changed_mode = match entry.old.filter(|_| kept) {
            Some(old) => old.exec != new.exec,
            None => new.exec,
        };
        writes.insert(
            rel.clone(),
            OverlayChange::Write {
                path: rel.clone(),
                kind: Kind::File,
                origin: match &entry.from {
                    Some((from, _)) => Some(from.clone()),
                    None => kept.then(|| rel.clone()),
                },
                upper: Some(store.join("upper").join(&n)),
                base_copy,
                perm: if new.exec { 0o755 } else { 0o644 },
                perm_changed: changed_mode,
                target: None,
                by_product: false,
            },
        );
    }

    let imported = Imported {
        commit: result.to_string(),
        changed: entries.values().map(|e| e.tree_path.clone()).collect(),
        outside: outside.into_iter().collect(),
        unchecked,
        case_clashes,
    };
    fs::write(
        staging.join("project.txt"),
        project.to_string_lossy().as_bytes(),
    )
    .map_err(io)?;
    let record = serde_json::to_vec(&imported).map_err(|e| e.to_string())?;
    fs::write(staging.join(RECORD), record).map_err(io)?;
    remove(&out)?;
    // A folder's deletes, then its entries, parents before children.
    let changes: Vec<OverlayChange> = deletes.into_iter().chain(writes.into_values()).collect();
    write_manifest(staging, next_file, changes.clone()).map_err(io)?;
    Ok((changes, imported))
}

/// What a file system that doesn't tell names apart by case can't hold of
/// the result: the paths it writes that would be one file here with
/// another, taken out of `entries`, by their paths in the trees. The other
/// is a path it writes too, or the project's file, unless the result
/// deletes that, which frees its place. Of the paths it writes, the one
/// spelled as the project's file stays. A moved file held back stays where
/// it was: its deletion there is held back with it.
fn hold_back_case_clashes(project: &Path, entries: &mut BTreeMap<String, Entry>) -> Vec<String> {
    let mut alike: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for rel in entries.keys() {
        alike.entry(rel.to_lowercase()).or_default().push(rel);
    }
    let name = |rel: &str| rel.rsplit('/').next().unwrap_or(rel).to_string();
    // The name of the project's entry where `rel` would be but for case,
    // spelled as it is here: each folder listed once.
    let mut listed: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut name_here = |rel: &str| {
        let (folder, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let names = listed
            .entry(folder.to_lowercase())
            .or_insert_with(|| names_by_case(&project.join(folder)));
        names.get(&name.to_lowercase()).cloned()
    };
    let mut held = Vec::new();
    for rels in alike.values() {
        let written: Vec<&String> = rels
            .iter()
            .copied()
            .filter(|rel| entries[*rel].new.is_some())
            .collect();
        let deletes = rels.len() > written.len();
        let here = if deletes { None } else { name_here(rels[0]) };
        let stays = match &here {
            Some(here) => written.iter().find(|rel| name(rel) == *here),
            None if written.len() == 1 => written.first(),
            None => None,
        };
        let clashing = written.iter().filter(|rel| Some(*rel) != stays);
        held.extend(clashing.map(|rel| rel.to_string()));
    }
    let mut clashes = Vec::new();
    for rel in held {
        let Some(entry) = entries.remove(&rel) else {
            continue;
        };
        if let Some((from, _)) = &entry.from {
            if entries.get(from).is_some_and(|e| e.new.is_none()) {
                entries.remove(from);
            }
        }
        clashes.push(entry.tree_path);
    }
    clashes
}

/// The names in `folder`, each by its lowercase: none if it isn't there.
fn names_by_case(folder: &Path) -> HashMap<String, String> {
    let entries = fs::read_dir(folder).into_iter().flatten().flatten();
    let names = entries.filter_map(|e| e.file_name().into_string().ok());
    names.map(|name| (name.to_lowercase(), name)).collect()
}

/// Whether the project's file system takes names that differ only in case
/// for one: Windows' does, and macOS's unless it was formatted otherwise.
/// An entry's name with the case of its letters turned names the same
/// file there, its own entries' first, as the project may be mounted.
pub fn case_insensitive(project: &Path) -> bool {
    if cfg!(windows) {
        return true;
    }
    let entries = fs::read_dir(project).into_iter().flatten().flatten();
    let paths = entries.map(|e| e.path()).take(64);
    for path in paths.chain([project.to_path_buf()]) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let turned: String = name
            .chars()
            .map(|c| match c.is_ascii_lowercase() {
                true => c.to_ascii_uppercase(),
                false => c.to_ascii_lowercase(),
            })
            .collect();
        if turned != name {
            return same_file(&path, &path.with_file_name(turned));
        }
    }
    false
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_a: &Path, b: &Path) -> bool {
    fs::symlink_metadata(b).is_ok()
}

/// Puts the built store in place of the chat's store, unless the user
/// started resolving conflicts there meanwhile. A result that changes
/// nothing here leaves no store.
fn swap(
    dialog_id: &str,
    store: &Path,
    staging: &Path,
    changes: Vec<OverlayChange>,
) -> Result<(), ImportError> {
    // Nothing reads the chat's saved changes meanwhile.
    let gate = chats::gate(dialog_id);
    let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
    if resolving(dialog_id) {
        return Err(ImportError::Resolving);
    }
    chats::let_go(dialog_id);
    // The old store goes aside first: the chat is without one only between
    // two renames, not while a large one is deleted.
    let old = store.with_file_name(format!(".replaced-{dialog_id}"));
    remove(&old)?;
    match fs::rename(store, &old) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(e).into()),
        _ => {}
    }
    if !changes.is_empty() {
        fs::rename(staging, store).map_err(io)?;
    }
    Ok(remove(&old)?)
}

/// Writes `paths` of `tree` under `into`, as a checkout of the repository's
/// work tree would write them, its index left as it is.
fn checkout(repo: &Repository, tree: &Tree, paths: &[&str], into: &Path) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut opts = CheckoutBuilder::new();
    opts.target_dir(into)
        .force()
        .update_index(false)
        .disable_pathspec_match(true);
    for path in paths {
        opts.path(*path);
    }
    repo.checkout_tree(tree.as_object(), Some(&mut opts))
        .map_err(git)
}

fn link_target(repo: &Repository, blob: Oid) -> Result<String, String> {
    let blob = repo.find_blob(blob).map_err(git)?;
    Ok(String::from_utf8_lossy(blob.content()).into_owned())
}

/// A folder the chat made.
fn folder(path: String) -> OverlayChange {
    OverlayChange::Write {
        path,
        kind: Kind::Dir,
        origin: None,
        upper: None,
        base_copy: None,
        perm: 0o755,
        perm_changed: false,
        target: None,
        by_product: false,
    }
}

/// A link the chat made.
fn link(path: &str, target: Option<String>) -> OverlayChange {
    OverlayChange::Write {
        path: path.to_string(),
        kind: Kind::Symlink,
        origin: None,
        upper: None,
        base_copy: None,
        perm: 0o777,
        perm_changed: false,
        target,
        by_product: false,
    }
}

/// The folders above a path, the outermost first.
fn ancestors(rel: &str) -> Vec<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    (1..parts.len()).map(|end| parts[..end].join("/")).collect()
}

/// The project's folder in the trees as a prefix of their paths.
fn prefix(subpath: &str) -> String {
    let subpath = subpath.trim_matches('/');
    if subpath.is_empty() {
        String::new()
    } else {
        format!("{subpath}/")
    }
}

fn remove(dir: &Path) -> Result<(), String> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
        _ => Ok(()),
    }
}

fn git(e: git2::Error) -> String {
    e.message().to_string()
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests;

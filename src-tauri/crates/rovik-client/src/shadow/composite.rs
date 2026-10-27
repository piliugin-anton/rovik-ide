//! A workspace's composite shadow (workspaces-plan.md, 4.11, D30). A chat
//! across a workspace's folders goes to a daemon as one project whose
//! top-level folders are the chat's. Each folder is snapshotted into its
//! own shadow, as a folder chat's project is; the composite commits, for
//! each chat, one tree with each of its folders' trees under the folder's
//! name, at the chat's snapshot ref, for one push and one sync. It reads
//! the folders' shadows' objects, and through theirs the folders' own
//! repositories', by its alternates (two levels deep), so a compose writes
//! only its top tree and its commit.
//!
//! A chat's results are fetched into it. Each folder's part goes back into
//! the folder's shadow (`Composite::graft`), which doesn't read the
//! composite's objects: the objects of the part are copied there, and the
//! part's base and result are committed there on the folder's own tree, so
//! the folder's import checks its files out with its repository's settings.

use super::{busy, create, empty_tree, git, hold, mirror, rovik_commit, slashed};
use super::{snapshot_message, upkeep};
use super::{Result, Shadow, SNAPSHOTS};
use git2::build::TreeUpdateBuilder;
use git2::{FileMode, ObjectType, Odb, Oid, Reference, Repository, Tree};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A workspace's composite shadow.
pub struct Composite {
    repo: Repository,
    busy: Arc<Mutex<()>>,
}

impl Composite {
    /// Opens the workspace's composite at `dir`, creating it the first
    /// time, to read the objects of the folders' shadows at `shadows`.
    pub fn open(dir: &Path, shadows: &[PathBuf]) -> Result<Composite> {
        let busy = busy(dir);
        let held = hold(&busy);
        if !dir.join("HEAD").is_file() {
            create(dir)?;
        }
        let mut alternates = String::new();
        for shadow in shadows {
            let objects = std::path::absolute(shadow.join("objects")).map_err(|e| e.to_string())?;
            alternates.push_str(&format!("{}\n", slashed(&objects)));
        }
        mirror(
            &dir.join("objects/info/alternates"),
            Some(alternates.as_bytes()),
        )?;
        let repo = Repository::open(dir).map_err(git)?;
        drop(held);
        Ok(Composite { repo, busy })
    }

    /// The composite repository, for the packs.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// The chat's snapshot of its folders: one tree with each of `parts`
    /// (a folder's name and its own tree, `Shadow::folder_tree`) under its
    /// name, committed on the chat's previous one, at the chat's ref. A
    /// folder with nothing in it is left out, as git keeps no empty
    /// folder. An unchanged tree keeps the chat's previous commit.
    pub fn compose(&self, chat: &str, parts: &[(String, Oid)]) -> Result<Oid> {
        let name = format!("{SNAPSHOTS}{chat}");
        if !Reference::is_valid_name(&name) {
            return Err(format!("Not a chat's id: {chat}"));
        }
        let _held = hold(&self.busy);
        let mut top = self.repo.treebuilder(None).map_err(git)?;
        for (folder, tree) in parts {
            if self.repo.find_tree(*tree).map_err(git)?.is_empty() {
                continue;
            }
            top.insert(folder, *tree, FileMode::Tree.into())
                .map_err(|e| {
                    format!(
                        "“{folder}” can't name a folder of the chat: {}",
                        e.message()
                    )
                })?;
        }
        let tree = top.write().map_err(git)?;
        let previous = self
            .repo
            .find_reference(&name)
            .and_then(|reference| reference.peel_to_commit())
            .ok();
        let commit = match previous {
            Some(previous) if previous.tree_id() == tree => previous.id(),
            previous => {
                let parent = previous.map(|p| p.id());
                rovik_commit(&self.repo, tree, parent, &snapshot_message(chat))?
            }
        };
        self.repo
            .reference(&name, commit, true, "snapshot")
            .map_err(git)?;
        upkeep(&self.repo)?;
        Ok(commit)
    }

    /// The chat's result `result`, on its base `base`, both fetched into
    /// the composite, grafted into the folder `name`'s `shadow`: the
    /// objects of each one's part for the folder copied there, and each
    /// committed there on the folder's own tree (the chat's snapshot of
    /// it), the result on the base. Returns the result's commit there,
    /// which the folder's import takes (`Shadow::import`).
    pub fn graft(
        &self,
        name: &str,
        shadow: &Shadow,
        chat: &str,
        (base, result): (Oid, Oid),
    ) -> Result<Oid> {
        let _held = hold(&self.busy);
        let part = |commit: Oid| -> Result<Option<Oid>> {
            let commit = self.repo.find_commit(commit).map_err(git)?;
            let tree = commit.tree().map_err(git)?;
            let entry = tree.get_name(name);
            Ok(entry
                .filter(|entry| entry.kind() == Some(ObjectType::Tree))
                .map(|entry| entry.id()))
        };
        let (base_part, result_part) = (part(base)?, part(result)?);
        let _shadow = hold(&shadow.busy);
        let (from, to) = (
            self.repo.odb().map_err(git)?,
            shadow.repo.odb().map_err(git)?,
        );
        for tree in [base_part, result_part].into_iter().flatten() {
            copy_tree(&self.repo, &from, &to, tree)?;
        }
        let snapshot = format!("{SNAPSHOTS}{chat}");
        let own = (shadow.repo.find_reference(&snapshot))
            .and_then(|reference| reference.peel_to_tree())
            .ok();
        let on_own = |part: Option<Oid>| on_tree(&shadow.repo, own.as_ref(), &shadow.subpath, part);
        let base_tree = on_own(base_part)?;
        let result_tree = on_own(result_part)?;
        let grafted_base = rovik_commit(
            &shadow.repo,
            base_tree,
            None,
            &format!("Base of a result of chat {chat}"),
        )?;
        rovik_commit(
            &shadow.repo,
            result_tree,
            Some(grafted_base),
            &format!("Result of chat {chat}"),
        )
    }
}

/// `own`, the folder's tree in its repository's (none: an empty one), with
/// `part` at the folder's place in it (`subpath`), or nothing there for
/// none.
fn on_tree(repo: &Repository, own: Option<&Tree>, subpath: &str, part: Option<Oid>) -> Result<Oid> {
    if subpath.is_empty() {
        return part.map_or_else(|| empty_tree(repo), Ok);
    }
    let empty;
    let own = match own {
        Some(own) => own,
        None => {
            empty = repo.find_tree(empty_tree(repo)?).map_err(git)?;
            &empty
        }
    };
    let mut update = TreeUpdateBuilder::new();
    match part {
        Some(part) => update.upsert(subpath, part, FileMode::Tree),
        None => update.remove(subpath),
    };
    update.create_updated(repo, own).map_err(git)
}

/// Copies the tree `id` and what it holds that `to` lacks from `from`
/// (read through `repo`), each object after what it holds, so a tree `to`
/// has is whole there. A submodule's commit isn't carried, as snapshots
/// carry none.
fn copy_tree(repo: &Repository, from: &Odb, to: &Odb, id: Oid) -> Result<()> {
    if to.exists(id) {
        return Ok(());
    }
    let tree = repo.find_tree(id).map_err(git)?;
    for entry in tree.iter() {
        match entry.kind() {
            Some(ObjectType::Tree) => copy_tree(repo, from, to, entry.id())?,
            Some(ObjectType::Blob) if !to.exists(entry.id()) => copy(from, to, entry.id())?,
            _ => {}
        }
    }
    copy(from, to, id)
}

/// Copies the object `id` from `from` to `to`.
fn copy(from: &Odb, to: &Odb, id: Oid) -> Result<()> {
    let object = from.read(id).map_err(git)?;
    to.write(object.kind(), object.data())
        .map(drop)
        .map_err(git)
}

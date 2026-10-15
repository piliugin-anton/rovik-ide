//! A chat's result on the daemon (6.5): its worktree committed on top of
//! the landing base, at the chat's result ref. The tree is built through
//! the daemon's own index for the chat, `results/<chat>.index`, staged
//! against the base (`rovik_core::staging`): a file in the base that an
//! ignore rule matches stays in instead of landing as a deletion (K30), the
//! agent's `git status` in the worktree never changes under it, and later
//! results read only the files that changed. A result is committed when the
//! chat's run ends and on `WorkResult`; the newest is kept in `state.json`,
//! numbered after the one before it, with its outcome, the reason a local
//! chat's landing would give and the files a sync marked that still have
//! their markers (6.7), and `ResultReady` tells the chat's computer.
//!
//! Once a result has landed there, `WorkLanded` moves the landing base to
//! the result's content for the paths that landed, so the next result
//! holds only what hasn't (K34). A newer result made meanwhile, on the old
//! base, is committed again on the new one.

use crate::events::Events;
use crate::store::{Place, Store};
use git2::build::TreeUpdateBuilder;
use git2::{
    Delta, DiffFile, DiffFindOptions, FileMode, Index, ObjectType, Oid, Repository, ResetType,
    Signature,
};
use rovik_core::agent_run::{self, pending_reason, project, AgentConfig};
use rovik_core::overlay::markers;
use rovik_core::rt;
use rovik_core::staging::{self, Base};
use rovik_protocol::packs::result_ref;
use rovik_protocol::work::{Change, ChatResult, WorkChange};
use rovik_protocol::{Event, Fail, FailCode};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Results {
    /// The chats' result indexes.
    dir: PathBuf,
    /// The core's configuration folder, with the projects' settings.
    config_dir: PathBuf,
    store: Arc<Store>,
    events: Arc<Events>,
    /// One commit or move of a landing base at a time (a result, a landing,
    /// a sync): a result is always committed on the base as it is.
    base: tokio::sync::Mutex<()>,
}

impl Results {
    pub fn new(
        dir: PathBuf,
        config_dir: PathBuf,
        store: Arc<Store>,
        events: Arc<Events>,
    ) -> Arc<Results> {
        Arc::new(Results {
            dir,
            config_dir,
            store,
            events,
            base: tokio::sync::Mutex::new(()),
        })
    }

    /// After the chat's run ended with `outcome`: its result, when it has a
    /// worktree.
    pub fn after_run(self: &Arc<Self>, dialog_id: &str, outcome: &str) {
        if self.store.place(dialog_id).is_none() {
            return;
        }
        let (results, chat, outcome) = (self.clone(), dialog_id.to_string(), outcome.to_string());
        rt::spawn(async move {
            if let Err(fail) = results.commit(&chat, &outcome).await {
                log::warn!("{chat}'s result wasn't committed: {fail}");
            }
        });
    }

    /// Commits the chat's worktree as its result, after a run that ended
    /// with `outcome`, and tells the chat's computer.
    pub async fn commit(&self, dialog_id: &str, outcome: &str) -> Result<ChatResult, Fail> {
        let _base = self.base.lock().await;
        let (place, base) = self.worktree(dialog_id)?;
        let (index, chat) = (
            self.dir.join(format!("{dialog_id}.index")),
            dialog_id.to_string(),
        );
        let marked = self.store.marked_of(dialog_id);
        let root = place.worktree.join(self.store.subpath_of(dialog_id));
        let config = self.store.config_of(dialog_id);
        let config_dir = self.config_dir.clone();
        let committed = rt::spawn_blocking(move || {
            let commit = commit_worktree(&place.worktree, &index, &base, &chat)?;
            let marked = markers_left(&place.worktree, marked);
            let checks = config.is_some_and(|c| checks_would_run(&config_dir, &root, &c));
            Ok::<_, String>((commit, marked, checks))
        });
        let (commit, marked, checks) = committed
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)?;
        self.store.marks_left(dialog_id, marked.clone());
        let made = ChatResult {
            dialog_id: dialog_id.to_string(),
            commit: commit.to_string(),
            seq: 0,
            outcome: outcome.to_string(),
            pending: pending_reason(outcome),
            marked,
            unchecked: !checks,
        };
        let result = self.store.result_made(dialog_id, made);
        self.events
            .chat(dialog_id, Event::ResultReady(result.clone()));
        Ok(result)
    }

    /// `WorkLanded`: the chat's result `commit` landed on its computer. For
    /// `paths`, the landing base takes the result's content.
    pub async fn landed(
        &self,
        dialog_id: &str,
        commit: &str,
        paths: Vec<String>,
    ) -> Result<(), Fail> {
        let landed = Oid::from_str(commit)
            .map_err(|_| Fail::new(FailCode::BadRequest, "Not a commit's id."))?;
        {
            let _base = self.base.lock().await;
            let (place, base) = self.worktree(dialog_id)?;
            let chat = dialog_id.to_string();
            let moved = rt::spawn_blocking(move || {
                move_base(&place.worktree, &base, landed, &paths, &chat)
            });
            let base = moved.await.map_err(|e| internal(e.to_string()))??;
            self.store.base_moved(dialog_id, &base.to_string());
        }
        // A newer result, on the old base, would bring back what landed:
        // it's committed again, on the new one.
        let newer = self.store.result_of(dialog_id);
        if let Some(newer) = newer.filter(|result| result.commit != commit) {
            self.commit(dialog_id, &newer.outcome).await?;
        }
        Ok(())
    }

    /// What the chat changed in its worktree so far, from its landing base:
    /// what its result would change, in the project's folder (`subpath`).
    pub async fn changes(&self, dialog_id: &str, subpath: &str) -> Result<Vec<WorkChange>, Fail> {
        let _base = self.base.lock().await;
        let (place, base) = self.worktree(dialog_id)?;
        let (index, subpath) = (
            self.dir.join(format!("{dialog_id}.index")),
            subpath.to_string(),
        );
        let read = rt::spawn_blocking(move || changes_of(&place.worktree, &index, &base, &subpath));
        read.await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)
    }

    /// The landing bases, held while one is read and moved.
    pub async fn hold_base(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.base.lock().await
    }

    /// The chat's worktree and its landing base.
    fn worktree(&self, dialog_id: &str) -> Result<(Place, String), Fail> {
        let place = self.store.place(dialog_id).ok_or_else(|| {
            Fail::new(
                FailCode::BadRequest,
                "This chat has no worktree on this server.",
            )
        })?;
        let base = self.store.base_of(dialog_id).ok_or_else(|| {
            Fail::new(
                FailCode::Internal,
                "The chat's worktree has no base commit.",
            )
        })?;
        Ok((place, base))
    }
}

/// Stages the worktree through the chat's own index, against `base`, and
/// commits it on `base` at the chat's result ref. An unchanged tree keeps
/// the previous result.
fn commit_worktree(worktree: &Path, index: &Path, base: &str, chat: &str) -> Result<Oid, String> {
    let git = |e: git2::Error| e.message().to_string();
    let (repo, base, tree) = stage_worktree(worktree, index, base)?;
    let base = repo.find_commit(base).map_err(git)?;
    let name = result_ref(chat);
    let previous = repo
        .find_reference(&name)
        .and_then(|reference| reference.peel_to_commit())
        .ok();
    if let Some(previous) =
        previous.filter(|p| p.tree_id() == tree && p.parent_id(0).ok() == Some(base.id()))
    {
        return Ok(previous.id());
    }
    let rovik = Signature::now("Rovik", "results@rovik.invalid").map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let message = format!("Result of chat {chat}");
    let commit = repo
        .commit(None, &rovik, &rovik, &message, &tree, &[&base])
        .map_err(git)?;
    repo.reference(&name, commit, true, "result").map_err(git)?;
    Ok(commit)
}

/// The worktree staged through the chat's own index against `base`: its
/// repository, the base, and the tree a result of it has.
fn stage_worktree(
    worktree: &Path,
    index: &Path,
    base: &str,
) -> Result<(Repository, Oid, Oid), String> {
    let git = |e: git2::Error| e.message().to_string();
    let repo = Repository::open(worktree).map_err(git)?;
    if let Some(dir) = index.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut own = Index::open(index).map_err(git)?;
    repo.set_index(&mut own).map_err(git)?;
    let (base, tree) = {
        let base = (Oid::from_str(base).and_then(|base| repo.find_commit(base))).map_err(git)?;
        let base_tree = base.tree().map_err(git)?;
        (
            base.id(),
            staging::stage(&repo, Base::Tree(&base_tree))?.tree,
        )
    };
    Ok((repo, base, tree))
}

/// What a result of the worktree would change on its base, in the
/// project's folder (`subpath` in the tree).
fn changes_of(
    worktree: &Path,
    index: &Path,
    base: &str,
    subpath: &str,
) -> Result<Vec<WorkChange>, String> {
    let git = |e: git2::Error| e.message().to_string();
    let (repo, base, tree) = stage_worktree(worktree, index, base)?;
    let base = repo
        .find_commit(base)
        .and_then(|base| base.tree())
        .map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let mut diff = repo
        .diff_tree_to_tree(Some(&base), Some(&tree), None)
        .map_err(git)?;
    let mut renames = DiffFindOptions::new();
    renames.renames(true);
    diff.find_similar(Some(&mut renames)).map_err(git)?;
    let prefix = if subpath.is_empty() {
        String::new()
    } else {
        format!("{subpath}/")
    };
    let in_project = |file: DiffFile| {
        let path = file.path()?.to_string_lossy().replace('\\', "/");
        path.strip_prefix(&prefix).map(String::from)
    };
    let mut changes: Vec<WorkChange> = Vec::new();
    let mut add = |path: String, change, from| {
        // A change of kind comes as a deletion and an addition.
        match changes.iter_mut().find(|c| c.path == path) {
            Some(known) => known.change = Change::Changed,
            None => changes.push(WorkChange { path, change, from }),
        }
    };
    for delta in diff.deltas() {
        let (was, now) = (in_project(delta.old_file()), in_project(delta.new_file()));
        match (delta.status(), was, now) {
            (Delta::Added, _, Some(now)) => add(now, Change::Added, None),
            (Delta::Deleted, Some(was), _) => add(was, Change::Deleted, None),
            // A move, as a local chat lists one: from a place now deleted.
            // One across the project's folder's edge is what's inside it.
            (Delta::Renamed, Some(was), Some(now)) => {
                add(was.clone(), Change::Deleted, None);
                add(now, Change::Moved, Some(was));
            }
            (Delta::Renamed, Some(was), None) => add(was, Change::Deleted, None),
            (Delta::Renamed, None, Some(now)) => add(now, Change::Added, None),
            (Delta::Added | Delta::Deleted | Delta::Renamed, ..) => {}
            (_, _, Some(now)) => add(now, Change::Changed, None),
            _ => {}
        }
    }
    Ok(changes)
}

/// Whether the chat's checks would run on its result merged with the
/// project's newer changes: those its run in `root` has, by its computer's
/// configuration, as a local chat's landing asks (6.5).
fn checks_would_run(config_dir: &Path, root: &Path, config: &AgentConfig) -> bool {
    let (p, root) = (&config.permissions, root.to_string_lossy());
    let (checks, _) = project::load(config_dir, &root, &root, p.verify_changes);
    agent_run::checks_would_run(p, &checks)
}

/// Of the files the chat was given conflict markers in, those that still
/// have them, from its root: what its run is reminded of (6.7).
pub fn marked_left(store: &Store, chat: &str) -> Vec<String> {
    let Some(place) = store.place(chat) else {
        return Vec::new();
    };
    let left = markers_left(&place.worktree, store.marked_of(chat));
    in_root(&store.subpath_of(chat), left)
}

/// Paths from the worktree's root, from the chat's root instead: the
/// project's folder in it (`subpath`). Those outside it are left out.
pub fn in_root(subpath: &str, paths: Vec<String>) -> Vec<String> {
    if subpath.is_empty() {
        return paths;
    }
    let prefix = format!("{subpath}/");
    let paths = paths.into_iter();
    paths
        .filter_map(|path| path.strip_prefix(&prefix).map(String::from))
        .collect()
}

/// Of the files a sync marked, those whose markers are still in the
/// worktree.
fn markers_left(worktree: &Path, marked: Vec<String>) -> Vec<String> {
    let left = |path: &String| fs::read(worktree.join(path)).is_ok_and(|f| markers::left_in(&f));
    marked.into_iter().filter(left).collect()
}

/// A new landing base: `base` with `landed`'s entries for `paths`, as one
/// commit on it. The chat's branch moves to it and the worktree's index
/// follows (mixed), so the agent's `git status` shows only the changes that
/// didn't land; its files stay as they are.
pub fn move_base(
    worktree: &Path,
    base: &str,
    landed: Oid,
    paths: &[String],
    chat: &str,
) -> Result<Oid, Fail> {
    let git = |e: git2::Error| internal(e.message().to_string());
    let repo = Repository::open(worktree).map_err(git)?;
    let base = (Oid::from_str(base).and_then(|base| repo.find_commit(base))).map_err(git)?;
    let landed = repo
        .find_commit(landed)
        .map_err(|_| Fail::new(FailCode::BadRequest, "The result isn't on this server."))?;
    let (base_tree, landed_tree) = (base.tree().map_err(git)?, landed.tree().map_err(git)?);
    let mut update = TreeUpdateBuilder::new();
    for path in paths {
        let entry = landed_tree.get_path(Path::new(path)).ok();
        match entry.and_then(|entry| Some((entry.id(), file_mode(&entry)?))) {
            Some((id, mode)) => {
                update.upsert(path.as_str(), id, mode);
            }
            None if base_tree.get_path(Path::new(path)).is_ok() => {
                update.remove(path.as_str());
            }
            None => {}
        }
    }
    let tree = update.create_updated(&repo, &base_tree).map_err(git)?;
    if tree == base_tree.id() {
        return Ok(base.id());
    }
    let rovik = Signature::now("Rovik", "results@rovik.invalid").map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let message = format!("Landed from chat {chat}");
    let commit = repo
        .commit(None, &rovik, &rovik, &message, &tree, &[&base])
        .map_err(git)?;
    let target = repo.find_object(commit, None).map_err(git)?;
    repo.reset(&target, ResetType::Mixed, None).map_err(git)?;
    Ok(commit)
}

/// A file's mode in a tree: a blob or a link, never a folder or a
/// submodule.
fn file_mode(entry: &git2::TreeEntry) -> Option<FileMode> {
    if entry.kind() != Some(ObjectType::Blob) {
        return None;
    }
    Some(match entry.filemode() {
        0o100755 => FileMode::BlobExecutable,
        0o120000 => FileMode::Link,
        _ => FileMode::Blob,
    })
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::in_root;

    #[test]
    fn paths_from_the_worktree_are_named_from_the_chats_root() {
        let paths = || vec!["web/src/a.ts".to_string(), "api/main.go".to_string()];
        assert_eq!(in_root("", paths()), paths());
        assert_eq!(in_root("web", paths()), ["src/a.ts"], "api is outside");
    }
}

//! A chat's result on the daemon (6.5): its worktree committed on top of
//! the landing base, at the chat's result ref. The tree is built through
//! the daemon's own index for the chat, `results/<chat>.index`, staged
//! against the base (`rovik_core::staging`): a file in the base that an
//! ignore rule matches stays in instead of landing as a deletion (K30), the
//! agent's `git status` in the worktree never changes under it, and later
//! results read only the files that changed. A result is committed when the
//! chat's run ends and on `WorkResult`; the newest is kept in `state.json`
//! with its outcome, the reason a local chat's landing would give and the
//! files a sync marked that still have their markers (6.7), and
//! `ResultReady` tells the chat's computer.
//!
//! Once a result has landed there, `WorkLanded` moves the landing base to
//! the result's content for the paths that landed, so the next result
//! holds only what hasn't (K34). A newer result made meanwhile, on the old
//! base, is committed again on the new one.

use crate::events::Events;
use crate::store::{Place, Store};
use git2::build::TreeUpdateBuilder;
use git2::{FileMode, Index, ObjectType, Oid, Repository, ResetType, Signature};
use rovik_core::agent_run::pending_reason;
use rovik_core::overlay::markers;
use rovik_core::rt;
use rovik_core::staging::{self, Base};
use rovik_protocol::packs::result_ref;
use rovik_protocol::work::ChatResult;
use rovik_protocol::{Event, Fail, FailCode};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Results {
    /// The chats' result indexes.
    dir: PathBuf,
    store: Arc<Store>,
    events: Arc<Events>,
    /// One commit or move of a landing base at a time (a result, a landing,
    /// a sync): a result is always committed on the base as it is.
    base: tokio::sync::Mutex<()>,
}

impl Results {
    pub fn new(dir: PathBuf, store: Arc<Store>, events: Arc<Events>) -> Arc<Results> {
        Arc::new(Results {
            dir,
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
        let committed = rt::spawn_blocking(move || {
            let commit = commit_worktree(&place.worktree, &index, &base, &chat)?;
            Ok::<_, String>((commit, markers_left(&place.worktree, marked)))
        });
        let (commit, marked) = committed
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)?;
        self.store.marks_left(dialog_id, marked.clone());
        let result = ChatResult {
            dialog_id: dialog_id.to_string(),
            commit: commit.to_string(),
            outcome: outcome.to_string(),
            pending: pending_reason(outcome),
            marked,
        };
        self.store.result_made(dialog_id, result.clone());
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
    let repo = Repository::open(worktree).map_err(git)?;
    if let Some(dir) = index.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut own = Index::open(index).map_err(git)?;
    repo.set_index(&mut own).map_err(git)?;
    let base = repo
        .find_commit(Oid::from_str(base).map_err(git)?)
        .map_err(git)?;
    let base_tree = base.tree().map_err(git)?;
    let tree = staging::stage(&repo, Base::Tree(&base_tree))?.tree;
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
fn move_base(
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

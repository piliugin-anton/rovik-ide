//! A chat's result on the daemon (6.5): its worktree committed on top of
//! the landing base, at the chat's result ref. The tree is built through
//! the daemon's own index for the chat, `results/<chat>.index`, staged
//! against the base (`rovik_core::staging`): a file in the base that an
//! ignore rule matches stays in instead of landing as a deletion (K30), the
//! agent's `git status` in the worktree never changes under it, and later
//! results read only the files that changed. A result is committed when the
//! chat's run ends and on `WorkResult`; the newest is kept in `state.json`
//! with its outcome and the reason a local chat's landing would give, and
//! `ResultReady` tells the chat's computer.

use crate::events::Events;
use crate::store::Store;
use git2::{Index, Oid, Repository, Signature};
use rovik_core::agent_run::pending_reason;
use rovik_core::rt;
use rovik_core::staging::{self, Base};
use rovik_protocol::packs::result_ref;
use rovik_protocol::work::ChatResult;
use rovik_protocol::{Event, Fail, FailCode};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Results {
    /// The chats' result indexes.
    dir: PathBuf,
    store: Arc<Store>,
    events: Arc<Events>,
}

impl Results {
    pub fn new(dir: PathBuf, store: Arc<Store>, events: Arc<Events>) -> Arc<Results> {
        Arc::new(Results { dir, store, events })
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
        let (index, chat) = (
            self.dir.join(format!("{dialog_id}.index")),
            dialog_id.to_string(),
        );
        let committed =
            rt::spawn_blocking(move || commit_worktree(&place.worktree, &index, &base, &chat));
        let commit = committed
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)?;
        let result = ChatResult {
            dialog_id: dialog_id.to_string(),
            commit: commit.to_string(),
            outcome: outcome.to_string(),
            pending: pending_reason(outcome),
        };
        self.store.result_made(dialog_id, result.clone());
        self.events
            .chat(dialog_id, Event::ResultReady(result.clone()));
        Ok(result)
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

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

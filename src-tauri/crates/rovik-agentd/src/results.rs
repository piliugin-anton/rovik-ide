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
//!
//! What a merge or markers replaced in the chat's files, which Undo the
//! merge puts back (6.1), is kept in `state.json` and held by the chat's
//! `kept` ref, a commit of those blobs, so that no `git gc` prunes them.

use crate::deliver;
use crate::events::Events;
use crate::fail::{git, internal};
use crate::store::{Kept, Place, Store};
use git2::build::TreeUpdateBuilder;
use git2::{
    Delta, DiffFile, DiffFindOptions, FileMode, Index, ObjectType, Oid, Repository, ResetType,
    Signature,
};
use rovik_core::agent_run::autopilot::{DeliverAsk, Pushed};
use rovik_core::agent_run::{self, pending_reason_in, project, AgentConfig};
use rovik_core::overlay::markers;
use rovik_core::rt;
use rovik_core::staging::{self, Base};
use rovik_protocol::autopilot::Delivery;
use rovik_protocol::packs::{kept_ref, result_ref};
use rovik_protocol::work::{Change, ChatResult, WorkChange};
use rovik_protocol::{Event, Fail, FailCode};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A commit after a run's end, counted while it's to be made: uncounted
/// once dropped, however its task ends (a panic too), so what waits for
/// the chat's commits (`all_committed`) never waits for one that won't
/// come.
struct Committing {
    results: Arc<Results>,
    chat: String,
}

impl Committing {
    fn new(results: &Arc<Results>, chat: &str) -> Committing {
        results.counted(chat, 1);
        Committing {
            results: results.clone(),
            chat: chat.to_string(),
        }
    }
}

impl Drop for Committing {
    fn drop(&mut self) {
        self.results.counted(&self.chat, -1);
    }
}

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
    /// The commits after a run's end not made yet, by chat, and when one
    /// is: a delivery waits for its task's last (H04).
    committing: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    committed: tokio::sync::Notify,
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
            committing: Default::default(),
            committed: tokio::sync::Notify::new(),
        })
    }

    /// At start-up: the lock files a killed process left on the chats'
    /// result indexes, which libgit2 never removes and would refuse every
    /// later result for. This process is the only one on them.
    pub fn clear_stale_locks(&self) {
        remove_locks(&self.dir, false);
    }

    /// After the chat's run ended with `outcome`: its result, when it has a
    /// worktree. `autopilot`: the run carried out an Autopilot task's
    /// stage, whose result doesn't land unchecked (V02).
    pub fn after_run(self: &Arc<Self>, dialog_id: &str, outcome: &str, autopilot: bool) {
        if self.store.place(dialog_id).is_none() {
            return;
        }
        let (results, chat, outcome) = (self.clone(), dialog_id.to_string(), outcome.to_string());
        let counted = Committing::new(self, &chat);
        rt::spawn(async move {
            let _counted = counted;
            if let Err(fail) = results.commit_after(&chat, &outcome, autopilot).await {
                log::warn!("{chat}'s result wasn't committed: {fail}");
            }
        });
    }

    /// One more (or one fewer) commit after a run's end of the chat to make.
    fn counted(&self, dialog_id: &str, by: isize) {
        let mut committing = self.committing.lock().unwrap_or_else(|e| e.into_inner());
        let left = committing.get(dialog_id).copied().unwrap_or(0) as isize + by;
        match left > 0 {
            true => committing.insert(dialog_id.to_string(), left as usize),
            false => committing.remove(dialog_id),
        };
        if by < 0 {
            self.committed.notify_waiters();
        }
    }

    /// Once the chat's runs' results are committed: none left to make.
    async fn all_committed(&self, dialog_id: &str) {
        loop {
            let committed = self.committed.notified();
            tokio::pin!(committed);
            committed.as_mut().enable();
            let left = {
                let committing = self.committing.lock().unwrap_or_else(|e| e.into_inner());
                committing.get(dialog_id).copied().unwrap_or(0)
            };
            if left == 0 {
                return;
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(60), committed).await;
        }
    }

    /// D15, H04: the chat's task delivered a landable result, which goes to
    /// its branch on its project's remote when its computer has this server
    /// push its results, and its pull request opens when it asked. None:
    /// the computer keeps its results here. The result is the worktree as
    /// it is now, committed (the same result when nothing changed since its
    /// run's end); it reaches its computer as ever, with where it went.
    pub async fn deliver(self: &Arc<Self>, ask: DeliverAsk) -> Option<Pushed> {
        let chat = ask.dialog_id.as_str();
        let Delivery::PushBranch {
            deploy_key,
            open_pr,
        } = self.store.autonomy_of(chat).delivery
        else {
            return None;
        };
        let failed = |problem: String| {
            Some(Pushed {
                problem: Some(problem),
                ..Pushed::default()
            })
        };
        self.all_committed(chat).await;
        if self.store.result_of(chat).is_none() {
            return failed(
                "There's no result to push: this chat has no worktree on the server".into(),
            );
        }
        let result = match self.recommit(chat).await {
            Ok(result) => result,
            Err(fail) => {
                return failed(format!(
                    "Its result couldn't be committed: {}",
                    fail.message
                ))
            }
        };
        let branch = deliver::branch_name(&ask.title, &ask.task);
        // The same result delivered already to this task's branch (a
        // delivery whose report broke off, done again without it): where
        // it went, not a second push and pull request.
        if result.branch.as_deref() == Some(branch.as_str()) {
            return Some(Pushed {
                branch: Some(branch),
                pr: result.pr.clone(),
                problem: None,
            });
        }
        let Some(remote) = self.store.remote_of(chat) else {
            return failed(
                "The project has no remote to push to: its `origin` wasn't set on the computer it came from"
                    .into(),
            );
        };
        let worktree = match self.worktree(chat) {
            Ok((place, _)) => place.worktree,
            Err(fail) => return failed(fail.message),
        };
        let pushed = deliver::push(&worktree, &remote, &result.commit, &branch, &deploy_key).await;
        if let Err(e) = pushed {
            log::warn!("chat {chat}: the push of {branch} to {remote} failed: {e}");
            return failed(format!("The push to {remote} failed: {e}"));
        }
        log::info!("chat {chat}: its result went to {branch} on {remote}");
        let (pr, problem) = match open_pr {
            false => (None, None),
            true => match deliver::open_pull_request(&remote, &branch, &ask.title, &ask.body).await
            {
                Ok(url) => (Some(url), None),
                Err(e) => {
                    log::warn!("chat {chat}: no pull request for {branch}: {e}");
                    (None, Some(format!("No pull request: {e}")))
                }
            },
        };
        if let Some(result) = self.store.result_delivered(chat, &branch, pr.as_deref()) {
            self.events.chat(chat, Event::ResultReady(result));
        }
        Some(Pushed {
            branch: Some(branch),
            pr,
            problem,
        })
    }

    /// Commits the chat's worktree as its result, after a run that ended
    /// with `outcome`, and tells the chat's computer.
    pub async fn commit(&self, dialog_id: &str, outcome: &str) -> Result<ChatResult, Fail> {
        self.commit_after(dialog_id, outcome, false).await
    }

    /// `commit`, after an Autopilot task's run when `autopilot`.
    pub async fn commit_after(
        &self,
        dialog_id: &str,
        outcome: &str,
        autopilot: bool,
    ) -> Result<ChatResult, Fail> {
        let hold = pending_reason_in(outcome, autopilot);
        self.commit_holding(dialog_id, outcome, hold).await
    }

    /// Commits the chat's worktree again as its result when it changed with
    /// no run of its own (a merge of its project's newer changes, a merge
    /// undone, a landing that moved its base), or as its computer asks for
    /// it (a move back): once the results of its ended runs are in, what the
    /// last of them made of it stays, its outcome and why it waits to land.
    /// The last run of an Autopilot task, its delivery, brings no result,
    /// and doesn't make one landable. A chat with no result yet is committed
    /// as its last run ended.
    pub async fn recommit(&self, dialog_id: &str) -> Result<ChatResult, Fail> {
        self.all_committed(dialog_id).await;
        match self.store.result_of(dialog_id) {
            Some(last) => {
                self.commit_holding(dialog_id, &last.outcome, last.pending)
                    .await
            }
            None => {
                let outcome = self.store.outcome_of(dialog_id);
                self.commit(dialog_id, outcome.as_deref().unwrap_or("unknown"))
                    .await
            }
        }
    }

    /// `commit`, the result held back for `hold` (None: it may land), or for
    /// why the chat's changes waited when it moved here (6.8).
    async fn commit_holding(
        &self,
        dialog_id: &str,
        outcome: &str,
        hold: Option<String>,
    ) -> Result<ChatResult, Fail> {
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
            // A chat that moved here waits as it waited there until a run
            // ends here (6.8).
            pending: self.store.pending_of(dialog_id).or(hold),
            marked,
            unchecked: !checks,
            branch: None,
            pr: None,
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
            let (chat, landed_paths) = (dialog_id.to_string(), paths.clone());
            let moved = rt::spawn_blocking(move || {
                move_base(&place.worktree, &base, landed, &landed_paths, &chat)
            });
            let base = moved.await.map_err(|e| internal(e.to_string()))??;
            self.store.base_moved(dialog_id, &base.to_string());
            // What landed can't be taken back there any more: forgotten
            // once the base has moved, so a landing the server refuses
            // keeps Undo the merge.
            self.forget_kept(dialog_id, Some(&paths)).await;
        }
        // A newer result, on the old base, would bring back what landed:
        // it's committed again, on the new one.
        let newer = self.store.result_of(dialog_id);
        if newer.is_some_and(|result| result.commit != commit) {
            self.recommit(dialog_id).await?;
        }
        Ok(())
    }

    /// What the chat changed in its worktree so far, from its landing base:
    /// what its result would change, in the project's folder (`subpath`),
    /// with the files a merge or markers replaced.
    pub async fn changes(&self, dialog_id: &str, subpath: &str) -> Result<Vec<WorkChange>, Fail> {
        let _base = self.base.lock().await;
        let (place, base) = self.worktree(dialog_id)?;
        let kept = self.store.kept_of(dialog_id).into_keys().collect();
        let (index, subpath) = (
            self.dir.join(format!("{dialog_id}.index")),
            subpath.to_string(),
        );
        let read = rt::spawn_blocking(move || {
            let merged = in_root(&subpath, kept);
            changes_of(&place.worktree, &index, &base, &subpath, &merged)
        });
        read.await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)
    }

    /// The landing bases, held while one is read and moved.
    pub async fn hold_base(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.base.lock().await
    }

    /// The index a worktree is staged through for `name`'s commits: a
    /// chat's results (`<chat>.index`), a candidate's start (T05).
    pub fn index_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.index"))
    }

    /// Keeps what merges or markers replaced in the chat's files.
    pub async fn keep(&self, dialog_id: &str, kept: Vec<(String, Kept)>) {
        if kept.is_empty() {
            return;
        }
        self.store.keep(dialog_id, kept);
        self.hold_kept(dialog_id).await;
    }

    /// Forgets what was kept of `paths`, or of every file.
    pub async fn forget_kept(&self, dialog_id: &str, paths: Option<&[String]>) {
        self.store.forget_kept(dialog_id, paths);
        self.hold_kept(dialog_id).await;
    }

    /// The chat's `kept` ref, as its kept blobs are now. Without it, they'd
    /// still serve until a `git gc` pruned them: a failure is only told.
    async fn hold_kept(&self, dialog_id: &str) {
        let Some(place) = self.store.place(dialog_id) else {
            return;
        };
        let (kept, chat) = (self.store.kept_of(dialog_id), dialog_id.to_string());
        let held = rt::spawn_blocking(move || hold(&place.worktree, &chat, &kept));
        let held = held.await.map_err(|e| e.to_string());
        if let Err(e) = held.and_then(|held| held.map_err(|e| e.message().to_string())) {
            log::warn!("{dialog_id}'s kept versions aren't held by a ref: {e}");
        }
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

/// Rovik's own signature on the commits it makes.
pub fn rovik_signature() -> Result<Signature<'static>, git2::Error> {
    Signature::now("Rovik", "results@rovik.invalid")
}

/// Stages the worktree through the chat's own index, against `base`, and
/// commits it on `base` at the chat's result ref. An unchanged tree keeps
/// the previous result.
fn commit_worktree(worktree: &Path, index: &Path, base: &str, chat: &str) -> Result<Oid, String> {
    let message = format!("Result of chat {chat}");
    commit_at(worktree, index, base, &result_ref(chat), &message)
}

/// The worktree staged through `index` against `base` and committed on it
/// at the ref `name`, as a result is (`commit_worktree`): an unchanged
/// tree keeps the commit there. T05 commits a Thorough task's candidates'
/// starts and changes so (candidates.rs).
pub fn commit_at(
    worktree: &Path,
    index: &Path,
    base: &str,
    name: &str,
    message: &str,
) -> Result<Oid, String> {
    let git = |e: git2::Error| e.message().to_string();
    let (repo, base, tree) = stage_worktree(worktree, index, base)?;
    let base = repo.find_commit(base).map_err(git)?;
    let previous = repo
        .find_reference(name)
        .and_then(|reference| reference.peel_to_commit())
        .ok();
    if let Some(previous) =
        previous.filter(|p| p.tree_id() == tree && p.parent_id(0).ok() == Some(base.id()))
    {
        return Ok(previous.id());
    }
    let rovik = rovik_signature().map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let commit = repo
        .commit(None, &rovik, &rovik, message, &tree, &[&base])
        .map_err(git)?;
    repo.reference(name, commit, true, "result").map_err(git)?;
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
    merged: &[String],
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
            None => changes.push(WorkChange {
                merged: merged.contains(&path),
                path,
                change,
                from,
            }),
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

/// Points the chat's `kept` ref at a commit of its kept blobs, each named
/// by its id, or deletes it once nothing is kept.
fn hold(worktree: &Path, chat: &str, kept: &BTreeMap<String, Kept>) -> Result<(), git2::Error> {
    let repo = Repository::open(worktree)?;
    let name = kept_ref(chat);
    if kept.is_empty() {
        return match repo.find_reference(&name) {
            Ok(mut held) => held.delete(),
            Err(_) => Ok(()),
        };
    }
    let mut tree = repo.treebuilder(None)?;
    for kept in kept.values() {
        let blobs = [Some(&kept.chat), kept.base.as_ref(), Some(&kept.written)];
        for id in blobs.into_iter().flatten() {
            tree.insert(id, Oid::from_str(id)?, 0o100644)?;
        }
    }
    let tree = repo.find_tree(tree.write()?)?;
    let rovik = rovik_signature()?;
    let message = format!("What Undo the merge puts back in chat {chat}");
    let held = repo.commit(None, &rovik, &rovik, &message, &tree, &[])?;
    repo.reference(&name, held, true, "kept")?;
    Ok(())
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
    let repo = Repository::open(worktree).map_err(git)?;
    let base = (Oid::from_str(base).and_then(|base| repo.find_commit(base))).map_err(git)?;
    let landed = repo
        .find_commit(landed)
        .map_err(|_| Fail::new(FailCode::BadRequest, "The result isn't on this server."))?;
    let landed_tree = landed.tree().map_err(git)?;
    let entries: Vec<(String, Option<(Oid, FileMode)>)> = paths
        .iter()
        .map(|path| {
            let entry = landed_tree.get_path(Path::new(path)).ok();
            let entry = entry.and_then(|entry| Some((entry.id(), file_mode(&entry)?)));
            (path.clone(), entry)
        })
        .collect();
    set_base(&repo, &base, &entries, &format!("Landed from chat {chat}"))
}

/// A new landing base: `base` with `entries` (a path's blob and mode, or
/// none: not there), as one commit on it. The chat's branch moves to it
/// and the worktree's index follows (mixed); its files stay as they are.
pub fn set_base(
    repo: &Repository,
    base: &git2::Commit,
    entries: &[(String, Option<(Oid, FileMode)>)],
    message: &str,
) -> Result<Oid, Fail> {
    let base_tree = base.tree().map_err(git)?;
    let mut update = TreeUpdateBuilder::new();
    for (path, entry) in entries {
        match entry {
            Some((id, mode)) => {
                update.upsert(path.as_str(), *id, *mode);
            }
            None if base_tree.get_path(Path::new(path)).is_ok() => {
                update.remove(path.as_str());
            }
            None => {}
        }
    }
    let tree = update.create_updated(repo, &base_tree).map_err(git)?;
    if tree == base_tree.id() {
        return Ok(base.id());
    }
    let rovik = rovik_signature().map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let commit = repo
        .commit(None, &rovik, &rovik, message, &tree, &[base])
        .map_err(git)?;
    let target = repo.find_object(commit, None).map_err(git)?;
    repo.reset(&target, ResetType::Mixed, None).map_err(git)?;
    Ok(commit)
}

/// Removes the `.lock` files in `dir`, and under it when `deep`: what a
/// process killed in the middle of a write left, which the next write
/// takes for one still going. Only for folders this process alone writes.
pub fn remove_locks(dir: &Path, deep: bool) {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    for entry in entries {
        let path = entry.path();
        let Ok(meta) = entry.file_type() else {
            continue;
        };
        if meta.is_dir() {
            if deep {
                remove_locks(&path, true);
            }
        } else if path.extension().is_some_and(|e| e == "lock") {
            match fs::remove_file(&path) {
                Ok(()) => log::warn!("{}: a stale lock, removed", path.display()),
                Err(e) => log::warn!("{}: a stale lock stays: {e}", path.display()),
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// H04: a commit whose task panicked isn't waited for: the delivery
    /// and the rechecks after it go on.
    #[tokio::test]
    async fn a_commit_that_panicked_is_waited_for_no_more() {
        let dir = rovik_core::testing::temp_dir();
        let store = Arc::new(Store::open(&dir.0).unwrap());
        let events = Events::new("boot".into(), Arc::new(|_: &str| None));
        let results = Results::new(dir.0.join("results"), dir.0.join("config"), store, events);
        let counted = Committing::new(&results, "chat");
        let broke = tokio::spawn(async move {
            let _counted = counted;
            panic!("the commit broke");
        });
        assert!(broke.await.is_err());
        let waited = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            results.all_committed("chat"),
        );
        assert!(waited.await.is_ok(), "nothing left to wait for");
    }

    #[test]
    fn paths_from_the_worktree_are_named_from_the_chats_root() {
        let paths = || vec!["web/src/a.ts".to_string(), "api/main.go".to_string()];
        assert_eq!(in_root("", paths()), paths());
        assert_eq!(in_root("web", paths()), ["src/a.ts"], "api is outside");
    }
}

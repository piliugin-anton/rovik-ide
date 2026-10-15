//! Each chat's worktree on the daemon (6.4, D8). A project's repository
//! holds the snapshots its desktop pushed (T02). A chat works in a worktree
//! of its own in `work/<chat>`, on branch `rovik/<chat>` at the snapshot it
//! started from, and each later sync merges the project's newer snapshot
//! into it (sync.rs). Its root, the worktree or the project's folder in
//! it, is one of its project's folders and has the project's ID file, so a
//! run there works as a local chat does in its project. A deleted chat
//! takes its worktree, branch and refs with it, and start-up collects
//! those of chats deleted while the daemon was away. A chat that moves
//! here gets its worktree the way a first sync makes one (moves.rs).

use crate::daemon::Daemon;
use crate::store::{Place, ProjectState};
use crate::sync::{self, Merged};
use git2::build::CheckoutBuilder;
use git2::{BranchType, Oid, Repository, WorktreeAddOptions, WorktreePruneOptions};
use rovik_core::chat_db::{projects, store};
use rovik_core::{project_profile, rt};
use rovik_protocol::packs::{snapshot_ref, PUSHED_REFS};
use rovik_protocol::work::{ChatSync, Project, SyncState, Synced};
use rovik_protocol::{Event, Fail, FailCode, Response};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// `ProjectEnsure`: the project's repository with the client's ignore
/// rules, its row, and its settings for its worktrees.
pub async fn ensure_project(
    daemon: &Daemon,
    project: Project,
    settings: Option<Value>,
    excludes: Vec<String>,
) -> Result<Response, Fail> {
    let (projects, id) = (daemon.projects.clone(), project.id.clone());
    let prepared = rt::spawn_blocking(move || {
        let repo = projects.prepare(&id, &excludes)?;
        Ok::<_, Fail>(repo.path().to_path_buf())
    });
    let repo = prepared.await.map_err(joined)??;
    let (id, name) = (project.id.clone(), project.name.clone());
    let row = daemon
        .core
        .db
        .write(move |c| projects::ensure(c, &id, &name, &repo));
    row.await.map_err(internal)?;
    daemon
        .store
        .ensure_project(&project, settings)
        .map_err(internal)?;
    Ok(Response::Done)
}

/// `WorkSync` of `(chat, project)`: a chat without a worktree gets one at
/// the snapshot, with `changes` written into it when it moved with its
/// overlay (T09). A chat that has one gets the snapshot merged into it.
pub async fn sync(
    daemon: &Daemon,
    computer: &str,
    (chat, project): (&str, &str),
    snapshot: &str,
    excludes: Vec<String>,
    changes: Option<String>,
) -> Result<Response, Fail> {
    if !plain(chat) {
        return Err(bad_request(format!("{chat} isn't a chat's id.")));
    }
    daemon.store.check_owner(computer, chat)?;
    let state = daemon.store.project(project).ok_or_else(|| {
        bad_request("The project isn't on this server yet: ProjectEnsure comes first.")
    })?;
    let base = Oid::from_str(snapshot).map_err(|_| bad_request("Not a commit's id."))?;
    if let Some(root) = root(daemon, chat) {
        let merged = merge_into(daemon, chat, project, base, excludes).await?;
        return Ok(Response::Synced(Synced {
            root: text(&root),
            state: merged.state,
            files: merged.files,
        }));
    }
    let changes = commit_of(changes)?;
    let at = (project, &state);
    let (worktree, root) = new_worktree(daemon, chat, at, base, changes, excludes).await?;
    let (id, name, folder) = (project.to_string(), state.name.clone(), root.clone());
    let row = daemon
        .core
        .db
        .write(move |c| projects::ensure(c, &id, &name, &folder));
    row.await.map_err(internal)?;
    daemon.ensure_chat(chat, Some(project)).await?;
    let place = Place {
        project: project.to_string(),
        worktree,
    };
    daemon
        .store
        .worktree_made(computer, chat, place, snapshot)?;
    Ok(Response::Synced(synced(&root)))
}

/// `WorkRecheck` (6.6): the snapshot merged into the chat's worktree as a
/// sync does. What merged is checked by a run of the chat that starts with
/// its checks, whose end brings a new result; a merge that leaves nothing
/// to check is committed as a result at once.
pub async fn recheck(
    daemon: &Daemon,
    computer: &str,
    chat: &str,
    project: &str,
    snapshot: &str,
    excludes: Vec<String>,
) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    let root = root(daemon, chat)
        .ok_or_else(|| bad_request("This chat has no worktree on this server."))?;
    let snapshot = Oid::from_str(snapshot).map_err(|_| bad_request("Not a commit's id."))?;
    let merged = merge_into(daemon, chat, project, snapshot, excludes).await?;
    if merged.state == SyncState::Synced {
        if merged.files.is_empty() {
            let outcome = daemon.store.outcome_of(chat);
            let outcome = outcome.as_deref().unwrap_or("unknown");
            daemon.results.commit(chat, outcome).await?;
        } else {
            let (files, diffs) = (merged.files.clone(), merged.diffs.clone());
            daemon.check_merged(chat, files, diffs).await?;
        }
    }
    Ok(Response::Synced(Synced {
        root: text(&root),
        state: merged.state,
        files: merged.files,
    }))
}

/// Merges the snapshot into the chat's worktree, under the client's ignore
/// rules (sync.rs), and tells the chat's computer what it did.
async fn merge_into(
    daemon: &Daemon,
    chat: &str,
    project: &str,
    snapshot: Oid,
    excludes: Vec<String>,
) -> Result<Merged, Fail> {
    let place = daemon.store.place(chat).filter(|p| p.project == project);
    let place = place.ok_or_else(|| bad_request("This chat works in another project here."))?;
    let config = daemon.store.config_of(chat).unwrap_or_default();
    let resolves = config.permissions.chat_resolves_conflicts;
    let _base = daemon.results.hold_base().await;
    let base = daemon
        .store
        .base_of(chat)
        .ok_or_else(|| internal("The chat's worktree has no base commit.".into()))?;
    let (projects, id) = (daemon.projects.clone(), project.to_string());
    let merged = rt::spawn_blocking(move || {
        projects.prepare(&id, &excludes)?;
        sync::merge(&place.worktree, &base, snapshot, resolves)
    });
    let merged = merged.await.map_err(joined)??;
    if merged.applied() {
        daemon.store.base_moved(chat, &snapshot.to_string());
        daemon.results.keep(chat, merged.kept.clone()).await;
    }
    if merged.state == SyncState::Marked {
        daemon.store.mark(chat, &merged.files, true);
    }
    let told = ChatSync {
        dialog_id: chat.to_string(),
        state: merged.state,
        files: merged.files.clone(),
    };
    daemon.events.chat(chat, Event::SyncState(told));
    Ok(merged)
}

/// A new worktree for the chat at `base`, in place of what an earlier one
/// left, with `changes` written into it when it moved with its overlay
/// (T09), the project's ID file at its root and the project's settings for
/// it. Returns its folder and its root, which the caller makes one of the
/// project's folders with the chat's row.
pub async fn new_worktree(
    daemon: &Daemon,
    chat: &str,
    (project, state): (&str, &ProjectState),
    base: Oid,
    changes: Option<Oid>,
    excludes: Vec<String>,
) -> Result<(PathBuf, PathBuf), Fail> {
    let (caps, worktrees, name) = (
        daemon.caps.clone(),
        daemon.worktrees.clone(),
        chat.to_string(),
    );
    let room = rt::spawn_blocking(move || caps.room_for_worktree(&worktrees, &name));
    room.await
        .map_err(joined)?
        .map_err(|why| Fail::new(FailCode::Busy, why))?;
    let worktree = daemon.worktrees.join(chat);
    let (projects, id, name, at) = (
        daemon.projects.clone(),
        project.to_string(),
        chat.to_string(),
        worktree.clone(),
    );
    let made = rt::spawn_blocking(move || {
        let repo = projects.prepare(&id, &excludes)?;
        make_worktree(&repo, &name, base, &at)?;
        match changes {
            Some(changes) => write_changes(base, changes, &at),
            None => Ok(()),
        }
    });
    made.await.map_err(joined)??;
    let root = worktree.join(&state.subpath);
    projects::write_id_file(&root, project).map_err(|e| internal(e.to_string()))?;
    if let Some(settings) = state.settings.clone() {
        let config_dir = &daemon.core.paths.config_dir;
        let saved = project_profile::save_project_settings(config_dir, text(&root), settings);
        saved.map_err(internal)?;
    }
    Ok((worktree, root))
}

/// The project's repository, made with the client's ignore rules as
/// `ProjectEnsure` makes it, once it's found to have the snapshot a
/// worktree starts from and the chat's changes on it (T09): a move pushes
/// them before its import, which checks them before anything of the chat's
/// earlier stay goes. Returns the repository's folder.
pub async fn repository_with(
    daemon: &Daemon,
    project: &str,
    excludes: Vec<String>,
    base: Oid,
    changes: Option<Oid>,
) -> Result<PathBuf, Fail> {
    let (projects, id) = (daemon.projects.clone(), project.to_string());
    let checked = rt::spawn_blocking(move || {
        let repo = projects.prepare(&id, &excludes)?;
        repo.find_commit(base)
            .map_err(|_| bad_request("The snapshot isn't on this server: push it first."))?;
        if let Some(changes) = changes {
            let changes = repo.find_commit(changes).map_err(|_| {
                bad_request("The chat's changes aren't on this server: push them first.")
            })?;
            if changes.parent_id(0).ok() != Some(base) {
                return Err(bad_request(
                    "The chat's changes aren't a commit on its snapshot.",
                ));
            }
        }
        Ok(repo.path().to_path_buf())
    });
    checked.await.map_err(joined)?
}

/// A commit's id, as a request names it.
pub fn commit_of(id: Option<String>) -> Result<Option<Oid>, Fail> {
    id.map(|id| Oid::from_str(&id))
        .transpose()
        .map_err(|_| bad_request("Not a commit's id."))
}

/// The chat's root on this server, once it has a worktree: the worktree,
/// or the project's folder in it. Its runs work there (D8).
pub fn root(daemon: &Daemon, chat: &str) -> Option<PathBuf> {
    let place = daemon.store.place(chat).filter(|p| p.worktree.is_dir())?;
    let subpath = daemon.store.project(&place.project)?.subpath;
    Some(place.worktree.join(subpath))
}

/// After `ChatDelete`: the chat's worktree, its branch and its refs in
/// every project's repository, and its root among its project's folders.
pub async fn remove(daemon: &Daemon, chat: &str) {
    // No sync takes an id that isn't plain: nothing of it is on disk.
    if plain(chat) {
        clear(daemon, chat, None).await;
    }
    daemon.store.forget(chat);
}

/// What an earlier stay of the chat left (K36), before a move brings it
/// again: as `remove` takes, but the snapshot the move pushed to `project`.
/// Its rows go in the import's transaction, its state with the import.
pub async fn clear_stay(daemon: &Daemon, chat: &str, project: &str) {
    clear(daemon, chat, Some(project)).await
}

/// The chat's worktree, its branch and its refs in every project's
/// repository but its snapshot in `keep`'s, and its root among its
/// project's folders.
async fn clear(daemon: &Daemon, chat: &str, keep: Option<&str>) {
    if let Some(place) = daemon.store.place(chat) {
        let subpath = daemon
            .store
            .project(&place.project)
            .unwrap_or_default()
            .subpath;
        let root = place.worktree.join(subpath);
        let project = place.project.clone();
        let forgotten = daemon
            .core
            .db
            .write(move |c| projects::forget_path(c, &project, &root));
        if let Err(e) = forgotten.await {
            log::warn!("{chat}'s worktree stays among its project's folders: {e}");
        }
    }
    let (projects, name, worktree, keep) = (
        daemon.projects.clone(),
        chat.to_string(),
        daemon.worktrees.join(chat),
        keep.map(String::from),
    );
    let removed = rt::spawn_blocking(move || {
        let snapshot = snapshot_ref(&name);
        for id in projects.ids() {
            if let Ok(Some(repo)) = projects.open(&id) {
                let kept = (keep.as_deref() == Some(id.as_str())).then_some(snapshot.as_str());
                drop_chat(&repo, &name, kept);
            }
        }
        match fs::remove_dir_all(&worktree) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    });
    if let Ok(Err(e)) = removed.await {
        log::warn!("{chat}'s worktree stays: {e}");
    }
}

/// At start-up: the worktrees of chats deleted while the daemon was away,
/// and folders in `work/` that no chat has.
pub async fn collect(daemon: &Daemon) {
    let known = daemon.store.chats_with_worktrees();
    for chat in &known {
        let id = chat.clone();
        let row = daemon
            .core
            .db
            .read(move |c| store::get_dialog(c, &id))
            .await;
        if matches!(row, Ok(None)) {
            log::info!("{chat} was deleted: its worktree goes");
            remove(daemon, chat).await;
        }
    }
    let folders = fs::read_dir(&daemon.worktrees)
        .into_iter()
        .flatten()
        .flatten();
    for folder in folders {
        let name = folder.file_name().to_string_lossy().into_owned();
        if known.contains(&name) {
            continue;
        }
        log::info!("work/{name} belongs to no chat: it goes");
        if plain(&name) {
            remove(daemon, &name).await;
        } else if let Err(e) = fs::remove_dir_all(folder.path()) {
            log::warn!("work/{name} stays: {e}");
        }
    }
}

/// Makes the chat's worktree at `base`, on its branch, in place of what an
/// earlier one left. The checkout writes the files as the snapshot holds
/// them.
fn make_worktree(repo: &Repository, chat: &str, base: Oid, at: &Path) -> Result<(), Fail> {
    let commit = repo
        .find_commit(base)
        .map_err(|_| bad_request("The snapshot isn't on this server: push it first."))?;
    drop_chat_worktree(repo, chat);
    match fs::remove_dir_all(at) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(internal(e.to_string())),
        _ => {}
    }
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).map_err(|e| internal(e.to_string()))?;
    }
    let branch = repo.branch(&branch(chat), &commit, true).map_err(git)?;
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(branch.get()));
    repo.worktree(chat, at, Some(&opts)).map_err(git)?;
    Ok(())
}

/// Writes the chat's changes, a commit on its base, into its new worktree
/// as changes, its index left on the base: the agent's `git status` shows
/// them as the chat's own (T09).
fn write_changes(base: Oid, changes: Oid, at: &Path) -> Result<(), Fail> {
    let worktree = Repository::open(at).map_err(git)?;
    let changes = worktree
        .find_commit(changes)
        .map_err(|_| bad_request("The chat's changes aren't on this server: push them first."))?;
    if changes.parent_id(0).ok() != Some(base) {
        return Err(bad_request(
            "The chat's changes aren't a commit on its snapshot.",
        ));
    }
    let mut opts = CheckoutBuilder::new();
    opts.force().update_index(false);
    worktree
        .checkout_tree(changes.as_object(), Some(&mut opts))
        .map_err(git)
}

/// What a chat leaves in a project's repository: its worktree's record, its
/// branch and its refs, but `keep`.
fn drop_chat(repo: &Repository, chat: &str, keep: Option<&str>) {
    drop_chat_worktree(repo, chat);
    if let Ok(mut branch) = repo.find_branch(&branch(chat), BranchType::Local) {
        let _ = branch.delete();
    }
    let glob = format!("{PUSHED_REFS}chats/{chat}/*");
    let names: Vec<String> = repo
        .references_glob(&glob)
        .map(|mut refs| refs.names().flatten().map(String::from).collect())
        .unwrap_or_default();
    for name in names.into_iter().filter(|name| Some(name.as_str()) != keep) {
        if let Ok(mut reference) = repo.find_reference(&name) {
            let _ = reference.delete();
        }
    }
}

fn drop_chat_worktree(repo: &Repository, chat: &str) {
    if let Ok(worktree) = repo.find_worktree(chat) {
        let mut opts = WorktreePruneOptions::new();
        opts.valid(true).locked(true).working_tree(true);
        let _ = worktree.prune(Some(&mut opts));
    }
}

/// A chat's branch.
fn branch(chat: &str) -> String {
    format!("rovik/{chat}")
}

/// Whether a chat's id can name its folder, worktree and branch as it is.
pub fn plain(chat: &str) -> bool {
    let fits = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    !chat.is_empty() && chat.len() <= 64 && chat.chars().all(fits)
}

fn synced(root: &Path) -> Synced {
    Synced {
        root: text(root),
        ..Synced::default()
    }
}

/// The form a root takes in a start and in the project's settings.
pub fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bad_request(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

fn git(e: git2::Error) -> Fail {
    internal(e.message().to_string())
}

fn joined(e: tokio::task::JoinError) -> Fail {
    internal(e.to_string())
}

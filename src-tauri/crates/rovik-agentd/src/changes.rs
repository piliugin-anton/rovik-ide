//! A chat's changes in its worktree, for the isolation bar's remote half
//! (6.1): what the chat changed so far, a file's versions, throwing its
//! changes away, and handing it its conflicts. Paths are from the project's
//! folder, as a local chat's overlay has them.

use crate::daemon::Daemon;
use crate::results::move_base;
use crate::store::Place;
use git2::build::CheckoutBuilder;
use git2::{Oid, Repository, ResetType, Tree};
use rovik_core::overlay::markers;
use rovik_core::rt;
use rovik_protocol::work::{Handed, WorkFile};
use rovik_protocol::{Fail, FailCode, Response};
use std::fs;
use std::path::Path;

/// `WorkStatus`: what the chat changed so far.
pub async fn status(daemon: &Daemon, computer: &str, chat: &str) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    let (_, _, subpath) = place(daemon, chat)?;
    let changes = daemon.results.changes(chat, &subpath).await?;
    Ok(Response::Changes(changes))
}

/// `WorkFile`: a file as the landing base has it and as the chat has it.
pub async fn file(
    daemon: &Daemon,
    computer: &str,
    chat: &str,
    path: &str,
) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    check(path)?;
    let (place, base, subpath) = place(daemon, chat)?;
    let at = in_tree(&subpath, path);
    let read = rt::spawn_blocking(move || {
        let repo = Repository::open(&place.worktree).map_err(git)?;
        let base = base_tree(&repo, &base)?;
        let text = |bytes: Option<Vec<u8>>| bytes.and_then(|b| String::from_utf8(b).ok());
        Ok::<_, Fail>(WorkFile {
            base: text(blob(&repo, &base, &at)),
            chat: text(on_disk(&place.worktree, &at)),
        })
    });
    Ok(Response::WorkFile(read.await.map_err(joined)??))
}

/// `WorkDiscard`: the worktree back at the landing base, without the files
/// the chat made; the ones an ignore rule matches stay.
pub async fn discard(daemon: &Daemon, computer: &str, chat: &str) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    idle(daemon, chat)?;
    let _base = daemon.results.hold_base().await;
    let (place, base, _) = place(daemon, chat)?;
    let reset = rt::spawn_blocking(move || {
        let repo = Repository::open(&place.worktree).map_err(git)?;
        let base = Oid::from_str(&base).and_then(|base| repo.find_object(base, None));
        repo.reset(&base.map_err(git)?, ResetType::Hard, None)
            .map_err(git)?;
        // A hard reset keeps untracked files: the chat's new ones go now.
        let mut opts = CheckoutBuilder::new();
        opts.force().remove_untracked(true);
        repo.checkout_head(Some(&mut opts)).map_err(git)
    });
    reset.await.map_err(joined)??;
    daemon.store.marks_left(chat, Vec::new());
    Ok(Response::Done)
}

/// `WorkConflictsToChat`: the local kind of markers in `files`, between
/// the chat's version and the project's in `snapshot`, which their landing
/// base takes, as a local chat's base copies do (`overlay::chats`).
pub async fn to_chat(
    daemon: &Daemon,
    computer: &str,
    chat: &str,
    snapshot: &str,
    files: Vec<String>,
) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    idle(daemon, chat)?;
    let snapshot = Oid::from_str(snapshot).map_err(|_| bad_request("Not a commit's id."))?;
    for path in &files {
        check(path)?;
    }
    let _base = daemon.results.hold_base().await;
    let (place, base, subpath) = place(daemon, chat)?;
    let name = chat.to_string();
    let marked = rt::spawn_blocking(move || {
        let repo = Repository::open(&place.worktree).map_err(git)?;
        let old = base_tree(&repo, &base)?;
        let project = repo
            .find_commit(snapshot)
            .and_then(|commit| commit.tree())
            .map_err(|_| bad_request("The snapshot isn't on this server: push it first."))?;
        let mut marked = Vec::new();
        for path in files {
            let at = in_tree(&subpath, &path);
            let sides = (blob(&repo, &project, &at), on_disk(&place.worktree, &at));
            let (Some(theirs), Some(ours)) = sides else {
                continue;
            };
            let was = blob(&repo, &old, &at);
            let Some(text) = markers::mark(was.as_deref(), &theirs, &ours) else {
                continue;
            };
            fs::write(place.worktree.join(&at), text).map_err(|e| internal(e.to_string()))?;
            marked.push((path, at));
        }
        let paths: Vec<String> = marked.iter().map(|(_, at)| at.clone()).collect();
        let moved = move_base(&place.worktree, &base, snapshot, &paths, &name)?;
        let files = marked.into_iter().map(|(path, _)| path).collect::<Vec<_>>();
        Ok::<_, Fail>((files, paths, moved))
    });
    let (files, paths, base) = marked.await.map_err(joined)??;
    daemon.store.base_moved(chat, &base.to_string());
    // Kept from the worktree's root, as a sync's are. The message that
    // hands them over tells the chat itself.
    daemon.store.mark(chat, &paths, false);
    Ok(Response::Handed(Handed { files }))
}

/// The chat's worktree, its landing base, and the project's folder in it.
fn place(daemon: &Daemon, chat: &str) -> Result<(Place, String, String), Fail> {
    let place = daemon
        .store
        .place(chat)
        .ok_or_else(|| bad_request("This chat has no worktree on this server."))?;
    let base = daemon
        .store
        .base_of(chat)
        .ok_or_else(|| internal("The chat's worktree has no base commit.".into()))?;
    let subpath = daemon
        .store
        .project(&place.project)
        .unwrap_or_default()
        .subpath;
    Ok((place, base, subpath))
}

/// What changes the chat's files waits for its run to end.
fn idle(daemon: &Daemon, chat: &str) -> Result<(), Fail> {
    if daemon.core.agents.is_running(chat) {
        return Err(Fail::new(
            FailCode::Busy,
            "This chat's run is still going: stop it first.",
        ));
    }
    Ok(())
}

/// A path in the project's folder, and nowhere else.
fn check(path: &str) -> Result<(), Fail> {
    let escapes = path.split(['/', '\\']).any(|part| part == "..");
    if path.is_empty() || path.starts_with(['/', '\\']) || escapes || path.contains(':') {
        return Err(bad_request(format!("{path}: not a path in the project.")));
    }
    Ok(())
}

fn in_tree(subpath: &str, path: &str) -> String {
    if subpath.is_empty() {
        path.to_string()
    } else {
        format!("{subpath}/{path}")
    }
}

fn base_tree<'r>(repo: &'r Repository, base: &str) -> Result<Tree<'r>, Fail> {
    let base = Oid::from_str(base).and_then(|base| repo.find_commit(base));
    base.and_then(|base| base.tree()).map_err(git)
}

/// A file's content in a tree, when it's a file there.
fn blob(repo: &Repository, tree: &Tree, path: &str) -> Option<Vec<u8>> {
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(blob.content().to_vec())
}

/// A file's content in the worktree, when it's a file there.
fn on_disk(worktree: &Path, path: &str) -> Option<Vec<u8>> {
    let file = worktree.join(path);
    let meta = fs::symlink_metadata(&file).ok()?;
    meta.is_file().then(|| fs::read(&file).ok()).flatten()
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

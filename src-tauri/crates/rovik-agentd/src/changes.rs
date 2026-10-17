//! A chat's changes in its worktree, for the isolation bar's remote half
//! (6.1): what the chat changed so far, a file's versions, throwing its
//! changes away, handing it its conflicts, and taking a merge or markers
//! back out. Paths are from the project's folder, as a local chat's overlay
//! has them.

use crate::daemon::Daemon;
use crate::results::{self, move_base, set_base};
use crate::store::{Kept, Place};
use crate::sync::Replaced;
use git2::build::CheckoutBuilder;
use git2::{FileMode, ObjectType, Oid, Repository, ResetType, Tree};
use rovik_core::overlay::markers;
use rovik_core::rt;
use rovik_protocol::work::{Handed, UndoneMerge, WorkFile};
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
    daemon.results.forget_kept(chat, None).await;
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
        let (mut marked, mut kept) = (Vec::new(), Vec::new());
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
            let exec = old
                .get_path(Path::new(&at))
                .is_ok_and(|e| e.filemode() == 0o100755);
            let base = was.as_deref().map(|was| (was, exec));
            let replaced = Replaced::new(&at, &ours, base, &text);
            kept.push(replaced.keep(&repo).map_err(git)?);
            fs::write(place.worktree.join(&at), text).map_err(|e| internal(e.to_string()))?;
            marked.push((path, at));
        }
        let paths: Vec<String> = marked.iter().map(|(_, at)| at.clone()).collect();
        let moved = move_base(&place.worktree, &base, snapshot, &paths, &name)?;
        let files = marked.into_iter().map(|(path, _)| path).collect::<Vec<_>>();
        Ok::<_, Fail>((files, paths, moved, kept))
    });
    let (files, paths, base, kept) = marked.await.map_err(joined)??;
    daemon.store.base_moved(chat, &base.to_string());
    daemon.results.keep(chat, kept).await;
    // Kept from the worktree's root, as a sync's are. The message that
    // hands them over tells the chat itself.
    daemon.store.mark(chat, &paths, false);
    Ok(Response::Handed(Handed { files }))
}

/// `WorkUndoMerge`: the project's merged changes and conflict markers taken
/// back out of the chat's files (`files`: of these, from the project's
/// folder), as a local chat's Undo the merge and Unmark do
/// (`overlay::chats::undo_merge`). Each is the chat's own version again, on
/// its landing base from before; a file changed since stays as it is. The
/// chat's next result, committed at once, brings what conflicts with the
/// project back to its computer.
pub async fn undo_merge(
    daemon: &Daemon,
    computer: &str,
    chat: &str,
    files: Option<Vec<String>>,
) -> Result<Response, Fail> {
    daemon.store.check_owner(computer, chat)?;
    idle(daemon, chat)?;
    for path in files.iter().flatten() {
        check(path)?;
    }
    let (restored, changed_since, subpath) = {
        let _base = daemon.results.hold_base().await;
        let (place, base, subpath) = place(daemon, chat)?;
        let wanted = |path: &String| {
            files
                .as_ref()
                .is_none_or(|files| files.iter().any(|f| in_tree(&subpath, f) == *path))
        };
        let kept = daemon.store.kept_of(chat).into_iter();
        let kept: Vec<(String, Kept)> = kept.filter(|(path, _)| wanted(path)).collect();
        let name = chat.to_string();
        let put = rt::spawn_blocking(move || put_back(&place.worktree, &base, kept, &name));
        let (restored, mut changed_since, lost, base) = put.await.map_err(joined)??;
        daemon.store.base_moved(chat, &base.to_string());
        let forgotten = [restored.as_slice(), lost.as_slice()].concat();
        daemon.results.forget_kept(chat, Some(&forgotten)).await;
        changed_since.extend(lost);
        let marks = daemon.store.marked_of(chat).into_iter();
        let marks = marks.filter(|m| !restored.contains(m)).collect();
        daemon.store.marks_left(chat, marks);
        (restored, changed_since, subpath)
    };
    if !restored.is_empty() {
        daemon.results.recommit(chat).await?;
    }
    Ok(Response::UndoneMerge(UndoneMerge {
        restored: results::in_root(&subpath, restored),
        changed_since: results::in_root(&subpath, changed_since),
    }))
}

/// Puts what was kept back: the chat's own versions in the worktree, their
/// old landing base in a new one. Returns the files put back, those changed
/// since, those whose earlier versions are gone (left as they are), and
/// the new landing base.
fn put_back(
    worktree: &Path,
    base: &str,
    kept: Vec<(String, Kept)>,
    chat: &str,
) -> Result<(Vec<String>, Vec<String>, Vec<String>, Oid), Fail> {
    let repo = Repository::open(worktree).map_err(git)?;
    let base = Oid::from_str(base)
        .and_then(|base| repo.find_commit(base))
        .map_err(git)?;
    let (mut restored, mut changed_since, mut lost) = (Vec::new(), Vec::new(), Vec::new());
    let mut entries = Vec::new();
    for (path, kept) in kept {
        let now = on_disk(worktree, &path).map(|bytes| Oid::hash_object(ObjectType::Blob, &bytes));
        let written = Oid::from_str(&kept.written).ok();
        if now.transpose().map_err(git)? != written {
            changed_since.push(path);
            continue;
        }
        let Some((chat_version, was)) = earlier(&repo, &kept) else {
            lost.push(path);
            continue;
        };
        fs::write(worktree.join(&path), chat_version)
            .map_err(|e| internal(format!("{path}: {e}")))?;
        let mode = match kept.base_exec {
            true => FileMode::BlobExecutable,
            false => FileMode::Blob,
        };
        entries.push((path.clone(), was.map(|id| (id, mode))));
        restored.push(path);
    }
    let message = format!("Merge taken back in chat {chat}");
    let base = set_base(&repo, &base, &entries, &message)?;
    Ok((restored, changed_since, lost, base))
}

/// A kept file's earlier versions in the repository: the chat's content,
/// and its base's blob when it had one. None when any of them is gone.
fn earlier(repo: &Repository, kept: &Kept) -> Option<(Vec<u8>, Option<Oid>)> {
    let blob = |id: &str| repo.find_blob(Oid::from_str(id).ok()?).ok();
    let chat = blob(&kept.chat)?.content().to_vec();
    let base = match &kept.base {
        Some(id) => Some(blob(id)?.id()),
        None => None,
    };
    Some((chat, base))
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

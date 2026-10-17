//! T05 on a server (docs/design/autopilot-plan.md 4.5): a Thorough task's
//! candidates work in worktrees beside the task chat's, `work/<candidate>`
//! on branch `rovik/<candidate>`, each made from the chat's worktree as
//! its changes so far have it: the tests written first, what Prepare
//! installed in tracked files. That start is a commit on the chat's
//! landing base (`refs/rovik/chats/<candidate>/start`); what a candidate
//! made is its own result commit (`result_ref`), committed as the pick
//! reads its change. The winner's files are written into the chat's
//! worktree, which its result and its delivery are made from as ever; the
//! candidates' worktrees go, with their branches and refs.
//!
//! What a lockfile installed in the chat's worktree (`node_modules`), which
//! git ignores, isn't in a candidate's: the task's run installs it there
//! before the candidate starts (`prepare_copy`, D19), as the Prepare stage
//! did in the chat's.

use crate::daemon::Daemon;
use crate::{results, work};
use git2::{Delta, FileMode, Oid, Repository};
use rovik_core::agent_run::candidates::{Copies, Copying};
use rovik_core::agent_run::change_review::FileChange;
use rovik_core::rt;
use rovik_protocol::packs::{result_ref, PUSHED_REFS};
use std::path::{Path, PathBuf};
use std::sync::Weak;

/// The daemon's copies for the core (`AgentState::set_copies`).
pub struct Worktrees(pub Weak<Daemon>);

/// Where a candidate's start is kept: its task chat's worktree as it was.
fn start_ref(chat: &str) -> String {
    format!("{PUSHED_REFS}chats/{chat}/start")
}

impl Copies for Worktrees {
    fn open(
        &self,
        main: String,
        chat: String,
        _project: Option<String>,
    ) -> Copying<Result<String, String>> {
        let daemon = self.0.clone();
        Box::pin(async move {
            let daemon = daemon.upgrade().ok_or("the server is stopping")?;
            open(&daemon, &main, &chat).await
        })
    }

    fn change(&self, main: String, chat: String) -> Copying<Result<Vec<FileChange>, String>> {
        let daemon = self.0.clone();
        Box::pin(async move {
            let daemon = daemon.upgrade().ok_or("the server is stopping")?;
            let (_, files) = made(&daemon, &main, &chat).await?;
            Ok(files)
        })
    }

    fn adopt(&self, main: String, chat: String) -> Copying<Result<Vec<String>, String>> {
        let daemon = self.0.clone();
        Box::pin(async move {
            let daemon = daemon.upgrade().ok_or("the server is stopping")?;
            let written = adopt(&daemon, &main, &chat).await?;
            discard(&daemon, &main, &chat).await;
            Ok(written)
        })
    }

    fn discard(&self, main: String, chat: String) -> Copying<()> {
        let daemon = self.0.clone();
        Box::pin(async move {
            if let Some(daemon) = daemon.upgrade() {
                discard(&daemon, &main, &chat).await;
            }
        })
    }
}

/// The candidate's worktree's root: made from the task chat's worktree as
/// it is now, or the one made before.
async fn open(daemon: &Daemon, main: &str, chat: &str) -> Result<String, String> {
    let place = daemon
        .store
        .place(main)
        .ok_or("the chat has no worktree on this server")?;
    let base = daemon
        .store
        .base_of(main)
        .ok_or("the chat's worktree has no base commit")?;
    let at = daemon.worktrees.join(chat);
    if at.join(".git").exists() {
        return Ok(work::text(&at.join(daemon.store.subpath_of(main))));
    }
    let start = {
        let _base = daemon.results.hold_base().await;
        let (worktree, index, name, on) = (
            place.worktree.clone(),
            daemon.results.index_of(&format!("{chat}.start")),
            start_ref(chat),
            base.clone(),
        );
        let message = format!("Start of candidate {chat}");
        let committed =
            rt::spawn_blocking(move || results::commit_at(&worktree, &index, &on, &name, &message));
        committed.await.map_err(|e| e.to_string())??
    };
    let base = Oid::from_str(&base).map_err(|e| e.message().to_string())?;
    let root = work::candidate_worktree(daemon, chat, &place.project, base, start)
        .await
        .map_err(|fail| fail.message)?;
    log::info!("chat {main}: candidate {chat} works in {}", root.display());
    Ok(work::text(&root))
}

/// What the candidate made: its worktree committed as its own result, and
/// each file it changed from its start, by its path from the project's
/// folder, as the start and the result have it.
async fn made(daemon: &Daemon, main: &str, chat: &str) -> Result<(Oid, Vec<FileChange>), String> {
    let at = daemon.worktrees.join(chat);
    if !at.join(".git").exists() {
        return Err("the candidate's worktree is gone".into());
    }
    let base = daemon
        .store
        .base_of(main)
        .ok_or("the chat's worktree has no base commit")?;
    let subpath = daemon.store.subpath_of(main);
    let _base = daemon.results.hold_base().await;
    let (index, chat) = (daemon.results.index_of(chat), chat.to_string());
    let read = rt::spawn_blocking(move || {
        let git = |e: git2::Error| e.message().to_string();
        let message = format!("Result of candidate {chat}");
        let made = results::commit_at(&at, &index, &base, &result_ref(&chat), &message)?;
        let repo = Repository::open(&at).map_err(git)?;
        let start = repo
            .find_reference(&start_ref(&chat))
            .and_then(|r| r.peel_to_commit())
            .map_err(git)?;
        let result = repo.find_commit(made).map_err(git)?;
        let (from, to) = (start.tree().map_err(git)?, result.tree().map_err(git)?);
        let diff = repo
            .diff_tree_to_tree(Some(&from), Some(&to), None)
            .map_err(git)?;
        let prefix = if subpath.is_empty() {
            String::new()
        } else {
            format!("{subpath}/")
        };
        let text = |id: Oid| -> Option<String> {
            if id.is_zero() {
                return None;
            }
            let blob = repo.find_blob(id).ok()?;
            Some(String::from_utf8_lossy(blob.content()).into_owned())
        };
        let mut files = Vec::new();
        for delta in diff.deltas() {
            let file = match delta.status() {
                Delta::Deleted => delta.old_file(),
                _ => delta.new_file(),
            };
            let Some(path) = file.path().map(|p| p.to_string_lossy().replace('\\', "/")) else {
                continue;
            };
            let Some(path) = path.strip_prefix(&prefix).map(String::from) else {
                continue;
            };
            files.push(FileChange {
                path,
                before: text(delta.old_file().id()),
                after: text(delta.new_file().id()),
            });
        }
        Ok::<_, String>((made, files))
    });
    read.await.map_err(|e| e.to_string())?
}

/// The candidate's change written into the task chat's worktree: each file
/// it changed from its start as its result has it, or gone. The files.
async fn adopt(daemon: &Daemon, main: &str, chat: &str) -> Result<Vec<String>, String> {
    let place = daemon
        .store
        .place(main)
        .ok_or("the chat has no worktree on this server")?;
    let (made, _) = made(daemon, main, chat).await?;
    let (at, worktree, chat) = (
        daemon.worktrees.join(chat),
        place.worktree.clone(),
        chat.to_string(),
    );
    let _base = daemon.results.hold_base().await;
    let written = rt::spawn_blocking(move || write_into(&at, &chat, made, &worktree));
    written.await.map_err(|e| e.to_string())?
}

/// Writes what changed from candidate `chat`'s start to `made` (in its
/// worktree `at`) into `worktree`: the paths, from the worktree's root.
fn write_into(at: &Path, chat: &str, made: Oid, worktree: &Path) -> Result<Vec<String>, String> {
    let git = |e: git2::Error| e.message().to_string();
    let io = |e: std::io::Error| e.to_string();
    let repo = Repository::open(at).map_err(git)?;
    let start = repo
        .find_reference(&start_ref(chat))
        .and_then(|r| r.peel_to_commit())
        .map_err(git)?;
    let result = repo.find_commit(made).map_err(git)?;
    let (from, to) = (start.tree().map_err(git)?, result.tree().map_err(git)?);
    let diff = repo
        .diff_tree_to_tree(Some(&from), Some(&to), None)
        .map_err(git)?;
    let mut written = Vec::new();
    for delta in diff.deltas() {
        let (old, new) = (delta.old_file(), delta.new_file());
        let path = match delta.status() {
            Delta::Deleted => old.path(),
            _ => new.path(),
        };
        let Some(path) = path.map(PathBuf::from) else {
            continue;
        };
        let target = worktree.join(&path);
        if delta.status() == Delta::Deleted {
            match std::fs::remove_file(&target) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(e)),
                _ => {}
            }
        } else {
            let blob = repo.find_blob(new.id()).map_err(git)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            let _ = std::fs::remove_file(&target);
            match new.mode() {
                #[cfg(unix)]
                FileMode::Link => {
                    let link = String::from_utf8_lossy(blob.content()).into_owned();
                    std::os::unix::fs::symlink(link, &target).map_err(io)?;
                }
                mode => {
                    std::fs::write(&target, blob.content()).map_err(io)?;
                    #[cfg(unix)]
                    if mode == FileMode::BlobExecutable {
                        use std::os::unix::fs::PermissionsExt;
                        let executable = std::fs::Permissions::from_mode(0o755);
                        std::fs::set_permissions(&target, executable).map_err(io)?;
                    }
                }
            }
        }
        written.push(path.to_string_lossy().replace('\\', "/"));
    }
    Ok(written)
}

/// The candidate's worktree and branch, and its indexes; its result's
/// commit stays, at its ref, with the pick's record of it.
async fn discard(daemon: &Daemon, main: &str, chat: &str) {
    match daemon.store.place(main) {
        Some(place) => work::remove_candidate(daemon, &place.project, chat).await,
        None => work::remove(daemon, chat).await,
    }
    for name in [chat.to_string(), format!("{chat}.start")] {
        let _ = std::fs::remove_file(daemon.results.index_of(&name));
    }
}

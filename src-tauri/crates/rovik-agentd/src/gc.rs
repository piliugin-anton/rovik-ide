//! What Rovik's folders no longer need goes (R01, design §10.9): each day,
//! and at once when the hosts page asks ("Clean up now", `CleanUp`).
//!
//! - The worktree, branch and refs of a chat whose rows are gone: deleted
//!   while the daemon was away, or taken back by its computer and
//!   forgotten (`ChatForget`) a week ago, at once when asked. A chat taken
//!   back but not yet forgotten keeps them (`work::collect_chats`).
//! - Attachments no message names, once their grace period is over: an
//!   upload `BlobsMissing` renewed for a message on its way stays (K43).
//! - In each project's bare repository, `git gc --auto` where git is
//!   installed: libgit2 has no gc. The chats' kept blobs have refs, so it
//!   doesn't prune them.
//!
//! Folders in `work/` that no chat has go only at start-up (serve.rs):
//! while the daemon serves, one may be a worktree a sync is making.

use crate::daemon::Daemon;
use crate::host;
use crate::work;
use rovik_core::chat_db::attachments;
use rovik_core::chat_db::store::now_ms;
use rovik_core::rt;
use rovik_protocol::host::Storage;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// How long a chat its computer took back keeps its worktree here.
pub const KEEP_RELEASED_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// Attachments go a batch at a time: at most this many batches a cleanup.
const BATCHES: usize = 100;

/// Cleans up each day from now on.
pub fn daily(daemon: Arc<Daemon>) {
    rt::spawn(async move {
        loop {
            tokio::time::sleep(DAY).await;
            clean_up(&daemon, now_ms() - KEEP_RELEASED_MS).await;
        }
    });
}

/// "Clean up now": as each day does, the chats taken back without their
/// week. What Rovik's folders take after.
pub async fn now(daemon: &Daemon) -> Storage {
    clean_up(daemon, i64::MAX).await;
    host::storage(daemon).await
}

async fn clean_up(daemon: &Daemon, released_before: i64) {
    // Not while a move brings a chat, its worktree with its rows.
    let chats = {
        let _one = daemon.imports.lock().await;
        work::collect_chats(daemon, released_before).await
    };
    let files = collect_attachments(daemon).await;
    let repositories = gc_repositories(daemon.projects.repositories()).await;
    log::info!(
        "cleaned up: {chats} worktrees and {files} attachments went, \
         {repositories} repositories collected"
    );
}

/// Attachments no message names, past their grace period. How many went.
async fn collect_attachments(daemon: &Daemon) -> usize {
    let data_dir = daemon.core.db.data_dir().to_path_buf();
    let mut total = 0;
    for _ in 0..BATCHES {
        let dir = data_dir.clone();
        let batch = daemon
            .core
            .db
            .write(move |c| attachments::collect_garbage(c, &dir))
            .await;
        match batch {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => {
                log::warn!("the attachments' cleanup: {e}");
                break;
            }
        }
    }
    total
}

/// `git gc --auto` in each repository, in the foreground, where git is
/// installed. How many it went through.
async fn gc_repositories(repositories: Vec<PathBuf>) -> usize {
    let gc = move || {
        let git = Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !git.is_ok_and(|s| s.success()) {
            return 0;
        }
        let mut done = 0;
        for repository in &repositories {
            let out = Command::new("git")
                .arg("--git-dir")
                .arg(repository)
                .args(["-c", "gc.autoDetach=false", "gc", "--auto", "--quiet"])
                .stdin(Stdio::null())
                .output();
            match out {
                Ok(out) if out.status.success() => done += 1,
                Ok(out) => log::warn!(
                    "git gc in {}: {}",
                    repository.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => log::warn!("git gc in {}: {e}", repository.display()),
            }
        }
        done
    };
    rt::spawn_blocking(gc).await.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn each_repository_is_collected_where_git_is_installed() {
        let dir = rovik_core::testing::temp_dir();
        let repository = dir.0.join("project.git");
        git2::Repository::init_bare(&repository).unwrap();
        let git = Command::new("git").arg("--version").output().is_ok();
        assert_eq!(gc_repositories(vec![repository]).await, usize::from(git));
    }
}

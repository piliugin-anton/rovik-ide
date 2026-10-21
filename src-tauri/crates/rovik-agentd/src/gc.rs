//! What Rovik's folders no longer need goes (R01, design §10.9): each day,
//! and at once when the hosts page asks ("Clean up now", `CleanUp`).
//!
//! - The worktree, branch and refs of a chat whose rows are gone: deleted
//!   while the daemon was away, or taken back by its computer and
//!   forgotten (`ChatForget`) a week ago, at once when asked. A chat taken
//!   back but not yet forgotten keeps them (`work::collect_chats`).
//! - Attachments no message names, once their grace period is over: an
//!   upload `BlobsMissing` renewed for a message on its way stays (K43).
//! - The temporary files, the TMPDIR of the agent's commands (K13), which
//!   nothing else empties: what nothing changed for a day, at start-up
//!   too; when asked, for an hour, as a command going may still use what
//!   it made since. What holds a socket (a server may still listen on it)
//!   or a mount stays, and so does Rovik's own folder of chats' views.
//! - In each project's bare repository, `git gc --auto` where git is
//!   installed: libgit2 has no gc. When asked, a full `git gc`, which drops
//!   what no ref has reached for an hour: what deleted chats and older
//!   snapshots left. The chats' kept blobs have refs, so it doesn't prune
//!   them.
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
use std::fs::{self, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a chat its computer took back keeps its worktree here.
pub const KEEP_RELEASED_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// What "Clean up now" leaves of the temporary files: what changed this
/// recently, which a command going may still use.
const HOUR: Duration = Duration::from_secs(60 * 60);
/// What a full `git gc` leaves of the objects no ref reaches: those an
/// hour old at most, which a push or a commit on its way may still name.
const PRUNE: &str = "--prune=1.hour.ago";
/// Attachments go a batch at a time: at most this many batches a cleanup.
const BATCHES: usize = 100;
/// Rovik's own entries in the temporary folder, which the cleanup leaves:
/// `rovik-ide-<uid>` holds the chats' views where there's no
/// XDG_RUNTIME_DIR (`rovik_core::overlay::chats::mountpoint`).
const ROVIKS_OWN: &str = "rovik-ide";

/// What a cleanup takes.
struct Sweep {
    /// Chats taken back before this (ms) leave their worktree
    /// (`work::collect_chats`).
    released_before: i64,
    /// The temporary files nothing changed for this long go.
    unused_for: Duration,
    /// `git gc` in full rather than `--auto`.
    full_gc: bool,
}

/// Cleans up each day from now on.
pub fn daily(daemon: Arc<Daemon>) {
    rt::spawn(async move {
        loop {
            tokio::time::sleep(DAY).await;
            let sweep = Sweep {
                released_before: now_ms() - KEEP_RELEASED_MS,
                unused_for: DAY,
                full_gc: false,
            };
            clean_up(&daemon, sweep).await;
        }
    });
}

/// "Clean up now": as each day does, the chats taken back without their
/// week, the temporary files nothing changed for an hour, and each
/// repository in full. What Rovik's folders take after.
pub async fn now(daemon: &Daemon) -> Storage {
    let sweep = Sweep {
        released_before: i64::MAX,
        unused_for: HOUR,
        full_gc: true,
    };
    clean_up(daemon, sweep).await;
    host::storage(daemon).await
}

/// At start-up: the temporary files nothing changed for a day. The daily
/// cleanup is a day away, which a daemon started again sooner never sees.
pub async fn at_start(daemon: &Daemon) {
    let went = collect_temporary(daemon.tmp.clone(), DAY).await;
    if went > 0 {
        log::info!("{went} of the temporary files' entries went, a day unused");
    }
}

async fn clean_up(daemon: &Daemon, sweep: Sweep) {
    // Not while a move brings a chat, its worktree with its rows.
    let chats = {
        let _one = daemon.imports.lock().await;
        work::collect_chats(daemon, sweep.released_before).await
    };
    let files = collect_attachments(daemon).await;
    let temporary = collect_temporary(daemon.tmp.clone(), sweep.unused_for).await;
    let repositories = gc_repositories(daemon.projects.repositories(), sweep.full_gc).await;
    log::info!(
        "cleaned up: {chats} worktrees, {files} attachments and {temporary} of the \
         temporary files' entries went, {repositories} repositories collected"
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

/// The entries of the temporary folder `dir` nothing changed in for
/// `unused_for`. How many went.
async fn collect_temporary(dir: PathBuf, unused_for: Duration) -> usize {
    let before = SystemTime::now()
        .checked_sub(unused_for)
        .unwrap_or(UNIX_EPOCH);
    rt::spawn_blocking(move || collect_temporary_in(&dir, before))
        .await
        .unwrap_or(0)
}

/// The entries of `dir` nothing changed in since `before` (`unused_since`),
/// but Rovik's own. How many went.
fn collect_temporary_in(dir: &Path, before: SystemTime) -> usize {
    let (Ok(folder), Ok(entries)) = (fs::metadata(dir), fs::read_dir(dir)) else {
        return 0;
    };
    let mut went = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with(ROVIKS_OWN)
            || !unused_since(&path, before, folder.dev())
        {
            continue;
        }
        // A link goes, not what it names.
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
            _ => fs::remove_file(&path),
        };
        match removed {
            Ok(()) => went += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("{} stays: {e}", path.display()),
        }
    }
    went
}

/// Whether nothing in `path` changed since `before`, looked at to its
/// bottom without following a link or leaving the file system `dev`. A
/// socket (a server may still listen on it), a mount, or what can't be
/// read or changes while it's looked at, keeps it.
fn unused_since(path: &Path, before: SystemTime, dev: u64) -> bool {
    let Ok(top) = fs::symlink_metadata(path) else {
        return false;
    };
    let unused = |meta: &Metadata| !meta.file_type().is_socket() && changed(meta) < before;
    if !unused(&top) || top.dev() != dev {
        return false;
    }
    let mut folders = match top.is_dir() {
        true => vec![path.to_path_buf()],
        false => Vec::new(),
    };
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            let Ok(meta) = entry.metadata() else {
                return false;
            };
            if !unused(&meta) || (meta.is_dir() && meta.dev() != dev) {
                return false;
            }
            if meta.is_dir() {
                folders.push(entry.path());
            }
        }
    }
    true
}

/// When a file last changed: its contents (mtime), or the file itself
/// (ctime), which unpacking an archive can't set back as it does the mtime.
fn changed(meta: &Metadata) -> SystemTime {
    let seconds = u64::try_from(meta.ctime()).unwrap_or(0);
    let nanos = u32::try_from(meta.ctime_nsec())
        .unwrap_or(0)
        .min(999_999_999);
    let ctime = UNIX_EPOCH + Duration::new(seconds, nanos);
    meta.modified().map_or(ctime, |mtime| mtime.max(ctime))
}

/// `git gc` in each repository, in the foreground, where git is installed:
/// `--auto`, or in `full`, which drops the objects no ref has reached for
/// an hour. How many it went through.
async fn gc_repositories(repositories: Vec<PathBuf>, full: bool) -> usize {
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
                .args(["-c", "gc.autoDetach=false", "gc", "--quiet"])
                .arg(if full { PRUNE } else { "--auto" })
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
    use rovik_core::testing::temp_dir;
    use std::os::unix::net::UnixListener;

    fn git_installed() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// The names in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn each_repository_is_collected_where_git_is_installed() {
        let dir = temp_dir();
        let repository = dir.0.join("project.git");
        git2::Repository::init_bare(&repository).unwrap();
        for full in [false, true] {
            let collected = gc_repositories(vec![repository.clone()], full).await;
            assert_eq!(collected, usize::from(git_installed()), "full: {full}");
        }
    }

    /// "Clean up now": the objects no ref has reached for an hour go (what a
    /// deleted chat's commits left), younger ones stay (a push on its way),
    /// and so does what a ref reaches.
    #[tokio::test]
    async fn a_full_gc_drops_what_no_ref_reached_for_an_hour() {
        if !git_installed() {
            return;
        }
        let dir = temp_dir();
        let repository = dir.0.join("project.git");
        let repo = git2::Repository::init_bare(&repository).unwrap();
        let old = repo.blob(b"what a deleted chat made").unwrap();
        let young = repo.blob(b"a push on its way").unwrap();
        let kept = repo.blob(b"a snapshot's file").unwrap();
        let mut tree = repo.treebuilder(None).unwrap();
        tree.insert("file.txt", kept, 0o100644).unwrap();
        let tree = repo.find_tree(tree.write().unwrap()).unwrap();
        let me = git2::Signature::now("Rovik", "rovik@example.com").unwrap();
        let snapshot = repo
            .commit(Some("refs/heads/main"), &me, &me, "Snapshot", &tree, &[])
            .unwrap();
        let hex = old.to_string();
        let loose = repository.join("objects").join(&hex[..2]).join(&hex[2..]);
        let two_hours_ago = SystemTime::now() - 2 * HOUR;
        fs::File::open(&loose)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();

        assert_eq!(gc_repositories(vec![repository.clone()], true).await, 1);
        let repo = git2::Repository::open_bare(&repository).unwrap();
        assert!(repo.find_blob(old).is_err(), "unreached for two hours");
        assert!(repo.find_blob(young).is_ok(), "unreached for a moment");
        assert!(repo.find_commit(snapshot).is_ok());
        assert!(repo.find_blob(kept).is_ok());
    }

    /// The entries nothing changed in since the moment given go: a file, a
    /// folder, a link (not what it names). One holding a socket stays, and
    /// so does Rovik's own; nothing goes that changed since.
    #[test]
    fn temporary_files_go_once_nothing_changes_them() {
        let dir = temp_dir();
        let tmp = dir.0.join("tmp");
        let outside = dir.0.join("outside.txt");
        fs::create_dir_all(tmp.join("build-1/deep")).unwrap();
        fs::write(tmp.join("build-1/deep/out.o"), "x").unwrap();
        fs::write(tmp.join("leftover.txt"), "x").unwrap();
        fs::write(&outside, "kept").unwrap();
        std::os::unix::fs::symlink(&outside, tmp.join("link")).unwrap();
        fs::create_dir_all(tmp.join("ssh-agent")).unwrap();
        let _agent = UnixListener::bind(tmp.join("ssh-agent/agent.sock")).unwrap();
        fs::create_dir_all(tmp.join("rovik-ide-1000/chats/c1")).unwrap();

        let an_hour_ago = SystemTime::now() - HOUR;
        assert_eq!(collect_temporary_in(&tmp, an_hour_ago), 0);
        assert_eq!(names(&tmp).len(), 5);

        let in_an_hour = SystemTime::now() + HOUR;
        assert_eq!(collect_temporary_in(&tmp, in_an_hour), 3);
        assert_eq!(names(&tmp), ["rovik-ide-1000", "ssh-agent"]);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "kept");
    }

    /// A change deep in an entry keeps all of it: a command may still use
    /// the folder it made long ago.
    #[test]
    fn a_change_deep_inside_keeps_the_whole_entry() {
        let dir = temp_dir();
        let tmp = dir.0.join("tmp");
        fs::create_dir_all(tmp.join("old")).unwrap();
        fs::write(tmp.join("old/file"), "x").unwrap();
        fs::create_dir_all(tmp.join("going/a/b")).unwrap();
        // File times come from a coarse clock: well apart from `moment`.
        std::thread::sleep(Duration::from_millis(100));
        let moment = SystemTime::now();
        std::thread::sleep(Duration::from_millis(100));
        fs::write(tmp.join("going/a/b/new"), "x").unwrap();

        let dev = fs::metadata(&tmp).unwrap().dev();
        assert!(unused_since(&tmp.join("old"), moment, dev));
        assert!(!unused_since(&tmp.join("going"), moment, dev));
        assert!(
            !unused_since(&tmp.join("old"), moment, dev + 1),
            "on another file system"
        );
        assert_eq!(collect_temporary_in(&tmp, moment), 1);
        assert_eq!(names(&tmp), ["going"]);
    }
}

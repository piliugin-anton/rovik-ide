// git.rs
//
// Cross-platform Git backend for the IDE's GitPanel, built on `git2`
// (Rust bindings for libgit2) rather than shelling out to a `git` binary.
// This means it works even on machines without git installed, and behaves
// identically on Windows/macOS/Linux.

use git2::{
    BranchType, Cred, CredentialHelper, CredentialType, FetchOptions, PushOptions, RemoteCallbacks,
    Repository, ResetType, StashFlags, Status, StatusOptions,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Git commands run on a worker thread. As plain commands they ran on the
/// app's main thread, so a slow commit (hashing every changed file), or a
/// push waiting on the network, froze the whole window.
pub(crate) async fn off_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitFile {
    pub path: String,
    pub status: String, // "M" | "A" | "D" | "R" | "U" | "?"
    /// Changed in the index (staged) / in the working tree (not staged).
    pub staged: bool,
    pub unstaged: bool,
    /// Added / removed lines vs HEAD (`git diff --numstat`). 0 for binary,
    /// untracked-but-unreadable, or when the diff is unavailable.
    #[serde(default)]
    pub additions: usize,
    #[serde(default)]
    pub deletions: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<GitFile>,
    /// The repository's working folder: file paths are relative to it,
    /// which isn't the project folder when the project is inside a repo.
    pub workdir: String,
    /// A merge / rebase in progress (conflicts to resolve, or to continue).
    #[serde(default)]
    pub operation: Option<crate::git_sync::GitOperation>,
    /// The branch's upstream ("origin/feature"). None for a branch with no
    /// remote branch yet (new, never pushed, or its remote branch gone),
    /// and off a branch.
    #[serde(default)]
    pub upstream: Option<String>,
    /// The remote the branch pushes to (`git push`'s): where publishing a
    /// branch with no upstream makes it. None with no remote, and off a
    /// branch.
    #[serde(default)]
    pub push_remote: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn open_repo(path: &str) -> Result<Repository, String> {
    Repository::discover(path).map_err(|e| format!("Not a git repository: {e}"))
}

/// Reduce a git2 status bitflag combination down to the single-character
/// code the frontend expects. Priority: conflicts > untracked > staged
/// (index) changes > working-tree changes.
fn classify_status(s: Status) -> &'static str {
    if s.is_conflicted() {
        return "U";
    }
    if s.is_wt_new() {
        return "?";
    }
    if s.is_index_new() {
        return "A";
    }
    if s.is_index_deleted() || s.is_wt_deleted() {
        return "D";
    }
    if s.is_index_renamed() || s.is_wt_renamed() {
        return "R";
    }
    // Covers modified + typechange, staged or unstaged.
    "M"
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Builds credential callbacks that try, in order: an SSH agent, default
/// SSH key locations (~/.ssh/id_ed25519, ~/.ssh/id_rsa), then whatever the
/// system's git credential helper knows (stored HTTPS tokens/passwords).
/// This covers the common cases without requiring any UI for auth, at the
/// cost of not being able to prompt interactively — repos needing a fresh
/// login will surface an error asking the user to authenticate in a
/// terminal once.
///
/// libgit2 calls back again after every credential the server turns down,
/// for as long as the callback keeps answering. Each candidate is offered
/// once; when they're all refused the attempt ends with an error instead
/// of handing the same rejected key or token over for ever (a push that
/// never returns, with the panel stuck on its spinner).
fn make_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    let mut tried_agent = false;
    let mut tried_keys: Vec<PathBuf> = Vec::new();
    let mut tried_helper = false;
    callbacks.credentials(move |url, username_from_url, allowed_types| {
        if allowed_types.contains(CredentialType::SSH_KEY) {
            let username = username_from_url.unwrap_or("git");

            if !tried_agent {
                tried_agent = true;
                if let Ok(cred) = Cred::ssh_key_from_agent(username) {
                    return Ok(cred);
                }
            }

            if let Some(home) = home_dir() {
                let ssh_dir = home.join(".ssh");
                for key_name in ["id_ed25519", "id_rsa"] {
                    let private_key = ssh_dir.join(key_name);
                    if tried_keys.contains(&private_key) || !private_key.exists() {
                        continue;
                    }
                    tried_keys.push(private_key.clone());
                    if let Ok(cred) = Cred::ssh_key(username, None, &private_key, None) {
                        return Ok(cred);
                    }
                }
            }
        }

        if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) && !tried_helper {
            tried_helper = true;
            if let Ok(config) = git2::Config::open_default() {
                let mut helper = CredentialHelper::new(url);
                helper.config(&config);
                if let Some(username) = username_from_url {
                    helper.username(Some(username));
                }
                if let Some((username, password)) = helper.execute() {
                    if let Ok(cred) = Cred::userpass_plaintext(&username, &password) {
                        return Ok(cred);
                    }
                }
            }
        }

        if allowed_types.contains(CredentialType::DEFAULT) {
            return Cred::default();
        }
        Err(git2::Error::from_str(
            "authentication failed: no SSH key or stored credentials were accepted \
             (sign in from a terminal once, with `git fetch`, so git remembers them)",
        ))
    });
    callbacks
}

fn default_remote_name(repo: &Repository) -> Result<String, String> {
    let remotes = repo.remotes().map_err(|e| e.to_string())?;
    if remotes.iter().flatten().any(|r| r == Some("origin")) {
        return Ok("origin".to_string());
    }
    remotes
        .iter()
        .filter_map(|r| r.ok().flatten())
        .map(|s| s.to_string())
        .next()
        .ok_or_else(|| "No remotes configured".to_string())
}

/// The remote the current branch fetches from (`branch.<name>.remote`),
/// if it's on a branch that has one: where `git fetch` with no arguments
/// goes, before falling back to origin.
fn upstream_remote_name(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let buf = repo.branch_upstream_remote(head.name().ok()?).ok()?;
    buf.as_str().ok().map(str::to_string)
}

/// The remote `git push` with no arguments would use: `remote.pushDefault`,
/// then `branch.<name>.pushRemote`, then the branch's upstream remote,
/// then origin (or the only remote there is).
fn push_remote_name(repo: &Repository, branch: &str) -> Result<String, String> {
    let config = repo.config().and_then(|mut c| c.snapshot()).ok();
    let configured = config.as_ref().and_then(|c| {
        c.get_string("remote.pushDefault")
            .ok()
            .or_else(|| c.get_string(&format!("branch.{branch}.pushRemote")).ok())
            .filter(|r| !r.is_empty())
    });
    match configured.or_else(|| upstream_remote_name(repo)) {
        Some(name) if repo.find_remote(&name).is_ok() => Ok(name),
        _ => default_remote_name(repo),
    }
}

/// Per-file added/removed line counts for one diff, keyed by the
/// repo-relative `/`-separated path. Binary files report no hunks, so
/// they (and any patch that fails to materialize) come back as 0/0.
fn diff_line_stats(diff: &git2::Diff) -> std::collections::HashMap<String, (usize, usize)> {
    let mut out = std::collections::HashMap::new();
    let mut i = 0;
    while let Some(delta) = diff.get_delta(i) {
        let rel = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned());
        let counts = git2::Patch::from_diff(diff, i)
            .ok()
            .flatten()
            .and_then(|patch| patch.line_stats().ok())
            .map(|(_, added, removed)| (added, removed))
            .unwrap_or((0, 0));
        if let Some(rel) = rel {
            // A file appearing twice (staged + unstaged hunks in separate
            // diffs) is merged by summing by the caller; within one diff
            // each path appears once.
            out.insert(rel, counts);
        }
        i += 1;
    }
    out
}

/// Added/removed lines vs HEAD for every dirty file (`git diff --numstat`).
/// Merges the staged (HEAD→index) and unstaged (index→workdir) diffs so a
/// file changed in both places reports the total. Untracked files report
/// their line count as additions (when readable); anything that fails
/// (binary, missing, diff error) stays 0/0 rather than failing status.
fn workdir_line_stats(repo: &Repository) -> std::collections::HashMap<String, (usize, usize)> {
    let mut merged: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    let mut add = |path: String, a: usize, d: usize| {
        merged
            .entry(path)
            .and_modify(|e| {
                e.0 += a;
                e.1 += d;
            })
            .or_insert((a, d));
    };
    // Files that are new, the only ones the fallback below may count: a
    // tracked file at 0/0 changed only its mode (or type), not its lines.
    let mut new_files = std::collections::HashSet::new();
    let mut note_new = |diff: &git2::Diff| {
        for delta in diff.deltas() {
            if matches!(delta.status(), git2::Delta::Added | git2::Delta::Untracked) {
                if let Some(p) = delta.new_file().path() {
                    new_files.insert(p.to_string_lossy().into_owned());
                }
            }
        }
    };
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    // Staged: HEAD → index.
    if let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), None, None) {
        note_new(&diff);
        for (path, (a, d)) in diff_line_stats(&diff) {
            add(path, a, d);
        }
    }
    // Unstaged: index → workdir, with untracked content so new files get
    // real counts instead of 0/0.
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if let Ok(diff) = repo.diff_index_to_workdir(None, Some(&mut opts)) {
        note_new(&diff);
        for (path, (a, d)) in diff_line_stats(&diff) {
            add(path, a, d);
        }
    }
    // Fallback for untracked files libgit2 didn't emit content for
    // (empty diff, binary check skipped): count lines on disk.
    let workdir = repo.workdir().map(|w| w.to_path_buf());
    for (path, counts) in merged.clone() {
        if counts != (0, 0) || !new_files.contains(&path) {
            continue;
        }
        let abs = match &workdir {
            Some(w) => w.join(&path),
            None => continue,
        };
        if !abs.is_file() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&abs) {
            if bytes.len() > 1024 * 1024 || bytes.contains(&0) {
                continue; // binary or too large: leave 0/0
            }
            let lines = bytes.iter().filter(|&&b| b == b'\n').count();
            // A trailing-newline-less last line still counts.
            let lines = if bytes.is_empty() || bytes.ends_with(b"\n") {
                lines
            } else {
                lines + 1
            };
            if lines > 0 {
                merged.insert(path, (lines, 0));
            }
        }
    }
    merged
}

/// The checked-out branch's upstream ("origin/feature") and the remote
/// its push goes to: (None, None) off a branch.
fn branch_remotes(repo: &Repository) -> (Option<String>, Option<String>) {
    let Ok(head) = repo.head() else {
        return (None, None);
    };
    let Some(name) = head.is_branch().then(|| head.shorthand().ok()).flatten() else {
        return (None, None);
    };
    let upstream = repo
        .find_branch(name, BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
        .and_then(|u| u.name().ok().flatten().map(str::to_string));
    (upstream, push_remote_name(repo, name).ok())
}

fn compute_ahead_behind(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return Some((0, 0));
    }
    let local_oid = head.target()?;
    let branch_name = head.shorthand().ok()?;
    let branch = repo.find_branch(branch_name, BranchType::Local).ok()?;
    let upstream = branch.upstream().ok()?;
    let upstream_oid = upstream.get().target()?;
    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn git_status(path: String) -> Result<GitStatus, String> {
    off_main_thread(move || git_status_blocking(path)).await
}

pub fn git_status_blocking(path: String) -> Result<GitStatus, String> {
    let repo = open_repo(&path)?;

    let branch = match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().unwrap_or("HEAD").to_string(),
        Ok(head) => {
            let oid = head.target().map(|o| o.to_string()).unwrap_or_default();

            format!("detached@{}", &oid[..oid.len().min(7)])
        }
        Err(_) => "unborn".to_string(), // repo has no commits yet
    };

    let operation = crate::git_sync::current_operation(&repo);
    // A rebase detaches HEAD; it's still the branch being rebased.
    let branch = match operation.as_ref().and_then(|o| o.branch.clone()) {
        Some(b) if operation.as_ref().is_some_and(|o| o.kind == "rebase") => b,
        _ => branch,
    };

    let (ahead, behind) = compute_ahead_behind(&repo).unwrap_or((0, 0));
    let (upstream, push_remote) = branch_remotes(&repo);

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;

    let stats = workdir_line_stats(&repo);

    let files = statuses
        .iter()
        .filter(|entry| !entry.status().is_ignored())
        .map(|entry| {
            let s = entry.status();
            // Not UTF-8: shown as near as it gets, not as an empty row.
            let path = String::from_utf8_lossy(entry.path_bytes()).into_owned();
            let (additions, deletions) = stats.get(&path).copied().unwrap_or((0, 0));
            GitFile {
                path,
                status: classify_status(s).to_string(),
                staged: s.intersects(
                    Status::INDEX_NEW
                        | Status::INDEX_MODIFIED
                        | Status::INDEX_DELETED
                        | Status::INDEX_RENAMED
                        | Status::INDEX_TYPECHANGE,
                ),
                unstaged: s.intersects(
                    Status::WT_NEW
                        | Status::WT_MODIFIED
                        | Status::WT_DELETED
                        | Status::WT_RENAMED
                        | Status::WT_TYPECHANGE,
                ),
                additions,
                deletions,
            }
        })
        .collect();

    Ok(GitStatus {
        branch,
        ahead,
        behind,
        files,
        workdir: repo
            .workdir()
            // libgit2 writes `C:/p/` on Windows; the app names files `C:\p\…`.
            .map(|w| {
                let w = w.to_string_lossy();
                let w = if cfg!(windows) {
                    w.replace('/', "\\")
                } else {
                    w.into_owned()
                };
                w.trim_end_matches(['/', '\\']).to_string()
            })
            .unwrap_or_default(),
        operation,
        upstream,
        push_remote,
    })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitBranch {
    pub name: String,
    /// `origin/main` style remote-tracking branch (can't be deleted here,
    /// checking it out creates a local tracking branch).
    pub is_remote: bool,
}

#[tauri::command]
pub async fn git_branches(path: String) -> Result<Vec<GitBranch>, String> {
    off_main_thread(move || git_branches_blocking(path)).await
}

pub fn git_branches_blocking(path: String) -> Result<Vec<GitBranch>, String> {
    let repo = open_repo(&path)?;
    let mut local: Vec<GitBranch> = Vec::new();
    let mut remote: Vec<GitBranch> = Vec::new();

    let branches = repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?;

    for item in branches {
        let (branch, _) = item.map_err(|e| e.to_string())?;
        if let Some(name) = branch.name().map_err(|e| e.to_string())? {
            local.push(GitBranch {
                name: name.to_string(),
                is_remote: false,
            });
        }
    }

    let branches = repo
        .branches(Some(BranchType::Remote))
        .map_err(|e| e.to_string())?;

    for item in branches {
        let (branch, _) = item.map_err(|e| e.to_string())?;
        if let Some(name) = branch.name().map_err(|e| e.to_string())? {
            // Skip the `origin/HEAD` symref; it's not a branch to switch to.
            if name.ends_with("/HEAD") {
                continue;
            }
            remote.push(GitBranch {
                name: name.to_string(),
                is_remote: true,
            });
        }
    }

    local.sort_by(|a, b| a.name.cmp(&b.name));
    remote.sort_by(|a, b| a.name.cmp(&b.name));
    local.extend(remote);
    Ok(local)
}

#[tauri::command]
pub async fn git_checkout(path: String, branch: String) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_checkout_blocking(path, branch)).await
}

pub fn git_checkout_blocking(path: String, branch: String) -> Result<(), String> {
    let repo = open_repo(&path)?;

    // Switches to a local branch: its tree into the working folder (safely:
    // uncommitted changes to files that differ between the branches stop
    // it, like `git checkout` does), then HEAD onto it.
    let switch_to_local = |name: &str| -> Result<(), String> {
        let refname = format!("refs/heads/{name}");
        let object = repo
            .revparse_single(&refname)
            .map_err(|e| format!("Checkout failed: {e}"))?;
        repo.checkout_tree(&object, None)
            .map_err(|e| format!("Checkout failed: {e}"))?;
        repo.set_head(&refname).map_err(|e| e.to_string())
    };

    // A local branch: switch to it directly.
    if repo.find_branch(&branch, BranchType::Local).is_ok() {
        return switch_to_local(&branch);
    }

    // A remote-tracking branch (`origin/foo`): create a local tracking
    // branch (`foo`, or `feature/foo` for `origin/feature/foo`) and switch
    // to it, like `git switch` does. If that local branch already exists
    // (created since the list was loaded), just switch to it — to its own
    // commit, not the remote's: the two can differ, and a working folder
    // at one with HEAD on the other would show every difference as an
    // uncommitted change.
    if let Ok(remote) = repo.find_branch(&branch, BranchType::Remote) {
        let short = branch
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(branch.as_str());
        if short.is_empty() || short == "HEAD" {
            return Err(format!("Can't check out '{branch}'"));
        }
        if repo.find_branch(short, BranchType::Local).is_err() {
            let commit = remote
                .get()
                .peel_to_commit()
                .map_err(|e| format!("Checkout failed: {e}"))?;
            let mut local = repo
                .branch(short, &commit, false)
                .map_err(|e| format!("Couldn't create tracking branch '{short}': {e}"))?;
            // Point `branch.<short>.remote/.merge` at the remote branch so
            // pull/push and the ahead/behind badges work.
            let _ = local.set_upstream(Some(branch.as_str()));
        }
        return switch_to_local(short);
    }

    // Fallback: a commit hash, tag, or anything else `git checkout` accepts.
    let (object, reference) = repo
        .revparse_ext(&branch)
        .map_err(|e| format!("Branch '{branch}' not found: {e}"))?;

    repo.checkout_tree(&object, None)
        .map_err(|e| format!("Checkout failed: {e}"))?;

    // Only a local branch is something HEAD can sit on; a tag or a
    // remote-tracking ref (`refs/remotes/origin/foo` spelled out) leaves it
    // detached at the commit, as git does, rather than pointing HEAD at a
    // ref that commits must never move.
    let local_ref = reference
        .as_ref()
        .and_then(|r| r.name().ok())
        .filter(|n| n.starts_with("refs/heads/"))
        .map(str::to_string);
    match local_ref {
        Some(name) => repo.set_head(&name),
        None => repo.set_head_detached(object.id()),
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn git_commit(path: String, message: String) -> Result<(), String> {
    off_main_thread(move || git_commit_blocking(path, message)).await
}

pub fn git_commit_blocking(path: String, message: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let mut index = repo.index().map_err(|e| e.to_string())?;

    // Mirrors `git add -A`: stage new/modified files, and drop deleted
    // ones from the index. Both calls respect .gitignore.
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| e.to_string())?;
    index
        .update_all(["*"].iter(), None)
        .map_err(|e| e.to_string())?;
    index.write().map_err(|e| e.to_string())?;

    let tree_id = index.write_tree().map_err(|e| e.to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|e| e.to_string())?;

    let signature = repo
        .signature()
        .map_err(|e| format!("No Git identity configured (user.name / user.email): {e}"))?;

    let parent_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent_commit.iter().collect();

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &parents,
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn git_fetch(path: String) -> Result<(), String> {
    off_main_thread(move || git_fetch_blocking(path)).await
}

pub fn git_fetch_blocking(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    // Like `git fetch`: the current branch's remote, else origin.
    let remote_name = match upstream_remote_name(&repo) {
        Some(name) if repo.find_remote(&name).is_ok() => name,
        _ => default_remote_name(&repo)?,
    };
    let mut remote = repo.find_remote(&remote_name).map_err(|e| e.to_string())?;

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks());

    let refspecs: Vec<String> = remote
        .fetch_refspecs()
        .map_err(|e| e.to_string())?
        .iter()
        .filter_map(|r| r.ok().flatten())
        .map(String::from)
        .collect();

    remote
        .fetch(&refspecs, Some(&mut fetch_opts), None)
        .map_err(|e| format!("Fetch failed: {e}"))
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PushOutcome {
    /// "pushed" | "rejected": the remote branch has commits this one
    /// doesn't, so they have to be pulled (merged or rebased) first.
    pub result: &'static str,
    /// The remote's reason, for "rejected".
    pub message: Option<String>,
}

/// The remote has moved on: what a pull fixes, as opposed to a hook or a
/// protected branch turning the push down.
fn is_behind_remote(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    r.contains("non-fast-forward")
        || r.contains("non-fastforward")
        || r.contains("fetch first")
        || r.contains("not present locally")
}

#[tauri::command]
pub async fn git_push(path: String) -> Result<PushOutcome, String> {
    off_main_thread(move || git_push_blocking(path)).await
}

pub fn git_push_blocking(path: String) -> Result<PushOutcome, String> {
    let repo = open_repo(&path)?;
    let head = repo.head().map_err(|e| e.to_string())?;

    if !head.is_branch() {
        return Err("Cannot push: HEAD is detached, not on a branch".to_string());
    }

    let branch_name = head
        .shorthand()
        .map_err(|e| format!("Could not determine current branch name: {e}"))?
        .to_string();

    let remote_name = push_remote_name(&repo, &branch_name)?;
    let mut remote = repo.find_remote(&remote_name).map_err(|e| e.to_string())?;

    let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");
    // The server turns a ref down through this callback, not as an error.
    let refused = std::cell::RefCell::new(None::<String>);
    let mut callbacks = make_callbacks();
    callbacks.push_update_reference(|_, status| {
        if let Some(reason) = status {
            *refused.borrow_mut() = Some(reason.to_string());
        }
        Ok(())
    });
    let mut push_opts = PushOptions::new();
    push_opts.remote_callbacks(callbacks);

    let pushed = remote.push(&[&refspec], Some(&mut push_opts));
    drop(push_opts);
    match pushed {
        Err(e) if e.code() == git2::ErrorCode::NotFastForward || is_behind_remote(e.message()) => {
            return Ok(PushOutcome {
                result: "rejected",
                message: Some(e.message().to_string()),
            });
        }
        Err(e) => return Err(format!("Push failed: {e}")),
        Ok(()) => {}
    }
    if let Some(reason) = refused.into_inner() {
        if is_behind_remote(&reason) {
            return Ok(PushOutcome {
                result: "rejected",
                message: Some(reason),
            });
        }
        return Err(format!("Push rejected by the remote: {reason}"));
    }

    // First push on a branch with no upstream yet: set one, so future
    // fetch/pull/ahead-behind calculations have something to compare against.
    let has_upstream = repo
        .find_branch(&branch_name, BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
        .is_some();

    if !has_upstream {
        if let Ok(mut config) = repo.config() {
            let _ = config.set_str(&format!("branch.{branch_name}.remote"), &remote_name);
            let _ = config.set_str(
                &format!("branch.{branch_name}.merge"),
                &format!("refs/heads/{branch_name}"),
            );
        }
    }

    Ok(PushOutcome {
        result: "pushed",
        message: None,
    })
}

#[tauri::command]
pub async fn git_init(path: String) -> Result<(), String> {
    off_main_thread(move || git_init_blocking(path)).await
}

pub fn git_init_blocking(path: String) -> Result<(), String> {
    Repository::init(&path).map_err(|e| format!("Failed to initialize repository: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Extra operations for the Git panel's "More actions" menu.
// What the panel already does elsewhere (commit, push, fetch, pull,
// branch switching, reverting one file) is deliberately not repeated here.
// ---------------------------------------------------------------------------

/// The obvious mistakes in a branch name, rejected before libgit2 sees them
/// so the error reads like guidance rather than a plumbing complaint.
fn validate_branch_name(name: &str) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Branch name can't be empty".to_string());
    }
    // The rules of `git check-ref-format --branch`: no component may start
    // with `.` or end with `.lock`, and the name can't be `@` or `HEAD`.
    if name == "HEAD"
        || name == "@"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"))
        || name.contains(|c: char| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
    {
        return Err(format!("'{name}' isn't a valid branch name"));
    }
    Ok(name)
}

/// Where a new branch starts: a branch from the panel's list first (a
/// local one, then a remote-tracking one), so that a tag or a file that
/// happens to share the name can't win the way `git rev-parse` would
/// resolve it; then anything a commit can be found by (a tag, a hash).
/// Returns the commit and, when it's a remote-tracking branch, its name.
fn resolve_start_point<'r>(
    repo: &'r Repository,
    start: &str,
) -> Result<(git2::Commit<'r>, Option<String>), String> {
    if let Ok(local) = repo.find_branch(start, BranchType::Local) {
        let commit = local
            .get()
            .peel_to_commit()
            .map_err(|e| format!("Couldn't use '{start}' as a starting point: {e}"))?;
        return Ok((commit, None));
    }
    if let Ok(remote) = repo.find_branch(start, BranchType::Remote) {
        let commit = remote
            .get()
            .peel_to_commit()
            .map_err(|e| format!("Couldn't use '{start}' as a starting point: {e}"))?;
        return Ok((commit, Some(start.to_string())));
    }
    let commit = repo
        .revparse_single(start)
        .map_err(|_| format!("Couldn't find starting point '{start}'"))?
        .peel_to_commit()
        .map_err(|e| format!("Couldn't use '{start}' as a starting point: {e}"))?;
    Ok((commit, None))
}

/// Creates a local branch at `from` (a local branch, remote-tracking
/// branch, tag or commit; defaults to the current HEAD when empty);
/// with `checkout`, switches to it straight away (the panel's usual
/// "new branch" flow). `foo` started from `origin/foo` tracks it, so
/// pull/push and the ahead/behind badges work (what checking the remote
/// branch out does). A branch under another name doesn't: the panel's
/// push goes to the remote branch of the same name, and that's what the
/// badges would be wrong against.
#[tauri::command]
pub async fn git_create_branch(
    path: String,
    name: String,
    checkout: bool,
    from: Option<String>,
) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy =
        checkout.then(|| rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path)));
    off_main_thread(move || git_create_branch_blocking(path, name, checkout, from)).await
}

pub fn git_create_branch_blocking(
    path: String,
    name: String,
    checkout: bool,
    from: Option<String>,
) -> Result<(), String> {
    let name = validate_branch_name(&name)?;
    let from = from.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let repo = open_repo(&path)?;
    let (target, remote_start) = match &from {
        Some(start) => resolve_start_point(&repo, start)?,
        None => (
            repo.head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .ok_or_else(|| "Can't create a branch yet: there are no commits".to_string())?,
            None,
        ),
    };
    let mut branch = repo
        .branch(&name, &target, false)
        .map_err(|e| format!("Couldn't create branch '{name}': {e}"))?;
    if let Some(start) = &remote_start {
        let tracks_same_name = start
            .split_once('/')
            .is_some_and(|(_, short)| short == name);
        if tracks_same_name {
            let _ = branch.set_upstream(Some(start));
        }
    }
    drop(target);
    drop(branch);
    if checkout {
        // A switch the working folder refuses (uncommitted changes to files
        // the branches differ in) leaves no branch behind, as `git checkout
        // -b` wouldn't: the panel reports the error, and the user's next try
        // under the same name must not hit "already exists".
        if let Err(e) = git_checkout_blocking(path, name.clone()) {
            if let Ok(mut created) = repo.find_branch(&name, BranchType::Local) {
                let _ = created.delete();
            }
            return Err(e);
        }
    }
    Ok(())
}

/// Deletes a local branch. The branch that's checked out can't be deleted.
#[tauri::command]
pub async fn git_delete_branch(path: String, name: String) -> Result<(), String> {
    off_main_thread(move || git_delete_branch_blocking(path, name)).await
}

pub fn git_delete_branch_blocking(path: String, name: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    if let Ok(head) = repo.head() {
        if head.is_branch() && head.shorthand().ok() == Some(name.as_str()) {
            return Err(format!("Can't delete '{name}': it's the current branch"));
        }
    }
    let mut branch = repo
        .find_branch(&name, BranchType::Local)
        .map_err(|_| format!("Branch '{name}' not found"))?;
    branch.delete().map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStashEntry {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitCommitEntry {
    pub oid: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    /// Commit time, seconds since the epoch.
    pub time: i64,
    /// Total files changed in this commit.
    #[serde(default)]
    pub file_count: usize,
    /// Files matching the active history search (`None` when no query).
    #[serde(default)]
    pub matched_file_count: Option<usize>,
}

/// Shelves the working tree (tracked changes and untracked files) on the
/// stash, so the tree comes back clean. Needs a git identity, like commit.
#[tauri::command]
pub async fn git_stash_push(path: String, message: Option<String>) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_stash_push_blocking(path, message)).await
}

pub fn git_stash_push_blocking(path: String, message: Option<String>) -> Result<(), String> {
    let mut repo = open_repo(&path)?;
    let signature = repo
        .signature()
        .map_err(|e| format!("No Git identity configured (user.name / user.email): {e}"))?;
    let message = match message {
        Some(m) if !m.trim().is_empty() => Some(m.trim().to_string()),
        _ => None,
    };
    repo.stash_save2(
        &signature,
        message.as_deref(),
        Some(StashFlags::INCLUDE_UNTRACKED),
    )
    .map_err(|e| {
        if e.message().contains("no changes") || e.message().contains("No changes") {
            "Nothing to stash: there are no changes".to_string()
        } else {
            format!("Couldn't stash the changes: {e}")
        }
    })?;
    Ok(())
}

/// The stash list, newest first — what `git stash list` shows.
#[tauri::command]
pub async fn git_stash_list(path: String) -> Result<Vec<GitStashEntry>, String> {
    off_main_thread(move || git_stash_list_blocking(path)).await
}

pub fn git_stash_list_blocking(path: String) -> Result<Vec<GitStashEntry>, String> {
    let mut repo = open_repo(&path)?;
    let mut entries = Vec::new();
    repo.stash_foreach(|index, message, _oid| {
        entries.push(GitStashEntry {
            index,
            message: message.to_string(),
        });
        true
    })
    .map_err(|e| e.to_string())?;
    Ok(entries)
}

/// Restores a stashed state and drops it from the list (`git stash pop`).
/// `index` defaults to the newest entry. On conflicts nothing is dropped —
/// the error says to resolve them first.
#[tauri::command]
pub async fn git_stash_pop(path: String, index: Option<usize>) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_stash_pop_blocking(path, index)).await
}

pub fn git_stash_pop_blocking(path: String, index: Option<usize>) -> Result<(), String> {
    let mut repo = open_repo(&path)?;
    repo.stash_pop(index.unwrap_or(0), None)
        .map_err(|e| format!("Couldn't restore the stash: {e}"))?;
    Ok(())
}

/// Forgets a stashed state without restoring it.
#[tauri::command]
pub async fn git_stash_drop(path: String, index: usize) -> Result<(), String> {
    off_main_thread(move || git_stash_drop_blocking(path, index)).await
}

pub fn git_stash_drop_blocking(path: String, index: usize) -> Result<(), String> {
    let mut repo = open_repo(&path)?;
    repo.stash_drop(index).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStashFile {
    pub path: String,
    pub status: String, // "M" | "A" | "D" | "R" | "?"
    /// Added / removed lines in this stash (`git stash show --numstat`).
    #[serde(default)]
    pub additions: usize,
    #[serde(default)]
    pub deletions: usize,
    /// Whether this file matches the active history search query.
    /// True for all files when no query is given (backward compatible).
    #[serde(default = "default_matched")]
    pub matched: bool,
    /// Why it matched: "filename", "content", or "both". None when
    /// unmatched or when no query was given.
    #[serde(default)]
    pub match_kind: Option<String>,
}

fn default_matched() -> bool {
    true
}

fn delta_status(s: git2::Delta) -> &'static str {
    match s {
        git2::Delta::Added | git2::Delta::Untracked | git2::Delta::Copied => "A",
        git2::Delta::Deleted => "D",
        git2::Delta::Renamed => "R",
        git2::Delta::Typechange => "M",
        _ => "M",
    }
}

/// The stash commit plus its base tree and optional untracked-files tree.
///
/// A stash is a merge commit: parent 0 is the HEAD it was taken on (the
/// base), parent 1 the index state, and — only when there were untracked
/// files — parent 2 the untracked files. The stash commit's own tree holds
/// the tracked working-tree state; untracked files live only in parent 2.
fn stash_trees(
    repo: &Repository,
    index: usize,
) -> Result<
    (
        git2::Tree<'_>,
        Option<git2::Tree<'_>>,
        Option<git2::Tree<'_>>,
    ),
    String,
> {
    let obj = repo
        .revparse_single(&format!("stash@{{{index}}}"))
        .map_err(|_| format!("Stash entry {index} not found"))?;
    let commit = obj.peel_to_commit().map_err(|e| e.to_string())?;
    let stash = commit.tree().map_err(|e| e.to_string())?;
    let base = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let untracked = if commit.parent_count() >= 3 {
        commit.parent(2).ok().and_then(|p| p.tree().ok())
    } else {
        None
    };
    Ok((stash, base, untracked))
}

/// What a stash contains: the files it touched, from diffing the base
/// against the stash tree (tracked changes) and — when present — the
/// untracked-files tree. What `git stash show` lists.
#[tauri::command]
pub async fn git_stash_show(path: String, index: usize) -> Result<Vec<GitStashFile>, String> {
    off_main_thread(move || git_stash_show_blocking(path, index)).await
}

pub fn git_stash_show_blocking(path: String, index: usize) -> Result<Vec<GitStashFile>, String> {
    let repo = open_repo(&path)?;
    let (stash, base, untracked) = stash_trees(&repo, index)?;
    let mut files: std::collections::HashMap<String, (String, usize, usize)> =
        std::collections::HashMap::new();
    // Untracked first: on overlap the tracked diff wins (both status and counts).
    for other in [untracked.as_ref(), Some(&stash)] {
        let Some(other) = other else { continue };
        let diff = repo
            .diff_tree_to_tree(base.as_ref(), Some(other), None)
            .map_err(|e| e.to_string())?;
        let stats = diff_line_stats(&diff);
        for delta in diff.deltas() {
            let rel = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned());
            if let Some(rel) = rel {
                let (a, d) = stats.get(&rel).copied().unwrap_or((0, 0));
                files.insert(rel, (delta_status(delta.status()).to_string(), a, d));
            }
        }
    }
    let mut out: Vec<GitStashFile> = files
        .into_iter()
        .map(|(path, (status, additions, deletions))| GitStashFile {
            path,
            status,
            additions,
            deletions,
            matched: true,
            match_kind: None,
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The stashed version of one file's content, for previewing a stash.
/// `file` is relative to the repository's working folder. None: the file
/// was deleted in the stash.
#[tauri::command]
pub async fn git_stash_file_content(
    path: String,
    index: usize,
    file: String,
) -> Result<Option<String>, String> {
    off_main_thread(move || git_stash_file_content_blocking(path, index, file)).await
}

pub fn git_stash_file_content_blocking(
    path: String,
    index: usize,
    file: String,
) -> Result<Option<String>, String> {
    Ok(git_stash_file_preview_blocking(path, index, file)?.content)
}

/// One file's stashed content plus the base it changed from, in a single
/// call: enough to open a read-only editor tab with change marks, showing
/// what the stash changes. `content`: the stashed version (None: deleted
/// in the stash); `base`: the version at the stash's base commit (None:
/// the file is new in the stash). `file` is relative to the repository's
/// working folder.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStashFilePreview {
    pub content: Option<String>,
    pub base: Option<String>,
}

#[tauri::command]
pub async fn git_stash_file_preview(
    path: String,
    index: usize,
    file: String,
) -> Result<GitStashFilePreview, String> {
    off_main_thread(move || git_stash_file_preview_blocking(path, index, file)).await
}

fn read_tree_blob(
    _repo: &Repository,
    tree: Option<&git2::Tree<'_>>,
    rel: &std::path::Path,
) -> Result<Option<git2::Oid>, String> {
    let Some(tree) = tree else { return Ok(None) };
    Ok(tree.get_path(rel).ok().and_then(|e| {
        if e.kind() == Some(git2::ObjectType::Blob) {
            Some(e.id())
        } else {
            None
        }
    }))
}

fn read_blob_text(repo: &Repository, id: git2::Oid, file: &str) -> Result<String, String> {
    let blob = repo.find_blob(id).map_err(|e| e.to_string())?;
    if blob.is_binary() {
        return Err(format!("{file}: binary file"));
    }
    if blob.size() > 512 * 1024 {
        return Err(format!("{file}: too large to preview"));
    }
    Ok(String::from_utf8_lossy(blob.content()).into_owned())
}

pub fn git_stash_file_preview_blocking(
    path: String,
    index: usize,
    file: String,
) -> Result<GitStashFilePreview, String> {
    let repo = open_repo(&path)?;
    let rel = std::path::Path::new(&file);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("{file}: not a path inside the repository"));
    }
    let (stash, base_tree, untracked) = stash_trees(&repo, index)?;
    let mut content_id = read_tree_blob(&repo, Some(&stash), rel)?;
    if content_id.is_none() {
        content_id = read_tree_blob(&repo, untracked.as_ref(), rel)?;
    }
    let content = content_id
        .map(|id| read_blob_text(&repo, id, &file))
        .transpose()?;
    let base = read_tree_blob(&repo, base_tree.as_ref(), rel)?
        .map(|id| read_blob_text(&repo, id, &file))
        .transpose()?;
    Ok(GitStashFilePreview { content, base })
}

/// Throws away every change in the working tree: tracked files go back to
/// HEAD, and files git doesn't know yet go to the trash (as with reverting
/// one file, so the action can be undone from there).
#[tauri::command]
pub async fn git_discard_all(path: String) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_discard_all_blocking(path)).await
}

pub fn git_discard_all_blocking(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let workdir = repo
        .workdir()
        .ok_or("the repository has no working folder")?
        .to_path_buf();

    // Files git doesn't know go to the trash; a hard reset leaves them (and
    // previously staged-new files) on disk, so collect them first.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);
    let mut trash_untracked = |repo: &Repository| -> Result<(), String> {
        let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;
        for entry in statuses.iter() {
            if !entry.status().is_wt_new() {
                continue;
            }
            let rel = entry.path().unwrap_or_default();
            if rel.is_empty() {
                continue;
            }
            let abs = workdir.join(rel);
            if abs.is_file() {
                crate::filesystem::to_trash(&abs)
                    .map_err(|e| format!("{rel}: couldn't move it to the trash: {e}"))?;
            }
        }
        Ok(())
    };
    trash_untracked(&repo)?;

    match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        // Tracked changes (staged or not) back to the last commit.
        Some(head) => repo
            .reset(head.as_object(), ResetType::Hard, None)
            .map_err(|e| e.to_string())?,
        // No commits yet: nothing to reset to, so empty the index instead.
        None => {
            let mut index = repo.index().map_err(|e| e.to_string())?;
            index.clear().map_err(|e| e.to_string())?;
            index.write().map_err(|e| e.to_string())?;
        }
    }

    // Staged-new files survived the reset as untracked files; they were part
    // of "all changes" too.
    trash_untracked(&repo)?;
    Ok(())
}

/// Recent commits on the current HEAD, newest first, for the history view.
///
/// With `query`, only commits matching the search are returned (smart
/// history search). Matching is case-insensitive; the query is split into
/// whitespace-separated terms and EVERY term must match somewhere in the
/// commit (AND semantics). Each term matches when it appears in:
/// - the message, author, or oid (prefix or substring), or
/// - a changed file's path: full path, basename, or with `_`/`-`/etc.
///   ignored so `get_user` also matches `getUserName` (compact form), or
/// - the patch itself: added/removed lines, raw or compacted the same way,
///   so function/variable/class names (or any identifier, partial) match.
///   Single-character terms never scan patch content: they match
///   metadata/filenames only (a 1-char content scan matches almost every
///   commit and costs a full diff walk for no signal).
///
/// Approach (researched `git log -S/-G` pickaxe, `git log --name-status`,
/// and `git grep $(git rev-list …)`): rather than shelling out, walk with
/// revwalk and diff parent→commit via libgit2 in one backend call. Cheap
/// metadata (message/author/oid) is checked first so most non-matching
/// commits never pay for a diff; filenames come from the same diff's
/// deltas; content comes from the diff's added/deleted lines only
/// (context lines excluded), which is exactly what `-S`/`-G` semantics
/// care about. Full tree-sitter symbol extraction per historic blob was
/// considered and rejected: substring + compact-form matching on patch
/// lines already covers partial identifier matches in every language at a
/// fraction of the cost.
///
/// Pagination: `offset` counts raw revwalk commits already scanned
/// (not results), so a search page can resume where the previous one
/// stopped without re-scanning. Offsets count from `tip`: the first page
/// walks from HEAD and returns it as `tip`, and later pages pass it back,
/// so commits made meanwhile (a terminal, another tool) can't shift the
/// offsets and repeat or skip a commit. A tip that no longer exists
/// (history rewritten and pruned) falls back to HEAD; the reply's `tip`
/// then differs from the one sent, and the UI starts over. Each call scans at most `SCAN_CAP`
/// commits and returns at most `limit` matches; `has_more` tells the UI
/// whether older commits remain, and `next_offset` is the `offset` for
/// the next page. The walk is capped per call so typing stays interactive
/// on large histories; "load more / search further" pages further back
/// instead of raising the cap.
#[tauri::command]
pub async fn git_log(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
    tip: Option<String>,
) -> Result<GitLogPage, String> {
    // Each call supersedes the ones before it (the panel shows only the
    // latest reply): an older search still scanning stops at its next
    // commit instead of finishing hundreds of diffs nobody will see.
    let generation = GIT_LOG_LATEST.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    off_main_thread(move || {
        let superseded = || GIT_LOG_LATEST.load(std::sync::atomic::Ordering::SeqCst) != generation;
        git_log_scan(path, limit, query, offset, tip.as_deref(), &superseded)
    })
    .await
}

/// The latest `git_log` call's generation (see `git_log`).
static GIT_LOG_LATEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One page of history: the matches plus the cursor for the next page.
/// `offset`/`next_offset` count scanned (raw) commits, so pages resume
/// the revwalk without re-scanning. `has_more` is true when the walk
/// still has older commits the cap stopped us from reaching — a query
/// can return fewer than `limit` matches and still have more history
/// worth searching.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitLogPage {
    pub commits: Vec<GitCommitEntry>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub next_offset: usize,
    #[serde(default)]
    pub scanned: usize,
    /// The commit this page's walk started from (offsets count from it);
    /// pass it back with the next page. None for an empty repository.
    #[serde(default)]
    pub tip: Option<String>,
}

/// Commits scanned for one page before stopping (results still capped
/// to `limit`; newest first, so this bounds latency, not correctness of
/// the top matches — the UI pages further with `offset`).
const GIT_LOG_SCAN_CAP: usize = 400;

/// Added/deleted content lines examined per commit before stopping the
/// content scan (a match past this is missed; 20k lines is far beyond a
/// typical commit and keeps one giant generated-file commit from hanging
/// a keystroke).
const GIT_LOG_LINES_PER_COMMIT_CAP: usize = 20_000;

/// Per-file share of the content budget: one giant generated file must
/// not eat the whole per-commit budget and starve the other files.
const GIT_LOG_LINES_PER_FILE_CAP: usize = 5_000;

/// Binary deltas never match content (their patch has no usable text).
/// Filename/message matches still count.
fn delta_is_binary(delta: &git2::DiffDelta) -> bool {
    delta.new_file().is_binary() || delta.old_file().is_binary()
}

/// Lowercase alphanumeric only: `get_user-name` → `getusername`, so
/// snake_case, kebab-case and camelCase match each other partially.
/// Only used for short strings (query terms, filenames). Patch lines use
/// `compact_contains` instead to avoid allocating per line.
fn compact_ident(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Compact-substring check without allocating the compacted haystack:
/// true when `needle_compact` (already lowercase alnum-only) appears in
/// `haystack_lower` after ignoring non-alphanumeric characters — i.e.
/// `compact(haystack).contains(needle)` without building the string.
/// Byte-oriented; both sides are lowercased before calling.
fn compact_contains(haystack_lower: &str, needle_compact: &str) -> bool {
    if needle_compact.is_empty() {
        return false;
    }
    let needle = needle_compact.as_bytes();
    let hay = haystack_lower.as_bytes();
    // Streaming match: needle bytes must appear in order, skipping
    // non-alphanumeric haystack bytes (which `compact()` would drop).
    // Comparison is ASCII case-insensitive so callers may pass a raw
    // (non-lowered) haystack; the needle is already compact+lowercase.
    let mut ni = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < hay.len() {
        let b = hay[i].to_ascii_lowercase();
        if !b.is_ascii_alphanumeric() {
            // Separator: skippable mid-match, never a match itself.
            i += 1;
            continue;
        }
        if b == needle[ni] {
            if ni == 0 {
                start = i;
            }
            ni += 1;
            if ni == needle.len() {
                return true;
            }
            i += 1;
        } else if ni > 0 {
            // Mismatch mid-match: restart just after the attempt's start.
            ni = 0;
            i = start + 1;
        } else {
            i += 1;
        }
    }
    false
}

/// One normalized search term: raw lowercase plus compact form.
struct SearchTerm {
    raw: String,
    compact: String,
}

/// Compact characters a term needs before its compact form is matched:
/// `c++` compacts to `c`, which would match nearly every path and line.
const MIN_COMPACT_CHARS: usize = 2;

impl SearchTerm {
    /// Whether the compact (separator-blind) form is worth matching.
    fn compact_usable(&self) -> bool {
        self.compact.len() >= MIN_COMPACT_CHARS
    }
}

/// True when this term may scan patch content. Single-character terms
/// match metadata/filenames only: a 1-char content scan hits nearly
/// every commit and costs a full patch walk for no signal.
fn term_scans_content(term: &SearchTerm) -> bool {
    term.raw.chars().count() > 1
}

fn normalize_query(query: &str) -> Vec<SearchTerm> {
    query
        .split_whitespace()
        .map(|t| {
            let raw = t.to_lowercase();
            let compact = compact_ident(t);
            SearchTerm { raw, compact }
        })
        .filter(|t| !t.raw.is_empty())
        .collect()
}

/// Filenames for one side of the match: full repo-relative path plus
/// basename, lowercased once per commit. Compact matching streams over
/// these via `compact_contains` (no compact allocation per file).
struct FileHaystack {
    path: String,
    base: String,
}

fn file_haystack(rel: &str) -> FileHaystack {
    let path = rel.to_lowercase();
    let base = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
    FileHaystack { path, base }
}

fn term_matches_file(term: &SearchTerm, f: &FileHaystack) -> bool {
    f.path.contains(&term.raw)
        || f.base.contains(&term.raw)
        || (term.compact_usable()
            && (compact_contains(&f.path, &term.compact)
                || compact_contains(&f.base, &term.compact)))
}

/// Added/removed line matches when the raw line or its compact form
/// contains the term (raw or compact). Covers identifiers, strings,
/// comments — anything textual in the patch. The compact check streams
/// (no allocation); callers check the cheap raw `contains` first only
/// when they already hold the lowered line.
fn term_matches_line(term: &SearchTerm, line_lower: &str) -> bool {
    line_lower.contains(&term.raw)
        || (term.compact_usable() && compact_contains(line_lower, &term.compact))
}

/// Added/removed patch lines only: context is excluded by the options, and
/// '>'/'<' are git's "\\ No newline at end of file" markers, not code.
fn is_change_line(origin: char) -> bool {
    matches!(origin, '+' | '-')
}

/// A commit's change: its first parent's tree against its own (the
/// initial commit against nothing), with renames detected, so a moved
/// file is one rename instead of a full delete plus a full add (whose
/// every line would match a content search).
fn commit_diff<'r>(
    repo: &'r Repository,
    commit: &git2::Commit,
    diff_opts: &mut git2::DiffOptions,
) -> Result<git2::Diff<'r>, git2::Error> {
    let tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(diff_opts))?;
    let mut find = git2::DiffFindOptions::new();
    find.renames(true);
    // Best effort: without detection the diff is still right, just coarser.
    let _ = diff.find_similar(Some(&mut find));
    Ok(diff)
}

/// The path a delta is listed under: the new side, or the old one for a
/// deletion.
fn delta_path(diff: &git2::Diff, i: usize) -> Option<String> {
    diff.get_delta(i).and_then(|d| {
        d.new_file()
            .path()
            .or_else(|| d.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Per changed file: (filename matches a term, added/removed lines match a
/// term). The one definition of a matching file, for the list's badge
/// (`git_log`) and the expanded file list (`git_commit_show`), so the two
/// always agree. Binary files never match by content; single-character
/// terms never scan content; each file's scan is capped.
fn file_match_flags(diff: &git2::Diff, terms: &[SearchTerm]) -> Vec<(bool, bool)> {
    let scannable: Vec<bool> = terms.iter().map(term_scans_content).collect();
    let scannable_count = scannable.iter().filter(|v| **v).count();
    let n = diff.deltas().len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let file_hit = delta_path(diff, i)
            .map(|rel| file_haystack(&rel))
            .is_some_and(|h| terms.iter().any(|t| term_matches_file(t, &h)));
        let binary = diff.get_delta(i).is_some_and(|d| delta_is_binary(&d));
        let content_hit =
            scannable_count > 0 && !binary && file_content_matches(diff, i, terms, &scannable);
        out.push((file_hit, content_hit));
    }
    out
}

/// Whether file `i`'s added/removed lines match any scannable term.
fn file_content_matches(
    diff: &git2::Diff,
    i: usize,
    terms: &[SearchTerm],
    scannable: &[bool],
) -> bool {
    let patch = match git2::Patch::from_diff(diff, i) {
        Ok(Some(p)) => p,
        Ok(None) | Err(_) => return false,
    };
    let mut examined = 0usize;
    for h in 0..patch.num_hunks() {
        let lines = match patch.num_lines_in_hunk(h) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for l in 0..lines {
            if examined >= GIT_LOG_LINES_PER_FILE_CAP {
                return false;
            }
            let line = match patch.line_in_hunk(h, l) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if !is_change_line(line.origin()) {
                continue;
            }
            examined += 1;
            let text = String::from_utf8_lossy(line.content()).to_lowercase();
            if text.trim().is_empty() {
                continue;
            }
            if terms
                .iter()
                .zip(scannable)
                .any(|(t, ok)| *ok && term_matches_line(t, &text))
            {
                return true;
            }
        }
    }
    false
}

/// `git_log` without cancellation (tests).
#[cfg(test)]
pub fn git_log_blocking(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
) -> Result<GitLogPage, String> {
    git_log_scan(path, limit, query, offset, None, &|| false)
}

/// A history walk, newest first, from `tip` when it's still a commit in
/// the repository, else from HEAD. Returns the walk and the commit it
/// starts from; None when the repository has no commits yet.
fn history_walk<'r>(
    repo: &'r Repository,
    tip: Option<&str>,
) -> Result<Option<(git2::Revwalk<'r>, git2::Oid)>, String> {
    let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
    // Newest first, but never a parent before its child (git's
    // --date-order): commits made in the same second (rebases, scripts,
    // quick amends) otherwise come out in any order.
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .map_err(|e| e.to_string())?;
    let pinned = tip
        .and_then(|t| t.parse::<git2::Oid>().ok())
        .filter(|oid| repo.find_commit(*oid).is_ok());
    let start = match pinned {
        Some(oid) => oid,
        None => match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(c) => c.id(),
            // No commits yet.
            None => return Ok(None),
        },
    };
    walk.push(start).map_err(|e| e.to_string())?;
    Ok(Some((walk, start)))
}

/// `git_log_blocking`, stopping early once `superseded` says a newer call
/// replaced this one (the partial page is never shown).
fn git_log_scan(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
    tip: Option<&str>,
    superseded: &dyn Fn() -> bool,
) -> Result<GitLogPage, String> {
    let repo = open_repo(&path)?;
    let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
    let skip = offset.unwrap_or(0);
    let terms = query
        .as_deref()
        .map(normalize_query)
        .filter(|t| !t.is_empty());
    // No commits yet: empty history, not an error.
    let Some((walk, start)) = history_walk(&repo, tip)? else {
        return Ok(GitLogPage {
            commits: Vec::new(),
            has_more: false,
            next_offset: skip,
            scanned: 0,
            tip: None,
        });
    };
    // A tip that no longer resolves fell back to HEAD: the offset counted
    // from the old tip means nothing here, so this page starts over.
    let skip = if tip.is_some_and(|t| t != start.to_string()) {
        0
    } else {
        skip
    };
    let scan_cap = if terms.is_some() {
        GIT_LOG_SCAN_CAP
    } else {
        limit
    };
    let mut diff_opts = git2::DiffOptions::new();
    diff_opts.context_lines(0);
    diff_opts.interhunk_lines(0);
    let mut entries = Vec::new();
    let mut scanned = 0usize;
    let mut exhausted = true;
    for oid in walk.skip(skip).take(scan_cap + 1) {
        if scanned >= scan_cap {
            // The +1 probe: there is at least one more commit beyond
            // this page's budget.
            exhausted = false;
            break;
        }
        if superseded() {
            exhausted = false;
            break;
        }
        let oid = oid.map_err(|e| e.to_string())?;
        scanned += 1;
        let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
        let (file_count, matched_file_count) = if let Some(terms) = &terms {
            match commit_match_details(&repo, &commit, terms, &mut diff_opts) {
                Some((total, matched)) => (total, Some(matched)),
                None => continue,
            }
        } else {
            // No query: cheap file count via diff without content scan
            (commit_file_count(&repo, &commit, &mut diff_opts), None)
        };
        // When query is present, commit_match_details already filtered non-matches
        // (continue above). For no-query path we always include.
        let full = oid.to_string();
        entries.push(GitCommitEntry {
            short: full.chars().take(7).collect(),
            oid: full,
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or("(no message)")
                .to_string(),
            author: commit.author().name().unwrap_or("?").to_string(),
            time: commit.time().seconds(),
            file_count,
            matched_file_count,
        });
        if entries.len() >= limit {
            // Page is full. Older commits may still hold (more) matches;
            // whether anything remains is resolved by the probe below.
            break;
        }
    }
    // If we stopped because the page filled up, we don't yet know
    // whether older commits remain. The loop above breaks out of the
    // iterator, so re-open a cheap walk to probe: skip(skip+scanned)
    // and check for one more oid. Bounded to a single step.
    let mut has_more = !exhausted;
    if !has_more && entries.len() >= limit {
        if let Some((mut probe_walk, _)) = history_walk(&repo, Some(&start.to_string()))? {
            if probe_walk.nth(skip + scanned).is_some() {
                has_more = true;
            }
        }
    }
    Ok(GitLogPage {
        commits: entries,
        has_more,
        next_offset: skip + scanned,
        scanned,
        tip: Some(start.to_string()),
    })
}

/// Total files changed in a commit (parent→commit diff, no content scan).
fn commit_file_count(
    repo: &Repository,
    commit: &git2::Commit,
    diff_opts: &mut git2::DiffOptions,
) -> usize {
    match commit_diff(repo, commit, diff_opts) {
        Ok(d) => d.deltas().len(),
        Err(_) => 0,
    }
}

/// Detailed match for one commit: `None` when the commit doesn't satisfy
/// the query (AND semantics), otherwise `(total_files, matched_files)`
/// where `matched_files` counts files matching ANY term via filename or
/// content. Message-only matches yield `matched_files == 0` — the caller
/// still shows the commit, and the file list shows all files unfiltered.
///
/// Two phases: metadata + filenames first (one diff, no patch text), then
/// patch content only for terms still unmatched. Single-character terms
/// never reach the content phase (see `term_scans_content`).
fn commit_match_details(
    repo: &Repository,
    commit: &git2::Commit,
    terms: &[SearchTerm],
    diff_opts: &mut git2::DiffOptions,
) -> Option<(usize, usize)> {
    // The whole message, body included (not just the summary line).
    let msg_lower = commit.message().unwrap_or("").to_lowercase();
    let author_lower = commit.author().name().unwrap_or("?").to_lowercase();
    let oid_str = commit.id().to_string();
    let short = oid_str.chars().take(7).collect::<String>();

    let mut pending: Vec<&SearchTerm> = Vec::with_capacity(terms.len());
    for term in terms.iter() {
        // Cheap raw checks first; the compact (streaming, allocation-free)
        // check runs only when the raw substring missed.
        let hit = oid_str.contains(&term.raw)
            || short.starts_with(&term.raw)
            || msg_lower.contains(&term.raw)
            || author_lower.contains(&term.raw)
            || (term.compact_usable() && compact_contains(&msg_lower, &term.compact));
        if !hit {
            pending.push(term);
        }
    }

    let diff = match commit_diff(repo, commit, diff_opts) {
        Ok(d) => d,
        Err(_) => {
            return if pending.is_empty() {
                Some((0, 0))
            } else {
                None
            }
        }
    };
    let n = diff.deltas().len();
    if n == 0 {
        return if pending.is_empty() {
            Some((0, 0))
        } else {
            None
        };
    }

    // Which pending terms are satisfied by a filename somewhere?
    let mut file_satisfied = vec![false; pending.len()];
    for i in 0..n {
        let Some(rel) = delta_path(&diff, i) else {
            continue;
        };
        let h = file_haystack(&rel);
        for (pi, term) in pending.iter().enumerate() {
            if term_matches_file(term, &h) {
                file_satisfied[pi] = true;
            }
        }
    }
    // Terms still needing content. Single-char terms never scan content:
    // unmatched by now, they fail the commit.
    let mut need_content: Vec<(usize, &SearchTerm)> = Vec::new();
    for (pi, term) in pending.iter().enumerate() {
        if file_satisfied[pi] {
            continue;
        }
        if !term_scans_content(term) {
            return None;
        }
        need_content.push((pi, *term));
    }
    // Content scan per file (pickaxe-style, added/removed lines only).
    // Binary files are skipped (no usable text); each file gets its own
    // budget share so one generated file can't starve the rest; the loop
    // stops as soon as every needed term has hit somewhere.
    let mut content_satisfied = vec![false; pending.len()];
    let mut remaining = need_content.len();
    if remaining > 0 {
        let mut examined = 0usize;
        'files: for i in 0..n {
            if examined >= GIT_LOG_LINES_PER_COMMIT_CAP || remaining == 0 {
                break;
            }
            if let Some(d) = diff.get_delta(i) {
                if delta_is_binary(&d) {
                    continue;
                }
            }
            let patch = match git2::Patch::from_diff(&diff, i) {
                Ok(Some(p)) => p,
                Ok(None) | Err(_) => continue,
            };
            let mut per_file = 0usize;
            let hunks = patch.num_hunks();
            for h in 0..hunks {
                let lines = match patch.num_lines_in_hunk(h) {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                for l in 0..lines {
                    if examined >= GIT_LOG_LINES_PER_COMMIT_CAP
                        || per_file >= GIT_LOG_LINES_PER_FILE_CAP
                        || remaining == 0
                    {
                        break;
                    }
                    let line = match patch.line_in_hunk(h, l) {
                        Ok(line) => line,
                        Err(_) => continue,
                    };
                    if !is_change_line(line.origin()) {
                        continue;
                    }
                    examined += 1;
                    per_file += 1;
                    let text = String::from_utf8_lossy(line.content()).to_lowercase();
                    if text.trim().is_empty() {
                        continue;
                    }
                    for (pi, term) in need_content.iter() {
                        if !content_satisfied[*pi] && term_matches_line(term, &text) {
                            content_satisfied[*pi] = true;
                            remaining -= 1;
                            if remaining == 0 {
                                break 'files;
                            }
                        }
                    }
                }
            }
        }
    }
    // Overall AND: every term must be meta-hit, filename-hit, or content-hit.
    for (pi, _) in pending.iter().enumerate() {
        if !file_satisfied[pi] && !content_satisfied[pi] {
            return None;
        }
    }
    // The badge: files matching any term, by the same rule (and the same
    // full scan) as the expanded file list in `git_commit_show`. The scan
    // above stops early to decide the match; this one runs only for
    // commits that matched, at most a page's worth.
    let matched_files = file_match_flags(&diff, terms)
        .iter()
        .filter(|(f, c)| *f || *c)
        .count();
    Some((n, matched_files))
}

/// Every term must match somewhere in the commit (message/author/oid,
/// changed filename, or added/removed patch line).
#[allow(dead_code)]
fn commit_matches_query(
    repo: &Repository,
    commit: &git2::Commit,
    terms: &[SearchTerm],
    diff_opts: &mut git2::DiffOptions,
) -> bool {
    commit_match_details(repo, commit, terms, diff_opts).is_some()
}

/// What a commit changed: the files it touched, from diffing its first
/// parent against the commit tree (no parent = initial commit, everything
/// is added). Same shape as `git_stash_show` so the history view reuses
/// the stash file list UI.
///
/// With `query`, each file is flagged with `matched` / `match_kind`
/// ("filename", "content", "both") against the same smart-search terms
/// used by `git_log`, so the UI can pin matching files to the top and
/// show "N of M files match". Without a query every file is `matched`.
#[tauri::command]
pub async fn git_commit_show(
    path: String,
    oid: String,
    query: Option<String>,
) -> Result<Vec<GitStashFile>, String> {
    off_main_thread(move || git_commit_show_blocking(path, oid, query)).await
}

pub fn git_commit_show_blocking(
    path: String,
    oid: String,
    query: Option<String>,
) -> Result<Vec<GitStashFile>, String> {
    let repo = open_repo(&path)?;
    let oid = oid
        .parse::<git2::Oid>()
        .map_err(|_| format!("'{oid}' is not a valid commit id"))?;
    let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
    // No context lines: only added/removed lines are ever scanned, and
    // stats (added/removed counts) don't need context either.
    let mut show_opts = git2::DiffOptions::new();
    show_opts.context_lines(0);
    show_opts.interhunk_lines(0);
    let diff = commit_diff(&repo, &commit, &mut show_opts).map_err(|e| e.to_string())?;
    let stats = diff_line_stats(&diff);
    let terms = query
        .as_deref()
        .map(normalize_query)
        .filter(|t| !t.is_empty());
    let n = diff.deltas().len();
    // Per file: (filename hit, content hit), the same rule as the badge.
    let flags = terms.as_ref().map(|terms| file_match_flags(&diff, terms));
    let mut out = Vec::new();
    for i in 0..n {
        let Some(rel) = delta_path(&diff, i) else {
            continue;
        };
        let delta = match diff.get_delta(i) {
            Some(d) => d,
            None => continue,
        };
        let (additions, deletions) = stats.get(&rel).copied().unwrap_or((0, 0));
        let (matched, match_kind) = match &flags {
            None => (true, None),
            Some(flags) => {
                let (file_hit, content_hit) = flags[i];
                let kind = match (file_hit, content_hit) {
                    (true, true) => Some("both".to_string()),
                    (true, false) => Some("filename".to_string()),
                    (false, true) => Some("content".to_string()),
                    (false, false) => None,
                };
                (file_hit || content_hit, kind)
            }
        };
        out.push(GitStashFile {
            path: rel,
            status: delta_status(delta.status()).to_string(),
            additions,
            deletions,
            matched,
            match_kind,
        });
    }
    // Matched files first (when filtering), then by path — so the answer
    // to "which file contains it" is at the top even with 100 files.
    out.sort_by(|a, b| (b.matched.cmp(&a.matched)).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

/// One file's content at a commit plus the version it changed from, in a
/// single call: enough to open a read-only editor tab with change marks.
/// `content`: the file at the commit (None: deleted in the commit);
/// `base`: the version at the first parent (None: new in the commit, or
/// the initial commit). Same shape as the stash preview.
#[tauri::command]
pub async fn git_commit_file_preview(
    path: String,
    oid: String,
    file: String,
) -> Result<GitStashFilePreview, String> {
    off_main_thread(move || git_commit_file_preview_blocking(path, oid, file)).await
}

pub fn git_commit_file_preview_blocking(
    path: String,
    oid: String,
    file: String,
) -> Result<GitStashFilePreview, String> {
    let repo = open_repo(&path)?;
    let rel = std::path::Path::new(&file);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("{file}: not a path inside the repository"));
    }
    let oid = oid
        .parse::<git2::Oid>()
        .map_err(|_| format!("'{oid}' is not a valid commit id"))?;
    let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
    let tree = commit.tree().map_err(|e| e.to_string())?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let content = read_tree_blob(&repo, Some(&tree), rel)?
        .map(|id| read_blob_text(&repo, id, &file))
        .transpose()?;
    let mut base_id = read_tree_blob(&repo, parent_tree.as_ref(), rel)?;
    // Not in the parent under this name: moved (or copied) there by the
    // commit, its change is against where it was. As a new file, every
    // line of a plain rename showed as added.
    if base_id.is_none() {
        if let Some(old) = renamed_from(&repo, &commit, rel) {
            base_id = read_tree_blob(&repo, parent_tree.as_ref(), &old)?;
        }
    }
    let base = base_id
        .map(|id| read_blob_text(&repo, id, &file))
        .transpose()?;
    Ok(GitStashFilePreview { content, base })
}

/// Where a file of `commit` was in its parent, when the commit moved or
/// copied it there (git's rename detection, as the commit's file list).
fn renamed_from(
    repo: &Repository,
    commit: &git2::Commit,
    path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let diff = commit_diff(repo, commit, &mut git2::DiffOptions::new()).ok()?;
    let delta = diff.deltas().find(|d| {
        matches!(d.status(), git2::Delta::Renamed | git2::Delta::Copied)
            && d.new_file().path() == Some(path)
    })?;
    delta.old_file().path().map(std::path::Path::to_path_buf)
}

// ---------------------------------------------------------------------------
// One file: its earlier version, and reverting to it
// ---------------------------------------------------------------------------

/// The version a revert goes back to: the staged one when the file has
/// unstaged changes (what `git restore` restores), else the last commit's.
/// None: the file is new (untracked, or added and never committed).
fn base_blob<'r>(repo: &'r Repository, file: &str) -> Result<Option<git2::Blob<'r>>, String> {
    let status = repo
        .status_file(std::path::Path::new(file))
        .map_err(|e| e.to_string())?;
    let index = repo.index().map_err(|e| e.to_string())?;
    let from_index =
        || -> Option<git2::Oid> { index.get_path(std::path::Path::new(file), 0).map(|e| e.id) };
    let from_head = || -> Option<git2::Oid> {
        let tree = repo.head().ok()?.peel_to_tree().ok()?;
        tree.get_path(std::path::Path::new(file))
            .ok()
            .map(|e| e.id())
    };
    let unstaged =
        status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_TYPECHANGE);
    let oid = if unstaged {
        from_index().or_else(from_head)
    } else {
        from_head()
    };
    Ok(match oid {
        Some(oid) => Some(repo.find_blob(oid).map_err(|e| e.to_string())?),
        None => None,
    })
}

/// The file's earlier version (see base_blob), for the editor's change marks.
/// `file` is relative to the repository's working folder. None: new file.
#[tauri::command]
pub async fn git_base_content(path: String, file: String) -> Result<Option<String>, String> {
    off_main_thread(move || git_base_content_blocking(path, file)).await
}

pub fn git_base_content_blocking(path: String, file: String) -> Result<Option<String>, String> {
    let repo = open_repo(&path)?;
    let blob = base_blob(&repo, &file)?;
    let content = match blob {
        None => None,
        Some(blob) if blob.is_binary() => return Err(format!("{file}: binary file")),
        Some(blob) => Some(String::from_utf8_lossy(blob.content()).into_owned()),
    };
    Ok(content)
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RevertOutcome {
    /// The version before the revert is in the trash (as "<name> (before revert)").
    pub backed_up: bool,
    /// A new file was moved to the trash (there was nothing to go back to).
    pub removed: bool,
}

/// "name (before revert).ext", next to the file (so it's trashed on the
/// same drive, and restoring it from the trash can't overwrite anything).
fn backup_name(path: &std::path::Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem} (before revert).{}", ext.to_string_lossy()),
        None => format!("{stem} (before revert)"),
    };
    path.with_file_name(name)
}

/// Puts the file back as it was (see base_blob). The current version goes to
/// the trash first, so a revert can itself be undone; a new file is moved to
/// the trash, since there is no earlier version.
#[tauri::command]
pub async fn git_revert_file(path: String, file: String) -> Result<RevertOutcome, String> {
    off_main_thread(move || git_revert_file_blocking(path, file)).await
}

pub fn git_revert_file_blocking(path: String, file: String) -> Result<RevertOutcome, String> {
    let repo = open_repo(&path)?;
    let workdir = repo
        .workdir()
        .ok_or("the repository has no working folder")?
        .to_path_buf();
    inside_repo(&file)?;
    revert_in(&repo, &workdir, &file)
}

/// `rel`, a path the frontend sends, as one inside the repository: relative,
/// not empty, and never going up.
fn inside_repo(rel: &str) -> Result<&std::path::Path, String> {
    let path = std::path::Path::new(rel);
    if rel.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!("{rel}: not a path inside the repository"));
    }
    Ok(path)
}

/// Reverts one file of `repo` (see git_revert_file); `file` is relative to
/// `workdir` and checked already.
fn revert_in(
    repo: &Repository,
    workdir: &std::path::Path,
    file: &str,
) -> Result<RevertOutcome, String> {
    let rel = std::path::Path::new(file);
    let abs = workdir.join(rel);
    let status = repo.status_file(rel).map_err(|e| e.to_string())?;
    if status.is_conflicted() {
        return Err(format!("{file} has merge conflicts: resolve them first"));
    }

    // New: nothing to go back to. Out of the index, into the trash.
    if base_blob(repo, file)?.is_none() {
        if status.is_index_new() {
            let mut index = repo.index().map_err(|e| e.to_string())?;
            index.remove_path(rel).map_err(|e| e.to_string())?;
            index.write().map_err(|e| e.to_string())?;
        }
        if abs.exists() {
            crate::filesystem::to_trash(&abs)
                .map_err(|e| format!("{file}: couldn't move it to the trash: {e}"))?;
        }
        return Ok(RevertOutcome {
            backed_up: false,
            removed: true,
        });
    }

    // The current version to the trash, under a name of its own.
    let mut backed_up = false;
    if abs.is_file() {
        let copy = backup_name(&abs);
        std::fs::copy(&abs, &copy).map_err(|e| format!("{file}: couldn't keep a copy: {e}"))?;
        match crate::filesystem::to_trash(&copy) {
            Ok(()) => backed_up = true,
            Err(e) => {
                let _ = std::fs::remove_file(&copy);
                return Err(format!(
                    "{file}: couldn't put a copy of your version in the trash ({e}), so nothing was reverted"
                ));
            }
        }
    }

    let unstaged =
        status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_TYPECHANGE);
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().path(rel);
    if unstaged {
        // Back to the staged version.
        repo.checkout_index(None, Some(&mut checkout))
            .map_err(|e| e.to_string())?;
    } else {
        // Only staged changes: back to the last commit, index included.
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| e.to_string())?;
        repo.reset_default(Some(head.as_object()), [rel])
            .map_err(|e| e.to_string())?;
        repo.checkout_head(Some(&mut checkout))
            .map_err(|e| e.to_string())?;
    }
    Ok(RevertOutcome {
        backed_up,
        removed: false,
    })
}

/// What reverting a folder did. Paths are relative to the working folder
/// and `/`-separated, as the status lists them.
#[derive(Debug, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FolderRevertOutcome {
    /// Put back, or moved to the trash when new (see RevertOutcome).
    pub reverted: Vec<String>,
    /// Left as they are: conflicted, or renamed (their rows offer no revert
    /// either).
    pub skipped: Vec<String>,
    /// Left as they are because their revert failed, with why.
    pub failed: Vec<FailedRevert>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FailedRevert {
    pub path: String,
    pub reason: String,
}

/// Reverts every changed file under `folder` (relative to the working
/// folder), each as git_revert_file does. Conflicted and renamed files are
/// left as they are, as is a file whose revert fails; the rest go on, and
/// the outcome lists each.
#[tauri::command]
pub async fn git_revert_folder(
    path: String,
    folder: String,
) -> Result<FolderRevertOutcome, String> {
    // Isolated chats on the project wait while its files are rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_revert_folder_blocking(path, folder)).await
}

pub fn git_revert_folder_blocking(
    path: String,
    folder: String,
) -> Result<FolderRevertOutcome, String> {
    let repo = open_repo(&path)?;
    let workdir = repo
        .workdir()
        .ok_or("the repository has no working folder")?
        .to_path_buf();
    let folder = folder.trim_end_matches('/');
    inside_repo(folder)?;
    let prefix = format!("{folder}/");

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;
    let mut outcome = FolderRevertOutcome::default();
    for entry in statuses.iter() {
        let s = entry.status();
        if s.is_ignored() {
            continue;
        }
        let file = String::from_utf8_lossy(entry.path_bytes()).into_owned();
        if !file.starts_with(&prefix) {
            continue;
        }
        if s.is_conflicted() || s.is_index_renamed() || s.is_wt_renamed() {
            outcome.skipped.push(file);
            continue;
        }
        match revert_in(&repo, &workdir, &file) {
            Ok(_) => outcome.reverted.push(file),
            Err(e) => {
                // The reasons name the file; here its row does.
                let reason = e
                    .strip_prefix(&format!("{file}: "))
                    .map_or(e.clone(), str::to_string);
                outcome.failed.push(FailedRevert { path: file, reason });
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod revert_tests {
    use super::*;
    use std::fs;

    fn repo_with_commit() -> (PathBuf, Repository) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-revert-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let repo = Repository::init(&dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    fn path(dir: &std::path::Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn unstaged_change_goes_back_to_the_staged_version() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "src/a.rs".into())
                .unwrap()
                .as_deref(),
            Some("fn a() {}\n")
        );
        let out = git_revert_file_blocking(path(&dir), "src/a.rs".into()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert!(out.backed_up && !out.removed);
        assert!(
            !dir.join("src/a (before revert).rs").exists(),
            "the copy went to the trash"
        );
    }

    #[test]
    fn deleted_file_comes_back() {
        let (dir, _repo) = repo_with_commit();
        fs::remove_file(dir.join("b.txt")).unwrap();
        let out = git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert!(!out.backed_up, "nothing to back up");
    }

    #[test]
    fn staged_only_change_goes_back_to_the_last_commit() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "b.txt".into())
                .unwrap()
                .as_deref(),
            Some("b\n")
        );
        git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        let st = git_status_blocking(path(&dir)).unwrap();
        assert!(
            st.files.iter().all(|f| f.path != "b.txt"),
            "clean again: {:?}",
            st.files
        );
    }

    #[test]
    fn staged_and_unstaged_goes_back_to_the_staged_version() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        fs::write(dir.join("b.txt"), "staged\nand more\n").unwrap();
        git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "staged\n");
    }

    #[test]
    fn new_files_go_to_the_trash() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("new.txt"), "x").unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "new.txt".into()).unwrap(),
            None
        );
        let out = git_revert_file_blocking(path(&dir), "new.txt".into()).unwrap();
        assert!(out.removed && !dir.join("new.txt").exists());
        // Added to the index but never committed: same, and out of the index.
        fs::write(dir.join("added.txt"), "y").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("added.txt")).unwrap();
        index.write().unwrap();
        git_revert_file_blocking(path(&dir), "added.txt".into()).unwrap();
        assert!(!dir.join("added.txt").exists());
        assert!(git_status_blocking(path(&dir)).unwrap().files.is_empty());
    }

    #[test]
    fn refuses_paths_outside_the_repository() {
        let (dir, _repo) = repo_with_commit();
        assert!(git_revert_file_blocking(path(&dir), "../etc/passwd".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "../etc".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "/".into()).is_err());
    }

    #[test]
    fn a_folder_reverts_every_file_below_it_and_leaves_what_it_cant() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        fs::write(dir.join("src/new.rs"), "new").unwrap();
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::write(dir.join("src/deep/n.txt"), "n").unwrap();
        fs::write(dir.join("b.txt"), "outside\n").unwrap();
        let out = git_revert_folder_blocking(path(&dir), "src/".into()).unwrap();
        assert_eq!(out.reverted, ["src/a.rs", "src/deep/n.txt", "src/new.rs"]);
        assert!(out.skipped.is_empty() && out.failed.is_empty(), "{out:?}");
        assert_eq!(
            fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert!(!dir.join("src/new.rs").exists() && !dir.join("src/deep/n.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.join("b.txt")).unwrap(),
            "outside\n",
            "a file outside the folder is left alone"
        );

        // A staged rename in the folder is left as it is, and listed under
        // the path git lists it by (the old one, as its row in the Git tab);
        // the folder's other change goes back.
        fs::rename(dir.join("src/a.rs"), dir.join("src/c.rs")).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new("src/a.rs")).unwrap();
        index.add_path(std::path::Path::new("src/c.rs")).unwrap();
        index.write().unwrap();
        fs::write(dir.join("src/d.rs"), "d").unwrap();
        let out = git_revert_folder_blocking(path(&dir), "src".into()).unwrap();
        assert_eq!(out.skipped, ["src/a.rs"]);
        assert_eq!(out.reverted, ["src/d.rs"]);
        assert!(dir.join("src/c.rs").exists() && !dir.join("src/d.rs").exists());
    }

    #[test]
    fn status_reports_workdir_and_staging() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "changed\n").unwrap();
        let st = git_status_blocking(path(&dir.join("src"))).unwrap();
        // The repository's folder, however it's spelled there (macOS's /var
        // is /private/var; Windows' temporary folder has a short 8.3 name).
        let canonical = |p: &std::path::Path| fs::canonicalize(p).unwrap();
        assert_eq!(
            canonical(std::path::Path::new(&st.workdir)),
            canonical(&dir)
        );
        assert!(!st.workdir.ends_with(['/', '\\']), "{}", st.workdir);
        let b = st.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.unstaged && !b.staged);
    }

    fn stash_repo() -> PathBuf {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() { stashed }\n").unwrap();
        fs::write(dir.join("new.txt"), "untracked stash me\n").unwrap();
        git_stash_push_blocking(path(&dir), Some("my stash".into())).unwrap();
        dir
    }

    #[test]
    fn stash_show_lists_tracked_and_untracked_files() {
        let dir = stash_repo();
        let files = git_stash_show_blocking(path(&dir), 0).unwrap();
        let a = files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(a.status, "M");
        let n = files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(n.status, "A");
        assert!(
            git_status_blocking(path(&dir)).unwrap().files.is_empty(),
            "tree is clean after stash"
        );
    }

    #[test]
    fn stash_file_content_returns_stashed_versions() {
        let dir = stash_repo();
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "src/a.rs".into())
                .unwrap()
                .as_deref(),
            Some("fn a() { stashed }\n")
        );
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "new.txt".into())
                .unwrap()
                .as_deref(),
            Some("untracked stash me\n")
        );
        // A file untouched by the stash still resolves from the stash commit.
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "b.txt".into())
                .unwrap()
                .as_deref(),
            Some("b\n")
        );
    }

    #[test]
    fn stash_file_content_deleted_returns_none_and_bad_index_errors() {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        git_stash_push_blocking(path(&dir), None).unwrap();
        let files = git_stash_show_blocking(path(&dir), 0).unwrap();
        assert!(
            files.iter().any(|f| f.path == "b.txt" && f.status == "D"),
            "{files:?}"
        );
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "b.txt".into()).unwrap(),
            None
        );
        assert!(git_stash_show_blocking(path(&dir), 42).is_err());
        assert!(git_stash_file_content_blocking(path(&dir), 0, "../etc/passwd".into()).is_err());
    }

    #[test]
    fn stash_file_preview_returns_content_and_base() {
        let dir = stash_repo();
        // Tracked modification: stashed content plus the base it changed from.
        let p = git_stash_file_preview_blocking(path(&dir), 0, "src/a.rs".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("fn a() { stashed }\n"));
        assert_eq!(p.base.as_deref(), Some("fn a() {}\n"));
        // Untracked file: new in the stash, so no base.
        let p = git_stash_file_preview_blocking(path(&dir), 0, "new.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("untracked stash me\n"));
        assert_eq!(p.base, None);
        // Bad index and outside paths error.
        assert!(git_stash_file_preview_blocking(path(&dir), 42, "src/a.rs".into()).is_err());
        assert!(git_stash_file_preview_blocking(path(&dir), 0, "../etc/passwd".into()).is_err());
    }

    /// Second commit on top of the fixture: modifies a.rs, deletes b.txt,
    /// adds c.txt. Returns the new commit's full oid.
    fn second_commit(dir: &std::path::Path, repo: &Repository) -> String {
        fs::write(dir.join("src/a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        fs::write(dir.join("c.txt"), "c\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index
            .remove_path(std::path::Path::new("b.txt"))
            .unwrap_or(());
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&head])
            .unwrap();
        oid.to_string()
    }

    #[test]
    fn commit_show_lists_changed_files_with_stats() {
        let (dir, repo) = repo_with_commit();
        let oid = second_commit(&dir, &repo);
        let files = git_commit_show_blocking(path(&dir), oid.clone(), None).unwrap();
        let a = files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(a.status, "M");
        assert_eq!((a.additions, a.deletions), (1, 0));
        let b = files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(b.status, "D");
        let c = files.iter().find(|f| f.path == "c.txt").unwrap();
        assert_eq!(c.status, "A");
        assert_eq!((c.additions, c.deletions), (1, 0));
        // Bad oid and outside paths error.
        assert!(git_commit_show_blocking(path(&dir), "not-an-oid".into(), None).is_err());
        assert!(git_commit_show_blocking(
            path(&dir),
            "0000000000000000000000000000000000000000".into(),
            None
        )
        .is_err());
    }

    #[test]
    fn commit_file_preview_returns_content_and_parent_base() {
        let (dir, repo) = repo_with_commit();
        let oid = second_commit(&dir, &repo);
        // Modified: content at the commit plus the parent's version.
        let p =
            git_commit_file_preview_blocking(path(&dir), oid.clone(), "src/a.rs".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("fn a() {}\nfn b() {}\n"));
        assert_eq!(p.base.as_deref(), Some("fn a() {}\n"));
        // Deleted: no content, parent base present.
        let p = git_commit_file_preview_blocking(path(&dir), oid.clone(), "b.txt".into()).unwrap();
        assert_eq!(p.content, None);
        assert_eq!(p.base.as_deref(), Some("b\n"));
        // Added: content present, no base.
        let p = git_commit_file_preview_blocking(path(&dir), oid.clone(), "c.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("c\n"));
        assert_eq!(p.base, None);
        assert!(git_commit_file_preview_blocking(path(&dir), oid, "../etc/passwd".into()).is_err());
    }

    #[test]
    fn commit_file_preview_bases_a_renamed_file_on_its_old_path() {
        let (dir, repo) = repo_with_commit();
        let commit_all = |message: &str| {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.update_all(["*"].iter(), None).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
                .unwrap()
                .to_string()
        };
        let before: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.join("notes.txt"), &before).unwrap();
        commit_all("add notes");
        // Moved into docs/ with a line added.
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::remove_file(dir.join("notes.txt")).unwrap();
        fs::write(dir.join("docs/notes.txt"), format!("{before}more\n")).unwrap();
        let oid = commit_all("move notes");
        let p = git_commit_file_preview_blocking(path(&dir), oid, "docs/notes.txt".into()).unwrap();
        assert_eq!(p.content, Some(format!("{before}more\n")));
        // What it was before the move: only "more" is new.
        assert_eq!(p.base, Some(before));
    }

    #[test]
    fn commit_show_initial_commit_reports_everything_added() {
        let (dir, _repo) = repo_with_commit();
        let repo = Repository::open(&dir).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = head.id().to_string();
        let files = git_commit_show_blocking(path(&dir), oid.clone(), None).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.status == "A"));
        let p = git_commit_file_preview_blocking(path(&dir), oid, "b.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("b\n"));
        assert_eq!(p.base, None);
    }

    #[test]
    fn git_log_query_matches_message_filename_and_content() {
        let (dir, repo) = repo_with_commit();
        // Distinctive commit: message, filename, and symbol share "widget"
        // but are otherwise unrelated to the fixture's "init"/a.rs/b.txt.
        fs::write(dir.join("unique_widget.rs"), "fn unique_foobar_xyz() {}\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new("unique_widget.rs"))
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid2 = repo
            .commit(Some("HEAD"), &sig, &sig, "add widget", &tree, &[&head])
            .unwrap()
            .to_string();
        // Baseline: no query returns both commits, newest first.
        let all = git_log_blocking(path(&dir), Some(20), None, None).unwrap();
        assert_eq!(all.commits.len(), 2);
        assert_eq!(all.commits[0].oid, oid2);
        assert!(!all.has_more);
        // Message match.
        let by_msg = git_log_blocking(path(&dir), Some(20), Some("widget".into()), None).unwrap();
        assert_eq!(by_msg.commits.len(), 1);
        assert_eq!(by_msg.commits[0].oid, oid2);
        // Filename match: partial basename.
        let by_file =
            git_log_blocking(path(&dir), Some(20), Some("unique_widget".into()), None).unwrap();
        assert_eq!(by_file.commits.len(), 1);
        assert_eq!(by_file.commits[0].oid, oid2);
        // Content match: identifier and its partial form.
        let by_symbol =
            git_log_blocking(path(&dir), Some(20), Some("unique_foobar_xyz".into()), None).unwrap();
        assert_eq!(by_symbol.commits.len(), 1);
        assert_eq!(by_symbol.commits[0].oid, oid2);
        let by_partial =
            git_log_blocking(path(&dir), Some(20), Some("foobar".into()), None).unwrap();
        assert_eq!(by_partial.commits.len(), 1);
        assert_eq!(by_partial.commits[0].oid, oid2);
        // AND semantics: every term must match somewhere.
        let both = git_log_blocking(
            path(&dir),
            Some(20),
            Some("widget unique_foobar".into()),
            None,
        )
        .unwrap();
        assert_eq!(both.commits.len(), 1);
        let none = git_log_blocking(
            path(&dir),
            Some(20),
            Some("widget nosuchthingzzz".into()),
            None,
        )
        .unwrap();
        assert!(none.commits.is_empty());
        // Empty/blank query behaves like no query.
        let blank = git_log_blocking(path(&dir), Some(20), Some("   ".into()), None).unwrap();
        assert_eq!(blank.commits.len(), 2);
    }

    #[test]
    fn git_log_reports_file_counts_and_commit_show_flags_matches() {
        let (dir, repo) = repo_with_commit();
        let oid2 = second_commit(&dir, &repo);
        // No query: file counts present, no matched count.
        let all = git_log_blocking(path(&dir), Some(20), None, None).unwrap();
        let newest = all.commits.iter().find(|e| e.oid == oid2).unwrap();
        assert_eq!(newest.file_count, 3);
        assert_eq!(newest.matched_file_count, None);
        // Query matching one filename: matched count narrows to that file.
        let filtered = git_log_blocking(path(&dir), Some(20), Some("c.txt".into()), None).unwrap();
        let entry = filtered.commits.iter().find(|e| e.oid == oid2).unwrap();
        assert_eq!(entry.file_count, 3);
        assert_eq!(entry.matched_file_count, Some(1));
        // Per-file flags: only c.txt matches, sorted first.
        let files =
            git_commit_show_blocking(path(&dir), oid2.clone(), Some("c.txt".into())).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "c.txt");
        assert!(files[0].matched);
        assert_eq!(files[0].match_kind.as_deref(), Some("filename"));
        assert!(!files[1].matched && !files[2].matched);
        // No query: all files matched, no kinds.
        let plain = git_commit_show_blocking(path(&dir), oid2, None).unwrap();
        assert!(plain.iter().all(|f| f.matched));
        assert!(plain.iter().all(|f| f.match_kind.is_none()));
    }

    #[test]
    fn git_log_pages_through_limit_and_offset() {
        let (dir, repo) = repo_with_commit();
        // Three more commits on top of the fixture (4 total).
        for i in 0..3 {
            fs::write(dir.join(format!("p{i}.txt")), format!("p{i}\n")).unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new(&format!("p{i}.txt")))
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("page {i}"),
                &tree,
                &[&head],
            )
            .unwrap();
        }
        // Page 1: newest 2, more remain.
        let first = git_log_blocking(path(&dir), Some(2), None, None).unwrap();
        assert_eq!(first.commits.len(), 2);
        assert!(first.has_more);
        assert_eq!(first.next_offset, 2);
        // Page 2 resumes where page 1 stopped: next 2, exhausted.
        let second = git_log_blocking(path(&dir), Some(2), None, Some(first.next_offset)).unwrap();
        assert_eq!(second.commits.len(), 2);
        assert!(!second.has_more);
        assert_ne!(first.commits[0].oid, second.commits[0].oid);
        // Query pages the same way: "page-1" is one compact term
        // (compact "page1" matches "page 1" with the space ignored,
        // but not "page 0"/"page 2" — and never a hex oid).
        let q1 = git_log_blocking(path(&dir), Some(20), Some("page-1".into()), None).unwrap();
        assert_eq!(q1.commits.len(), 1);
        let q2 = git_log_blocking(
            path(&dir),
            Some(20),
            Some("page-1".into()),
            Some(q1.next_offset),
        )
        .unwrap();
        assert!(q2.commits.is_empty());
    }

    #[test]
    fn git_log_single_char_skips_content_scan() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("zz_top.txt"), "qqqqqq\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("zz_top.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add zz", &tree, &[&head])
            .unwrap();
        // "q" appears only in content: single-char terms match
        // metadata/filenames, so this must NOT match.
        let page = git_log_blocking(path(&dir), Some(20), Some("q".into()), None).unwrap();
        assert!(page.commits.is_empty());
        // The filename itself still matches.
        let by_name = git_log_blocking(path(&dir), Some(20), Some("zz_top".into()), None).unwrap();
        assert_eq!(by_name.commits.len(), 1);
    }

    #[test]
    fn compact_contains_matches_compacted_forms() {
        assert!(compact_contains("get_user-name", "getusername"));
        assert!(compact_contains("fn getUserName() {", "getusername"));
        assert!(!compact_contains("fn other() {", "getusername"));
        assert!(!compact_contains("abc", ""));
    }

    /// Stage everything (deletions included) and commit it on HEAD.
    fn commit_all(repo: &Repository, message: &str) -> String {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"].iter(), None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    fn found(dir: &std::path::Path, query: &str) -> Vec<String> {
        git_log_blocking(path(dir), Some(20), Some(query.into()), None)
            .unwrap()
            .commits
            .into_iter()
            .map(|c| c.summary)
            .collect()
    }

    #[test]
    fn git_log_searches_the_whole_message() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("notes.txt"), "plain\n").unwrap();
        commit_all(&repo, "Add notes\n\nMentions the frobnicator in the body.");
        assert_eq!(found(&dir, "frobnicator"), ["Add notes"]);
    }

    #[test]
    fn symbol_terms_dont_match_on_one_leftover_character() {
        let (dir, repo) = repo_with_commit();
        // The fixture's `src/a.rs` has a "c" in its path; "c++" compacts to
        // "c" and used to match it (and nearly every other commit).
        assert!(found(&dir, "c++").is_empty());
        assert!(found(&dir, "c#").is_empty());
        // A real "c++" in the content still matches by its raw form.
        fs::write(dir.join("README.md"), "Built with c++ and care.\n").unwrap();
        commit_all(&repo, "readme");
        assert_eq!(found(&dir, "c++"), ["readme"]);
        // Two compact characters are enough for the separator-blind form.
        fs::write(dir.join("x.txt"), "set a_b here\n").unwrap();
        commit_all(&repo, "x");
        assert_eq!(found(&dir, "a-b"), ["x"]);
    }

    #[test]
    fn the_no_newline_marker_isnt_content() {
        let (dir, repo) = repo_with_commit();
        // No trailing newline: git adds a "\\ No newline at end of file" line.
        fs::write(dir.join("notes.txt"), "plain text").unwrap();
        commit_all(&repo, "Add notes");
        assert!(found(&dir, "newline").is_empty());
        assert_eq!(found(&dir, "plain"), ["Add notes"]);
    }

    #[test]
    fn the_badge_counts_files_like_the_expanded_list() {
        let (dir, repo) = repo_with_commit();
        for n in ["one", "two", "three"] {
            fs::write(
                dir.join(format!("w_{n}.rs")),
                format!("fn widget_{n}() {{}}\n"),
            )
            .unwrap();
        }
        // The message alone satisfies the query; the files match by content.
        let oid = commit_all(&repo, "widget update");
        let page = git_log_blocking(path(&dir), Some(20), Some("widget".into()), None).unwrap();
        let entry = page.commits.iter().find(|c| c.oid == oid).unwrap();
        let shown = git_commit_show_blocking(path(&dir), oid, Some("widget".into())).unwrap();
        let matched = shown.iter().filter(|f| f.matched).count();
        assert_eq!(matched, 3);
        assert_eq!(
            entry.matched_file_count,
            Some(matched),
            "badge and expanded list agree"
        );
    }

    #[test]
    fn a_renamed_file_isnt_all_new_content() {
        let (dir, repo) = repo_with_commit();
        fs::write(
            dir.join("old_name.rs"),
            "fn renamed_marker_xyz() {}\nfn other() {}\nfn third() {}\n",
        )
        .unwrap();
        commit_all(&repo, "add file");
        fs::rename(dir.join("old_name.rs"), dir.join("new_name.rs")).unwrap();
        let moved = commit_all(&repo, "move file");
        // Only the commit that wrote the line matches its content.
        assert_eq!(found(&dir, "renamed_marker_xyz"), ["add file"]);
        let files = git_commit_show_blocking(path(&dir), moved, None).unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(
            (files[0].path.as_str(), files[0].status.as_str()),
            ("new_name.rs", "R")
        );
    }

    #[test]
    fn a_superseded_search_stops_scanning() {
        let (dir, _repo) = repo_with_commit();
        let page = git_log_scan(
            path(&dir),
            Some(20),
            Some("init".into()),
            None,
            None,
            &|| true,
        )
        .unwrap();
        assert!(page.commits.is_empty() && page.scanned == 0);
        assert!(page.has_more, "stopped, not exhausted");
        let page = git_log_scan(
            path(&dir),
            Some(20),
            Some("init".into()),
            None,
            None,
            &|| false,
        )
        .unwrap();
        assert_eq!(page.commits.len(), 1);
    }

    fn page_from(dir: &std::path::Path, offset: Option<usize>, tip: Option<&str>) -> GitLogPage {
        git_log_scan(path(dir), Some(2), None, offset, tip, &|| false).unwrap()
    }

    #[test]
    fn a_commit_made_while_paging_doesnt_repeat_or_skip_one() {
        let (dir, repo) = repo_with_commit();
        for i in 0..3 {
            fs::write(dir.join(format!("p{i}.txt")), format!("p{i}\n")).unwrap();
            commit_all(&repo, &format!("page {i}"));
        }
        // 4 commits: page 2 of 2 each.
        let first = page_from(&dir, None, None);
        let tip = first.tip.clone().expect("the walk's start");
        assert_eq!(
            tip,
            repo.head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        );
        // Someone commits from a terminal before "load more".
        fs::write(dir.join("late.txt"), "late\n").unwrap();
        commit_all(&repo, "made meanwhile");
        let second = page_from(&dir, Some(first.next_offset), Some(&tip));
        assert_eq!(
            second.tip.as_deref(),
            Some(tip.as_str()),
            "still the same session"
        );
        let summaries: Vec<&str> = first
            .commits
            .iter()
            .chain(&second.commits)
            .map(|c| c.summary.as_str())
            .collect();
        // Every commit of the session once, in order; the new one isn't in it.
        assert_eq!(summaries, ["page 2", "page 1", "page 0", "init"]);
        assert!(!second.has_more);
        // Counting from HEAD instead (what paging did before) repeats one.
        let shifted = page_from(&dir, Some(first.next_offset), None);
        assert_eq!(
            shifted.commits[0].summary, "page 1",
            "the old behaviour: page 1 again"
        );
        // A fresh session sees the new commit.
        assert_eq!(
            page_from(&dir, None, None).commits[0].summary,
            "made meanwhile"
        );
    }

    #[test]
    fn a_tip_that_no_longer_exists_starts_over_from_head() {
        let (dir, _repo) = repo_with_commit();
        let gone = "0123456789abcdef0123456789abcdef01234567";
        let page = page_from(&dir, Some(5), Some(gone));
        // Fell back to HEAD, from the start: the old offset meant nothing here.
        assert_ne!(page.tip.as_deref(), Some(gone));
        assert_eq!(page.commits.len(), 1);
        assert_eq!(page.commits[0].summary, "init");
    }
}

#[cfg(test)]
mod branch_tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-branch-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(dir: &Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    fn commit_all(repo: &Repository, message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"].iter(), None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    /// A repository on `main` with one commit (f.txt = "one").
    fn repo_on_main(tag: &str) -> (PathBuf, Repository) {
        let dir = temp_dir(tag);
        let repo = Repository::init(&dir).unwrap();
        {
            let mut c = repo.config().unwrap();
            c.set_str("user.name", "t").unwrap();
            c.set_str("user.email", "t@t").unwrap();
        }
        fs::write(dir.join("f.txt"), "one\n").unwrap();
        commit_all(&repo, "start");
        if repo.head().unwrap().shorthand().ok() != Some("main") {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("main", &head, true).unwrap();
            repo.set_head("refs/heads/main").unwrap();
        }
        (dir, repo)
    }

    fn head_branch(repo: &Repository) -> Option<String> {
        let head = repo.head().unwrap();
        head.is_branch()
            .then(|| head.shorthand().unwrap().to_string())
    }

    fn tip(repo: &Repository, refname: &str) -> git2::Oid {
        repo.refname_to_id(refname).unwrap()
    }

    fn upstream_of(repo: &Repository, branch: &str) -> Option<String> {
        repo.find_branch(branch, BranchType::Local)
            .unwrap()
            .upstream()
            .ok()
            .map(|u| u.name().unwrap().unwrap().to_string())
    }

    /// `origin` (a bare repository) with `main` and `feature` pushed to
    /// it and fetched back, so `origin/main` and `origin/feature` are
    /// known; the local `feature` is deleted again. Both at the start commit.
    fn repo_with_origin(tag: &str) -> (PathBuf, Repository, PathBuf) {
        let (dir, repo) = repo_on_main(tag);
        let remote = temp_dir(&format!("{tag}-origin"));
        Repository::init_bare(&remote).unwrap();
        repo.remote("origin", &p(&remote)).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        git_create_branch_blocking(p(&dir), "feature".into(), true, None).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        git_delete_branch_blocking(p(&dir), "feature".into()).unwrap();
        git_fetch_blocking(p(&dir)).unwrap();
        assert!(repo
            .find_branch("origin/feature", BranchType::Remote)
            .is_ok());
        (dir, repo, remote)
    }

    #[test]
    fn branch_names_follow_gits_rules() {
        for ok in ["feature", "feature/x", "v1.2", "a-b_c", "ünïcode"] {
            assert_eq!(validate_branch_name(ok).unwrap(), ok, "{ok}");
        }
        for bad in [
            "", " ", "HEAD", "@", "-x", "/x", "x/", "x.", "a..b", "a//b", "a@{1}", "a b", "a~b",
            "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b", ".hidden", "a/.b", "a.lock", "a.lock/b",
            "a\x07b",
        ] {
            assert!(
                validate_branch_name(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_branch_from_a_local_branch_starts_at_its_commit_without_tracking() {
        let (dir, repo) = repo_on_main("from-local");
        let start = tip(&repo, "refs/heads/main");
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two");
        git_create_branch_blocking(
            p(&dir),
            "old".into(),
            false,
            Some("refs/heads/main~1".into()),
        )
        .unwrap();
        assert_eq!(tip(&repo, "refs/heads/old"), start);
        git_create_branch_blocking(p(&dir), "copy".into(), false, Some("old".into())).unwrap();
        assert_eq!(tip(&repo, "refs/heads/copy"), start);
        assert_eq!(upstream_of(&repo, "copy"), None);
        // Still on main, with its tree: nothing was checked out.
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "two\n");
    }

    #[test]
    fn a_branch_from_a_remote_branch_tracks_it_only_under_the_same_name() {
        let (dir, repo, _remote) = repo_with_origin("from-remote");
        let remote_tip = tip(&repo, "refs/remotes/origin/feature");

        git_create_branch_blocking(
            p(&dir),
            "feature".into(),
            false,
            Some("origin/feature".into()),
        )
        .unwrap();
        assert_eq!(tip(&repo, "refs/heads/feature"), remote_tip);
        assert_eq!(
            upstream_of(&repo, "feature").as_deref(),
            Some("origin/feature"),
            "`feature` from `origin/feature` pulls from and pushes to it"
        );

        git_create_branch_blocking(p(&dir), "mine".into(), false, Some("origin/feature".into()))
            .unwrap();
        assert_eq!(tip(&repo, "refs/heads/mine"), remote_tip);
        assert_eq!(
            upstream_of(&repo, "mine"),
            None,
            "the panel pushes `mine` to origin/mine; tracking origin/feature would keep it 'ahead' for ever"
        );
        // Its first push sets the upstream, as for any new branch.
        git_checkout_blocking(p(&dir), "mine".into()).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        assert_eq!(upstream_of(&repo, "mine").as_deref(), Some("origin/mine"));
    }

    /// A new branch has no remote branch until its first push, which makes
    /// one and tracks it: the panel asks to publish it then (GitPanel.tsx).
    #[test]
    fn a_new_branch_has_no_upstream_until_its_first_push() {
        let (dir, repo, remote) = repo_with_origin("publish");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.push_remote.as_deref(), Some("origin"));

        git_create_branch_blocking(p(&dir), "topic".into(), true, None).unwrap();
        fs::write(dir.join("f.txt"), "topic\n").unwrap();
        commit_all(&repo, "topic");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.branch, "topic");
        assert_eq!(status.upstream, None);
        assert_eq!(status.push_remote.as_deref(), Some("origin"));
        assert_eq!(status.ahead, 0, "nothing to be ahead of");

        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/topic"));
        assert_eq!((status.ahead, status.behind), (0, 0));
        let origin = Repository::open_bare(&remote).unwrap();
        assert_eq!(
            tip(&origin, "refs/heads/topic"),
            tip(&repo, "refs/heads/topic")
        );

        // No remote: nowhere to publish it.
        repo.remote_delete("origin").unwrap();
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!((status.upstream, status.push_remote), (None, None));
    }

    #[test]
    fn a_branch_in_the_list_beats_a_tag_of_the_same_name() {
        let (dir, repo) = repo_on_main("branch-vs-tag");
        let first = tip(&repo, "refs/heads/main");
        repo.branch("rel", &repo.find_commit(first).unwrap(), false)
            .unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        let second = commit_all(&repo, "two");
        // `git rev-parse rel` would pick the tag (refs/tags before refs/heads).
        repo.tag_lightweight("rel", &repo.find_object(second, None).unwrap(), false)
            .unwrap();
        git_create_branch_blocking(p(&dir), "from-rel".into(), false, Some("rel".into())).unwrap();
        assert_eq!(tip(&repo, "refs/heads/from-rel"), first);
    }

    #[test]
    fn create_and_switch_carries_uncommitted_work_over() {
        let (dir, repo) = repo_on_main("carry");
        fs::write(dir.join("f.txt"), "edited\n").unwrap();
        git_create_branch_blocking(p(&dir), "work".into(), true, Some("main".into())).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("work"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "edited\n");
    }

    #[test]
    fn a_switch_the_working_folder_refuses_leaves_no_branch_behind() {
        let (dir, repo) = repo_on_main("refused");
        git_create_branch_blocking(p(&dir), "other".into(), true, None).unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two on other");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        // An edit to the file the branches differ in: switching would lose it.
        fs::write(dir.join("f.txt"), "local edit\n").unwrap();

        let err = git_create_branch_blocking(p(&dir), "new".into(), true, Some("other".into()))
            .unwrap_err();
        assert!(err.contains("Checkout failed"), "{err}");
        assert!(
            repo.find_branch("new", BranchType::Local).is_err(),
            "the branch that couldn't be switched to isn't left behind"
        );
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
        assert_eq!(
            fs::read_to_string(dir.join("f.txt")).unwrap(),
            "local edit\n"
        );
        // The same name works once the edit is out of the way.
        fs::write(dir.join("f.txt"), "one\n").unwrap();
        git_create_branch_blocking(p(&dir), "new".into(), true, Some("other".into())).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("new"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "two\n");
    }

    #[test]
    fn a_missing_start_point_is_an_error_and_creates_nothing() {
        let (dir, repo) = repo_on_main("missing");
        let err =
            git_create_branch_blocking(p(&dir), "x".into(), true, Some("nope".into())).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(repo.find_branch("x", BranchType::Local).is_err());
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
    }

    #[test]
    fn checking_out_a_remote_branch_creates_a_tracking_branch() {
        let (dir, repo, _remote) = repo_with_origin("remote-checkout");
        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("feature"));
        assert_eq!(
            upstream_of(&repo, "feature").as_deref(),
            Some("origin/feature")
        );
        assert_eq!(
            tip(&repo, "refs/heads/feature"),
            tip(&repo, "refs/remotes/origin/feature")
        );
    }

    #[test]
    fn checking_out_a_remote_branch_switches_to_the_local_one_it_already_has() {
        let (dir, repo, _remote) = repo_with_origin("remote-vs-local");
        // A local `feature`, one commit past origin/feature.
        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        fs::write(dir.join("f.txt"), "local two\n").unwrap();
        let local_tip = commit_all(&repo, "two");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "one\n");

        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("feature"));
        assert_eq!(tip(&repo, "refs/heads/feature"), local_tip);
        // The working folder is at the local branch's commit, not the
        // remote's: nothing shows up as changed.
        assert_eq!(
            fs::read_to_string(dir.join("f.txt")).unwrap(),
            "local two\n"
        );
        assert!(git_status_blocking(p(&dir)).unwrap().files.is_empty());
    }

    #[test]
    fn checking_out_a_tag_detaches_head() {
        let (dir, repo) = repo_on_main("tag");
        let first = tip(&repo, "refs/heads/main");
        repo.tag_lightweight("v1", &repo.find_object(first, None).unwrap(), false)
            .unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two");
        git_checkout_blocking(p(&dir), "v1".into()).unwrap();
        let head = repo.head().unwrap();
        assert!(!head.is_branch(), "HEAD must not point at refs/tags/v1");
        assert!(repo.head_detached().unwrap());
        assert_eq!(head.target(), Some(first));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "one\n");
        assert!(git_status_blocking(p(&dir))
            .unwrap()
            .branch
            .starts_with("detached@"));
    }

    #[test]
    fn fetch_and_push_go_to_the_branches_own_remote() {
        let (dir, repo, origin) = repo_with_origin("two-remotes");
        let fork = temp_dir("two-remotes-fork");
        Repository::init_bare(&fork).unwrap();
        repo.remote("fork", &p(&fork)).unwrap();
        {
            let mut c = repo.config().unwrap();
            c.set_str("branch.main.remote", "fork").unwrap();
            c.set_str("branch.main.merge", "refs/heads/main").unwrap();
        }
        let before = tip(&repo, "refs/heads/main");
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        let after = commit_all(&repo, "two");

        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        let fork_tip = Repository::open_bare(&fork)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(fork_tip, after, "pushed to the remote the branch tracks");
        let origin_tip = Repository::open_bare(&origin)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(origin_tip, before, "origin wasn't touched");

        git_fetch_blocking(p(&dir)).unwrap();
        assert_eq!(tip(&repo, "refs/remotes/fork/main"), after);
        assert_eq!(tip(&repo, "refs/remotes/origin/main"), before);
        assert_eq!(compute_ahead_behind(&repo), Some((0, 0)));
    }
}

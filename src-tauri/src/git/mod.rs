// git/mod.rs
//
// Cross-platform Git backend for the IDE's GitPanel, built on `git2`
// (Rust bindings for libgit2) rather than shelling out to a `git` binary.
// This means it works even on machines without git installed, and behaves
// identically on Windows/macOS/Linux.
//
// What the panel does with a repository is a module each: the status is
// here, with what the others share; `remote` fetches and pushes, `branches`
// lists, switches, creates and deletes branches, `changes` commits, reverts
// and discards, `stash` shelves and shows, and `history` is the log and its
// search. Every command runs its work on a worker thread (util::blocking).

mod branches;
mod changes;
mod history;
mod remote;
mod stash;

pub use branches::*;
pub use changes::*;
pub use history::*;
pub use remote::*;
pub use stash::*;

use crate::util::{blocking, ErrText};
use git2::{Repository, Status, StatusOptions};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

// ---------------------------------------------------------------------------
// What the modules share
// ---------------------------------------------------------------------------

pub(crate) fn open_repo(path: &str) -> Result<Repository, String> {
    Repository::discover(path).map_err(|e| format!("Not a git repository: {e}"))
}

/// `rel`, a path the frontend sends, as one inside the repository: relative,
/// not empty, and never going up.
pub(crate) fn inside_repo(rel: &str) -> Result<&Path, String> {
    let path = Path::new(rel);
    if rel.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("{rel}: not a path inside the repository"));
    }
    Ok(path)
}

/// The repository's working folder.
pub(crate) fn workdir(repo: &Repository) -> Result<PathBuf, String> {
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| "the repository has no working folder".to_string())
}

/// The commit HEAD is at.
pub(crate) fn head_commit(repo: &Repository) -> Result<git2::Commit<'_>, String> {
    repo.head().and_then(|h| h.peel_to_commit()).err_text()
}

/// Who commits: what a commit, a stash or a merge needs. libgit2 reads
/// `user.name` and `user.email` each from the highest-priority config level
/// that sets it, so the repository's own `.git/config` comes before the
/// global (and XDG, system) config, key by key.
pub(crate) fn signature(repo: &Repository) -> Result<git2::Signature<'static>, String> {
    repo.signature()
        .map_err(|e| format!("No Git identity configured (user.name / user.email): {e}"))
}

/// The blob at `rel` in `tree`, when a file is there.
pub(crate) fn tree_blob(tree: Option<&git2::Tree<'_>>, rel: &Path) -> Option<git2::Oid> {
    let entry = tree?.get_path(rel).ok()?;
    (entry.kind() == Some(git2::ObjectType::Blob)).then(|| entry.id())
}

/// A blob as text, for a preview: not a binary, and not too big.
pub(crate) fn blob_text(repo: &Repository, id: git2::Oid, file: &str) -> Result<String, String> {
    let blob = repo.find_blob(id).err_text()?;
    if blob.is_binary() {
        return Err(format!("{file}: binary file"));
    }
    if blob.size() > 512 * 1024 {
        return Err(format!("{file}: too large to preview"));
    }
    Ok(String::from_utf8_lossy(blob.content()).into_owned())
}

/// Every change into the index, as `git add -A` does: new and modified
/// files staged, deleted ones dropped; .gitignore respected by both.
pub(crate) fn stage_all(index: &mut git2::Index) -> Result<(), String> {
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .err_text()?;
    index.update_all(["*"].iter(), None).err_text()?;
    index.write().err_text()
}

/// Every file git has an opinion on, untracked ones included.
pub(crate) fn status_options() -> StatusOptions {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);
    opts
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

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

/// Reduce a git2 status bitflag combination down to the single-character
/// code the frontend expects. Priority: conflicts > untracked > staged
/// (index) changes > working-tree changes.
fn classify_status(s: Status) -> &'static str {
    if s.is_conflicted() {
        "U"
    } else if s.is_wt_new() {
        "?"
    } else if s.is_index_new() {
        "A"
    } else if s.is_index_deleted() || s.is_wt_deleted() {
        "D"
    } else if s.is_index_renamed() || s.is_wt_renamed() {
        "R"
    } else {
        // Covers modified + typechange, staged or unstaged.
        "M"
    }
}

/// Per-file added/removed line counts for one diff, keyed by the
/// repo-relative `/`-separated path. Binary files report no hunks, so
/// they (and any patch that fails to materialize) come back as 0/0.
pub(crate) fn diff_line_stats(diff: &git2::Diff) -> HashMap<String, (usize, usize)> {
    (0..diff.deltas().len())
        .filter_map(|i| {
            let counts = git2::Patch::from_diff(diff, i)
                .ok()
                .flatten()
                .and_then(|patch| patch.line_stats().ok())
                .map(|(_, added, removed)| (added, removed))
                .unwrap_or((0, 0));
            Some((delta_path(diff, i)?, counts))
        })
        .collect()
}

/// The path a delta is listed under: the new side, or the old one for a
/// deletion.
pub(crate) fn delta_path(diff: &git2::Diff, i: usize) -> Option<String> {
    diff.get_delta(i).and_then(|d| {
        d.new_file()
            .path()
            .or_else(|| d.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Added/removed lines vs HEAD for every dirty file (`git diff --numstat`).
/// Merges the staged (HEAD→index) and unstaged (index→workdir) diffs so a
/// file changed in both places reports the total. Untracked files report
/// their line count as additions (when readable); anything that fails
/// (binary, missing, diff error) stays 0/0 rather than failing status.
fn workdir_line_stats(repo: &Repository) -> HashMap<String, (usize, usize)> {
    let mut merged: HashMap<String, (usize, usize)> = HashMap::new();
    // Files that are new, the only ones the fallback below may count: a
    // tracked file at 0/0 changed only its mode (or type), not its lines.
    let mut new_files = HashSet::new();
    let mut count = |diff: &git2::Diff| {
        for delta in diff.deltas() {
            if matches!(delta.status(), git2::Delta::Added | git2::Delta::Untracked) {
                if let Some(p) = delta.new_file().path() {
                    new_files.insert(p.to_string_lossy().into_owned());
                }
            }
        }
        for (path, (a, d)) in diff_line_stats(diff) {
            let total = merged.entry(path).or_insert((0, 0));
            total.0 += a;
            total.1 += d;
        }
    };
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    // Staged: HEAD → index.
    if let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), None, None) {
        count(&diff);
    }
    // Unstaged: index → workdir, with untracked content so new files get
    // real counts instead of 0/0.
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if let Ok(diff) = repo.diff_index_to_workdir(None, Some(&mut opts)) {
        count(&diff);
    }
    // Fallback for untracked files libgit2 didn't emit content for
    // (empty diff, binary check skipped): count lines on disk.
    let Some(workdir) = repo.workdir() else {
        return merged;
    };
    for path in new_files {
        if merged.get(&path) != Some(&(0, 0)) {
            continue;
        }
        if let Some(lines) = text_lines(&workdir.join(&path)) {
            merged.insert(path, (lines, 0));
        }
    }
    merged
}

/// How many lines the text file at `abs` has: none for a binary or a big
/// file, an empty one, or anything that isn't a file.
fn text_lines(abs: &Path) -> Option<usize> {
    if !abs.is_file() {
        return None;
    }
    let bytes = std::fs::read(abs).ok()?;
    if bytes.len() > 1024 * 1024 || bytes.contains(&0) {
        return None; // binary or too large: leave 0/0
    }
    let lines = bytes.iter().filter(|&&b| b == b'\n').count();
    // A trailing-newline-less last line still counts.
    let lines = if bytes.is_empty() || bytes.ends_with(b"\n") {
        lines
    } else {
        lines + 1
    };
    (lines > 0).then_some(lines)
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
        .find_branch(name, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
        .and_then(|u| u.name().ok().flatten().map(str::to_string));
    (upstream, push_remote_name(repo, name).ok())
}

pub(crate) fn compute_ahead_behind(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return Some((0, 0));
    }
    let local_oid = head.target()?;
    let branch_name = head.shorthand().ok()?;
    let branch = repo
        .find_branch(branch_name, git2::BranchType::Local)
        .ok()?;
    let upstream = branch.upstream().ok()?;
    let upstream_oid = upstream.get().target()?;
    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}

#[tauri::command]
pub async fn git_status(path: String) -> Result<GitStatus, String> {
    blocking(move || git_status_blocking(path)).await
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
    let statuses = repo.statuses(Some(&mut status_options())).err_text()?;
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

#[tauri::command]
pub async fn git_init(path: String) -> Result<(), String> {
    blocking(move || git_init_blocking(path)).await
}

pub fn git_init_blocking(path: String) -> Result<(), String> {
    Repository::init(&path).map_err(|e| format!("Failed to initialize repository: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;

// git/mod.rs
//
// Cross-platform Git backend for the IDE's GitPanel, built on `git2`
// (Rust bindings for libgit2) rather than shelling out to a `git` binary.
// This means it works even on machines without git installed, and behaves
// identically on Windows/macOS/Linux.
//
// What the panel does with a repository is a module each: the status is
// here, with what the others share; `remote` fetches and pushes, `branches`
// lists branches, says where HEAD is, switches, creates and deletes them,
// `changes` commits, reverts and discards, `stash` shelves and shows, and
// `history` is the log and its search. Every command runs its work on a
// worker thread (util::blocking). libgit2 runs no git hooks: `hooks` runs
// the repository's, where git would.

mod branches;
mod changes;
mod history;
mod hook_files;
pub(crate) mod hooks;
mod remote;
pub mod repos;
mod stash;

pub use branches::*;
pub use changes::*;
pub use history::*;
pub use hook_files::*;
pub use remote::*;
pub use repos::*;
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

/// `folder`'s path inside the repository's working folder, "/"-separated: ""
/// when the folder is the working folder (all of it), and when the two can't
/// be compared (a path that's gone). The repository is found upward from the
/// folder (`open_repo`), so a folder can be a package of a monorepo, or a
/// folder under a home folder kept in git (dotfiles): the panel's actions on
/// the whole tree (its list of changes, Commit's staging, Discard all) keep to
/// the window's folders, and never list, commit or throw away what's beside
/// them.
pub(crate) fn relative_folder(repo: &Repository, folder: &str) -> String {
    let (Some(workdir), Ok(folder)) = (repo.workdir(), Path::new(folder).canonicalize()) else {
        return String::new();
    };
    let Ok(workdir) = workdir.canonicalize() else {
        return String::new();
    };
    let Ok(rel) = folder.strip_prefix(&workdir) else {
        return String::new();
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// The scopes a command works in: those the frontend gave (the window's
/// folders in the repository, relative to its working folder, "" for all of
/// it), or the folder `path` is in — a project's one folder, as before (D10).
pub(crate) fn scopes_in(repo: &Repository, path: &str, scopes: Option<Vec<String>>) -> Vec<String> {
    match scopes {
        Some(given) if !given.is_empty() => given,
        _ => vec![relative_folder(repo, path)],
    }
}

/// The pathspec of `scope` ([`relative_folder`]): everything in it.
pub(crate) fn scope_pathspec(scope: &str) -> String {
    if scope.is_empty() {
        "*".to_string()
    } else {
        changes::literal_pathspec(Path::new(scope))
    }
}
/// Everything the working tree of `scope` ([`relative_folder`]) has into the
/// index, as `git add -A` would: new and modified files staged, deleted ones
/// dropped, .gitignore respected (the status leaves ignored files out). What's
/// staged already, with nothing more in the working tree, is left as it is (a
/// terminal's `git add`). A file renamed in a terminal is staged by the name
/// it has now — its status entry is named by the old one, which is gone from
/// disk — and what was changed or deleted after the rename with it. A folder
/// that is a repository of its own is never taken (D12): `add_all` would
/// refuse the whole scope for it.
pub(crate) fn stage_all(
    index: &mut git2::Index,
    repo: &Repository,
    scope: &str,
) -> Result<(), String> {
    let statuses = statuses_in(repo, &[scope.to_string()])?;
    stage_entries(index, repo, &statuses, None)?;
    index.write().err_text()
}

/// Stages what the working tree has, entry by entry (see [`stage_all`]).
/// `within`: the folder whose read this is (a folder row's own read), which
/// stops a row naming that folder from being read again.
fn stage_entries(
    index: &mut git2::Index,
    repo: &Repository,
    statuses: &git2::Statuses<'_>,
    within: Option<&Path>,
) -> Result<(), String> {
    let workdir = workdir(repo).ok();
    for entry in statuses.iter() {
        let status = entry.status();
        if status.is_ignored() || other_repository(repo, &entry) {
            continue;
        }
        // What the working tree has that the index doesn't. The entry's own
        // path is its head-to-index one: for a file renamed in a terminal
        // that's the OLD name, gone from disk, so staging by it would fail or
        // stage the wrong file. The working tree's side names the file as it
        // is now (a deleted one by the name the index has). An untracked
        // folder listed as one row has no working-tree delta at all: its own
        // path is the one the working tree has.
        let touches_worktree = status.intersects(
            Status::WT_NEW
                | Status::WT_MODIFIED
                | Status::WT_DELETED
                | Status::WT_RENAMED
                | Status::WT_TYPECHANGE,
        );
        if !touches_worktree {
            // Staged already, and nothing more in the working tree: the index
            // has it (a `git mv`'s old path isn't on disk to be added again).
            continue;
        }
        let (deleted, path) = match entry.index_to_workdir() {
            Some(delta) => {
                let deleted = delta.status() == git2::Delta::Deleted;
                let file = if deleted {
                    delta.old_file()
                } else {
                    delta.new_file()
                };
                let path = file
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(Path::to_path_buf);
                (deleted, path)
            }
            None => (status.is_wt_deleted(), entry.path().ok().map(PathBuf::from)),
        };
        let Some(path) = path else {
            continue;
        };
        let text = path.to_string_lossy();
        let rel = Path::new(text.trim_end_matches(['/', '\\']));
        if deleted {
            // Gone from the working tree: out of the index if it's in it.
            if index.get_path(rel, 0).is_some() {
                index.remove_path(rel).err_text()?;
            }
            continue;
        }
        match workdir
            .as_ref()
            .and_then(|workdir| std::fs::symlink_metadata(workdir.join(rel)).ok())
        {
            // A folder listed as one row: an untracked folder the read didn't
            // descend into, because something in the scope (elsewhere) is a
            // repository of its own. Its own files are read on their own, so
            // git's ignore rules still apply and they're not left out of the
            // commit; a folder of its own inside it is left out as ever.
            Some(meta) if meta.is_dir() => {
                // A submodule: what's recorded is the commit it has checked
                // out, which `add_path` writes as a gitlink, as `git add`
                // does. Its files aren't this repository's.
                if is_submodule(repo, rel) {
                    index.add_path(rel).err_text()?;
                    continue;
                }
                if within.is_some_and(|within| within == rel) {
                    continue; // the folder this read is of: read already
                }
                let inside = statuses_in_folder(repo, rel)?;
                stage_entries(index, repo, &inside, Some(rel))?;
            }
            // A file that's there: new, changed, or a link.
            Some(_) => index.add_path(rel).err_text()?,
            // Gone since the status was read: nothing to add.
            None => {}
        }
    }
    Ok(())
}

/// The changes in `folder` (relative to the working folder) alone: a read of
/// its own, so a folder the scope's read couldn't descend into can be, git's
/// ignore rules still applying. A repository of its own deeper inside it makes
/// this read fall back too, and its folder row again (see [`read_statuses`]),
/// so every level is read until one of them is a repository of its own. A
/// failure says which folder's files weren't staged, rather than leaving them
/// out of the commit silently.
fn statuses_in_folder<'r>(
    repo: &'r Repository,
    folder: &Path,
) -> Result<git2::Statuses<'r>, String> {
    let read = read_statuses(repo, |recurse| {
        let mut options = status_options();
        options.pathspec(scope_pathspec(&folder.to_string_lossy()));
        options.recurse_untracked_dirs(recurse);
        options
    });
    read.map_err(|err| format!("{}: its files can't be staged: {err}", folder.display()))
}

/// Every file git has an opinion on, untracked ones included.
pub(crate) fn status_options() -> StatusOptions {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);
    opts
}

/// [`status_options`] for the files in `scopes` ([`relative_folder`]) only:
/// git matches the pathspecs, so `app` doesn't take `app2/` (D10).
pub(crate) fn status_options_in_scopes(scopes: &[String]) -> StatusOptions {
    let mut opts = status_options();
    for scope in scopes {
        // "" is all of the working folder: no pathspec narrows it.
        if !scope.is_empty() {
            opts.pathspec(scope_pathspec(scope));
        }
    }
    opts
}

/// The changes in `scopes`, as the panel lists them. A repository of its own
/// inside an untracked folder is listed as that folder alone (one row), not
/// as its files; a read that can't walk such a folder at all is read again
/// without descending into untracked folders (see [`read_statuses`]).
pub(crate) fn statuses_in<'r>(
    repo: &'r Repository,
    scopes: &[String],
) -> Result<git2::Statuses<'r>, String> {
    read_statuses(repo, |recurse| {
        let mut options = status_options_in_scopes(scopes);
        options.recurse_untracked_dirs(recurse);
        options
    })
}

/// Reads the status with `options(recurse)`: descending into untracked folders,
/// or listing each one as a single row. A read that's refused — libgit2 builds
/// differ on a folder holding a repository of its own, and an unreadable one
/// can refuse too — is made again without descending, which always works: each
/// row is then read on its own (`statuses_in_folder`), so nothing is left out
/// of what the caller does with the changes (D12).
pub(crate) fn read_statuses<'r>(
    repo: &'r Repository,
    options: impl Fn(bool) -> StatusOptions,
) -> Result<git2::Statuses<'r>, String> {
    let mut full = options(true);
    match repo.statuses(Some(&mut full)) {
        Ok(statuses) => Ok(statuses),
        Err(err) => {
            let mut shallow = options(false);
            repo.statuses(Some(&mut shallow))
                .map_err(|_| err.message().to_string())
        }
    }
}

/// Whether a status entry is another repository's business (D12): an untracked
/// folder that is a repository of its own, or a submodule whose only change is
/// inside its own working tree. Committing the outer repository would stage
/// the whole of the first (its `.git` included), or record a change of the
/// second that it can't see.
pub(crate) fn other_repository(repo: &Repository, entry: &git2::StatusEntry) -> bool {
    let Ok(path) = entry.path() else {
        return false;
    };
    // A folder's entry may come with a trailing separator; git's index takes
    // a plain relative path.
    let rel = Path::new(path.trim_end_matches(['/', '\\']));
    let status = entry.status();
    if is_submodule(repo, rel) {
        // The commit the module has checked out against the one this
        // repository records: a newer commit is this repository's change (git
        // records the submodule's commit, and `git add` stages it), while
        // edits inside it alone are the module's own. Both arrive as
        // `WT_MODIFIED`, so the status can't tell them apart.
        if let Ok(sub) = repo.find_submodule(&rel.to_string_lossy()) {
            if let (Some(checked_out), Some(recorded)) = (sub.workdir_id(), sub.index_id()) {
                return checked_out == recorded;
            }
        }
        // No commit to compare (not checked out, no `.gitmodules` entry, or
        // its folder gone): what's added, changed or deleted here is this
        // repository's, and the rest is the module's.
        return !status.intersects(
            Status::INDEX_NEW | Status::INDEX_MODIFIED | Status::INDEX_DELETED | Status::WT_DELETED,
        );
    }
    if !(status.is_wt_new() || status.is_index_new()) {
        return false;
    }
    repo.workdir()
        .is_some_and(|workdir| inside_repository_of_its_own(workdir, rel))
}

/// Whether `rel` (relative to the repository's working folder) is recorded in
/// the index as a submodule's commit. A folder's entry may come with a
/// separator at the end, which the index refuses.
pub(crate) fn is_submodule(repo: &Repository, rel: &Path) -> bool {
    let trimmed = rel.to_string_lossy();
    let rel = Path::new(trimmed.trim_end_matches(['/', '\\']));
    repo.index()
        .ok()
        .and_then(|index| index.get_path(rel, 0).map(|entry| entry.mode))
        .is_some_and(|mode| mode == 0o160000)
}

/// Whether `rel` is in a folder that is a repository of its own: a `.git` in
/// it, or in a folder above it — down to `workdir`, which isn't one of them.
pub(crate) fn inside_repository_of_its_own(workdir: &Path, rel: &Path) -> bool {
    let mut dir = workdir.join(rel);
    while dir.starts_with(workdir) && dir != *workdir {
        if dir.join(".git").exists() {
            return true;
        }
        match dir.parent() {
            Some(parent) if parent.starts_with(workdir) => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    false
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

/// Added/removed lines vs HEAD for every dirty file in `scopes`
/// (`git diff --numstat`), scoped as the status is: a folder of a
/// repository counts its own lines, not its neighbours' (D10).
/// Merges the staged (HEAD→index) and unstaged (index→workdir) diffs so a
/// file changed in both places reports the total. Untracked files report
/// their line count as additions (when readable); anything that fails
/// (binary, missing, diff error) stays 0/0 rather than failing status.
fn workdir_line_stats_in(repo: &Repository, scopes: &[String]) -> HashMap<String, (usize, usize)> {
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
    // Staged: HEAD → index, and unstaged: index → workdir, with untracked
    // content so new files get real counts instead of 0/0.
    let mut staged = git2::DiffOptions::new();
    let mut unstaged = git2::DiffOptions::new();
    unstaged
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    for scope in scopes {
        // "" is all of the working folder: no pathspec narrows it.
        if scope.is_empty() {
            continue;
        }
        let spec = scope_pathspec(scope);
        staged.pathspec(&spec);
        unstaged.pathspec(&spec);
    }
    if let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut staged)) {
        count(&diff);
    }
    if let Ok(diff) = repo.diff_index_to_workdir(None, Some(&mut unstaged)) {
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
pub async fn git_status(path: String, scopes: Option<Vec<String>>) -> Result<GitStatus, String> {
    blocking(move || git_status_in(path, scopes)).await
}

/// The status of one folder, in that folder's scope: a project's Git panel.
#[cfg(test)]
pub fn git_status_blocking(path: String) -> Result<GitStatus, String> {
    git_status_in(path, None)
}

fn git_status_in(path: String, scopes: Option<Vec<String>>) -> Result<GitStatus, String> {
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
    let scopes = scopes_in(&repo, &path, scopes);
    let statuses = statuses_in(&repo, &scopes)?;
    let stats = workdir_line_stats_in(&repo, &scopes);

    let files = statuses
        .iter()
        .filter(|entry| !entry.status().is_ignored())
        // Another repository's own changes aren't this one's: an untracked
        // folder that is a repository of its own, or a submodule changed only
        // inside its own working tree (D12).
        .filter(|entry| !other_repository(&repo, entry))
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

//! The working tree's changes: committing them, a file's earlier version
//! and reverting to it (a file or a whole folder), and discarding them all.

use super::hooks::{not_committed, Hooks};
use super::{
    head_commit, inside_repo, open_repo, project_scope, signature, stage_all, status_options,
    status_options_in, workdir, ErrText,
};
use crate::util::{blocking, rewriting};
use git2::{Repository, ResetType, Status};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[tauri::command]
pub async fn git_commit(
    path: String,
    message: String,
    no_verify: Option<bool>,
) -> Result<(), String> {
    // A pre-commit hook may rewrite files (a formatter's fixes).
    let repo = path.clone();
    let no_verify = no_verify.unwrap_or(false);
    rewriting(&path, move || {
        git_commit_blocking_with(repo, message, no_verify)
    })
    .await
}

/// With the hooks, as the tests run it.
#[cfg(test)]
pub fn git_commit_blocking(path: String, message: String) -> Result<(), String> {
    git_commit_blocking_with(path, message, false)
}

/// Commits every change in the project's folder (`project_scope`: all of
/// the repository's when the project is its working folder), as
/// `git add -A <folder> && git commit` would, with the
/// repository's commit hooks: pre-commit, prepare-commit-msg and commit-msg
/// can stop it (or change what's staged, the message), post-commit comes
/// after. Not in the middle of a merge or a rebase (that's
/// `git_operation_continue`'s), and not with conflicts: staged, their
/// markers would be committed as text. `no_verify`: as git's
/// `--no-verify`, without pre-commit and commit-msg.
pub fn git_commit_blocking_with(
    path: String,
    message: String,
    no_verify: bool,
) -> Result<(), String> {
    let repo = open_repo(&path)?;
    crate::git_sync::nothing_in_progress(&repo)?;
    let mut index = repo.index().err_text()?;
    if index.has_conflicts() {
        return Err("Resolve the conflicts first.".to_string());
    }
    stage_all(&mut index, &project_scope(&repo, &path))?;
    let hooks = Hooks::of(&repo).no_verify(no_verify);
    if hooks.has("pre-commit") {
        hooks.run("pre-commit", &[], None).map_err(not_committed)?;
        // What it staged (or unstaged) is what's committed.
        index.read(true).err_text()?;
        if index.has_conflicts() {
            return Err("Resolve the conflicts first.".to_string());
        }
    }
    let message = hooks
        .commit_message(&repo, &message, "message")
        .map_err(not_committed)?;
    let tree = repo.find_tree(index.write_tree().err_text()?).err_text()?;
    let signature = signature(&repo)?;
    let parent = head_commit(&repo).ok();
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &parents,
    )
    .err_text()?;
    hooks.run_after("post-commit", &[], None);
    Ok(())
}

/// Throws away every change in the project's folder (`project_scope`: the
/// whole working tree when the project is the repository's working
/// folder): tracked files go back to HEAD, and new files, untracked or
/// staged, go to the trash (as with reverting one file, so the action can
/// be undone from there).
#[tauri::command]
pub async fn git_discard_all(path: String) -> Result<(), String> {
    let repo = path.clone();
    rewriting(&path, move || git_discard_all_blocking(repo)).await
}

pub fn git_discard_all_blocking(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    // A rebase's hard reset would drop the rebase's state, leaving HEAD
    // detached in the middle of it: the operation is finished or aborted
    // first.
    crate::git_sync::nothing_in_progress(&repo)?;
    let workdir = workdir(&repo)?;
    let scope = project_scope(&repo, &path);

    // New files go to the trash first: untracked ones, which a reset leaves
    // on disk, and staged-new ones, which a hard reset deletes for good
    // (HEAD doesn't have them).
    let statuses = repo
        .statuses(Some(&mut status_options_in(&scope)))
        .err_text()?;
    for entry in statuses.iter() {
        let s = entry.status();
        if !(s.is_wt_new() || s.is_index_new()) {
            continue;
        }
        let rel = entry.path().unwrap_or_default();
        let abs = workdir.join(rel);
        if !rel.is_empty() && abs.is_file() {
            crate::filesystem::to_trash(&abs)
                .map_err(|e| format!("{rel}: couldn't move it to the trash: {e}"))?;
        }
    }
    drop(statuses);

    match head_commit(&repo).ok() {
        // Tracked changes (staged or not) back to the last commit.
        Some(head) if scope.is_empty() => repo
            .reset(head.as_object(), ResetType::Hard, None)
            .err_text()?,
        // Only the project's folder: its index entries back to HEAD's
        // (`git reset -- <folder>`), then its files back to those.
        Some(head) => {
            repo.reset_default(Some(head.as_object()), [scope.as_str()])
                .err_text()?;
            let mut checkout = git2::build::CheckoutBuilder::new();
            checkout.force().path(&scope);
            repo.checkout_head(Some(&mut checkout)).err_text()?;
        }
        // No commits yet: nothing to reset to, so the index is emptied (of
        // the folder's entries) instead; their files went to the trash.
        None => {
            let mut index = repo.index().err_text()?;
            if scope.is_empty() {
                index.clear().err_text()?;
            } else {
                index.remove_all([scope.as_str()], None).err_text()?;
            }
            index.write().err_text()?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// One file: its earlier version, and reverting to it
// ---------------------------------------------------------------------------

/// Changed in the working tree, against the index.
fn is_unstaged(status: Status) -> bool {
    status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_TYPECHANGE)
}

/// The version a revert goes back to: the staged one when the file has
/// unstaged changes (what `git restore` restores), else the last commit's.
/// None: the file is new (untracked, or added and never committed).
fn base_blob<'r>(repo: &'r Repository, file: &str) -> Result<Option<git2::Blob<'r>>, String> {
    let rel = Path::new(file);
    let status = repo.status_file(rel).err_text()?;
    let index = repo.index().err_text()?;
    let from_index = || index.get_path(rel, 0).map(|e| e.id);
    let from_head = || {
        let tree = repo.head().ok()?.peel_to_tree().ok()?;
        tree.get_path(rel).ok().map(|e| e.id())
    };
    let oid = if is_unstaged(status) {
        from_index().or_else(from_head)
    } else {
        from_head()
    };
    oid.map(|oid| repo.find_blob(oid).err_text()).transpose()
}

/// The file's earlier version (see base_blob), for the editor's change marks.
/// `file` is relative to the repository's working folder. None: new file.
#[tauri::command]
pub async fn git_base_content(path: String, file: String) -> Result<Option<String>, String> {
    blocking(move || git_base_content_blocking(path, file)).await
}

pub fn git_base_content_blocking(path: String, file: String) -> Result<Option<String>, String> {
    let repo = open_repo(&path)?;
    let content = match base_blob(&repo, &file)? {
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
/// same drive, and restoring it from the trash can't overwrite anything),
/// numbered when that name is taken (a copy put back from the trash).
fn backup_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let name = |n: u32| match n {
        1 => format!("{stem} (before revert){ext}"),
        n => format!("{stem} (before revert {n}){ext}"),
    };
    let mut n = 1;
    while path.with_file_name(name(n)).exists() {
        n += 1;
    }
    path.with_file_name(name(n))
}

/// `rel` as a pathspec that matches it alone: git's wildcards escaped, so
/// a file named `pages/[id].tsx` doesn't take `pages/i.tsx` with it.
pub(super) fn literal_pathspec(rel: &Path) -> String {
    let mut spec = String::new();
    for c in rel.to_string_lossy().chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            spec.push('\\');
        }
        spec.push(c);
    }
    spec
}

/// Puts the file back as it was (see base_blob). The current version goes to
/// the trash first, so a revert can itself be undone; a new file is moved to
/// the trash, since there is no earlier version.
#[tauri::command]
pub async fn git_revert_file(path: String, file: String) -> Result<RevertOutcome, String> {
    blocking(move || git_revert_file_blocking(path, file)).await
}

pub fn git_revert_file_blocking(path: String, file: String) -> Result<RevertOutcome, String> {
    let repo = open_repo(&path)?;
    let workdir = workdir(&repo)?;
    inside_repo(&file)?;
    revert_in(&repo, &workdir, &file)
}

/// Reverts one file of `repo` (see git_revert_file); `file` is relative to
/// `workdir` and checked already.
fn revert_in(repo: &Repository, workdir: &Path, file: &str) -> Result<RevertOutcome, String> {
    let rel = Path::new(file);
    let abs = workdir.join(rel);
    let status = repo.status_file(rel).err_text()?;
    if status.is_conflicted() {
        return Err(format!("{file} has merge conflicts: resolve them first"));
    }

    // New: nothing to go back to. Out of the index, into the trash.
    if base_blob(repo, file)?.is_none() {
        if status.is_index_new() {
            let mut index = repo.index().err_text()?;
            index.remove_path(rel).err_text()?;
            index.write().err_text()?;
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

    // This file alone: a path is a pattern to libgit2 unless told otherwise.
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().path(rel).disable_pathspec_match(true);
    if is_unstaged(status) {
        // Back to the staged version.
        repo.checkout_index(None, Some(&mut checkout)).err_text()?;
    } else {
        // Only staged changes: back to the last commit, index included.
        let head = head_commit(repo)?;
        repo.reset_default(Some(head.as_object()), [literal_pathspec(rel)])
            .err_text()?;
        repo.checkout_head(Some(&mut checkout)).err_text()?;
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
    let repo = path.clone();
    rewriting(&path, move || git_revert_folder_blocking(repo, folder)).await
}

pub fn git_revert_folder_blocking(
    path: String,
    folder: String,
) -> Result<FolderRevertOutcome, String> {
    let repo = open_repo(&path)?;
    let workdir = workdir(&repo)?;
    let folder = folder.trim_end_matches('/');
    inside_repo(folder)?;
    let prefix = format!("{folder}/");

    let statuses = repo.statuses(Some(&mut status_options())).err_text()?;
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

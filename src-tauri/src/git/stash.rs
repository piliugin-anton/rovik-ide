//! The stash: shelving the working tree, the list, and what an entry holds.

use super::{
    blob_text, delta_path, diff_line_stats, inside_repo, open_repo, signature, tree_blob, ErrText,
};
use crate::util::{blocking, rewriting};
use git2::{Repository, StashFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStashEntry {
    pub index: usize,
    pub message: String,
}

/// Shelves the working tree (tracked changes and untracked files) on the
/// stash, so the tree comes back clean. Needs a git identity, like commit.
#[tauri::command]
pub async fn git_stash_push(path: String, message: Option<String>) -> Result<(), String> {
    let repo = path.clone();
    rewriting(&path, move || git_stash_push_blocking(repo, message)).await
}

pub fn git_stash_push_blocking(path: String, message: Option<String>) -> Result<(), String> {
    let mut repo = open_repo(&path)?;
    let signature = signature(&repo)?;
    let message = message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
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
    blocking(move || git_stash_list_blocking(path)).await
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
    .err_text()?;
    Ok(entries)
}

/// Restores a stashed state and drops it from the list (`git stash pop`).
/// `index` defaults to the newest entry. On conflicts nothing is dropped —
/// the error says to resolve them first.
#[tauri::command]
pub async fn git_stash_pop(path: String, index: Option<usize>) -> Result<(), String> {
    let repo = path.clone();
    rewriting(&path, move || git_stash_pop_blocking(repo, index)).await
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
    blocking(move || git_stash_drop_blocking(path, index)).await
}

pub fn git_stash_drop_blocking(path: String, index: usize) -> Result<(), String> {
    let mut repo = open_repo(&path)?;
    repo.stash_drop(index).err_text()
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

pub(super) fn delta_status(s: git2::Delta) -> &'static str {
    match s {
        git2::Delta::Added | git2::Delta::Untracked | git2::Delta::Copied => "A",
        git2::Delta::Deleted => "D",
        git2::Delta::Renamed => "R",
        _ => "M",
    }
}

/// The stash commit's tree plus its base tree and optional untracked-files
/// tree.
///
/// A stash is a merge commit: parent 0 is the HEAD it was taken on (the
/// base), parent 1 the index state, and — only when there were untracked
/// files — parent 2 the untracked files. The stash commit's own tree holds
/// the tracked working-tree state; untracked files live only in parent 2.
struct StashTrees<'r> {
    stash: git2::Tree<'r>,
    base: Option<git2::Tree<'r>>,
    untracked: Option<git2::Tree<'r>>,
}

fn stash_trees(repo: &Repository, index: usize) -> Result<StashTrees<'_>, String> {
    let obj = repo
        .revparse_single(&format!("stash@{{{index}}}"))
        .map_err(|_| format!("Stash entry {index} not found"))?;
    let commit = obj.peel_to_commit().err_text()?;
    Ok(StashTrees {
        stash: commit.tree().err_text()?,
        base: commit.parent(0).ok().and_then(|p| p.tree().ok()),
        untracked: commit.parent(2).ok().and_then(|p| p.tree().ok()),
    })
}

/// What a stash contains: the files it touched, from diffing the base
/// against the stash tree (tracked changes) and — when present — the
/// untracked files, each new. What `git stash show --include-untracked`
/// lists.
#[tauri::command]
pub async fn git_stash_show(path: String, index: usize) -> Result<Vec<GitStashFile>, String> {
    blocking(move || git_stash_show_blocking(path, index)).await
}

pub fn git_stash_show_blocking(path: String, index: usize) -> Result<Vec<GitStashFile>, String> {
    let repo = open_repo(&path)?;
    let trees = stash_trees(&repo, index)?;
    let mut files: HashMap<String, (String, usize, usize)> = HashMap::new();
    // Untracked first: on overlap the tracked diff wins (both status and
    // counts). The untracked files' tree holds them alone: diffed against
    // no tree, each is new, where against the base every other file of the
    // branch would read as deleted.
    for (from, other) in [
        (None, trees.untracked.as_ref()),
        (trees.base.as_ref(), Some(&trees.stash)),
    ] {
        let Some(other) = other else { continue };
        let diff = repo.diff_tree_to_tree(from, Some(other), None).err_text()?;
        let stats = diff_line_stats(&diff);
        for (i, delta) in diff.deltas().enumerate() {
            if let Some(rel) = delta_path(&diff, i) {
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
    blocking(move || git_stash_file_content_blocking(path, index, file)).await
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
/// working folder. The history's file preview has the same shape.
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
    blocking(move || git_stash_file_preview_blocking(path, index, file)).await
}

pub fn git_stash_file_preview_blocking(
    path: String,
    index: usize,
    file: String,
) -> Result<GitStashFilePreview, String> {
    let repo = open_repo(&path)?;
    let rel = inside_repo(&file)?;
    let trees = stash_trees(&repo, index)?;
    let content = tree_blob(Some(&trees.stash), rel)
        .or_else(|| tree_blob(trees.untracked.as_ref(), rel))
        .map(|id| blob_text(&repo, id, &file))
        .transpose()?;
    let base = tree_blob(trees.base.as_ref(), rel)
        .map(|id| blob_text(&repo, id, &file))
        .transpose()?;
    Ok(GitStashFilePreview { content, base })
}

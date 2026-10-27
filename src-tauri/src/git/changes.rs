//! The working tree's changes: committing them, a file's earlier version
//! and reverting to it (a file or a whole folder), and discarding them all.

use super::hooks::{not_committed, Hooks};
use super::{
    head_commit, inside_repo, inside_repository_of_its_own, is_submodule, open_repo,
    scope_pathspec, scopes_in, signature, stage_all, status_options, status_options_in_scopes,
    workdir, ErrText,
};
use crate::util::{blocking, rewriting, rewriting_all};
use git2::{Repository, ResetType, Status};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[tauri::command]
pub async fn git_commit(
    path: String,
    message: String,
    no_verify: Option<bool>,
    scopes: Option<Vec<String>>,
) -> Result<(), String> {
    // A pre-commit hook may rewrite files (a formatter's fixes).
    let repo = path.clone();
    let no_verify = no_verify.unwrap_or(false);
    rewriting(&path, move || {
        git_commit_in(repo, message, no_verify, scopes)
    })
    .await
}

/// With the hooks, as the tests run it.
#[cfg(test)]
pub fn git_commit_blocking(path: String, message: String) -> Result<(), String> {
    git_commit_in(path, message, false, None)
}

/// A commit of the folder in `path` alone: what a project's panel makes.
#[cfg(test)]
pub fn git_commit_blocking_with(
    path: String,
    message: String,
    no_verify: bool,
) -> Result<(), String> {
    git_commit_in(path, message, no_verify, None)
}

/// Commits every change in the repository's folders the window has open
/// (`scopes`: the folder in `path` when none are given), as
/// `git add -A <folder> && git commit` would, with the
/// repository's commit hooks: pre-commit, prepare-commit-msg and commit-msg
/// can stop it (or change what's staged, the message), post-commit comes
/// after. Not in the middle of a merge or a rebase (that's
/// `git_operation_continue`'s), and not with conflicts: staged, their
/// markers would be committed as text. `no_verify`: as git's
/// `--no-verify`, without pre-commit and commit-msg. A commit across
/// repositories ([`git_commit_many`]) takes the same steps in each.
pub fn git_commit_in(
    path: String,
    message: String,
    no_verify: bool,
    scopes: Option<Vec<String>>,
) -> Result<(), String> {
    let commit = Committing::check(&path, scopes, no_verify)?;
    let outside = staged_outside(&commit.repo, &commit.scopes)?;
    if !outside.is_empty() {
        // A terminal's `git add` beside the window's folders: what's staged
        // stays staged, and nothing is committed or staged (D12).
        let workdir = workdir(&commit.repo)?;
        let named = workdir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| workdir.to_string_lossy().into_owned());
        return Err(format!(
            "{named} has changes staged outside this window's folders: {}. Commit or unstage them in a terminal first.",
            outside.join(", ")
        ));
    }
    let mut index = commit.stage()?;
    let message = commit.message(&message)?;
    if !commit.commit(&mut index, &message)? {
        return Err("Nothing to commit.".to_string());
    }
    commit.done();
    Ok(())
}

/// One repository of a commit across the window's repositories.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoCommit {
    /// A folder of the window in it: the repository is found from it.
    pub path: String,
    /// The window's folders in it, relative to its working folder ("" for
    /// all of it).
    pub scopes: Vec<String>,
}

/// What a repository of a commit across repositories said: the folder it
/// was asked from (its [`RepoCommit::path`]), and the error.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RepoProblem {
    pub path: String,
    pub error: String,
}

/// What a commit across repositories did (D11).
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum CommitOutcome {
    /// Nothing was committed anywhere: every repository a check found a
    /// problem in, or the one whose hook failed.
    Stopped { problems: Vec<RepoProblem> },
    /// The commits were made, but in `failed`: a commit that fails once
    /// others are written can't take them back. A repository in neither list
    /// had nothing to commit once its hooks had run.
    Done {
        committed: Vec<String>,
        failed: Vec<RepoProblem>,
    },
}

/// Commits one message in several repositories, each on the branch it's on
/// (D11): every repository is checked first, then each one's folders are
/// staged and its pre-commit hook runs, then its message hooks, and only
/// then are the commits made, and post-commit runs in each. A check, or a
/// hook that fails, stops it before any commit is written; `no_verify`
/// skips the hooks that check in every repository.
#[tauri::command]
pub async fn git_commit_many(
    repos: Vec<RepoCommit>,
    message: String,
    no_verify: Option<bool>,
) -> Result<CommitOutcome, String> {
    // Each one's pre-commit hook may rewrite its files: every repository's
    // folders wait until the last commit is made.
    let paths: Vec<String> = repos.iter().map(|one| one.path.clone()).collect();
    let no_verify = no_verify.unwrap_or(false);
    rewriting_all(&paths, move || {
        git_commit_many_in(repos, message, no_verify)
    })
    .await
}

pub fn git_commit_many_in(
    repos: Vec<RepoCommit>,
    message: String,
    no_verify: bool,
) -> Result<CommitOutcome, String> {
    // 1. Every repository is checked before anything is staged or run, and
    // every problem is named.
    let mut ready = Vec::new();
    let mut problems = Vec::new();
    for one in repos {
        let checked = Committing::check(&one.path, Some(one.scopes), no_verify).and_then(|commit| {
            let outside = staged_outside(&commit.repo, &commit.scopes)?;
            if outside.is_empty() {
                return Ok(commit);
            }
            Err(format!(
                "Changes are staged outside this window's folders: {}. Commit or unstage them in a terminal first.",
                outside.join(", ")
            ))
        });
        match checked {
            Ok(commit) => ready.push((one.path, commit)),
            Err(error) => problems.push(RepoProblem {
                path: one.path,
                error,
            }),
        }
    }
    if !problems.is_empty() {
        return Ok(CommitOutcome::Stopped { problems });
    }
    let stopped = |path: &str, error: String| CommitOutcome::Stopped {
        problems: vec![RepoProblem {
            path: path.to_string(),
            error,
        }],
    };
    // 2. Each one's folders are staged and its pre-commit hook runs.
    let mut indexes = Vec::new();
    for (path, commit) in &ready {
        match commit.stage() {
            Ok(index) => indexes.push(index),
            Err(error) => return Ok(stopped(path, error)),
        }
    }
    // 3. The message hooks of each: one that changes the message changes its
    // own repository's.
    let mut messages = Vec::new();
    for (path, commit) in &ready {
        match commit.message(&message) {
            Ok(message) => messages.push(message),
            Err(error) => return Ok(stopped(path, error)),
        }
    }
    // 4. The commits. One that fails is reported with those made before it,
    // and the ones after it are still made.
    let mut committed = Vec::new();
    let mut failed = Vec::new();
    for (((path, commit), index), message) in ready.iter().zip(&mut indexes).zip(&messages) {
        match commit.commit(index, message) {
            Ok(true) => committed.push(path.clone()),
            Ok(false) => {}
            Err(error) => failed.push(RepoProblem {
                path: path.clone(),
                error,
            }),
        }
    }
    if committed.is_empty() && failed.is_empty() {
        return Err("Nothing to commit.".to_string());
    }
    // 5. post-commit in each repository committed.
    for (path, commit) in &ready {
        if committed.contains(path) {
            commit.done();
        }
    }
    Ok(CommitOutcome::Done { committed, failed })
}

/// A repository on its way through a commit: checked, then its folders
/// staged with its pre-commit hook, its message, its commit, and post-commit.
struct Committing {
    repo: Repository,
    scopes: Vec<String>,
    signature: git2::Signature<'static>,
    hooks: Hooks,
}

impl Committing {
    /// The repository `path` is in, when it can be committed now: nothing in
    /// progress, no conflicts, and someone to commit as. What's staged
    /// outside its folders is for the caller to say ([`staged_outside`]).
    fn check(
        path: &str,
        scopes: Option<Vec<String>>,
        no_verify: bool,
    ) -> Result<Committing, String> {
        let repo = open_repo(path)?;
        crate::git_sync::nothing_in_progress(&repo)?;
        if repo.index().err_text()?.has_conflicts() {
            return Err("Resolve the conflicts first.".to_string());
        }
        let signature = signature(&repo)?;
        let scopes = scopes_in(&repo, path, scopes);
        let hooks = Hooks::of(&repo).no_verify(no_verify);
        Ok(Committing {
            repo,
            scopes,
            signature,
            hooks,
        })
    }

    /// The index to commit: the folders' changes staged, then the
    /// pre-commit hook, whose own staging (or unstaging) is what's committed.
    fn stage(&self) -> Result<git2::Index, String> {
        let mut index = self.repo.index().err_text()?;
        for scope in &self.scopes {
            stage_all(&mut index, &self.repo, scope)?;
        }
        if self.hooks.has("pre-commit") {
            self.hooks
                .run("pre-commit", &[], None)
                .map_err(not_committed)?;
            index.read(true).err_text()?;
            if index.has_conflicts() {
                return Err("Resolve the conflicts first.".to_string());
            }
        }
        Ok(index)
    }

    /// The message once prepare-commit-msg and commit-msg have had it.
    fn message(&self, message: &str) -> Result<String, String> {
        self.hooks
            .commit_message(&self.repo, message, "message")
            .map_err(not_committed)
    }

    /// Commits `index` on HEAD: the branch it's on, a detached HEAD as git
    /// would, or an unborn branch, which the commit makes. False when there's
    /// nothing to commit (a submodule whose only change is inside its own
    /// working tree, say): an empty commit isn't made (D12).
    fn commit(&self, index: &mut git2::Index, message: &str) -> Result<bool, String> {
        let repo = &self.repo;
        let tree = repo.find_tree(index.write_tree().err_text()?).err_text()?;
        let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
        if head_tree.is_some_and(|head| head.id() == tree.id()) {
            return Ok(false);
        }
        let parent = head_commit(repo).ok();
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &self.signature,
            &self.signature,
            message,
            &tree,
            &parents,
        )
        .err_text()?;
        Ok(true)
    }

    /// post-commit, which comes after and can't undo anything.
    fn done(&self) {
        self.hooks.run_after("post-commit", &[], None);
    }
}

/// The changes staged outside `scopes` (the window's folders in the
/// repository): a commit would take them in, though they're the user's other
/// work — a terminal's `git add` beside the folders (D12). Empty when a scope
/// is the whole working folder, or nothing is staged.
fn staged_outside(repo: &Repository, scopes: &[String]) -> Result<Vec<String>, String> {
    if scopes.iter().any(String::is_empty) {
        return Ok(Vec::new());
    }
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let index = repo.index().err_text()?;
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), None)
        .err_text()?;
    if diff.deltas().len() == 0 {
        return Ok(Vec::new());
    }
    let specs: Vec<String> = scopes.iter().map(|scope| scope_pathspec(scope)).collect();
    let pathspec = git2::Pathspec::new(specs.iter().map(String::as_str)).err_text()?;
    let mut outside = Vec::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(Path::to_path_buf);
        let Some(path) = path else { continue };
        if pathspec.matches_path(&path, git2::PathspecFlags::empty()) {
            continue;
        }
        outside.push(path.to_string_lossy().into_owned());
    }
    outside.sort();
    Ok(outside)
}

/// Throws away every change in the repository's folders the window has open
/// (`scopes`: the folder in `path` when none are given; all of the working
/// tree when a folder is it): tracked files go back to HEAD, and new files,
/// untracked or staged, go to the trash (as with reverting one file, so the
/// action can be undone from there).
#[tauri::command]
pub async fn git_discard_all(path: String, scopes: Option<Vec<String>>) -> Result<(), String> {
    let repo = path.clone();
    rewriting(&path, move || git_discard_all_in(repo, scopes)).await
}

/// Throws away every change in the folder in `path`: what a project's panel
/// does.
#[cfg(test)]
pub fn git_discard_all_blocking(path: String) -> Result<(), String> {
    git_discard_all_in(path, None)
}

pub fn git_discard_all_in(path: String, scopes: Option<Vec<String>>) -> Result<(), String> {
    let repo = open_repo(&path)?;
    // A rebase's hard reset would drop the rebase's state, leaving HEAD
    // detached in the middle of it: the operation is finished or aborted
    // first.
    crate::git_sync::nothing_in_progress(&repo)?;
    let workdir = workdir(&repo)?;
    let scopes = scopes_in(&repo, &path, scopes);
    let everything = scopes.iter().all(String::is_empty);

    // New files go to the trash first: untracked ones, which a reset leaves
    // on disk, and staged-new ones, which a hard reset deletes for good
    // (HEAD doesn't have them).
    let statuses = repo
        .statuses(Some(&mut status_options_in_scopes(&scopes)))
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

    let specs: Vec<String> = scopes.iter().map(|scope| scope_pathspec(scope)).collect();
    match head_commit(&repo).ok() {
        // Tracked changes (staged or not) back to the last commit.
        Some(head) if everything => repo
            .reset(head.as_object(), ResetType::Hard, None)
            .err_text()?,
        // Only the window's folders: their index entries back to HEAD's
        // (`git reset -- <folder>`), then their files back to those.
        Some(head) => {
            repo.reset_default(Some(head.as_object()), specs.iter().map(String::as_str))
                .err_text()?;
            for spec in &specs {
                let mut checkout = git2::build::CheckoutBuilder::new();
                checkout.force().path(spec);
                repo.checkout_head(Some(&mut checkout)).err_text()?;
            }
        }
        // No commits yet: nothing to reset to, so the index is emptied (of
        // the folders' entries) instead; their files went to the trash.
        None => {
            let mut index = repo.index().err_text()?;
            if everything {
                index.clear().err_text()?;
            } else {
                index
                    .remove_all(specs.iter().map(String::as_str), None)
                    .err_text()?;
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
    // Another repository's business: a folder that is a repository of its own,
    // or a submodule (D12).
    if inside_repository_of_its_own(workdir, rel) {
        return Err(format!(
            "{file} is a repository of its own: commit or revert it there"
        ));
    }
    if is_submodule(repo, rel) {
        return Err(format!(
            "{file} is a submodule: revert it in its own repository"
        ));
    }
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

//! Branches: the list, where HEAD is, switching to one, and creating and
//! deleting them.

use super::hooks::Hooks;
use super::{head_commit, open_repo, ErrText};
use crate::util::{blocking, rewriting};
use git2::{BranchType, Repository};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitBranch {
    pub name: String,
    /// `origin/main` style remote-tracking branch (can't be deleted here,
    /// checking it out creates a local tracking branch).
    pub is_remote: bool,
}

#[tauri::command]
pub async fn git_branches(path: String) -> Result<Vec<GitBranch>, String> {
    blocking(move || git_branches_blocking(path)).await
}

/// The local branches, then the remote-tracking ones, each set sorted.
pub fn git_branches_blocking(path: String) -> Result<Vec<GitBranch>, String> {
    let repo = open_repo(&path)?;
    let mut all = Vec::new();
    for kind in [BranchType::Local, BranchType::Remote] {
        let is_remote = matches!(kind, BranchType::Remote);
        let mut names = Vec::new();
        for item in repo.branches(Some(kind)).err_text()? {
            let (branch, _) = item.err_text()?;
            if let Some(name) = branch.name().err_text()? {
                // Skip the `origin/HEAD` symref; it's not a branch to switch to.
                if is_remote && name.ends_with("/HEAD") {
                    continue;
                }
                names.push(name.to_string());
            }
        }
        names.sort();
        all.extend(names.into_iter().map(|name| GitBranch { name, is_remote }));
    }
    Ok(all)
}

/// Where HEAD is: what the chat panel looks at before a new chat's first
/// run, to offer the chat a branch of its own when the project is on its
/// main branch (src/lib/chatBranch.ts).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct GitHead {
    /// The branch HEAD is on; None when it's detached (a rebase detaches
    /// it), or the repository has no commit yet.
    pub branch: Option<String>,
    /// The repository's main branch: the one `origin/HEAD` names, else the
    /// first of `init.defaultBranch`, `main` and `master` that's a local
    /// branch; None without any of them.
    pub main_branch: Option<String>,
}

/// Where HEAD is in the repository the project at `path` is in.
#[tauri::command]
pub async fn git_head(path: String) -> Result<GitHead, String> {
    blocking(move || git_head_blocking(path)).await
}

/// [`git_head`]'s work, on the calling thread. Outside a repository, an
/// error that starts with "Not a git repository".
pub fn git_head_blocking(path: String) -> Result<GitHead, String> {
    let repo = open_repo(&path)?;
    let branch = repo
        .head()
        .ok()
        .filter(|head| head.is_branch())
        .and_then(|head| head.shorthand().ok().map(str::to_string));
    Ok(GitHead {
        branch,
        main_branch: main_branch(&repo),
    })
}

/// The repository's main branch: the one `origin`'s HEAD names (its
/// default branch, which a clone records as `origin/HEAD`), else the first
/// of the branch `git init` starts on (`init.defaultBranch`), `main` and
/// `master` that's a local branch. None when there's none of them.
fn main_branch(repo: &Repository) -> Option<String> {
    let remote_default = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|head| {
            head.symbolic_target()
                .ok()
                .flatten()
                .and_then(|target| target.strip_prefix("refs/remotes/origin/"))
                .map(str::to_string)
        });
    if remote_default.is_some() {
        return remote_default;
    }
    let configured = repo
        .config()
        .ok()
        .and_then(|config| config.get_string("init.defaultBranch").ok());
    configured
        .into_iter()
        .chain(["main".to_string(), "master".to_string()])
        .find(|name| repo.find_branch(name, BranchType::Local).is_ok())
}

#[tauri::command]
pub async fn git_checkout(path: String, branch: String) -> Result<(), String> {
    let repo = path.clone();
    rewriting(&path, move || git_checkout_blocking(repo, branch)).await
}

/// Switches to `branch`, then runs the repository's post-checkout hook, as
/// git does after a switch: with the commits HEAD was and is at.
pub fn git_checkout_blocking(path: String, branch: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let head_id = |repo: &Repository| {
        repo.head()
            .ok()
            .and_then(|h| h.target())
            .unwrap_or(git2::Oid::ZERO_SHA1)
            .to_string()
    };
    let before = head_id(&repo);
    switch(&repo, branch)?;
    let after = head_id(&repo);
    Hooks::of(&repo).run_after("post-checkout", &[&before, &after, "1"], None);
    Ok(())
}

fn switch(repo: &Repository, branch: String) -> Result<(), String> {
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
        repo.set_head(&refname).err_text()
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
    .err_text()
}

/// The obvious mistakes in a branch name, rejected before libgit2 sees them
/// so the error reads like guidance rather than a plumbing complaint.
pub(super) fn validate_branch_name(name: &str) -> Result<String, String> {
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
    let unusable = |e: git2::Error| format!("Couldn't use '{start}' as a starting point: {e}");
    for kind in [BranchType::Local, BranchType::Remote] {
        if let Ok(branch) = repo.find_branch(start, kind) {
            let commit = branch.get().peel_to_commit().map_err(unusable)?;
            let remote = matches!(kind, BranchType::Remote).then(|| start.to_string());
            return Ok((commit, remote));
        }
    }
    let commit = repo
        .revparse_single(start)
        .map_err(|_| format!("Couldn't find starting point '{start}'"))?
        .peel_to_commit()
        .map_err(unusable)?;
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
    let repo = path.clone();
    let work = move || git_create_branch_blocking(repo, name, checkout, from);
    if checkout {
        // Isolated chats on the project wait while the working tree is rewritten.
        rewriting(&path, work).await
    } else {
        blocking(work).await
    }
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
            head_commit(&repo)
                .map_err(|_| "Can't create a branch yet: there are no commits".to_string())?,
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
    blocking(move || git_delete_branch_blocking(path, name)).await
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
    branch.delete().err_text()
}

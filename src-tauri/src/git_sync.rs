// git_sync.rs
//
// Bringing the remote's commits in, and settling what that leaves behind:
// a pull merges or rebases as the repository's git config says
// (branch.<name>.rebase, pull.rebase, pull.ff), a conflicted file is
// resolved from its two sides, and the merge or rebase is then continued
// or aborted. The Git panel's pull, a push the remote rejected, and the
// conflict resolver (src/components/ConflictResolver.tsx) all end up here,
// and so does a rebase onto a branch picked in the Git menu.
//
// Merges and rebases are libgit2's on-disk ones: the state lives in .git
// (MERGE_HEAD, rebase-merge/) like git's own, so a terminal sees it too.

use crate::git::{git_fetch_blocking, off_main_thread, open_repo};
use git2::{
    build::CheckoutBuilder, BranchType, ErrorCode, Oid, RebaseOptions, Reference, Repository,
    RepositoryState, ResetType, Status, StatusOptions,
};
use rovik_core::merge::{is_binary, ConflictFile, MergeSegment};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

// ---------------------------------------------------------------------------
// Pull strategy, from git config
// ---------------------------------------------------------------------------

/// How a pull brings in commits when the branches have diverged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullStrategy {
    Merge,
    Rebase,
    /// pull.ff=only: fast-forward or nothing.
    FfOnly,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PullConfig {
    /// None: the config doesn't say, so the user is asked when it matters.
    pub strategy: Option<PullStrategy>,
    /// The config key the strategy comes from ("pull.rebase", …).
    pub source: Option<String>,
    /// pull.ff=false: a merge commit even when a fast-forward would do.
    pub no_ff: bool,
}

/// Git's spelling of booleans, plus pull.rebase's own values: "merges" and
/// "interactive" rebase too (libgit2 can't keep merges or stop to edit, so
/// they're a plain rebase here).
fn parse_rebase(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "" | "merges" | "m" | "interactive" | "i" | "preserve"
        | "p" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn config_string(config: &git2::Config, key: &str) -> Option<String> {
    match config.get_string(key) {
        Ok(v) => Some(v),
        // A key with no value (`[pull] rebase`) is a boolean true.
        Err(e) if e.code() != ErrorCode::NotFound => {
            config.get_bool(key).ok().map(|b| b.to_string())
        }
        Err(_) => None,
    }
}

/// What `git pull` would do on `branch`, read the way git reads it:
/// branch.<name>.rebase wins over pull.rebase; pull.ff=only stands when
/// neither asks for a rebase.
pub fn pull_config(repo: &Repository, branch: &str) -> PullConfig {
    let Ok(config) = repo.config().and_then(|mut c| c.snapshot()) else {
        return PullConfig {
            strategy: None,
            source: None,
            no_ff: false,
        };
    };
    let ff = config_string(&config, "pull.ff").map(|v| v.trim().to_ascii_lowercase());
    let ff_only = ff.as_deref() == Some("only");
    let no_ff = matches!(ff.as_deref(), Some("false" | "no" | "off" | "0"));

    let rebase = [format!("branch.{branch}.rebase"), "pull.rebase".to_string()]
        .into_iter()
        .find_map(|key| {
            config_string(&config, &key)
                .and_then(|v| parse_rebase(&v))
                .map(|r| (r, key))
        });

    let (strategy, source) = match rebase {
        Some((true, key)) => (Some(PullStrategy::Rebase), Some(key)),
        _ if ff_only => (Some(PullStrategy::FfOnly), Some("pull.ff".to_string())),
        Some((false, key)) => (Some(PullStrategy::Merge), Some(key)),
        None => (None, None),
    };
    PullConfig {
        strategy,
        source,
        no_ff,
    }
}

/// The current branch and the remote branch it tracks.
fn tracking(repo: &Repository) -> Result<(String, Reference<'_>), String> {
    let head = repo.head().map_err(|e| e.to_string())?;
    if !head.is_branch() {
        return Err("Cannot pull: HEAD is detached, not on a branch".to_string());
    }
    let name = head
        .shorthand()
        .map_err(|e| format!("Could not determine current branch name: {e}"))?
        .to_string();
    let branch = repo
        .find_branch(&name, BranchType::Local)
        .map_err(|e| e.to_string())?;
    let upstream = branch
        .upstream()
        .map_err(|_| format!("Branch '{name}' has no upstream configured"))?;
    Ok((name, upstream.into_reference()))
}

#[tauri::command]
pub async fn git_pull_config(path: String) -> Result<PullConfig, String> {
    off_main_thread(move || {
        let repo = open_repo(&path)?;
        let head = repo.head().map_err(|e| e.to_string())?;
        let branch = head.shorthand().unwrap_or("HEAD").to_string();
        Ok(pull_config(&repo, &branch))
    })
    .await
}

// ---------------------------------------------------------------------------
// Pull
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PullOutcome {
    /// "up-to-date" | "fast-forward" | "merged" | "rebased" | "conflicts"
    /// | "needs-strategy" (diverged, and the config doesn't say how to pull)
    /// | "rewrites-pushed" (a rebase onto a branch would rewrite commits
    /// already pushed: asked about first).
    pub result: &'static str,
    /// For "conflicts": the files that need resolving.
    pub conflicts: Vec<String>,
}

impl PullOutcome {
    fn done(result: &'static str) -> Self {
        PullOutcome {
            result,
            conflicts: Vec::new(),
        }
    }
}

fn state_name(state: RepositoryState) -> Option<&'static str> {
    match state {
        RepositoryState::Clean => None,
        RepositoryState::Merge => Some("merge"),
        RepositoryState::Rebase
        | RepositoryState::RebaseInteractive
        | RepositoryState::RebaseMerge
        | RepositoryState::ApplyMailbox
        | RepositoryState::ApplyMailboxOrRebase => Some("rebase"),
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => Some("cherry-pick"),
        RepositoryState::Revert | RepositoryState::RevertSequence => Some("revert"),
        RepositoryState::Bisect => Some("bisect"),
    }
}

/// `conflictStyle = diff3 / zdiff3`: conflict markers in the files show the
/// common ancestor too, as git would write them.
fn checkout_for_merge(repo: &Repository) -> CheckoutBuilder<'static> {
    let style = repo
        .config()
        .ok()
        .and_then(|c| c.get_string("merge.conflictStyle").ok())
        .map(|s| s.to_ascii_lowercase());
    let mut checkout = CheckoutBuilder::new();
    checkout
        .safe()
        .allow_conflicts(true)
        .conflict_style_diff3(matches!(style.as_deref(), Some("diff3" | "zdiff3")));
    checkout
}

fn rebase_options(repo: &Repository) -> RebaseOptions<'static> {
    let mut opts = RebaseOptions::new();
    opts.checkout_options(checkout_for_merge(repo));
    opts
}

fn signature(repo: &Repository) -> Result<git2::Signature<'static>, String> {
    repo.signature()
        .map_err(|e| format!("No Git identity configured (user.name / user.email): {e}"))
}

/// Files with conflicts in the index, sorted.
pub fn conflicted_paths(repo: &Repository) -> Result<Vec<String>, String> {
    let mut index = repo.index().map_err(|e| e.to_string())?;
    // Another handle (a resolve) may have written it since.
    index.read(false).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    for conflict in index.conflicts().map_err(|e| e.to_string())? {
        let conflict = conflict.map_err(|e| e.to_string())?;
        if let Some(entry) = conflict.our.or(conflict.their).or(conflict.ancestor) {
            paths.push(String::from_utf8_lossy(&entry.path).into_owned());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// A merge or rebase rewrites tracked files; changes not yet committed to
/// them could be lost or mixed into the result, so it waits for them.
/// `what` is the action, to try again: "pull", "rebase".
fn ensure_clean(repo: &Repository, what: &str) -> Result<(), String> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;
    let dirty = statuses
        .iter()
        .any(|e| e.status() != Status::CURRENT && !e.status().is_ignored());
    if dirty {
        return Err(format!(
            "You have uncommitted changes. Commit or stash them first, then {what} again."
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn git_pull(
    path: String,
    strategy: Option<PullStrategy>,
    remember: Option<bool>,
) -> Result<PullOutcome, String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_pull_blocking(path, strategy, remember.unwrap_or(false))).await
}

/// Fetches, then brings the upstream's commits into the current branch.
/// `strategy` overrides the config (the user picked one); `remember` stores
/// that pick as this repository's pull.rebase.
pub fn git_pull_blocking(
    path: String,
    strategy: Option<PullStrategy>,
    remember: bool,
) -> Result<PullOutcome, String> {
    {
        let repo = open_repo(&path)?;
        if let Some(op) = state_name(repo.state()) {
            return Err(format!("A {op} is in progress: finish or abort it first."));
        }
    }
    git_fetch_blocking(path.clone())?;
    integrate_upstream(&open_repo(&path)?, strategy, remember)
}

/// The part of a pull after the fetch (tests call it on local branches).
pub fn integrate_upstream(
    repo: &Repository,
    strategy: Option<PullStrategy>,
    remember: bool,
) -> Result<PullOutcome, String> {
    let (branch, upstream) = tracking(repo)?;
    let upstream_oid = upstream
        .target()
        .ok_or("Upstream branch has no target commit")?;
    let incoming = repo
        .reference_to_annotated_commit(&upstream)
        .map_err(|e| e.to_string())?;
    let (analysis, _) = repo
        .merge_analysis(&[&incoming])
        .map_err(|e| e.to_string())?;
    if analysis.is_up_to_date() {
        return Ok(PullOutcome::done("up-to-date"));
    }

    let config = pull_config(repo, &branch);
    if remember {
        if let Some(s @ (PullStrategy::Merge | PullStrategy::Rebase)) = strategy {
            let mut local = repo
                .config()
                .and_then(|c| c.open_level(git2::ConfigLevel::Local))
                .map_err(|e| e.to_string())?;
            local
                .set_bool("pull.rebase", s == PullStrategy::Rebase)
                .map_err(|e| e.to_string())?;
        }
    }
    let strategy = strategy.or(config.strategy);

    let merge_anyway = strategy == Some(PullStrategy::Merge) && config.no_ff;
    if analysis.is_fast_forward() && !merge_anyway {
        // The working tree first, safely: local edits to files the upstream
        // changes stop it, rather than being overwritten.
        let target = repo
            .find_object(upstream_oid, None)
            .map_err(|e| e.to_string())?;
        repo.checkout_tree(&target, Some(CheckoutBuilder::new().safe()))
            .map_err(|e| format!("Pull would overwrite your local changes: {e}"))?;
        let refname = format!("refs/heads/{branch}");
        repo.find_reference(&refname)
            .and_then(|mut r| r.set_target(upstream_oid, "pull: Fast-forward"))
            .map_err(|e| e.to_string())?;
        repo.set_head(&refname).map_err(|e| e.to_string())?;
        return Ok(PullOutcome::done("fast-forward"));
    }

    match strategy {
        None => Ok(PullOutcome::done("needs-strategy")),
        Some(PullStrategy::FfOnly) => Err(format!(
            "Can't fast-forward: '{branch}' and its upstream have diverged, and pull.ff is \"only\". \
             Pull with merge or rebase instead."
        )),
        Some(PullStrategy::Merge) => {
            ensure_clean(repo, "pull")?;
            let mut checkout = checkout_for_merge(repo);
            repo.merge(&[&incoming], None, Some(&mut checkout)).map_err(|e| format!("Merge failed: {e}"))?;
            let conflicts = conflicted_paths(repo)?;
            if !conflicts.is_empty() {
                return Ok(PullOutcome { result: "conflicts", conflicts });
            }
            commit_merge(repo, None)?;
            Ok(PullOutcome::done("merged"))
        }
        Some(PullStrategy::Rebase) => {
            ensure_clean(repo, "pull")?;
            let mut opts = rebase_options(repo);
            let mut rebase = repo
                .rebase(None, Some(&incoming), None, Some(&mut opts))
                .map_err(|e| format!("Rebase failed: {e}"))?;
            drive_rebase(repo, &mut rebase)
        }
    }
}

// ---------------------------------------------------------------------------
// Rebase onto a branch (the Git menu's Rebase…)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn git_rebase(
    path: String,
    onto: String,
    remote: bool,
    rewrite_pushed: Option<bool>,
) -> Result<PullOutcome, String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || {
        git_rebase_blocking(path, onto, remote, rewrite_pushed.unwrap_or(false))
    })
    .await
}

/// Rebases the current branch onto the branch `onto` (a remote-tracking one
/// when `remote`): the branch's commits that `onto` doesn't have are
/// replayed on top of it. "up-to-date" when the branch has all of `onto`
/// already. "rewrites-pushed", with nothing done, when some of the commits
/// are on the branch's upstream and `rewrite_pushed` isn't set: rewritten,
/// they could only be pushed by force. A step with conflicts stops it
/// ("conflicts"), to continue or abort as a pull's rebase.
pub fn git_rebase_blocking(
    path: String,
    onto: String,
    remote: bool,
    rewrite_pushed: bool,
) -> Result<PullOutcome, String> {
    let repo = open_repo(&path)?;
    if let Some(op) = state_name(repo.state()) {
        return Err(format!("A {op} is in progress: finish or abort it first."));
    }
    let head = repo
        .head()
        .map_err(|_| "There are no commits to rebase yet.".to_string())?;
    if !head.is_branch() {
        return Err("Check out a branch to rebase it.".to_string());
    }
    let branch = head.shorthand().unwrap_or("HEAD").to_string();
    if !remote && onto == branch {
        return Err(format!("{branch} can't be rebased onto itself."));
    }
    let kind = if remote {
        BranchType::Remote
    } else {
        BranchType::Local
    };
    let target = repo
        .find_branch(&onto, kind)
        .map_err(|_| format!("There's no branch {onto}."))?;
    let head_id = head.peel_to_commit().map_err(|e| e.to_string())?.id();
    let onto_id = target
        .get()
        .peel_to_commit()
        .map_err(|e| e.to_string())?
        .id();
    let has_it = repo
        .graph_descendant_of(head_id, onto_id)
        .map_err(|e| e.to_string())?;
    if head_id == onto_id || has_it {
        return Ok(PullOutcome::done("up-to-date"));
    }
    ensure_clean(&repo, "rebase")?;
    if !rewrite_pushed && replays_pushed(&repo, &branch, head_id, onto_id)? {
        return Ok(PullOutcome::done("rewrites-pushed"));
    }
    let incoming = repo
        .reference_to_annotated_commit(target.get())
        .map_err(|e| e.to_string())?;
    let mut opts = rebase_options(&repo);
    let mut rebase = repo
        .rebase(None, Some(&incoming), None, Some(&mut opts))
        .map_err(|e| format!("Rebase failed: {e}"))?;
    drive_rebase(&repo, &mut rebase)
}

/// Whether rebasing `branch` (at `head`) onto `onto` replays a commit its
/// upstream has: one that's been pushed.
fn replays_pushed(repo: &Repository, branch: &str, head: Oid, onto: Oid) -> Result<bool, String> {
    let upstream = repo
        .find_branch(branch, BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
        .and_then(|u| u.get().target());
    let Some(upstream) = upstream else {
        return Ok(false);
    };
    // The commits on `tip` that `onto` doesn't have.
    let own = |tip: Oid| -> Result<std::collections::HashSet<Oid>, String> {
        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.push(tip).map_err(|e| e.to_string())?;
        walk.hide(onto).map_err(|e| e.to_string())?;
        walk.collect::<Result<_, _>>().map_err(|e| e.to_string())
    };
    let pushed = own(upstream)?;
    Ok(own(head)?.iter().any(|c| pushed.contains(c)))
}

// ---------------------------------------------------------------------------
// Finishing a merge / rebase
// ---------------------------------------------------------------------------

/// MERGE_MSG without git's comment lines.
fn merge_message(repo: &Repository) -> String {
    let raw = repo.message().unwrap_or_default();
    let text: Vec<&str> = raw.lines().filter(|l| !l.starts_with('#')).collect();
    let text = text.join("\n").trim().to_string();
    if text.is_empty() {
        "Merge".to_string()
    } else {
        format!("{text}\n")
    }
}

fn commit_merge(repo: &Repository, message: Option<String>) -> Result<(), String> {
    let mut index = repo.index().map_err(|e| e.to_string())?;
    if index.has_conflicts() {
        return Err("Resolve the remaining conflicts first.".to_string());
    }
    let tree = repo
        .find_tree(index.write_tree().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let sig = signature(repo)?;
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| e.to_string())?;
    let mut ids: Vec<Oid> = Vec::new();
    // mergehead_foreach wants `&mut`; a second handle on the same repository will do.
    let mut handle = Repository::open(repo.path()).map_err(|e| e.to_string())?;
    handle
        .mergehead_foreach(|id| {
            ids.push(*id);
            true
        })
        .map_err(|e| e.to_string())?;
    let mut parents = vec![head];
    for id in ids {
        parents.push(repo.find_commit(id).map_err(|e| e.to_string())?);
    }
    let parents: Vec<&git2::Commit> = parents.iter().collect();
    let message = message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| merge_message(repo));
    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)
        .map_err(|e| e.to_string())?;
    repo.cleanup_state().map_err(|e| e.to_string())
}

/// Commits the rebase step just applied. A step whose change is upstream
/// already leaves nothing to commit: skipped, as git does.
fn commit_rebase_step(
    rebase: &mut git2::Rebase<'_>,
    sig: &git2::Signature<'_>,
) -> Result<(), String> {
    match rebase.commit(None, sig, None) {
        Ok(_) => Ok(()),
        Err(e) if e.code() == ErrorCode::Applied => Ok(()),
        Err(e) => Err(format!("Rebase commit failed: {e}")),
    }
}

/// Applies the remaining steps, stopping at the first one with conflicts.
fn drive_rebase(repo: &Repository, rebase: &mut git2::Rebase<'_>) -> Result<PullOutcome, String> {
    let sig = signature(repo)?;
    while let Some(op) = rebase.next() {
        op.map_err(|e| format!("Rebase failed: {e}"))?;
        let conflicts = conflicted_paths(repo)?;
        if !conflicts.is_empty() {
            return Ok(PullOutcome {
                result: "conflicts",
                conflicts,
            });
        }
        commit_rebase_step(rebase, &sig)?;
    }
    rebase
        .finish(Some(&sig))
        .map_err(|e| format!("Couldn't finish the rebase: {e}"))?;
    Ok(PullOutcome::done("rebased"))
}

#[tauri::command]
pub async fn git_operation_continue(
    path: String,
    message: Option<String>,
) -> Result<PullOutcome, String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_operation_continue_blocking(path, message)).await
}

/// After the conflicts are resolved: commits the merge, or commits the
/// rebase step and goes on with the next ones (which may conflict again).
pub fn git_operation_continue_blocking(
    path: String,
    message: Option<String>,
) -> Result<PullOutcome, String> {
    let repo = open_repo(&path)?;
    let remaining = conflicted_paths(&repo)?;
    if !remaining.is_empty() {
        return Err(format!(
            "Resolve the remaining conflicts first: {}",
            remaining.join(", ")
        ));
    }
    match repo.state() {
        RepositoryState::Merge => {
            commit_merge(&repo, message)?;
            Ok(PullOutcome::done("merged"))
        }
        RepositoryState::RebaseMerge
        | RepositoryState::Rebase
        | RepositoryState::RebaseInteractive => {
            let mut opts = rebase_options(&repo);
            let mut rebase = repo
                .open_rebase(Some(&mut opts))
                .map_err(|e| format!("Couldn't open the rebase in progress: {e}"))?;
            if rebase.operation_current().is_some() {
                commit_rebase_step(&mut rebase, &signature(&repo)?)?;
            }
            drive_rebase(&repo, &mut rebase)
        }
        RepositoryState::Clean => Err("No merge or rebase is in progress.".to_string()),
        other => Err(format!(
            "A {} is in progress: continue it from a terminal.",
            state_name(other).unwrap_or("git operation")
        )),
    }
}

#[tauri::command]
pub async fn git_operation_abort(path: String) -> Result<(), String> {
    // Isolated chats on the project wait while the working tree is rewritten.
    let _busy = rovik_core::overlay::chats::hold_busy(std::path::Path::new(&path));
    off_main_thread(move || git_operation_abort_blocking(path)).await
}

/// Back to where the branch was before the merge / rebase.
pub fn git_operation_abort_blocking(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    match repo.state() {
        RepositoryState::Clean => Ok(()),
        RepositoryState::RebaseMerge
        | RepositoryState::Rebase
        | RepositoryState::RebaseInteractive => {
            let mut rebase = repo.open_rebase(None).map_err(|e| e.to_string())?;
            rebase
                .abort()
                .map_err(|e| format!("Couldn't abort the rebase: {e}"))
        }
        // A merge (or cherry-pick / revert) hasn't moved HEAD: back to it.
        _ => {
            let head = repo
                .head()
                .and_then(|h| h.peel_to_commit())
                .map_err(|e| e.to_string())?;
            repo.reset(head.as_object(), ResetType::Hard, None)
                .map_err(|e| e.to_string())?;
            repo.cleanup_state().map_err(|e| e.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// What's in progress (for the status)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitOperation {
    /// "merge" | "rebase" | "cherry-pick" | "revert" | "bisect"
    pub kind: String,
    /// The branch the operation is on (during a rebase HEAD is detached).
    pub branch: Option<String>,
    /// What's coming in: the merged branch, or what a rebase replays onto.
    pub incoming: Option<String>,
    /// Rebase: the step being applied (1-based), of how many.
    pub step: Option<usize>,
    pub total: Option<usize>,
    /// Rebase: the summary of the commit being applied.
    pub commit: Option<String>,
    pub conflicts: Vec<String>,
    /// This app can continue it (merge, rebase); the rest only abort.
    pub can_continue: bool,
}

fn upstream_name(repo: &Repository, branch: &str) -> Option<String> {
    let b = repo.find_branch(branch, BranchType::Local).ok()?;
    let up = b.upstream().ok()?;
    up.name().ok().flatten().map(str::to_string)
}

/// The branch a rebase replays onto, as libgit2 noted it when it started
/// the rebase ("main", "refs/remotes/origin/main"): from the Git menu, any
/// branch, not the upstream. git's own rebases don't note it.
fn rebase_onto_name(repo: &Repository) -> Option<String> {
    let noted = std::fs::read_to_string(repo.path().join("rebase-merge/onto_name")).ok()?;
    let name = noted.trim();
    let name = name
        .strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/remotes/"))
        .unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

pub fn current_operation(repo: &Repository) -> Option<GitOperation> {
    let kind = state_name(repo.state())?;
    let conflicts = conflicted_paths(repo).unwrap_or_default();
    let mut op = GitOperation {
        kind: kind.to_string(),
        branch: None,
        incoming: None,
        step: None,
        total: None,
        commit: None,
        conflicts,
        can_continue: matches!(kind, "merge" | "rebase"),
    };
    if kind == "rebase" {
        if let Ok(mut rebase) = repo.open_rebase(None) {
            op.branch = rebase
                .orig_head_name()
                .ok()
                .flatten()
                .map(|n| n.trim_start_matches("refs/heads/").to_string());
            op.total = Some(rebase.len());
            if let Some(i) = rebase.operation_current() {
                op.step = Some(i + 1);
                let id = rebase.nth(i).map(|o| o.id());
                op.commit = id
                    .and_then(|id| repo.find_commit(id).ok())
                    .and_then(|c| c.summary().ok().flatten().map(str::to_string));
            }
        } else {
            op.can_continue = false;
        }
        op.incoming = rebase_onto_name(repo)
            .or_else(|| op.branch.as_deref().and_then(|b| upstream_name(repo, b)));
    } else {
        op.branch = repo
            .head()
            .ok()
            .filter(|h| h.is_branch())
            .and_then(|h| h.shorthand().ok().map(str::to_string));
        if kind == "merge" {
            op.incoming = merge_message(repo)
                .lines()
                .next()
                .and_then(|l| l.split('\'').nth(1).map(str::to_string))
                .or_else(|| op.branch.as_deref().and_then(|b| upstream_name(repo, b)));
        }
    }
    Some(op)
}

// ---------------------------------------------------------------------------
// One conflicted file
// ---------------------------------------------------------------------------

fn blob_bytes(repo: &Repository, id: Option<Oid>) -> Result<Option<Vec<u8>>, String> {
    match id {
        None => Ok(None),
        Some(id) => Ok(Some(
            repo.find_blob(id)
                .map_err(|e| e.to_string())?
                .content()
                .to_vec(),
        )),
    }
}

fn tree_blob_id(tree: Option<&git2::Tree<'_>>, file: &str) -> Option<Oid> {
    let entry = tree?.get_path(Path::new(file)).ok()?;
    (entry.kind() == Some(git2::ObjectType::Blob)).then(|| entry.id())
}

/// The three versions of a file that's no longer conflicted, from the
/// commits the operation is combining.
fn sides_from_operation(repo: &Repository, file: &str) -> Result<[Option<Oid>; 3], String> {
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| e.to_string())?;
    let theirs = match repo.state() {
        RepositoryState::Merge => {
            let mut ids = Vec::new();
            let mut handle = Repository::open(repo.path()).map_err(|e| e.to_string())?;
            handle
                .mergehead_foreach(|id| {
                    ids.push(*id);
                    true
                })
                .map_err(|e| e.to_string())?;
            ids.first()
                .copied()
                .and_then(|id| repo.find_commit(id).ok())
        }
        RepositoryState::RebaseMerge
        | RepositoryState::Rebase
        | RepositoryState::RebaseInteractive => {
            let mut rebase = repo.open_rebase(None).map_err(|e| e.to_string())?;
            let id = rebase
                .operation_current()
                .and_then(|i| rebase.nth(i).map(|o| o.id()));
            id.and_then(|id| repo.find_commit(id).ok())
        }
        _ => None,
    }
    .ok_or_else(|| format!("{file} isn't conflicted"))?;
    let base = match repo.state() {
        RepositoryState::Merge => repo
            .merge_base(head.id(), theirs.id())
            .ok()
            .and_then(|id| repo.find_commit(id).ok()),
        _ => theirs.parent(0).ok(),
    };
    Ok([
        tree_blob_id(base.and_then(|c| c.tree().ok()).as_ref(), file),
        tree_blob_id(head.tree().ok().as_ref(), file),
        tree_blob_id(theirs.tree().ok().as_ref(), file),
    ])
}

fn check_rel_path(file: &str) -> Result<&Path, String> {
    let rel = Path::new(file);
    if file.is_empty()
        || rel.is_absolute()
        || rel.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(format!("{file}: not a path inside the repository"));
    }
    Ok(rel)
}

#[tauri::command]
pub async fn git_conflict_file(path: String, file: String) -> Result<ConflictFile, String> {
    off_main_thread(move || git_conflict_file_blocking(path, file)).await
}

pub fn git_conflict_file_blocking(path: String, file: String) -> Result<ConflictFile, String> {
    let repo = open_repo(&path)?;
    let rel = check_rel_path(&file)?;
    let workdir = repo
        .workdir()
        .ok_or("the repository has no working folder")?
        .to_path_buf();

    let index = repo.index().map_err(|e| e.to_string())?;
    let staged = index.conflict_get(rel).ok();
    let resolved = staged.is_none();
    let [base_id, ours_id, theirs_id] = match staged {
        Some(c) => [
            c.ancestor.map(|e| e.id),
            c.our.map(|e| e.id),
            c.their.map(|e| e.id),
        ],
        None => sides_from_operation(&repo, &file)?,
    };
    let base = blob_bytes(&repo, base_id)?;
    let ours = blob_bytes(&repo, ours_id)?;
    let theirs = blob_bytes(&repo, theirs_id)?;
    let current = std::fs::read(workdir.join(rel)).ok();

    let binary = [&base, &ours, &theirs]
        .iter()
        .any(|b| b.as_deref().is_some_and(is_binary));
    let text = |b: &Option<Vec<u8>>| {
        b.as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    };

    let segments = if binary {
        Vec::new()
    } else if let (Some(o), Some(t)) = (&ours, &theirs) {
        // Cut as git cut them: the conflicts its markers show in the file.
        rovik_core::merge::conflict_segments(base.as_deref().unwrap_or(&[]), o, t)?
    } else {
        // One side deleted the file: it's all one conflict.
        vec![MergeSegment::Conflict {
            ours: text(&ours).unwrap_or_default(),
            base: text(&base).unwrap_or_default(),
            theirs: text(&theirs).unwrap_or_default(),
        }]
    };

    let (ours_label, theirs_label) = labels(&repo);
    Ok(ConflictFile {
        path: file,
        ours: text(&ours),
        theirs: text(&theirs),
        base: text(&base),
        binary,
        segments,
        resolved,
        current: if binary { None } else { text(&current) },
        ours_label,
        theirs_label,
    })
}

fn labels(repo: &Repository) -> (String, String) {
    let op = current_operation(repo);
    let incoming = op.as_ref().and_then(|o| o.incoming.clone());
    let branch = op.as_ref().and_then(|o| o.branch.clone());
    match op.as_ref().map(|o| o.kind.as_str()) {
        Some("rebase") => (
            format!("Upstream ({})", incoming.unwrap_or_else(|| "onto".into())),
            match op.and_then(|o| o.commit) {
                Some(c) => format!("Your commit: {c}"),
                None => format!("Your commit ({})", branch.unwrap_or_else(|| "local".into())),
            },
        ),
        _ => (
            format!("Yours ({})", branch.unwrap_or_else(|| "HEAD".into())),
            format!("Incoming ({})", incoming.unwrap_or_else(|| "theirs".into())),
        ),
    }
}

#[tauri::command]
pub async fn git_conflict_resolve(
    path: String,
    file: String,
    content: Option<String>,
    side: Option<String>,
) -> Result<(), String> {
    off_main_thread(move || git_conflict_resolve_blocking(path, file, content, side.as_deref()))
        .await
}

/// Writes the resolved file and stages it, which clears its conflict.
/// `side` ("ours" / "theirs") takes that whole version as it is, bytes and
/// all (a binary file); otherwise `content` is the file, and None means the
/// resolution is to delete it.
pub fn git_conflict_resolve_blocking(
    path: String,
    file: String,
    content: Option<String>,
    side: Option<&str>,
) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let rel = check_rel_path(&file)?;
    let abs = repo
        .workdir()
        .ok_or("the repository has no working folder")?
        .join(rel);
    let bytes: Option<Vec<u8>> = match side {
        Some(side) => {
            let index = repo.index().map_err(|e| e.to_string())?;
            let [_, ours, theirs] = match index.conflict_get(rel).ok() {
                Some(c) => [None, c.our.map(|e| e.id), c.their.map(|e| e.id)],
                None => sides_from_operation(&repo, &file)?,
            };
            match side {
                "ours" => blob_bytes(&repo, ours)?,
                "theirs" => blob_bytes(&repo, theirs)?,
                other => return Err(format!("unknown side: {other}")),
            }
        }
        None => content.map(String::into_bytes),
    };
    let mut index = repo.index().map_err(|e| e.to_string())?;
    match bytes {
        Some(bytes) => {
            if let Some(dir) = abs.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{file}: {e}"))?;
            }
            std::fs::write(&abs, bytes).map_err(|e| format!("{file}: couldn't write it: {e}"))?;
            index.add_path(rel).map_err(|e| e.to_string())?;
        }
        None => {
            if abs.exists() {
                std::fs::remove_file(&abs)
                    .map_err(|e| format!("{file}: couldn't delete it: {e}"))?;
            }
            let _ = index.remove_path(rel);
        }
    }
    let _ = index.conflict_remove(rel);
    index.write().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;

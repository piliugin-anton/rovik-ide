// git.rs
//
// Cross-platform Git backend for the IDE's GitPanel, built on `git2`
// (Rust bindings for libgit2) rather than shelling out to a `git` binary.
// This means it works even on machines without git installed, and behaves
// identically on Windows/macOS/Linux.

use git2::{
    BranchType, Cred, CredentialHelper, CredentialType, FetchOptions, PushOptions,
    RemoteCallbacks, Repository, Status, StatusOptions,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitFile {
    pub path: String,
    pub status: String, // "M" | "A" | "D" | "R" | "U" | "?"
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<GitFile>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_repo(path: &str) -> Result<Repository, String> {
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
fn make_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        if allowed_types.contains(CredentialType::SSH_KEY) {
            let username = username_from_url.unwrap_or("git");

            if let Ok(cred) = Cred::ssh_key_from_agent(username) {
                return Ok(cred);
            }

            if let Some(home) = home_dir() {
                let ssh_dir = home.join(".ssh");
                for key_name in ["id_ed25519", "id_rsa"] {
                    let private_key = ssh_dir.join(key_name);
                    if private_key.exists() {
                        if let Ok(cred) = Cred::ssh_key(username, None, &private_key, None) {
                            return Ok(cred);
                        }
                    }
                }
            }
        }

        if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) {
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

        Cred::default()
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
        .filter_map(|r| r.ok().unwrap())
        .map(|s| s.to_string())
        .next()
        .ok_or_else(|| "No remotes configured".to_string())

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
pub fn git_status(path: String) -> Result<GitStatus, String> {
    let repo = open_repo(&path)?;

    let branch = match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().unwrap_or("HEAD").to_string(),
        Ok(head) => {
            let oid = head.target().map(|o| o.to_string()).unwrap_or_default();

            format!("detached@{}", &oid[..oid.len().min(7)])
        }
        Err(_) => "unborn".to_string(), // repo has no commits yet
    };

    let (ahead, behind) = compute_ahead_behind(&repo).unwrap_or((0, 0));

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;

    let files = statuses
        .iter()
        .filter(|entry| !entry.status().is_ignored())
        .map(|entry| GitFile {
            path: entry.path().unwrap_or_default().to_string(),
            status: classify_status(entry.status()).to_string(),
        })
        .collect();

    Ok(GitStatus {
        branch,
        ahead,
        behind,
        files,
    })
}

#[tauri::command]
pub fn git_branches(path: String) -> Result<Vec<String>, String> {
    let repo = open_repo(&path)?;
    let mut names = Vec::new();

    let branches = repo
        .branches(Some(BranchType::Local))
        .map_err(|e| e.to_string())?;

    for item in branches {
        let (branch, _) = item.map_err(|e| e.to_string())?;
        if let Some(name) = branch.name().map_err(|e| e.to_string())? {
            names.push(name.to_string());
        }
    }

    names.sort();
    Ok(names)
}

#[tauri::command]
pub fn git_checkout(path: String, branch: String) -> Result<(), String> {
    let repo = open_repo(&path)?;

    let (object, reference) = repo
        .revparse_ext(&branch)
        .map_err(|e| format!("Branch '{branch}' not found: {e}"))?;

    repo.checkout_tree(&object, None)
        .map_err(|e| format!("Checkout failed: {e}"))?;

    match reference {
        Some(r) => repo.set_head(r.name().map_err(|e| format!("Invalid reference name: {e}"))?),
        None => repo.set_head_detached(object.id()),
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn git_commit(path: String, message: String) -> Result<(), String> {
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
pub fn git_fetch(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let remote_name = default_remote_name(&repo)?;
    let mut remote = repo.find_remote(&remote_name).map_err(|e| e.to_string())?;

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks());

    let refspecs: Vec<String> = remote
        .fetch_refspecs()
        .map_err(|e| e.to_string())?
        .iter()
        .filter_map(|r| r.ok().unwrap())
        .map(String::from)
        .collect();

    remote
        .fetch(&refspecs, Some(&mut fetch_opts), None)
        .map_err(|e| format!("Fetch failed: {e}"))
}

#[tauri::command]
pub fn git_pull(path: String) -> Result<(), String> {
    git_fetch(path.clone())?;

    let repo = open_repo(&path)?;
    let head = repo.head().map_err(|e| e.to_string())?;

    if !head.is_branch() {
        return Err("Cannot pull: HEAD is detached, not on a branch".to_string());
    }

    let branch_name = head
        .shorthand()
        .map_err(|e| format!("Could not determine current branch name: {e}"))?
        .to_string();

    let branch = repo
        .find_branch(&branch_name, BranchType::Local)
        .map_err(|e| e.to_string())?;

    let upstream = branch
        .upstream()
        .map_err(|_| format!("Branch '{branch_name}' has no upstream configured"))?;

    let upstream_oid = upstream
        .get()
        .target()
        .ok_or("Upstream branch has no target commit")?;

    let upstream_commit = repo
        .find_annotated_commit(upstream_oid)
        .map_err(|e| e.to_string())?;

    let (analysis, _) = repo
        .merge_analysis(&[&upstream_commit])
        .map_err(|e| e.to_string())?;

    if analysis.is_up_to_date() {
        return Ok(());
    }

    if analysis.is_fast_forward() {
        let refname = format!("refs/heads/{branch_name}");
        let mut reference = repo.find_reference(&refname).map_err(|e| e.to_string())?;
        reference
            .set_target(upstream_oid, "Fast-forward")
            .map_err(|e| e.to_string())?;
        repo.set_head(&refname).map_err(|e| e.to_string())?;
        return repo
            .checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .map_err(|e| e.to_string());
    }

    // A real (non-fast-forward) merge needs conflict handling that's out of
    // scope for a lightweight panel command — surface it clearly instead of
    // silently doing something surprising.
    Err(
        "Pull requires a merge (branches have diverged). Please merge from a terminal."
            .to_string(),
    )
}

#[tauri::command]
pub fn git_push(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    let head = repo.head().map_err(|e| e.to_string())?;

    if !head.is_branch() {
        return Err("Cannot push: HEAD is detached, not on a branch".to_string());
    }

    let branch_name = head
        .shorthand()
        .map_err(|e| format!("Could not determine current branch name: {e}"))?
        .to_string();

    let remote_name = default_remote_name(&repo)?;
    let mut remote = repo.find_remote(&remote_name).map_err(|e| e.to_string())?;

    let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");
    let mut push_opts = PushOptions::new();
    push_opts.remote_callbacks(make_callbacks());

    remote
        .push(&[&refspec], Some(&mut push_opts))
        .map_err(|e| format!("Push failed: {e}"))?;

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

    Ok(())
}

#[tauri::command]
pub fn git_init(path: String) -> Result<(), String> {
    Repository::init(&path).map_err(|e| format!("Failed to initialize repository: {e}"))?;
    Ok(())
}
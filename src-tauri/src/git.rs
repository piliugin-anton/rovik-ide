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
    /// Changed in the index (staged) / in the working tree (not staged).
    pub staged: bool,
    pub unstaged: bool,
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
        .map(|entry| {
            let s = entry.status();
            GitFile {
                path: entry.path().unwrap_or_default().to_string(),
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
            .map(|w| w.to_string_lossy().trim_end_matches(['/', '\\']).to_string())
            .unwrap_or_default(),
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

// ---------------------------------------------------------------------------
// One file: its earlier version, and reverting to it
// ---------------------------------------------------------------------------

/// The version a revert goes back to: the staged one when the file has
/// unstaged changes (what `git restore` restores), else the last commit's.
/// None: the file is new (untracked, or added and never committed).
fn base_blob<'r>(repo: &'r Repository, file: &str) -> Result<Option<git2::Blob<'r>>, String> {
    let status = repo.status_file(std::path::Path::new(file)).map_err(|e| e.to_string())?;
    let index = repo.index().map_err(|e| e.to_string())?;
    let from_index = || -> Option<git2::Oid> { index.get_path(std::path::Path::new(file), 0).map(|e| e.id) };
    let from_head = || -> Option<git2::Oid> {
        let tree = repo.head().ok()?.peel_to_tree().ok()?;
        tree.get_path(std::path::Path::new(file)).ok().map(|e| e.id())
    };
    let unstaged = status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_TYPECHANGE);
    let oid = if unstaged { from_index().or_else(from_head) } else { from_head() };
    Ok(match oid {
        Some(oid) => Some(repo.find_blob(oid).map_err(|e| e.to_string())?),
        None => None,
    })
}

/// The file's earlier version (see base_blob), for the editor's change marks.
/// `file` is relative to the repository's working folder. None: new file.
#[tauri::command]
pub fn git_base_content(path: String, file: String) -> Result<Option<String>, String> {
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
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
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
pub fn git_revert_file(path: String, file: String) -> Result<RevertOutcome, String> {
    let repo = open_repo(&path)?;
    let workdir = repo.workdir().ok_or("the repository has no working folder")?.to_path_buf();
    let rel = std::path::Path::new(&file);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("{file}: not a path inside the repository"));
    }
    let abs = workdir.join(rel);
    let status = repo.status_file(rel).map_err(|e| e.to_string())?;
    if status.is_conflicted() {
        return Err(format!("{file} has merge conflicts: resolve them first"));
    }

    // New: nothing to go back to. Out of the index, into the trash.
    if base_blob(&repo, &file)?.is_none() {
        if status.is_index_new() {
            let mut index = repo.index().map_err(|e| e.to_string())?;
            index.remove_path(rel).map_err(|e| e.to_string())?;
            index.write().map_err(|e| e.to_string())?;
        }
        if abs.exists() {
            trash::delete(&abs).map_err(|e| format!("{file}: couldn't move it to the trash: {e}"))?;
        }
        return Ok(RevertOutcome { backed_up: false, removed: true });
    }

    // The current version to the trash, under a name of its own.
    let mut backed_up = false;
    if abs.is_file() {
        let copy = backup_name(&abs);
        std::fs::copy(&abs, &copy).map_err(|e| format!("{file}: couldn't keep a copy: {e}"))?;
        match trash::delete(&copy) {
            Ok(()) => backed_up = true,
            Err(e) => {
                let _ = std::fs::remove_file(&copy);
                return Err(format!(
                    "{file}: couldn't put a copy of your version in the trash ({e}), so nothing was reverted"
                ));
            }
        }
    }

    let unstaged = status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_TYPECHANGE);
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().path(rel);
    if unstaged {
        // Back to the staged version.
        repo.checkout_index(None, Some(&mut checkout)).map_err(|e| e.to_string())?;
    } else {
        // Only staged changes: back to the last commit, index included.
        let head = repo.head().and_then(|h| h.peel_to_commit()).map_err(|e| e.to_string())?;
        repo.reset_default(Some(head.as_object()), [rel]).map_err(|e| e.to_string())?;
        repo.checkout_head(Some(&mut checkout)).map_err(|e| e.to_string())?;
    }
    Ok(RevertOutcome { backed_up, removed: false })
}

#[cfg(test)]
mod revert_tests {
    use super::*;
    use std::fs;

    fn repo_with_commit() -> (PathBuf, Repository) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("git-revert-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let repo = Repository::init(&dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
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
        assert_eq!(git_base_content(path(&dir), "src/a.rs".into()).unwrap().as_deref(), Some("fn a() {}\n"));
        let out = git_revert_file(path(&dir), "src/a.rs".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("src/a.rs")).unwrap(), "fn a() {}\n");
        assert!(out.backed_up && !out.removed);
        assert!(!dir.join("src/a (before revert).rs").exists(), "the copy went to the trash");
    }

    #[test]
    fn deleted_file_comes_back() {
        let (dir, _repo) = repo_with_commit();
        fs::remove_file(dir.join("b.txt")).unwrap();
        let out = git_revert_file(path(&dir), "b.txt".into()).unwrap();
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
        assert_eq!(git_base_content(path(&dir), "b.txt".into()).unwrap().as_deref(), Some("b\n"));
        git_revert_file(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        let st = git_status(path(&dir)).unwrap();
        assert!(st.files.iter().all(|f| f.path != "b.txt"), "clean again: {:?}", st.files);
    }

    #[test]
    fn staged_and_unstaged_goes_back_to_the_staged_version() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        fs::write(dir.join("b.txt"), "staged\nand more\n").unwrap();
        git_revert_file(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "staged\n");
    }

    #[test]
    fn new_files_go_to_the_trash() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("new.txt"), "x").unwrap();
        assert_eq!(git_base_content(path(&dir), "new.txt".into()).unwrap(), None);
        let out = git_revert_file(path(&dir), "new.txt".into()).unwrap();
        assert!(out.removed && !dir.join("new.txt").exists());
        // Added to the index but never committed: same, and out of the index.
        fs::write(dir.join("added.txt"), "y").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("added.txt")).unwrap();
        index.write().unwrap();
        git_revert_file(path(&dir), "added.txt".into()).unwrap();
        assert!(!dir.join("added.txt").exists());
        assert!(git_status(path(&dir)).unwrap().files.is_empty());
    }

    #[test]
    fn refuses_paths_outside_the_repository() {
        let (dir, _repo) = repo_with_commit();
        assert!(git_revert_file(path(&dir), "../etc/passwd".into()).is_err());
    }

    #[test]
    fn status_reports_workdir_and_staging() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "changed\n").unwrap();
        let st = git_status(path(&dir.join("src"))).unwrap();
        assert_eq!(st.workdir, path(&dir).trim_end_matches('/'));
        let b = st.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.unstaged && !b.staged);
    }
}

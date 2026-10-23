//! The remotes: which one a branch fetches from and pushes to, the
//! credentials that get there, and the fetch and the push themselves.

use super::hooks::Hooks;
use super::{open_repo, ErrText};
use crate::util::blocking;
use git2::{
    BranchType, Cred, CredentialHelper, CredentialType, FetchOptions, PushOptions, RemoteCallbacks,
    Repository,
};
use serde::Serialize;
use std::path::PathBuf;

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
            if let Some((username, password)) = remembered_credential(url, username_from_url) {
                if let Ok(cred) = Cred::userpass_plaintext(&username, &password) {
                    return Ok(cred);
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

/// The username and password git remembers for `url`: what `git credential
/// fill` answers, from the helpers git is set up with, without a prompt.
/// git is started as every program the app starts is: run from an
/// AppImage, a helper such as libsecret's would load the AppImage's
/// libraries if git2 started it (it can't be told the environment). Where
/// git isn't installed, git2 asks the helpers itself.
fn remembered_credential(url: &str, username: Option<&str>) -> Option<(String, String)> {
    if let Some(answer) = credential_fill(rovik_core::process::command("git"), url, username) {
        return answer;
    }
    let config = git2::Config::open_default().ok()?;
    let mut helper = CredentialHelper::new(url);
    helper.config(&config);
    if let Some(username) = username {
        helper.username(Some(username));
    }
    helper.execute()
}

/// `git credential fill` for `url`, run with `git`: its username and
/// password (None when it has none), or None when git couldn't be started.
pub(super) fn credential_fill(
    mut git: std::process::Command,
    url: &str,
    username: Option<&str>,
) -> Option<Option<(String, String)>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = rovik_core::process::no_window(&mut git)
        .args(["credential", "fill"])
        // No terminal prompt and no askpass dialog: an empty GIT_ASKPASS
        // keeps git from core.askPass and SSH_ASKPASS too.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = format!("url={url}\n");
    if let Some(username) = username {
        input.push_str(&format!("username={username}\n"));
    }
    input.push('\n');
    if let Some(mut stdin) = child.stdin.take() {
        // git, done early, closed the pipe: its answer is read all the same.
        let _ = stdin.write_all(input.as_bytes());
    }
    let answer = child
        .wait_with_output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            let field = |key: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                    .map(str::to_string)
            };
            field("username").zip(field("password"))
        });
    Some(answer)
}

fn default_remote_name(repo: &Repository) -> Result<String, String> {
    let remotes = repo.remotes().err_text()?;
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
pub(super) fn push_remote_name(repo: &Repository, branch: &str) -> Result<String, String> {
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

#[tauri::command]
pub async fn git_fetch(path: String) -> Result<(), String> {
    blocking(move || git_fetch_blocking(path)).await
}

pub fn git_fetch_blocking(path: String) -> Result<(), String> {
    let repo = open_repo(&path)?;
    // Like `git fetch`: the current branch's remote, else origin.
    let remote_name = match upstream_remote_name(&repo) {
        Some(name) if repo.find_remote(&name).is_ok() => name,
        _ => default_remote_name(&repo)?,
    };
    let mut remote = repo.find_remote(&remote_name).err_text()?;

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks());

    let refspecs: Vec<String> = remote
        .fetch_refspecs()
        .err_text()?
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

impl PushOutcome {
    fn rejected(message: String) -> Self {
        PushOutcome {
            result: "rejected",
            message: Some(message),
        }
    }
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
pub async fn git_push(path: String, no_verify: Option<bool>) -> Result<PushOutcome, String> {
    blocking(move || git_push_blocking_with(path, no_verify.unwrap_or(false))).await
}

/// With the hooks, as the tests run it.
#[cfg(test)]
pub fn git_push_blocking(path: String) -> Result<PushOutcome, String> {
    git_push_blocking_with(path, false)
}

/// Pushes the current branch. `no_verify`: as git's `--no-verify`, without
/// the pre-push hook.
pub fn git_push_blocking_with(path: String, no_verify: bool) -> Result<PushOutcome, String> {
    let repo = open_repo(&path)?;
    let head = repo.head().err_text()?;

    if !head.is_branch() {
        return Err("Cannot push: HEAD is detached, not on a branch".to_string());
    }

    let branch_name = head
        .shorthand()
        .map_err(|e| format!("Could not determine current branch name: {e}"))?
        .to_string();

    let remote_name = push_remote_name(&repo, &branch_name)?;
    let mut remote = repo.find_remote(&remote_name).err_text()?;

    let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");
    // The server turns a ref down through this callback, not as an error.
    let refused = std::cell::RefCell::new(None::<String>);
    let hooks = Hooks::of(&repo).no_verify(no_verify);
    let url = match (remote.pushurl(), remote.url()) {
        (Ok(Some(url)), _) | (_, Ok(url)) if !url.is_empty() => url.to_string(),
        _ => remote_name.clone(),
    };
    let hook_failed = std::cell::RefCell::new(None::<String>);
    let mut callbacks = make_callbacks();
    callbacks.push_update_reference(|_, status| {
        if let Some(reason) = status {
            *refused.borrow_mut() = Some(reason.to_string());
        }
        Ok(())
    });
    if hooks.has("pre-push") {
        // Where git runs it: the remote's refs known, nothing sent yet. Its
        // input is a line per ref, as git writes it: `<local ref> <local
        // id> <remote ref> <remote id>`, zeros for a ref the remote hasn't.
        callbacks.push_negotiation(|updates| {
            let input: String = updates
                .iter()
                .map(|u| {
                    format!(
                        "{} {} {} {}\n",
                        u.src_refname().unwrap_or_default(),
                        u.dst(),
                        u.dst_refname().unwrap_or_default(),
                        u.src()
                    )
                })
                .collect();
            hooks
                .run("pre-push", &[&remote_name, &url], Some(input.as_bytes()))
                .map_err(|e| {
                    *hook_failed.borrow_mut() = Some(e);
                    git2::Error::from_str("the pre-push hook failed")
                })
        });
    }
    let mut push_opts = PushOptions::new();
    push_opts.remote_callbacks(callbacks);

    let pushed = remote.push(&[&refspec], Some(&mut push_opts));
    drop(push_opts);
    if let Some(e) = hook_failed.into_inner() {
        return Err(format!("Not pushed: {e}"));
    }
    match pushed {
        Err(e) if e.code() == git2::ErrorCode::NotFastForward || is_behind_remote(e.message()) => {
            return Ok(PushOutcome::rejected(e.message().to_string()));
        }
        Err(e) => return Err(format!("Push failed: {e}")),
        Ok(()) => {}
    }
    if let Some(reason) = refused.into_inner() {
        if is_behind_remote(&reason) {
            return Ok(PushOutcome::rejected(reason));
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

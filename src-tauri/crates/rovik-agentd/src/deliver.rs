//! Delivery on a server (docs/design/autopilot-plan.md D15, H04): a
//! landable result of an Autopilot task, committed as the chat's result,
//! is pushed to a branch `rovik/<task>` on the project's remote with the
//! deploy key the computer named, and a pull request opens for it when the
//! computer asked and `gh` is logged in here. The core asks at the task's
//! delivery (`Deliverer`), before the report is written; the report
//! becomes the pull request's description once it is.
//!
//! The system `git` pushes, with `ssh` and the key for an SSH remote, so
//! the daemon's static build needs no SSH of its own. Nothing waits for a
//! person: no prompt, no password; a push that can't go fails with git's
//! own words, and the result waits here for its computer, as it would
//! without a push. The result ref and its `ResultReady` stay as they are.

use crate::results::Results;
use rovik_core::agent_run::autopilot::{DeliverAsk, Deliverer, Delivering, Pushed};
use rovik_protocol::autopilot::{Delivery, DeliveryTest};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// How long a push or a `gh` call may take.
const WAIT: Duration = Duration::from_secs(300);

/// The daemon's delivery, for the core (`AgentState::set_deliverer`).
pub struct Pushing(pub Arc<Results>);

impl Deliverer for Pushing {
    fn push(&self, ask: DeliverAsk) -> Delivering<Option<Pushed>> {
        let results = self.0.clone();
        Box::pin(async move { results.deliver(ask).await })
    }

    fn describe(&self, dialog_id: String, pr: String, report: String) -> Delivering<()> {
        Box::pin(async move {
            let edit = gh(&["pr", "edit", &pr, "--body-file", "-"], &report).await;
            if let Err(e) = edit {
                log::warn!("chat {dialog_id}: its pull request kept the IDE's description: {e}");
            }
        })
    }
}

/// The task's branch: `rovik/`, its title in lowercase words joined by
/// dashes (at most 40 characters), and the first six characters of its
/// id, so two tasks of the same name don't meet.
pub fn branch_name(title: &str, task: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if slug.len() >= 40 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let id: String = task
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(6)
        .collect::<String>()
        .to_ascii_lowercase();
    match (slug.is_empty(), id.is_empty()) {
        (false, false) => format!("rovik/{slug}-{id}"),
        (true, false) => format!("rovik/task-{id}"),
        (false, true) => format!("rovik/{slug}"),
        (true, true) => "rovik/task".into(),
    }
}

/// A remote on this server itself, which no SSH reaches.
fn is_local(remote: &str) -> bool {
    remote.starts_with("file://") || remote.starts_with('/')
}

/// `~/` in a path on the server, as the daemon's home.
fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

/// `text` as one word of `sh`, quoted.
fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// What a failed command said, in a line or a few: its last lines that
/// say anything, at most 400 characters.
fn said(stderr: &[u8], stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(if stderr.is_empty() { stdout } else { stderr });
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
        .collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" ");
    let tail = tail
        .trim_start_matches("error: ")
        .trim_start_matches("fatal: ");
    match tail.char_indices().nth(400) {
        Some((cut, _)) => format!("{}…", &tail[..cut]),
        None if tail.is_empty() => "it ended without saying why".into(),
        None => tail.to_string(),
    }
}

/// Pushes `commit` of the repository at `repo` to `branch` on `remote`,
/// with the deploy key at `key` for an SSH remote. Never forced: a branch
/// of that name with other commits stays, and the push says so.
pub async fn push(
    repo: &Path,
    remote: &str,
    commit: &str,
    branch: &str,
    key: &str,
) -> Result<(), String> {
    let refspec = format!("{commit}:refs/heads/{branch}");
    git_push(repo, remote, &refspec, key).await
}

/// `git push` of `refspec` to `remote`, from the repository at `repo`.
async fn git_push(repo: &Path, remote: &str, refspec: &str, key: &str) -> Result<(), String> {
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(repo)
        .args(["push", "--porcelain", remote, refspec]);
    git.env("GIT_TERMINAL_PROMPT", "0");
    if !is_local(remote) {
        let key = expand_home(key);
        if !Path::new(&key).is_file() {
            return Err(format!("the deploy key isn't at {key} on the server"));
        }
        git.env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
                shell_word(&key)
            ),
        );
    }
    let out = run(git, None).await.map_err(|e| match e {
        Run::Missing => "git isn't installed on the server".to_string(),
        Run::Failed(why) => why,
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(said(&out.stderr, &out.stdout))
    }
}

/// `host/owner/repo` of an SSH remote, as `gh --repo` takes it: its host
/// and the last two folders of its path. None: a remote on this server.
pub fn forge_repo(remote: &str) -> Option<String> {
    if is_local(remote) {
        return None;
    }
    let (host, path) = match remote.strip_prefix("ssh://") {
        Some(rest) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            (host.split(':').next()?, path)
        }
        None => {
            let (authority, path) = remote.split_once(':')?;
            (authority.rsplit('@').next()?, path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let [.., owner, repo] = parts.as_slice() else {
        return None;
    };
    (!host.is_empty()).then(|| format!("{host}/{owner}/{repo}"))
}

/// Opens the pull request of `branch`, on `remote`'s repository, into its
/// default branch: its URL, or why it didn't open.
pub async fn open_pull_request(
    remote: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let Some(repo) = forge_repo(remote) else {
        return Err("the remote is on this server, where no pull request opens".into());
    };
    let title = if title.trim().is_empty() {
        branch
    } else {
        title
    };
    let args = [
        "pr",
        "create",
        "--repo",
        &repo,
        "--head",
        branch,
        "--title",
        title,
        "--body-file",
        "-",
    ];
    let out = gh(&args, body).await?;
    let url = out
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("https://") || l.starts_with("http://"));
    url.map(str::to_string)
        .ok_or_else(|| "`gh` opened no pull request it named".into())
}

/// "Test delivery" (3.5, H05): an empty branch `rovik/test-<date>`, of a
/// commit with nothing in it, pushed to `remote` as a task's result would
/// go, with the deploy key, then deleted. With pull requests asked for,
/// whether `gh` is logged in for the remote's host. Err: this computer
/// has this server keep its results.
pub async fn test(delivery: &Delivery, remote: &str) -> Result<DeliveryTest, String> {
    let Delivery::PushBranch {
        deploy_key,
        open_pr,
    } = delivery
    else {
        return Err(
            "This server keeps this computer's results: choose “Push to a branch” first.".into(),
        );
    };
    let iso = rovik_core::agent_run::log::iso_now();
    let stamp = format!(
        "{}-{}",
        iso[..10].replace('-', ""),
        iso[11..19].replace(':', "")
    );
    let mut report = DeliveryTest {
        remote: remote.to_string(),
        branch: format!("rovik/test-{stamp}"),
        ..DeliveryTest::default()
    };
    let scratch =
        std::env::temp_dir().join(format!("rovik-delivery-test-{}", uuid::Uuid::new_v4()));
    let made = {
        let scratch = scratch.clone();
        rovik_core::rt::spawn_blocking(move || empty_commit(&scratch)).await
    };
    let commit = made.map_err(|e| e.to_string()).and_then(|made| made);
    report.problem = match commit {
        Err(e) => Some(format!("Its commit couldn't be made: {e}")),
        Ok(commit) => match push(&scratch, remote, &commit, &report.branch, deploy_key).await {
            Err(e) => Some(format!("The push failed: {e}")),
            Ok(()) => {
                let delete = format!(":refs/heads/{}", report.branch);
                git_push(&scratch, remote, &delete, deploy_key)
                    .await
                    .err()
                    .map(|e| format!("Pushed, but the branch couldn't be deleted again: {e}"))
            }
        },
    };
    let _ = std::fs::remove_dir_all(&scratch);
    if *open_pr {
        report.pull_requests = Some(match gh_ready(remote).await {
            Ok(host) => format!("`gh` is logged in to {host}: pull requests open."),
            Err(e) => format!("Pull requests won't open: {e}."),
        });
    }
    Ok(report)
}

/// A bare repository at `dir` with one commit, of the empty tree: its id.
fn empty_commit(dir: &Path) -> Result<String, String> {
    let git = |e: git2::Error| e.message().to_string();
    let repo = git2::Repository::init_bare(dir).map_err(git)?;
    let tree = repo
        .treebuilder(None)
        .and_then(|t| t.write())
        .map_err(git)?;
    let tree = repo.find_tree(tree).map_err(git)?;
    let rovik = git2::Signature::now("Rovik", "results@rovik.invalid").map_err(git)?;
    let commit = repo
        .commit(None, &rovik, &rovik, "Rovik: a delivery test", &tree, &[])
        .map_err(git)?;
    Ok(commit.to_string())
}

/// The remote's host, when `gh` is logged in for it.
async fn gh_ready(remote: &str) -> Result<String, String> {
    let Some(repo) = forge_repo(remote) else {
        return Err("the remote is on this server, where no pull request opens".into());
    };
    let host = repo.split('/').next().unwrap_or(&repo).to_string();
    gh(&["auth", "status", "--hostname", &host], "").await?;
    Ok(host)
}

/// `gh` with `args`, `input` on its standard input: what it printed.
async fn gh(args: &[&str], input: &str) -> Result<String, String> {
    let mut gh = Command::new("gh");
    gh.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    let out = run(gh, Some(input)).await.map_err(|e| match e {
        Run::Missing => "`gh` isn't installed on the server".to_string(),
        Run::Failed(why) => why,
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(said(&out.stderr, &out.stdout))
    }
}

enum Run {
    /// The program isn't installed.
    Missing,
    Failed(String),
}

/// Runs `command`, `input` on its standard input, within `WAIT`.
async fn run(mut command: Command, input: Option<&str>) -> Result<std::process::Output, Run> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Run::Missing,
        _ => Run::Failed(e.to_string()),
    })?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let text = text.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(text.as_bytes()).await;
        });
    }
    match tokio::time::timeout(WAIT, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(Run::Failed(e.to_string())),
        Err(_) => Err(Run::Failed("it took longer than 5 minutes".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tasks_branch_is_its_name_and_its_id() {
        let id = "bea73f40-24fe-442a-b016-5183fe17f8e5";
        assert_eq!(
            branch_name("Make the test pass", id),
            "rovik/make-the-test-pass-bea73f"
        );
        assert_eq!(
            branch_name("Fix: `parse()` — ünïcode, again!", "u1"),
            "rovik/fix-parse-n-code-again-u1"
        );
        assert_eq!(branch_name("", "u1"), "rovik/task-u1");
        let long = branch_name(&"word ".repeat(30), "abcdef123");
        assert!(long.len() <= "rovik/".len() + 40 + 7, "{long}");
        assert!(long.ends_with("-abcdef"), "{long}");
        assert!(!long.contains("--"), "{long}");
    }

    #[test]
    fn a_remotes_repository_for_gh() {
        let cases = [
            ("git@github.com:acme/app.git", Some("github.com/acme/app")),
            (
                "ssh://git@github.com/acme/app.git",
                Some("github.com/acme/app"),
            ),
            (
                "ssh://me@127.0.0.1:2222/srv/git/team/app.git",
                Some("127.0.0.1/team/app"),
            ),
            (
                "git@gitlab.example.com:group/sub/app",
                Some("gitlab.example.com/sub/app"),
            ),
            ("file:///srv/app.git", None),
            ("/srv/app.git", None),
            ("git@github.com:app.git", None),
        ];
        for (remote, expected) in cases {
            assert_eq!(forge_repo(remote).as_deref(), expected, "{remote}");
        }
    }

    #[test]
    fn what_a_failed_command_said() {
        let stderr = b"hint: try again\nerror: failed to push some refs to 'x'\nfatal: Permission denied (publickey).\n";
        assert_eq!(
            said(stderr, b""),
            "failed to push some refs to 'x' fatal: Permission denied (publickey)."
        );
        assert_eq!(said(b"", b""), "it ended without saying why");
        assert_eq!(shell_word("/home/a b/it's"), r"'/home/a b/it'\''s'");
    }
}

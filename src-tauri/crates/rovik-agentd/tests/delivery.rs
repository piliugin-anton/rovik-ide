//! Delivery on a server (docs/design/autopilot-plan.md D15, H04): an
//! Autopilot task's landable result goes to a branch `rovik/<task>` on the
//! project's remote, over a file URL or SSH with the deploy key, and its
//! pull request opens through `gh` when the computer asked; a push that
//! fails is a failed delivery, and the result still reaches the computer.

mod common;

use common::project::{Setup, CHAT, PROJECT};
use common::ssh::{SshServer, USER};
use common::task::*;
use common::{commit_all, write, Daemon};
use git2::{Oid, Repository};
use rovik_core::testing::Reply;
use rovik_protocol::autopilot::{AutonomyConfig, Delivery};
use rovik_protocol::work::{ChatResult, Project};
use rovik_protocol::Request;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The branch of the task `task_start` starts: its plan's title, its
/// message's id.
const BRANCH: &str = "rovik/make-the-test-pass-u1";

/// The task's project, its test passing once ok.txt says ok, committed.
fn npm_project(setup: &Setup) {
    let files = [
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        (
            "check.js",
            "const fs = require('fs'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== 'ok') { console.error('not ok'); process.exit(1); }",
        ),
        ("ok.txt", "no"),
    ];
    for (path, content) in files {
        write(&setup.project.join(path), content);
    }
    commit_all(&setup.project);
}

/// A bare repository beside the project with its history on `main`, as its
/// forge would have it.
fn bare_remote(setup: &Setup) -> PathBuf {
    let remote = setup.daemon.dir.0.join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let pushed = std::process::Command::new("git")
        .arg("-C")
        .arg(&setup.project)
        .arg("push")
        .arg(&remote)
        .arg("HEAD:refs/heads/main")
        .output()
        .unwrap();
    assert!(pushed.status.success(), "{pushed:?}");
    remote
}

/// The chat's worktree, its project's remote `remote`, and its computer
/// having the server push its results there with `key`.
async fn ready(setup: &Setup, remote: &str, key: &str, open_pr: bool) {
    ready_telling(setup, remote, key, open_pr, None).await
}

/// `ready`, the task's end told to `notify` (H06).
async fn ready_telling(
    setup: &Setup,
    remote: &str,
    key: &str,
    open_pr: bool,
    notify: Option<&str>,
) {
    let push = AutonomyConfig {
        delivery: Delivery::PushBranch {
            deploy_key: key.into(),
            open_pr,
        },
        notify_url: notify.map(str::to_string),
    };
    let configured = setup
        .client
        .request::<()>(Request::AutonomyConfigure(push))
        .await;
    configured.unwrap();
    let snapshot = setup.pushed(CHAT).await;
    let ensure = Request::ProjectEnsure {
        project: Project {
            id: PROJECT.into(),
            name: "app".into(),
            subpath: String::new(),
        },
        settings: None,
        excludes: Vec::new(),
        remote: Some(remote.into()),
    };
    setup.client.request::<()>(ensure).await.unwrap();
    setup.sync(CHAT, &snapshot).await.unwrap();
}

/// The task, run to its delivery: its stage records then, and the
/// delivery's record.
async fn delivered(setup: &Setup) -> (Vec<(String, String)>, Value) {
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = task_model(look(), review, vec![nothing_to_test()]).await;
    let start = task_start(&url, &setup.project);
    let started = setup
        .client
        .request::<Option<String>>(Request::AgentStart(start))
        .await;
    assert!(started.unwrap().is_some());
    let found = until_delivered(&setup.daemon).await;
    let rows = setup.daemon.rows(CHAT);
    let record = rows
        .iter()
        .rev()
        .find_map(|r| r.meta.get("stage").filter(|s| s["name"] == "deliver"))
        .cloned()
        .expect("the delivery's record");
    (found, record)
}

/// The chat's result as the computer has it after connecting.
async fn newest_result(setup: &Setup) -> ChatResult {
    let results: Vec<ChatResult> = setup.client.request(Request::WorkResults).await.unwrap();
    results
        .into_iter()
        .find(|r| r.dialog_id == CHAT)
        .expect("the chat's result")
}

/// `branch`'s commit on the remote, which shares the project's history,
/// and its ok.txt.
fn pushed_there(remote: &Path, branch: &str) -> (Oid, String) {
    let repo = Repository::open_bare(remote).unwrap();
    let pushed = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap_or_else(|e| panic!("no {branch} on the remote: {e}"))
        .peel_to_commit()
        .unwrap();
    let main = repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(
        repo.merge_base(pushed.id(), main.id()).unwrap(),
        main.id(),
        "the branch goes on from the project's history"
    );
    let entry = pushed
        .tree()
        .unwrap()
        .get_path(Path::new("ok.txt"))
        .unwrap();
    let blob = repo.find_blob(entry.id()).unwrap();
    (
        pushed.id(),
        String::from_utf8_lossy(blob.content()).into_owned(),
    )
}

/// H04: over a file URL, the task's result goes to its branch, on top of
/// the project's history; the delivery says where; the result the
/// computer gets names the branch.
#[tokio::test(flavor = "multi_thread")]
async fn a_tasks_result_is_pushed_to_its_branch() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let setup = Setup::new().await;
    npm_project(&setup);
    let remote = bare_remote(&setup);
    let url = format!("file://{}", remote.display());
    let (hook, told) = common::listening(vec![200]).await;
    ready_telling(&setup, &url, "~/.ssh/unused", false, Some(&hook)).await;

    let (found, record) = delivered(&setup).await;
    assert_eq!(found, delivered_whole(), "{}", setup.daemon.output());
    assert_eq!(
        record["summary"],
        format!("Delivered: pushed to `{BRANCH}`"),
        "{record}"
    );
    // H06: the notice of its end names the branch.
    common::until("the notice", || {
        (!told.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    assert_eq!(told.lock().unwrap()[0].1["branch"], BRANCH);
    let (commit, ok) = pushed_there(&remote, BRANCH);
    assert_eq!(ok, "ok");
    let result = newest_result(&setup).await;
    assert_eq!(result.commit, commit.to_string(), "the result, as pushed");
    assert_eq!(result.branch.as_deref(), Some(BRANCH));
    assert_eq!(result.pr, None);
}

/// A `gh` that keeps what it's asked, its input beside it, and opens a
/// pull request at a fixed URL.
fn fake_gh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\ndir=$(dirname \"$0\")\necho \"$*\" >> \"$dir/gh.log\"\ncat > \"$dir/gh-$1-$2.body\"\nif [ \"$1 $2\" = \"pr create\" ]; then echo \"https://github.example/team/remote/pull/1\"; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// H04: over SSH with the deploy key, as a forge takes a push; then the
/// pull request opens with `gh`, into the remote's repository, and gets
/// the task's report as its description once it's written.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_over_ssh_opens_its_pull_request() {
    let ssh = std::process::Command::new("ssh").arg("-V").output();
    if !has_npm() || ssh.is_err() {
        eprintln!("npm or ssh isn't installed: skipped");
        return;
    }
    let server = SshServer::start().await;
    let dir = rovik_core::testing::temp_dir();
    let bin = fake_gh(&dir.0);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let daemon = Daemon::start_with(&[], &[("PATH", &path)]).await;
    let setup = Setup::on(daemon).await;
    npm_project(&setup);
    let remote = bare_remote(&setup);
    let key = server.plain_key_file(&setup.daemon.dir.0);
    let url = format!("ssh://{USER}@127.0.0.1:{}{}", server.port, remote.display());
    ready(&setup, &url, &key.to_string_lossy(), true).await;

    let (found, record) = delivered(&setup).await;
    assert_eq!(found, delivered_whole(), "{}", setup.daemon.output());
    let pr = "https://github.example/team/remote/pull/1";
    assert_eq!(
        record["summary"],
        format!("Delivered: pushed to `{BRANCH}`, pull request {pr}"),
        "{record}\n{}",
        setup.daemon.output()
    );
    let (_, ok) = pushed_there(&remote, BRANCH);
    assert_eq!(ok, "ok");
    let asked = std::fs::read_to_string(bin.join("gh.log")).unwrap();
    let mut lines = asked.lines();
    let create = lines.next().unwrap();
    let repo = format!(
        "127.0.0.1/{}/remote",
        setup.daemon.dir.0.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(
        create,
        format!("pr create --repo {repo} --head {BRANCH} --title Make the test pass --body-file -")
    );
    assert_eq!(
        lines.next(),
        Some(format!("pr edit {pr} --body-file -").as_str())
    );
    let first = std::fs::read_to_string(bin.join("gh-pr-create.body")).unwrap();
    assert!(first.contains("- How it ended: Its checks pass"), "{first}");
    // The report, as the chat has it, is the pull request's description.
    let rows = setup.daemon.rows(CHAT);
    let report = rows
        .iter()
        .rev()
        .find(|r| r.meta.get("report").is_some())
        .expect("the report");
    assert_eq!(report.meta["report"]["pr"], pr);
    let described = std::fs::read_to_string(bin.join("gh-pr-edit.body")).unwrap();
    assert_eq!(described, report.content);
    let result = newest_result(&setup).await;
    assert_eq!(result.pr.as_deref(), Some(pr));
}

/// H04: a push that fails (no repository at the remote) is a failed
/// delivery that says why, and the result still comes to the computer,
/// to land as it would without a push.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_that_fails_leaves_the_result_for_the_computer() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let setup = Setup::new().await;
    npm_project(&setup);
    let gone = setup.daemon.dir.0.join("gone.git");
    let url = format!("file://{}", gone.display());
    ready(&setup, &url, "~/.ssh/unused", true).await;

    let (found, record) = delivered(&setup).await;
    assert_eq!(found.last(), Some(&("deliver".into(), "failed".into())));
    let summary = record["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(&format!("Not delivered: the push to {url} failed: ")),
        "{summary}"
    );
    assert!(
        summary.ends_with("; the result waits on the server for its computer"),
        "{summary}"
    );
    let result = newest_result(&setup).await;
    assert!(!result.commit.is_empty());
    assert_eq!(result.branch, None);
    assert_eq!(result.outcome, "verified");
}

/*
 * "Test delivery" (H05)
 */

/// A bare repository that keeps, in `pushed.log` beside it, each ref
/// update it receives: "<old> <new> <ref>".
fn watched_remote(dir: &Path) -> PathBuf {
    let remote = dir.join("watched.git");
    Repository::init_bare(&remote).unwrap();
    let hook = remote.join("hooks/post-receive");
    let log = dir.join("pushed.log");
    std::fs::write(&hook, format!("#!/bin/sh\ncat >> '{}'\n", log.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    remote
}

/// This computer has the server push its results, with `key`.
async fn pushing(session: &mut common::Session<tokio::net::UnixStream>, key: &str, open_pr: bool) {
    let push = AutonomyConfig {
        delivery: Delivery::PushBranch {
            deploy_key: key.into(),
            open_pr,
        },
        notify_url: None,
    };
    let configured = session.call(Request::AutonomyConfigure(push)).await;
    assert_eq!(configured, Ok(rovik_protocol::Response::Done));
}

async fn delivery_test(
    session: &mut common::Session<tokio::net::UnixStream>,
    remote: &str,
) -> Result<rovik_protocol::autopilot::DeliveryTest, rovik_protocol::Fail> {
    let tested = session
        .call(Request::DeliveryTest {
            remote: remote.into(),
        })
        .await?;
    match tested {
        rovik_protocol::Response::DeliveryTested(report) => Ok(report),
        other => panic!("not a delivery test's report: {other:?}"),
    }
}

/// H05: the test pushes an empty branch `rovik/test-<date>` as a task's
/// result would go, then deletes it: the remote saw both, and has no
/// branch left. Asked for pull requests, it says none open from a remote
/// on the server itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_test_pushes_an_empty_branch_and_deletes_it() {
    let daemon = Daemon::start(&[]).await;
    let mut session = daemon.session().await;
    let remote = watched_remote(&daemon.dir.0);
    pushing(&mut session, "~/.ssh/unused", true).await;
    let url = format!("file://{}", remote.display());
    let report = delivery_test(&mut session, &url).await.unwrap();
    assert_eq!(report.problem, None, "{report:?}\n{}", daemon.output());
    assert_eq!(report.remote, url);
    let stamp = report
        .branch
        .strip_prefix("rovik/test-")
        .expect("rovik/test-<date>");
    assert_eq!(stamp.len(), "20261017-095301".len(), "{stamp}");
    let seen = std::fs::read_to_string(daemon.dir.0.join("pushed.log")).unwrap();
    let updates: Vec<Vec<&str>> = seen.lines().map(|l| l.split(' ').collect()).collect();
    let zero = "0".repeat(40);
    let branch = format!("refs/heads/{}", report.branch);
    assert_eq!(updates.len(), 2, "{seen}");
    assert_eq!(
        (updates[0][0], updates[0][2]),
        (zero.as_str(), branch.as_str()),
        "pushed"
    );
    assert_eq!(
        (updates[1][1], updates[1][2]),
        (zero.as_str(), branch.as_str()),
        "deleted"
    );
    let repo = Repository::open_bare(&remote).unwrap();
    assert_eq!(repo.references().unwrap().count(), 0, "nothing left there");
    assert_eq!(
        report.pull_requests.as_deref(),
        Some(
            "Pull requests won't open: the remote is on this server, where no pull request opens."
        )
    );
}

/// H05: over SSH with the deploy key, and with `gh` logged in for the
/// remote's host, the test says pull requests open.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_test_over_ssh_checks_gh() {
    let ssh = std::process::Command::new("ssh").arg("-V").output();
    if ssh.is_err() {
        eprintln!("ssh isn't installed: skipped");
        return;
    }
    let server = SshServer::start().await;
    let dir = rovik_core::testing::temp_dir();
    let bin = fake_gh(&dir.0);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let daemon = Daemon::start_with(&[], &[("PATH", &path)]).await;
    let mut session = daemon.session().await;
    let remote = watched_remote(&daemon.dir.0);
    let key = server.plain_key_file(&daemon.dir.0);
    pushing(&mut session, &key.to_string_lossy(), true).await;
    let url = format!("ssh://{USER}@127.0.0.1:{}{}", server.port, remote.display());
    let report = delivery_test(&mut session, &url).await.unwrap();
    assert_eq!(report.problem, None, "{report:?}\n{}", daemon.output());
    assert_eq!(
        report.pull_requests.as_deref(),
        Some("`gh` is logged in to 127.0.0.1: pull requests open.")
    );
    let asked = std::fs::read_to_string(bin.join("gh.log")).unwrap();
    assert_eq!(asked.trim(), "auth status --hostname 127.0.0.1");
    let seen = std::fs::read_to_string(daemon.dir.0.join("pushed.log")).unwrap();
    assert_eq!(seen.lines().count(), 2, "pushed and deleted: {seen}");
}

/// H05: a remote that isn't there fails the test with git's words; a
/// computer that has the server keep its results is told to choose a
/// push first.
#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_test_says_why_it_failed() {
    let daemon = Daemon::start(&[]).await;
    let mut session = daemon.session().await;
    let gone = format!("file://{}", daemon.dir.0.join("gone.git").display());
    let refused = delivery_test(&mut session, &gone).await.unwrap_err();
    assert_eq!(refused.code, rovik_protocol::FailCode::BadRequest);
    assert!(
        refused.message.contains("choose “Push to a branch” first"),
        "{refused:?}"
    );
    pushing(&mut session, "~/.ssh/unused", false).await;
    let report = delivery_test(&mut session, &gone).await.unwrap();
    let problem = report.problem.expect("it failed");
    assert!(problem.starts_with("The push failed: "), "{problem}");
    assert_eq!(report.pull_requests, None, "not asked for");
}

//! Workspace chats on the real daemon (docs/design/workspaces-plan.md,
//! 4.11): a chat across folders is carried as one project whose top-level
//! folders are the chat's. Its runs work in its worktree, where each folder
//! is `root/<name>`, with each folder's settings and checks; its results
//! hold its folders only, the rest staying on the server; a restart keeps
//! its folders, and a folder that leaves it leaves its worktree.

mod common;

use common::project::{
    client_as, ended, files, listening, next_result, read, ready_to_run, start, unattended, writes,
    Setup, CHAT, PROJECT,
};
use common::task::{implementation, task_start, thorough_model, until_delivered};
use common::{commit_all, until, write, Daemon, COMPUTER};
use rovik_core::agent_run::autopilot::Effort;
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, RunFolder, StartKind, StartRequest, UserMessage};
use rovik_core::testing::{model, Reply};
use rovik_protocol::work::Synced;
use rovik_protocol::{features, FailCode, Frame, Request};
use serde_json::{json, Value};

/// A workspace's `api` and `web` as the app carries a chat across them: one
/// project whose top-level folders they are, committed, and a client of the
/// daemon.
async fn shop() -> Setup {
    let daemon = Daemon::start(&[]).await;
    let project = daemon.dir.0.join("project");
    write(&project.join("api/a.txt"), "1\n2\n3\n");
    write(&project.join("web/b.txt"), "b\n");
    commit_all(&project);
    let client = client_as(&daemon, COMPUTER, &daemon.token).await;
    Setup {
        shadow: daemon.dir.0.join("shadow.git"),
        daemon,
        project,
        client,
    }
}

/// A folder of a start, with its check `command` when it has one.
fn folder(name: &str, command: Option<&str>) -> RunFolder {
    RunFolder {
        name: name.into(),
        settings: command.map(|command| {
            json!({ "checks": [{ "kind": "test", "command": command, "enabled": true }] })
        }),
    }
}

/// Any command, no approvals, and the checks at the end.
fn verified() -> Permissions {
    Permissions {
        verify_changes: true,
        ..unattended()
    }
}

/// `CHAT`'s message `id` across `folders`, checked at its end.
fn across(setup: &Setup, url: &str, id: &str, folders: Vec<RunFolder>) -> StartRequest {
    let mut start = start(setup, url, verified());
    start.start = StartKind::Send {
        message: UserMessage {
            id: id.into(),
            content: "Do it".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    start.folders = folders;
    start
}

async fn started(setup: &Setup, start: StartRequest) {
    let request = Request::AgentStart(Box::new(start));
    let started = setup.client.request::<Option<String>>(request).await;
    assert!(started.unwrap().is_some(), "{}", setup.daemon.output());
}

/// The checks the chat's newest check record ran: each command and how it
/// went.
fn checks_run(setup: &Setup) -> Vec<(String, String)> {
    let rows = setup.daemon.rows(CHAT);
    let record = (rows.iter().rev())
        .find_map(|r| r.meta.get("verification").cloned())
        .expect("a check record");
    let checks = record["checks"].as_array().cloned().unwrap_or_default();
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    (checks.iter())
        .map(|c| (text(&c["command"]), text(&c["status"])))
        .collect()
}

#[tokio::test]
async fn the_daemon_says_it_runs_chats_across_folders() {
    let daemon = Daemon::start(&[]).await;
    let session = daemon.session().await;
    let Frame::Welcome {
        features: served, ..
    } = &session.welcome
    else {
        unreachable!()
    };
    assert!(served.iter().any(|f| f == features::FOLDERS), "{served:?}");
}

#[tokio::test]
async fn a_two_folder_chat_works_in_one_worktree_with_each_folders_settings() {
    let setup = shop().await;
    let root = ready_to_run(&setup).await;
    // Its first sync: one worktree, each folder under its name.
    assert_eq!(read(&root.join("api/a.txt")), "1\n2\n3\n");
    assert_eq!(read(&root.join("web/b.txt")), "b\n");
    let (url, asked) = model(vec![
        writes("w1", "web/b.txt", "b2\n"),
        Reply::Text("Done."),
    ])
    .await;

    let folders = vec![
        folder("api", Some("test -f a.txt")),
        folder("web", Some("test -f b.txt")),
    ];
    started(&setup, across(&setup, &url, "u1", folders)).await;
    ended(&setup.client).await;

    assert_eq!(read(&root.join("web/b.txt")), "b2\n", "its worktree's own");
    let told = asked.lock().unwrap()[0].to_string();
    assert!(
        told.contains("This chat works in folders of a workspace"),
        "{told}"
    );
    assert!(
        told.contains("cd api && test -f a.txt") && told.contains("cd web && test -f b.txt"),
        "each folder's checks, by its settings: {told}"
    );
    // D27: the checks of the folder it changed.
    let ran = checks_run(&setup);
    assert_eq!(ran, [("cd web && test -f b.txt".into(), "passed".into())]);
}

#[tokio::test]
async fn a_result_lands_unchecked_only_when_none_of_its_folders_checks_would_run() {
    let setup = shop().await;
    ready_to_run(&setup).await;
    let (_listener, mut heard) = listening(&setup.daemon).await;
    let (url, _) = model(vec![
        writes("w1", "web/b.txt", "b2\n"),
        Reply::Text("Done."),
        writes("w2", "api/a.txt", "ONE\n2\n3\n"),
        Reply::Text("Done."),
    ])
    .await;
    let folders = || vec![folder("api", Some("test -f a.txt")), folder("web", None)];

    started(&setup, across(&setup, &url, "u1", folders())).await;
    let (first, _) = next_result(&mut heard).await;
    assert!(first.unchecked, "web has no checks: {first:?}");

    started(&setup, across(&setup, &url, "u2", folders())).await;
    let (second, _) = next_result(&mut heard).await;
    assert!(!second.unchecked, "api's would run: {second:?}");
}

#[tokio::test]
async fn a_merge_checked_on_the_server_runs_the_changed_folders_checks() {
    let setup = shop().await;
    let root = ready_to_run(&setup).await;
    let (_listener, mut heard) = listening(&setup.daemon).await;
    let (url, _) = model(vec![
        writes("w1", "api/a.txt", "ONE\n2\n3\n"),
        Reply::Text("Done."),
    ])
    .await;
    // web's check would fail: it isn't run, as web didn't change.
    let folders = vec![
        folder("api", Some("test -f a.txt")),
        folder("web", Some("exit 1")),
    ];
    started(&setup, across(&setup, &url, "u1", folders)).await;
    next_result(&mut heard).await;

    // The project changes api's last line, and the result is rechecked.
    write(&setup.project.join("api/a.txt"), "1\n2\nTHREE\n");
    let snapshot = setup.pushed(CHAT).await;
    let recheck = Request::WorkRecheck {
        dialog_id: CHAT.into(),
        project: PROJECT.into(),
        snapshot,
        excludes: vec!["*.mine".into()],
    };
    let synced: Synced = setup.client.request(recheck).await.unwrap();
    assert_eq!(synced.files, ["api/a.txt"]);
    let (checked, _) = next_result(&mut heard).await;

    assert_eq!(read(&root.join("api/a.txt")), "ONE\n2\nTHREE\n");
    let ran = checks_run(&setup);
    assert_eq!(ran, [("cd api && test -f a.txt".into(), "passed".into())]);
    assert!(!checked.unchecked);
}

#[tokio::test]
async fn a_check_that_fails_there_is_reported_as_failing_not_set_apart() {
    let setup = shop().await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "api/a.txt", "ONE\n2\n3\n"),
        Reply::Text("Done."),
        Reply::Text("It fails without my change too: I left it."),
    ])
    .await;
    // It fails on the code before the change as well.
    let folders = vec![folder("api", Some("test -f missing.txt"))];
    started(&setup, across(&setup, &url, "u1", folders)).await;
    ended(&setup.client).await;

    let rows = setup.daemon.rows(CHAT);
    let first = (rows.iter())
        .find_map(|r| r.meta.get("verification").cloned())
        .expect("a check record");
    let check = &first["checks"][0];
    assert_eq!(check["command"], "cd api && test -f missing.txt");
    assert_eq!(check["status"], "failed", "{check:#}");
    assert_eq!(check["preexisting"], Value::Null, "{check:#}");
}

#[tokio::test]
async fn a_file_made_at_the_worktrees_top_is_reported_as_not_brought_back() {
    let setup = shop().await;
    let root = ready_to_run(&setup).await;
    let (_listener, mut heard) = listening(&setup.daemon).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": "echo note > notes.md && mkdir -p out && echo o > out/x" }),
        },
        writes("w1", "api/a.txt", "ONE\n2\n3\n"),
        Reply::Text("Done."),
    ])
    .await;
    let mut start = across(
        &setup,
        &url,
        "u1",
        vec![folder("api", None), folder("web", None)],
    );
    start.config = Some(AgentConfig {
        permissions: unattended(),
        ..AgentConfig::default()
    });
    started(&setup, start).await;
    let (result, _) = next_result(&mut heard).await;

    assert_eq!(result.outside, ["notes.md", "out"]);
    let held = files(&setup.there(), &result.commit);
    assert_eq!(held, ["api/a.txt", "web/b.txt"], "nor the server's .rovik");
    assert_eq!(read(&root.join("notes.md")), "note\n", "it stays there");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_chats_folders() {
    let mut setup = shop().await;
    ready_to_run(&setup).await;
    let (url, asked) = model(vec![Reply::Hang(Some("Looking")), Reply::Text(" at both.")]).await;
    let folders = vec![folder("api", None), folder("web", None)];
    started(&setup, across(&setup, &url, "u1", folders)).await;
    until("the reply begun", || {
        (!asked.lock().unwrap().is_empty()).then_some(())
    })
    .await;

    setup.daemon.kill().await;
    setup.daemon.restart().await;

    until("the run picked up", || {
        (asked.lock().unwrap().len() > 1).then_some(())
    })
    .await;
    let told = asked.lock().unwrap()[1].to_string();
    assert!(
        told.contains("This chat works in folders of a workspace: each is a top-level folder of the project root (`api`, `web`)"),
        "{told}"
    );
}

#[tokio::test]
async fn a_folder_a_later_start_leaves_out_leaves_the_worktree_with_its_ignored_files() {
    let setup = shop().await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    started(
        &setup,
        across(
            &setup,
            &url,
            "u1",
            vec![folder("api", None), folder("web", None)],
        ),
    )
    .await;
    ended(&setup.client).await;
    // What git ignores there (the client's rules): a build's output.
    write(&root.join("web/build.mine"), "built\n");

    started(
        &setup,
        across(&setup, &url, "u2", vec![folder("api", None)]),
    )
    .await;
    ended(&setup.client).await;

    assert!(
        !root.join("web").exists(),
        "web left, ignored files and all"
    );
    assert_eq!(read(&root.join("api/a.txt")), "1\n2\n3\n");
}

#[tokio::test]
async fn root_and_folders_are_refused_when_a_folder_isnt_root_slash_its_name() {
    let setup = shop().await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    for bad in ["../outside", "api/src", ".."] {
        let start = across(
            &setup,
            &url,
            "u1",
            vec![folder("api", None), folder(bad, None)],
        );
        let request = Request::AgentStart(Box::new(start));
        let refused = setup.client.request::<Option<String>>(request).await;
        let refused = refused.unwrap_err();
        assert_eq!(refused.code, FailCode::BadRequest, "{bad}: {refused:?}");
        assert!(
            refused.message.contains("can't name a folder"),
            "{bad}: {refused:?}"
        );
    }
    assert!(!root.join("../outside").exists(), "nothing made out of it");

    // A file in a folder's place at the top is no folder of it.
    write(&root.join("docs"), "a file\n");
    let start = across(&setup, &url, "u1", vec![folder("docs", None)]);
    let refused = (setup.client)
        .request::<Option<String>>(Request::AgentStart(Box::new(start)))
        .await
        .unwrap_err();
    assert!(refused.message.contains("isn't a folder"), "{refused:?}");
}

/// A task's plan for api's check, its files named by their folders.
const API_PLAN: &str = "# Make the check pass\n\napi's check passes once api/ok.txt says ok.\n\n## Criteria\n\n\
    1. `cd api && sh check.sh` passes: `api/check.sh` finds `api/ok.txt` saying ok.\n\n\
    **Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `api/ok.txt`\n   - Replace the text of `api/ok.txt` with `ok`, which `api/check.sh` checks.\n\
    2. **Check**\n   - Files: `api/check.sh` (read)\n   - Read `api/check.sh` to confirm it checks `api/ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `cd api && sh check.sh`\n";

/// T05 on a server (4.9, 4.11): a Thorough task's candidates in a
/// workspace chat each work in a worktree of their own with the chat's
/// folders, so each is checked by the folder it changed: api's check
/// passes in both, and web's, which would fail, never runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_thorough_tasks_candidates_work_with_the_chats_folders() {
    let setup = shop().await;
    let check =
        "if [ \"$(cat ok.txt)\" = ok ]; then exit 0; fi\necho 'ok.txt:1: error: not ok'\nexit 1\n";
    write(&setup.project.join("api/check.sh"), check);
    write(&setup.project.join("api/ok.txt"), "no\n");
    let root = ready_to_run(&setup).await;
    let mut implementations = implementation("api/ok.txt", &[]);
    implementations.extend(implementation(
        "api/ok.txt",
        &[("api/notes.txt", "Wrote ok.\n")],
    ));
    let url = thorough_model(API_PLAN, implementations).await;
    let mut start = task_start(&url, &setup.project);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    start.folders = vec![
        folder("api", Some("sh check.sh")),
        folder("web", Some("exit 1")),
    ];
    started(&setup, start).await;
    let found = until_delivered(&setup.daemon).await;
    let picked = ("pick".to_string(), "done".to_string());
    assert!(
        found.contains(&picked),
        "{found:?}\n{}",
        setup.daemon.output()
    );

    let rows = setup.daemon.rows(CHAT);
    let candidates = (rows.iter())
        .find_map(|r| r.meta.get("candidates").cloned())
        .expect("the candidates' record");
    let chats = candidates["candidates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(chats.len(), 2, "{candidates:#}");
    for candidate in chats {
        let chat = candidate["chat"].as_str().unwrap_or_default();
        let theirs = setup.daemon.rows(chat);
        let record = (theirs.iter().rev())
            .find_map(|r| r.meta.get("verification").cloned())
            .unwrap_or_else(|| panic!("{chat}: no check record"));
        let checks = record["checks"].as_array().cloned().unwrap_or_default();
        let ran = |command: &str| checks.iter().find(|c| c["command"] == command).cloned();
        let api = ran("cd api && sh check.sh").unwrap_or_else(|| panic!("{record:#}"));
        assert_eq!(api["status"], "passed", "{chat}: {record:#}");
        assert_eq!(ran("cd web && exit 1"), None, "{chat}: {record:#}");
    }
    assert_eq!(read(&root.join("api/ok.txt")), "ok");
}

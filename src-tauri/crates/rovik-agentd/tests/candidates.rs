//! A Thorough task's candidates on a server (docs/design/autopilot-plan.md
//! 4.5, T05): each implements the plan in a worktree beside the chat's,
//! made from the chat's worktree as it was, and leaves a result commit of
//! its own; the winner's change is written into the chat's worktree, which
//! the chat's result is made from, as ever.

mod common;

use common::project::{Setup, CHAT, PROJECT};
use common::task::*;
use common::{commit_all, write};
use git2::Repository;
use rovik_core::agent_run::autopilot::Effort;
use rovik_core::agent_run::{breaker, judge, planning, report};
use rovik_core::testing::{model_routed, Reply};
use rovik_protocol::packs::result_ref;
use rovik_protocol::work::Project;
use rovik_protocol::Request;
use serde_json::{json, Value};
use std::path::Path;

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

/// `npm_project` whose check needs a package its lockfile installs: a
/// folder of its own (`file:dep`), so `npm ci` needs no registry. What it
/// installs (`node_modules`) git ignores, as a project's are.
fn npm_project_with_a_dependency(setup: &Setup) {
    let lock = r#"{
  "name": "t",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "t", "version": "1.0.0", "dependencies": { "dep": "file:dep" } },
    "dep": { "version": "1.0.0" },
    "node_modules/dep": { "resolved": "dep", "link": true }
  }
}
"#;
    let files = [
        (
            "package.json",
            r#"{ "name": "t", "version": "1.0.0", "scripts": { "test": "node check.js" }, "dependencies": { "dep": "file:dep" } }"#,
        ),
        ("package-lock.json", lock),
        ("dep/package.json", r#"{ "name": "dep", "version": "1.0.0" }"#),
        ("dep/index.js", "module.exports = 'ok';"),
        (".gitignore", "node_modules\n"),
        (
            "check.js",
            "const fs = require('fs'); const ok = require('dep'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== ok) { console.error('not ok'); process.exit(1); }",
        ),
        ("ok.txt", "no"),
    ];
    for (path, content) in files {
        write(&setup.project.join(path), content);
    }
    commit_all(&setup.project);
}

/// A candidate's implementation: ok.txt says ok, and the files `more`
/// names say what they say.
fn implementation(more: &[(&'static str, &'static str)]) -> Vec<Reply> {
    let mut replies = vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [
                { "title": "Write ok", "status": "in_progress" },
                { "title": "Check", "status": "pending" },
            ] }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "ok.txt", "content": "ok" }),
        },
    ];
    for ((path, content), id) in more.iter().zip(["w2", "w3"]) {
        replies.push(Reply::Call {
            id,
            name: "write",
            args: json!({ "path": path, "content": content }),
        });
    }
    replies.extend([
        Reply::Call {
            id: "t2",
            name: "todo",
            args: json!({ "items": [
                { "title": "Write ok", "status": "done" },
                { "title": "Check", "status": "done" },
            ] }),
        },
        Reply::Text("Done: ok.txt says ok."),
    ]);
    replies
}

/// T05: on a server, a Thorough task's two candidates each get a worktree
/// beside the chat's and implement the plan there, one after the other;
/// each leaves a result commit of its own. The smaller change wins: it's
/// written into the chat's worktree, the chat's result has it, and the
/// candidates' worktrees are gone.
#[tokio::test(flavor = "multi_thread")]
async fn two_candidates_implement_in_worktrees_beside_the_chats() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let setup = Setup::new().await;
    let root = worktree_chat(&setup).await;
    let mut implementations = implementation(&[]);
    implementations.extend(implementation(&[("notes.txt", "Wrote ok.\n")]));
    let url = thorough_model(implementations).await;
    let mut start = task_start(&url, &setup.project);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    let started = setup
        .client
        .request::<Option<String>>(Request::AgentStart(start))
        .await;
    assert!(started.unwrap().is_some());
    let found = until_delivered(&setup.daemon).await;
    let named = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect()
    };
    assert_eq!(
        found,
        named(&[
            ("plan", "done"),
            ("planReview", "done"),
            ("prepare", "skipped"),
            ("tests", "skipped"),
            ("implement", "done"),
            ("review", "skipped"),
            ("checks", "done"),
            ("break", "skipped"),
            ("pick", "done"),
            ("acceptance", "skipped"),
            ("deliver", "done"),
        ]),
        "{}",
        setup.daemon.output()
    );

    // Two candidates, each with its own chat and its implementation's run.
    let rows = setup.daemon.rows(CHAT);
    let candidates: &Value = rows
        .iter()
        .find_map(|r| r.meta.get("candidates"))
        .expect("the candidates' record");
    assert_eq!(candidates["status"], "done", "{candidates}");
    let chats: Vec<String> = candidates["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["chat"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(chats.len(), 2);
    let repo = setup.there();
    for chat in &chats {
        let theirs = setup.daemon.rows(chat);
        let implemented = theirs
            .iter()
            .filter(|r| r.role == "user" && r.meta["harness"] == "autopilot")
            .count();
        assert_eq!(implemented, 1, "{chat}: {}", setup.daemon.output());
        // Its worktree was beside the chat's, and is gone; its result
        // commit stays, on the chat's landing base, ok.txt saying ok.
        let log = setup.daemon.log();
        let beside = root.parent().unwrap().join(chat);
        assert!(
            log.contains(&format!("candidate {chat} works in {}", beside.display())),
            "{log}"
        );
        let result = repo
            .find_reference(&result_ref(chat))
            .unwrap_or_else(|e| panic!("{chat}'s result commit: {e}"))
            .peel_to_commit()
            .unwrap();
        assert_eq!(text_in(&repo, &result, "ok.txt"), "ok");
        assert_eq!(result.parent_id(0).unwrap().to_string(), setup.base());
    }
    let second = repo
        .find_reference(&result_ref(&chats[1]))
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(text_in(&repo, &second, "notes.txt"), "Wrote ok.\n");

    // The winner's change is the chat's: in its worktree, and its result.
    let pick: &Value = rows
        .iter()
        .find_map(|r| r.meta.get("pick"))
        .expect("the pick's record");
    assert_eq!(pick["winner"], "a", "{pick}");
    assert_eq!(std::fs::read_to_string(root.join("ok.txt")).unwrap(), "ok");
    assert!(!root.join("notes.txt").exists());
    let chat_result = repo
        .find_reference(&result_ref(CHAT))
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(text_in(&repo, &chat_result, "ok.txt"), "ok");
    assert!(chat_result
        .tree()
        .unwrap()
        .get_path(Path::new("notes.txt"))
        .is_err());
    for chat in &chats {
        assert!(
            !root.parent().unwrap().join(chat).exists(),
            "{chat}'s worktree is gone"
        );
    }
}

/// The model of a Thorough task whose replies after the plan's are
/// `implementations`, its subagents each answered apart.
async fn thorough_model(implementations: Vec<Reply>) -> String {
    let blocked = |why: &'static str| vec![Reply::Text(why)];
    let mut script = vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": PLAN }),
        },
        Reply::Text("Saved the plan."),
    ];
    script.extend(implementations);
    let view = format!("last:{}", report::VIEW_HEADING);
    let report = format!("last:{}", report::REPORT_HEADING);
    let (url, _) = model_routed(
        vec![
            (Box::leak(view.into_boxed_str()), blocked("")),
            (
                Box::leak(report.into_boxed_str()),
                blocked("## What changed\n\nok.txt says ok."),
            ),
            (PLAN_REVIEW, blocked("## Findings\n\nNone.")),
            (TESTS_FIRST, vec![nothing_to_test()]),
            ("# Write tests for this change", vec![nothing_to_test()]),
            (
                planning::CRITERIA_HEADING,
                blocked("## Blocked\n\nNo criteria here."),
            ),
            (
                judge::JUDGE_CRITERIA_HEADING,
                blocked("## Blocked\n\nNo criteria here."),
            ),
            (
                planning::PLAN_HEADING,
                blocked("## Blocked\n\nNo plan here."),
            ),
            (
                judge::JUDGE_PLANS_HEADING,
                blocked("## Blocked\n\nNo judgment here."),
            ),
            (
                judge::JUDGE_CANDIDATES_HEADING,
                blocked("## Blocked\n\nNo judgment here."),
            ),
            (
                breaker::BREAK_HEADING,
                blocked("## Blocked\n\nNothing to break here."),
            ),
            (
                "# Check this change against its criteria",
                blocked("## Blocked\n\nNothing to judge here."),
            ),
            ("# Review this change", blocked("## Findings\n\nNone.")),
        ],
        script,
    )
    .await;
    url
}

/// A chat with its worktree on the daemon: its root there.
async fn worktree_chat(setup: &Setup) -> std::path::PathBuf {
    worktree_chat_of(setup, npm_project).await
}

/// `worktree_chat`, the project `project` writes.
async fn worktree_chat_of(setup: &Setup, project: fn(&Setup)) -> std::path::PathBuf {
    project(setup);
    let snapshot = setup.pushed(CHAT).await;
    let ensure = Request::ProjectEnsure {
        project: Project {
            id: PROJECT.into(),
            name: "app".into(),
            subpath: String::new(),
        },
        settings: None,
        excludes: Vec::new(),
        remote: None,
    };
    setup.client.request::<()>(ensure).await.unwrap();
    std::path::PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root)
}

/// D19 in T05: a candidate's worktree has none of the chat's installs,
/// which git ignores. Each candidate's copy gets them before it starts,
/// as the chat's worktree got them at the Prepare stage, so its first
/// check doesn't fail for want of a package, and the record says so.
#[tokio::test]
async fn each_candidates_worktree_gets_its_installs_before_it_starts() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let setup = Setup::new().await;
    let root = worktree_chat_of(&setup, npm_project_with_a_dependency).await;
    let mut implementations = implementation(&[]);
    implementations.extend(implementation(&[("notes.txt", "Wrote ok.\n")]));
    let url = thorough_model(implementations).await;
    let mut start = task_start(&url, &setup.project);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    let started = setup
        .client
        .request::<Option<String>>(Request::AgentStart(start))
        .await;
    assert!(started.unwrap().is_some());
    let found = until_delivered(&setup.daemon).await;
    let stage = |name: &str| {
        found
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
    };
    let output = setup.daemon.output();
    assert_eq!(stage("prepare").as_deref(), Some("done"), "{output}");
    assert_eq!(stage("pick").as_deref(), Some("done"), "{output}");
    assert_eq!(stage("deliver").as_deref(), Some("done"), "{output}");
    assert!(
        root.join("node_modules").exists(),
        "the chat's own installs"
    );

    let rows = setup.daemon.rows(CHAT);
    let candidates: &Value = rows
        .iter()
        .find_map(|r| r.meta.get("candidates"))
        .expect("the candidates' record");
    for c in candidates["candidates"].as_array().unwrap() {
        assert_eq!(
            c["prepared"], "Installed: `npm ci --no-audit --no-fund`",
            "{candidates}"
        );
        assert_eq!(c["status"], "done", "{candidates}");
        // No check of its failed for want of the package.
        let chat = c["chat"].as_str().unwrap();
        let rounds: Vec<Value> = setup
            .daemon
            .rows(chat)
            .iter()
            .filter_map(|r| r.meta.get("verification").cloned())
            .collect();
        assert!(!rounds.is_empty(), "{chat} was checked");
        assert!(
            rounds.iter().all(|v| v.get("environment").is_none()),
            "{chat}: {rounds:?}"
        );
    }
}

/// T05 (A03): `kill -9` while the first candidate's reply streams, in its
/// worktree: started again, the daemon keeps that worktree, goes on with
/// the candidate where it was cut, then the second, and the task delivers.
#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_cut_by_a_kill_goes_on_in_its_worktree() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut setup = Setup::new().await;
    let root = worktree_chat(&setup).await;
    let mut implementations = vec![Reply::Hang(Some("Looking at ok.txt"))];
    implementations.extend(implementation(&[]));
    implementations.extend(implementation(&[("notes.txt", "Wrote ok.\n")]));
    let url = thorough_model(implementations).await;
    let mut start = task_start(&url, &setup.project);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    let started = setup
        .client
        .request::<Option<String>>(Request::AgentStart(start))
        .await;
    assert!(started.unwrap().is_some());
    // Killed once the first candidate's reply has a checkpoint stored.
    let first = common::until("the first candidate's cut reply", || {
        let rows = setup.daemon.rows(CHAT);
        let chat = rows.iter().find_map(|r| {
            r.meta.get("candidates")?["candidates"][0]["chat"]
                .as_str()
                .map(str::to_string)
        })?;
        let cut = setup
            .daemon
            .rows(&chat)
            .into_iter()
            .any(|r| r.role == "assistant" && !r.finalized && r.content.starts_with("Looking"));
        cut.then_some(chat)
    })
    .await;
    let worktree = root.parent().unwrap().join(&first);
    assert!(worktree.join("package.json").exists());
    setup.daemon.kill().await;
    setup.daemon.restart().await;

    let found = until_delivered(&setup.daemon).await;
    assert_eq!(
        found.last(),
        Some(&("deliver".to_string(), "done".to_string())),
        "{found:?}\n{}",
        setup.daemon.output()
    );
    let rows = setup.daemon.rows(CHAT);
    let pick: &Value = rows
        .iter()
        .find_map(|r| r.meta.get("pick"))
        .expect("the pick's record");
    assert_eq!(pick["winner"], "a", "{pick}");
    assert_eq!(std::fs::read_to_string(root.join("ok.txt")).unwrap(), "ok");
    // The cut reply went on, once; the candidate implemented once.
    let theirs = setup.daemon.rows(&first);
    let cut: Vec<_> = theirs
        .iter()
        .filter(|r| r.role == "assistant" && r.content.starts_with("Looking"))
        .collect();
    assert_eq!(cut.len(), 1, "{theirs:#?}");
    let implemented = theirs
        .iter()
        .filter(|r| r.role == "user" && r.meta["harness"] == "autopilot")
        .count();
    assert_eq!(implemented, 1);
    assert!(!worktree.exists(), "its worktree is gone once picked");
}

/// T05 (R02): the server stops while the first candidate's command runs:
/// the candidate pauses once it's done, then the task chat's run, and the
/// daemon counts both paused. Started again, the task goes on from there,
/// the candidate in its worktree, to its delivery.
#[tokio::test(flavor = "multi_thread")]
async fn a_candidate_paused_by_the_servers_stop_goes_on_after_it_starts_again() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut setup = Setup::new().await;
    let root = worktree_chat(&setup).await;
    let mut implementations = vec![Reply::Call {
        id: "s1",
        name: "bash",
        args: json!({ "command": "sleep 2" }),
    }];
    implementations.extend(implementation(&[]));
    implementations.extend(implementation(&[("notes.txt", "Wrote ok.\n")]));
    let url = thorough_model(implementations).await;
    let mut start = task_start(&url, &setup.project);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    let started = setup
        .client
        .request::<Option<String>>(Request::AgentStart(start))
        .await;
    assert!(started.unwrap().is_some());
    // Stopped once the first candidate's command runs.
    let first = common::until("the first candidate's command", || {
        let rows = setup.daemon.rows(CHAT);
        let chat = rows.iter().find_map(|r| {
            r.meta.get("candidates")?["candidates"][0]["chat"]
                .as_str()
                .map(str::to_string)
        })?;
        // Its reply that calls the command: no stage record, nothing back yet.
        let theirs = setup.daemon.rows(&chat);
        let called = theirs
            .iter()
            .any(|r| r.role == "assistant" && r.meta.get("stage").is_none());
        let answered = theirs.iter().any(|r| r.role == "tool");
        (called && !answered).then_some(chat)
    })
    .await;
    setup.daemon.stop().await;
    let log = setup.daemon.log();
    assert!(log.contains("paused for the next start"), "{log}");
    let paused = |rows: Vec<common::Row>| {
        rows.iter()
            .any(|r| r.meta.pointer("/paused/kind") == Some(&json!("drain")))
    };
    assert!(paused(setup.daemon.rows(&first)), "the candidate paused");
    assert!(
        paused(setup.daemon.rows(CHAT)),
        "the task chat's run paused"
    );
    setup.daemon.restart().await;

    let found = until_delivered(&setup.daemon).await;
    assert_eq!(
        found.last(),
        Some(&("deliver".to_string(), "done".to_string())),
        "{found:?}\n{}",
        setup.daemon.output()
    );
    let rows = setup.daemon.rows(CHAT);
    let pick: &Value = rows
        .iter()
        .find_map(|r| r.meta.get("pick"))
        .expect("the pick's record");
    assert_eq!(pick["winner"], "a", "{pick}");
    assert_eq!(std::fs::read_to_string(root.join("ok.txt")).unwrap(), "ok");
    let implemented = setup
        .daemon
        .rows(&first)
        .iter()
        .filter(|r| r.role == "user" && r.meta["harness"] == "autopilot")
        .count();
    assert_eq!(implemented, 1, "it went on, not again");
}

/// T05: a server chat with no worktree of its own (the daemon runs it in
/// the project's folder) has no second copy to give a candidate: one
/// implementer, and the Pick's record says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_without_a_worktree_has_one_implementer_and_says_why() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = common::Daemon::start(&["--same-machine"]).await;
    let folder = project(&daemon);
    let url = thorough_model(implementation(&[])).await;
    let mut start = task_start(&url, &folder);
    start.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    let reply = daemon.post_call(1, Request::AgentStart(start)).await;
    assert!(
        matches!(
            reply,
            rovik_protocol::Frame::Reply {
                res: Ok(rovik_protocol::Response::RunId(Some(_))),
                ..
            }
        ),
        "{reply:?}"
    );
    let found = until_delivered(&daemon).await;
    assert_eq!(
        found.last(),
        Some(&("deliver".to_string(), "done".to_string())),
        "{found:?}\n{}",
        daemon.output()
    );
    let rows = daemon.rows(CHAT);
    let pick = rows
        .iter()
        .find_map(|r| r.meta.get("stage").filter(|s| s["name"] == "pick"))
        .expect("the pick's record");
    assert_eq!(pick["status"], "skipped", "{pick}");
    assert_eq!(
        pick["summary"],
        "One implementer: candidate a's working copy couldn't be made: the chat has no worktree on this server",
        "{pick}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("ok.txt")).unwrap(),
        "ok"
    );
    assert!(rows.iter().all(|r| r.meta.get("pick").is_none()));
}

/// A file's text in a commit.
fn text_in(repo: &Repository, commit: &git2::Commit, path: &str) -> String {
    let entry = commit.tree().unwrap().get_path(Path::new(path)).unwrap();
    let blob = repo.find_blob(entry.id()).unwrap();
    String::from_utf8_lossy(blob.content()).into_owned()
}

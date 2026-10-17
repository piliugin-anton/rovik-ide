//! Autopilot's tasks on a server (docs/design/autopilot-plan.md, A03): a
//! task a `kill -9` cuts in the middle of a stage goes on when the daemon
//! starts again, with nobody connected, to its delivery; its stage records
//! are rows of the chat like any other.

mod common;

use common::{start_request, until, Daemon, Row};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind, UserMessage};
use rovik_core::testing::{model_routed, Reply};
use rovik_protocol::{Frame, Request, Response};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

const CHAT: &str = "chat-1";

const PLAN: &str = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
    2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `npm test`\n";

fn has_npm() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

/// A project whose test (`npm test`) passes once ok.txt says "ok".
fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let files = [
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        (
            "check.js",
            "const fs = require('fs'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== 'ok') { console.error('not ok'); process.exit(1); }",
        ),
        ("ok.txt", "no"),
    ];
    for (path, content) in files {
        std::fs::write(project.join(path), content).unwrap();
    }
    project
}

/// The chat's stage records: (name, status), oldest first.
fn stages(rows: &[Row]) -> Vec<(String, String)> {
    rows.iter()
        .filter(|r| r.role == "assistant")
        .filter_map(|r| {
            let stage = r.meta.get("stage")?;
            Some((
                stage["name"].as_str()?.to_string(),
                stage["status"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// The plan reviewer's brief starts so (plan_review.rs).
const PLAN_REVIEW: &str = "# Review this plan";

/// A03: the daemon is killed while the task's plan streams; started again,
/// it picks the plan's run up where it was cut, and the task goes on by
/// itself: the plan's review (A07: on for Autopilot, though the computer's
/// subagents are off), the implementation, its checks, the delivery.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_cut_by_a_kill_goes_on_when_the_daemon_starts_again() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, review)],
        vec![
            Reply::Hang(Some("Looking at the project")),
            Reply::Call {
                id: "p1",
                name: "submit_plan",
                args: json!({ "plan": PLAN }),
            },
            Reply::Text("Saved the plan."),
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
            Reply::Call {
                id: "t2",
                name: "todo",
                args: json!({ "items": [
                { "title": "Write ok", "status": "done" },
                { "title": "Check", "status": "done" },
            ] }),
            },
            Reply::Text("Done: ok.txt says ok."),
        ],
    )
    .await;
    let mut start = start_request(&url, &project, "Make the test pass".into());
    start.start = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "Make the test pass".into(),
            meta: Some(json!({ "mode": "autopilot" })),
            attachments: Vec::new(),
        },
    };
    start.mode = Some("autopilot".into());
    start.config = Some(AgentConfig {
        permissions: Permissions {
            allow_any_command: true,
            tool_call_checkpoint: None,
            ..Permissions::default()
        },
        ..AgentConfig::default()
    });
    let reply = daemon.post_call(1, Request::AgentStart(start)).await;
    assert!(
        matches!(
            reply,
            Frame::Reply {
                res: Ok(Response::RunId(Some(_))),
                ..
            }
        ),
        "{reply:?}"
    );
    // Killed once the plan's reply has a checkpoint stored.
    until("the plan's reply's checkpoint", || {
        let rows = daemon.rows(CHAT);
        let cut =
            |r: &Row| r.role == "assistant" && !r.finalized && r.content.starts_with("Looking");
        rows.iter().any(cut).then_some(())
    })
    .await;
    assert_eq!(
        stages(&daemon.rows(CHAT)),
        [("plan".into(), "running".into())]
    );
    daemon.kill().await;
    daemon.restart().await;

    let mut delivered = None;
    for _ in 0..2400 {
        let found = stages(&daemon.rows(CHAT));
        if found
            .last()
            .is_some_and(|(name, status)| name == "deliver" && status != "running")
        {
            delivered = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let found = delivered.unwrap_or_else(|| {
        panic!(
            "never delivered: {:?}\n{}",
            stages(&daemon.rows(CHAT)),
            daemon.output()
        )
    });
    let expected: Vec<(String, String)> = [
        ("plan", "done"),
        ("planReview", "done"),
        ("prepare", "skipped"),
        ("tests", "skipped"),
        ("implement", "done"),
        ("review", "skipped"),
        ("checks", "done"),
        ("acceptance", "skipped"),
        ("deliver", "done"),
    ]
    .iter()
    .map(|(n, s)| (n.to_string(), s.to_string()))
    .collect();
    assert_eq!(found, expected, "{}", daemon.output());
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
    // The plan's run went on from what came before the kill.
    let rows = daemon.rows(CHAT);
    let plan_reply: Vec<&Value> = rows
        .iter()
        .filter(|r| r.role == "assistant" && r.content.starts_with("Looking"))
        .map(|r| &r.meta)
        .collect();
    assert_eq!(plan_reply.len(), 1, "{rows:?}");
}

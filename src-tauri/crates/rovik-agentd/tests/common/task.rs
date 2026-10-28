//! An Autopilot task the daemon tests run (A03, H03, H04): its project,
//! whose test passes once ok.txt says ok, its scripted model, its start,
//! and the wait for its delivery.

use super::project::CHAT;
use super::{start_request, Daemon, Row};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind, StartRequest, UserMessage};
use rovik_core::testing::{model_routed, Reply};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PLAN: &str = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Criteria\n\n1. `npm test` passes: `check.js` finds `ok.txt` saying ok.\n\n**Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
    2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `npm test`\n";

pub fn has_npm() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

/// A project whose test (`npm test`) passes once ok.txt says "ok".
pub fn project(daemon: &Daemon) -> PathBuf {
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
pub fn stages(rows: &[Row]) -> Vec<(String, String)> {
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
pub const PLAN_REVIEW: &str = "# Review this plan";

/// The Tests stage's test writer's brief starts so (test_writer.rs, S04).
pub const TESTS_FIRST: &str = "# Write the tests before the change";

/// The Tests stage's writer finds nothing to test: it's skipped.
pub fn nothing_to_test() -> Reply {
    Reply::Text("## Blocked\n\nNothing to test here.")
}

/// The task's model: the plan's run, whose first reply is `first` (a
/// reply that hangs, for a kill to cut), the implementation's, and the
/// subagents' answers by their briefs, each in turn: the plan's review's
/// `review`, the Tests stage's writer's `tests`.
pub async fn task_model(
    first: Reply,
    review: Vec<Reply>,
    tests: Vec<Reply>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    task_model_after(Vec::new(), first, review, tests).await
}

/// `task_model`, the plan's run answered `before` first (each an answer to
/// a request of its own: rate limits, say).
pub async fn task_model_after(
    before: Vec<Reply>,
    first: Reply,
    review: Vec<Reply>,
    tests: Vec<Reply>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let mut script = before;
    script.extend([
        first,
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
    ]);
    model_routed(vec![(PLAN_REVIEW, review), (TESTS_FIRST, tests)], script).await
}

/// The plan's first step when nothing cuts it: a look at the check.
pub fn look() -> Reply {
    Reply::Call {
        id: "r0",
        name: "read",
        args: json!({ "path": "check.js" }),
    }
}

/// The task's start: "Make the test pass", in Autopilot, from a computer
/// whose commands need no approval.
pub fn task_start(url: &str, project: &Path) -> StartRequest {
    let mut start = start_request(url, project, "Make the test pass".into());
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
    start
}

/// The stage records of a task that went all the way.
pub fn delivered_whole() -> Vec<(String, String)> {
    [
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
    .collect()
}

/// The chat's stage records once its delivery ended.
pub async fn until_delivered(daemon: &Daemon) -> Vec<(String, String)> {
    for _ in 0..2400 {
        let found = stages(&daemon.rows(CHAT));
        if found
            .last()
            .is_some_and(|(name, status)| name == "deliver" && status != "running")
        {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "never delivered: {:?}\n{}",
        stages(&daemon.rows(CHAT)),
        daemon.output()
    )
}

/// Bodies of the requests whose first message has `marker`.
pub fn asked(seen: &Mutex<Vec<Value>>, marker: &str) -> usize {
    let bodies = seen.lock().unwrap();
    bodies
        .iter()
        .filter(|b| b.to_string().contains(marker))
        .count()
}

/// A Thorough candidate's implementation: `ok` says ok, and the files
/// `more` names say what they say.
pub fn implementation(ok: &str, more: &[(&'static str, &'static str)]) -> Vec<Reply> {
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
            args: json!({ "path": ok, "content": "ok" }),
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

/// The model of a Thorough task whose plan is `plan` and whose replies
/// after the plan's are `implementations`, its subagents each answered
/// apart.
pub async fn thorough_model(plan: &str, implementations: Vec<Reply>) -> String {
    use rovik_core::agent_run::{breaker, judge, planning, report};
    let blocked = |why: &'static str| vec![Reply::Text(why)];
    let mut script = vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": plan }),
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

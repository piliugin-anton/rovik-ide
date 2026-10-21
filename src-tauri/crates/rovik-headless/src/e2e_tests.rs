//! G00: a scripted task run by the headless runner from its command line,
//! to its JSON report, against rovik-core's scripted model (its `testing`
//! feature).

use super::*;
use rovik_core::agent_run::agent_set_effort;
use rovik_core::chat_db::{store, DbState};
use rovik_core::testing::{effort, model, model_routed, temp_dir, Reply, TempDir};

#[tokio::test]
async fn a_scripted_task_runs_headless_and_reports() {
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "old\n").unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "git push origin main" }),
        },
        // G02: an edit whose text isn't in the file.
        Reply::Call {
            id: "e1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "older", "content": "new" }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "new\n" }),
        },
        Reply::Text("Changed a.txt; pushing wasn't allowed."),
    ])
    .await;
    let settings = dir.0.join("settings.json");
    std::fs::write(
        &settings,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "apiKeyEnv": "ROVIK_HEADLESS_TEST_KEY",
            "mode": "agent",
            "config": {
                "permissions": {
                    "allowAnyCommand": true,
                    "commandApproval": { "required": true, "by": "user" },
                    "verifyChanges": false,
                    "checklist": "off",
                },
            },
        })
        .to_string(),
    )
    .unwrap();
    let policy = dir.0.join("policy.json");
    std::fs::write(
        &policy,
        json!({ "default": "allow", "rules": [{ "tool": "bash", "contains": "git push", "decision": "deny" }] })
            .to_string(),
    )
    .unwrap();
    let out = dir.0.join("out").join("report.json");
    let argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings.to_str().unwrap(),
        "--policy",
        policy.to_str().unwrap(),
        "--prompt",
        "Change a.txt and push",
        "--out",
        out.to_str().unwrap(),
        "--data",
        dir.0.join("data").to_str().unwrap(),
        "--timeout",
        "60",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let args = crate::parse_args(&argv).unwrap();
    crate::run(&args).await.unwrap();

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["outcome"], "done");
    assert_eq!(report["toolCalls"], 3);
    assert_eq!(report["filesWritten"], json!(["a.txt"]));
    // G02: from the run log's tool lines: the refused push and the edit
    // that missed; no checks ran, and the run is no Autopilot task's.
    assert_eq!(report["allToolCalls"], 3);
    assert_eq!(report["toolErrors"], 2);
    assert_eq!(
        report["toolErrorClasses"],
        json!({ "denied": 1, "old_text not found": 1 })
    );
    assert_eq!(report["editMisses"], 1);
    assert_eq!(report["verifyRounds"], 0);
    assert_eq!(report["stages"], Value::Null);
    // The policy's denied command is refused as the run asks for it,
    // before any approval.
    assert_eq!(report["approvals"], json!([]));
    assert_eq!(
        report["finalMessage"],
        "Changed a.txt; pushing wasn't allowed."
    );
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "new\n"
    );
    // The run log is in the report: its replies and its run line.
    let log = report["runLog"].as_array().unwrap();
    assert!(
        log.iter()
            .any(|l| l["event"] == "run" && l["outcome"] == "done"),
        "{log:?}"
    );
    assert!(log.iter().filter(|l| l["event"] == "reply").count() >= 3);
}

#[tokio::test]
async fn the_token_cap_stops_a_run_and_the_report_counts_its_cost() {
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "old\n").unwrap();
    // Each request reports 400k tokens in; the third would go past the cap,
    // and the model would carry on forever.
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "a.txt" }),
    };
    let (url, _) = model(vec![
        Reply::Usage(400_000, 1_000, Box::new(read("r1"))),
        // The second request's prompt was mostly served from the cache.
        Reply::UsageAs(
            json!({ "prompt_tokens": 400_000, "completion_tokens": 1_000, "prompt_tokens_details": { "cached_tokens": 300_000 } }),
            Box::new(read("r2")),
        ),
        Reply::Usage(
            400_000,
            1_000,
            Box::new(Reply::Slow(30_000, Box::new(read("r3")))),
        ),
        Reply::Hang(None),
    ])
    .await;
    let settings = dir.0.join("settings.json");
    std::fs::write(
        &settings,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 1000000 },
            "apiKeyEnv": "ROVIK_HEADLESS_TEST_KEY",
            "mode": "agent",
            "price": { "inputPerMillion": 1.0, "outputPerMillion": 10.0 },
            "config": { "permissions": { "verifyChanges": false, "checklist": "off" } },
        })
        .to_string(),
    )
    .unwrap();
    let out = dir.0.join("report.json");
    let argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings.to_str().unwrap(),
        "--prompt",
        "Read a.txt",
        "--out",
        out.to_str().unwrap(),
        "--data",
        dir.0.join("data").to_str().unwrap(),
        "--timeout",
        "60",
        "--max-tokens",
        "1000000",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let args = crate::parse_args(&argv).unwrap();
    let started = std::time::Instant::now();
    crate::run(&args).await.unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(25));

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["outcome"], "budget", "{report:#}");
    assert_eq!(report["usage"]["leadInputTokens"], 1_200_000);
    assert_eq!(report["usage"]["leadOutputTokens"], 3_000);
    assert_eq!(report["usage"]["peakLeadContext"], 400_000);
    // Cached tokens are counted and the hit rate shown (300k of 1.2M).
    assert_eq!(report["usage"]["leadCachedTokens"], 300_000);
    assert_eq!(report["usage"]["cacheHitPercent"], 25);
    assert_eq!(report["costSource"], "price");
    let cost = report["costUsd"].as_f64().unwrap();
    assert!((cost - (1.2 + 0.03)).abs() < 1e-9, "{cost}");
    assert_eq!(report["interventions"], 0);
}

/// Runs the headless runner with these settings and policy; the report.
async fn headless(
    dir: &TempDir,
    settings: Value,
    policy: Option<Value>,
    prompt: &str,
    extra: &[&str],
) -> Value {
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "old\n").unwrap();
    let settings_file = dir.0.join("settings.json");
    std::fs::write(&settings_file, settings.to_string()).unwrap();
    let out = dir.0.join("report.json");
    let mut argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings_file.to_str().unwrap(),
        "--prompt",
        prompt,
        "--out",
        out.to_str().unwrap(),
        "--data",
        dir.0.join("data").to_str().unwrap(),
        "--timeout",
        "60",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(policy) = policy {
        let file = dir.0.join("policy.json");
        std::fs::write(&file, policy.to_string()).unwrap();
        argv.extend(["--policy".to_string(), file.to_string_lossy().into_owned()]);
    }
    argv.extend(extra.iter().map(|s| s.to_string()));
    let args = crate::parse_args(&argv).unwrap();
    crate::run(&args).await.unwrap();
    serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap()
}

/// A provider that reports no usage: the cost isn't known, so it isn't
/// counted as free, and a run with a token cap stops (the cap can't be kept).
#[tokio::test]
async fn a_run_whose_provider_reports_no_usage_has_no_known_cost_and_stops() {
    let dir = temp_dir();
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "a.txt" }),
    };
    let (url, _) = model(vec![
        read("r1"),
        Reply::Slow(30_000, Box::new(read("r2"))),
        Reply::Hang(None),
    ])
    .await;
    let started = std::time::Instant::now();
    let report = headless(
        &dir,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "apiKeyEnv": "ROVIK_HEADLESS_TEST_KEY",
            "mode": "agent",
            "price": { "inputPerMillion": 1.0, "outputPerMillion": 10.0 },
            "config": { "permissions": { "verifyChanges": false, "checklist": "off" } },
        }),
        None,
        "Read a.txt",
        &["--max-tokens", "1000000"],
    )
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(25));
    assert_eq!(report["outcome"], "no-usage", "{report:#}");
    assert_eq!(report["costUsd"], Value::Null);
    assert_eq!(report["costSource"], "unknown");
    assert!(report["usage"]["repliesWithoutUsage"].as_u64().unwrap() >= 1);
}

/// The policy's denied commands are refused with no approvals asked (an
/// evaluation's settings ask none), however the command is written.
#[tokio::test]
async fn the_policys_denied_commands_are_refused_without_approvals() {
    let dir = temp_dir();
    let (url, _) = model(vec![
        Reply::Calls(vec![
            (
                "b1",
                "bash",
                json!({ "command": "git  -C . push origin main" }),
            ),
            ("b2", "bash", json!({ "command": "sudo ls" })),
            ("b3", "bash", json!({ "command": "bash -c 'git push'" })),
            ("b4", "bash", json!({ "command": "env X=1 git push" })),
            ("b5", "bash", json!({ "command": "echo fine" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    let report = headless(
        &dir,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "apiKeyEnv": "ROVIK_HEADLESS_TEST_KEY",
            "mode": "agent",
            "config": { "permissions": { "allowAnyCommand": true, "verifyChanges": false, "checklist": "off" } },
        }),
        Some(json!({ "default": "allow", "rules": [
            { "tool": "bash", "contains": "git push", "decision": "deny" },
            { "tool": "bash", "contains": "sudo ", "decision": "deny" },
        ] })),
        "Push it",
        &[],
    )
    .await;
    assert_eq!(report["outcome"], "done", "{report:#}");
    assert_eq!(report["approvals"], json!([]));
    let id = report["dialogId"].as_str().unwrap().to_string();
    let db = DbState::open(&dir.0.join("data")).unwrap();
    let messages = db
        .read(move |c| store::get_messages(c, &id, None, 100))
        .await
        .unwrap()
        .messages;
    let result = |id: &str| -> String {
        messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no result for {id}"))
            .content
            .clone()
    };
    for id in ["b1", "b2", "b3", "b4"] {
        assert!(
            result(id).contains("isn't allowed in this run"),
            "{id}: {}",
            result(id)
        );
    }
    assert!(result("b5").contains("fine"), "{}", result("b5"));
}

/// P6.4: without `--data`, the folder it made for the run is gone once the
/// report is written; the report still has the run's log, to its last line.
#[tokio::test]
async fn a_data_folder_it_made_goes_after_the_report() {
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (url, _) = model(vec![Reply::Text("Nothing to do.")]).await;
    let settings = dir.0.join("settings.json");
    std::fs::write(
        &settings,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "mode": "agent",
            "config": { "permissions": { "verifyChanges": false, "checklist": "off" } },
        })
        .to_string(),
    )
    .unwrap();
    let ours = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rovik-headless-"))
            })
            .collect()
    };
    let before = ours();
    let out = dir.0.join("report.json");
    let argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings.to_str().unwrap(),
        "--prompt",
        "Look around",
        "--out",
        out.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let args = crate::parse_args(&argv).unwrap();
    let report = crate::run(&args).await.unwrap();
    assert_eq!(report["outcome"], "done");
    assert!(report["runLog"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["event"] == "run"));
    let left: Vec<_> = ours().into_iter().filter(|p| !before.contains(p)).collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

fn settings_with(url: &str, permissions: Value) -> Value {
    json!({
        "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
        "mode": "agent",
        "config": { "permissions": permissions },
    })
}

/// Approvals the run asks for are answered by the policy, each recorded,
/// and the run goes on with the answer.
#[tokio::test]
async fn approvals_are_answered_by_the_policy_end_to_end() {
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("b.txt"), "b\n").unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "new a\n" }),
        },
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "b.txt", "content": "new b\n" }),
        },
        Reply::Text("Changed a.txt; b.txt was refused."),
    ])
    .await;
    let report = headless(
        &dir,
        settings_with(
            &url,
            json!({
                "editApproval": { "required": true, "by": "user" },
                "verifyChanges": false,
                "checklist": "off",
            }),
        ),
        Some(json!({ "default": "allow", "rules": [{ "tool": "write", "contains": "b.txt", "decision": "deny" }] })),
        "Change the files",
        &[],
    )
    .await;
    assert_eq!(report["outcome"], "done", "{report:#}");
    let approvals = report["approvals"].as_array().unwrap();
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[0]["decision"], "allow");
    assert_eq!(approvals[1]["decision"], "deny");
    assert_eq!(report["interventions"], 2);
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "new a\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("b.txt")).unwrap(),
        "b\n"
    );
}

/// `--timeout`: the run is stopped, ends, and the report says "timeout".
#[tokio::test]
async fn the_timeout_stops_the_run_and_the_report_says_so() {
    let dir = temp_dir();
    let (url, _) = model(vec![Reply::Hang(None)]).await;
    let started = std::time::Instant::now();
    let report = headless(
        &dir,
        settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" })),
        None,
        "Wait",
        &["--timeout", "1"],
    )
    .await;
    assert_eq!(report["outcome"], "timeout", "{report:#}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(report["runLog"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["event"] == "run" && l["outcome"] == "stopped"));
}

/// Round 2 (section 4.2), P6.4: when the run's `"event":"run"` line never
/// reaches the log (here no line can be written: the log's folder is a
/// file), the report still comes after the bounded 5 s wait, with an
/// empty run log.
#[tokio::test]
async fn a_run_line_that_never_arrives_is_waited_for_5_s_at_most() {
    let dir = temp_dir();
    let (url, _) = model(vec![Reply::Text("Nothing to do.")]).await;
    let data = dir.0.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("log"), "").unwrap();
    let started = std::time::Instant::now();
    let report = headless(
        &dir,
        settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" })),
        None,
        "Look around",
        &[],
    )
    .await;
    // The call waited for the line, 5 s; were the wait unbounded, it would
    // never return. (Timed from the call: on a loaded machine a timer
    // started at the model's first request can start late.)
    let waited = started.elapsed();
    assert!(waited >= std::time::Duration::from_secs(5), "{waited:?}");
    // And not for the run's own `--timeout`: the wait is the 5 s.
    assert!(waited < std::time::Duration::from_secs(40), "{waited:?}");
    assert_eq!(report["outcome"], "done", "{report:#}");
    assert_eq!(report["runLog"], json!([]));
    // A request was made: not free, unknown.
    assert_eq!(report["costUsd"], Value::Null);
    assert_eq!(report["costSource"], "unknown");
}

/// Round 2 (section 4.2): the token cap counts a subagent's tokens, by its
/// lines in the run log: a scout's one 600k-token request is past the cap
/// by itself (the lead used 1k).
#[tokio::test]
async fn the_token_cap_counts_a_subagents_600k_prompt_tokens() {
    const REPORT: &str = "## Answer\nIt is in a.txt.\n\n## Evidence\n- `a.txt:1`: the line\n\n## Open questions\nNone\n\n## Web sources\nNone";
    let dir = temp_dir();
    let (url, _) = model_routed(
        vec![(
            "MARK-CAP",
            vec![Reply::Usage(600_000, 1_000, Box::new(Reply::Text(REPORT)))],
        )],
        vec![
            Reply::Usage(
                1_000,
                10,
                Box::new(Reply::Call {
                    id: "t1",
                    name: "task",
                    args: json!({ "kind": "scout", "goal": "MARK-CAP: where is it?" }),
                }),
            ),
            // The lead would wait for ever: only the cap ends the run.
            Reply::Hang(None),
        ],
    )
    .await;
    let mut settings = settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" }));
    settings["config"]["subagents"] =
        json!({ "enabled": true, "kinds": { "scout": { "enabled": true } } });
    let report = headless(&dir, settings, None, "Find it", &["--max-tokens", "500000"]).await;
    assert_eq!(report["outcome"], "budget", "{report:#}");
    let usage = &report["usage"];
    assert_eq!(usage["subagentInputTokens"], 600_000);
    assert_eq!(usage["subagentOutputTokens"], 1_000);
    assert_eq!(usage["leadInputTokens"], 1_000);
    assert_eq!(usage["peakLeadContext"], 1_000);
}

/// Round 2 (section 4.2), P6.4: a `--data` folder is the caller's: kept
/// after the report, with the run's database and log.
#[tokio::test]
async fn a_data_folder_given_is_kept() {
    let dir = temp_dir();
    let (url, _) = model(vec![Reply::Text("Nothing to do.")]).await;
    let report = headless(
        &dir,
        settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" })),
        None,
        "Look around",
        &[],
    )
    .await;
    assert_eq!(report["outcome"], "done", "{report:#}");
    let data = dir.0.join("data");
    assert!(data.join("chats.db").is_file());
    let id = report["dialogId"].as_str().unwrap();
    let log = std::fs::read_to_string(data.join("log").join("agent-log.jsonl")).unwrap();
    assert!(
        log.lines().any(|l| serde_json::from_str::<Value>(l)
            .is_ok_and(|v| v["event"] == "run" && v["dialogId"] == id)),
        "{log}"
    );
}

/// Round 2 (section 4.2), P6.2: a provider that sends `usage: {}` (no
/// token counts) reports no usage: a run with a token cap stops, `no-usage`.
#[tokio::test]
async fn an_empty_usage_with_a_token_cap_ends_no_usage() {
    let dir = temp_dir();
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "a.txt" }),
    };
    let (url, _) = model(vec![
        Reply::UsageAs(json!({}), Box::new(read("r1"))),
        Reply::Slow(30_000, Box::new(read("r2"))),
        Reply::Hang(None),
    ])
    .await;
    let report = headless(
        &dir,
        settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" })),
        None,
        "Read a.txt",
        &["--max-tokens", "1000000"],
    )
    .await;
    assert_eq!(report["outcome"], "no-usage", "{report:#}");
    assert_eq!(report["costUsd"], Value::Null);
    assert_eq!(report["costSource"], "unknown");
    assert_eq!(report["usage"]["replies"], 0);
    assert!(
        report["runLog"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["event"] == "reply" && l["usage"] == json!({})),
        "{report:#}"
    );
}

/// Round 2 (section 4.2), P6.2: a request asked again with another effort
/// after the provider had reported its usage counts as a reply, with the
/// one that answered: two replies, 1000 tokens in.
#[tokio::test]
async fn an_asked_again_request_with_usage_counts_as_a_reply() {
    let dir = temp_dir();
    let (url, seen) = model(vec![
        // Its usage, then silence: stuck before any of the reply.
        Reply::Usage(500, 0, Box::new(Reply::Hang(None))),
        Reply::Usage(500, 20, Box::new(Reply::Text("Hello."))),
    ])
    .await;
    let mut settings = settings_with(&url, json!({ "verifyChanges": false, "checklist": "off" }));
    settings["options"] =
        json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort"] });
    // Once this run has asked (it may first wait for another headless
    // run), another effort is picked in its chat, as in the app.
    let pick = async {
        for _ in 0..2_400 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let running = crate::TEST_RUN.lock().unwrap().clone();
        let (core, dialog) = running.expect("the run asked the model");
        assert_eq!(
            agent_set_effort(&core, "p::m".into(), effort("low"), Some(dialog)),
            1
        );
    };
    let (report, ()) = tokio::join!(headless(&dir, settings, None, "Hi", &[]), pick);
    assert_eq!(report["outcome"], "done", "{report:#}");
    assert_eq!(seen.lock().unwrap().len(), 2, "asked again once");
    let usage = &report["usage"];
    assert_eq!(usage["replies"], 2, "{report:#}");
    assert_eq!(usage["repliesWithoutUsage"], 0);
    assert_eq!(usage["leadInputTokens"], 1_000);
    assert_eq!(usage["leadOutputTokens"], 20);
    assert!(report["runLog"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["error"] == "asked-again" && l["usage"]["prompt_tokens"] == 500));
}

/// S10: a Balanced Autopilot task, end to end from the command line: the
/// runner waits for the whole task, and its report lists the stages, the
/// task's outcome and its report's facts.
#[tokio::test]
async fn an_autopilot_task_runs_headless_to_its_report() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    for (path, content) in [
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        (
            "check.js",
            "const fs = require('fs'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== 'ok') { console.error('ok.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("ok.txt", "no"),
    ] {
        std::fs::write(project.join(path), content).unwrap();
    }
    let plan = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Criteria\n\n1. `npm test` passes: `check.js` finds `ok.txt` saying ok.\n\n**Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
        1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
        2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
        ## Validation\n- `npm test`\n";
    let report_text = "## Assumptions\n\nNone.\n\n## What changed\n\n`ok.txt` says ok.\n\n## How it was verified\n\n`npm test` passes.\n\n## Open\n\nNothing.";
    let (url, _) = model_routed(
        vec![
            ("# Review this plan", vec![Reply::Text("## Findings\n\nNone.")]),
            (
                "# Write the tests before the change",
                vec![Reply::Text("## Blocked\n\nNothing to test here.")],
            ),
            (
                "# Check this change against its criteria",
                vec![Reply::Text("## Verdicts\n\n1. satisfied — `npm test` passes: check.js finds ok.txt saying ok.\n\n## Evidence\n\nRead ok.txt.\n\n## Unmet\n\nNone.")],
            ),
            ("last:# Write the task's report", vec![Reply::Text(report_text)]),
        ],
        vec![
            Reply::Call {
                id: "p1",
                name: "submit_plan",
                args: json!({ "plan": plan }),
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
    let settings = dir.0.join("settings.json");
    std::fs::write(
        &settings,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "apiKeyEnv": "ROVIK_HEADLESS_TEST_KEY",
            "mode": "agent",
        })
        .to_string(),
    )
    .unwrap();
    let out = dir.0.join("out").join("report.json");
    let argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings.to_str().unwrap(),
        "--prompt",
        "Make the test pass",
        "--out",
        out.to_str().unwrap(),
        "--data",
        dir.0.join("data").to_str().unwrap(),
        "--timeout",
        "120",
        "--mode",
        "autopilot",
        "--effort",
        "balanced",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let args = crate::parse_args(&argv).unwrap();
    crate::run(&args).await.unwrap();

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["mode"], "autopilot");
    assert_eq!(report["effort"], "balanced");
    assert_eq!(report["outcome"], "verified", "{report:#}");
    // G02: the checks' rounds, and no failed call.
    assert_eq!(report["verifyRounds"], 1, "{report:#}");
    assert_eq!(report["toolErrors"], 0, "{report:#}");
    assert_eq!(report["editMisses"], 0);
    let stages: Vec<(String, String)> = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = [
        ("plan", "done"),
        ("planReview", "done"),
        ("prepare", "skipped"),
        ("tests", "skipped"),
        ("implement", "done"),
        ("review", "skipped"),
        ("checks", "done"),
        ("acceptance", "done"),
        ("deliver", "done"),
    ]
    .iter()
    .map(|(n, s)| (n.to_string(), s.to_string()))
    .collect();
    assert_eq!(stages, expected, "{report:#}");
    assert_eq!(report["report"]["outcome"], "verified");
    assert_eq!(report["report"]["delivered"], true);
    assert_eq!(report["finalMessage"], report_text);
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
}

/// D01 (steering-plan.md): a scripted message sent once the lead chat has
/// a tool result is read at the run's next step, and the report says so.
#[tokio::test]
async fn a_scripted_steer_is_read_and_reported() {
    let dir = temp_dir();
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "old\n").unwrap();
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        // Slow: the message goes while this reply is on its way.
        Reply::Slow(
            400,
            Box::new(Reply::Call {
                id: "r2",
                name: "read",
                args: json!({ "path": "a.txt" }),
            }),
        ),
        Reply::Text("Kept a.txt as it is, as asked."),
    ])
    .await;
    let settings = dir.0.join("settings.json");
    std::fs::write(
        &settings,
        json!({
            "profile": { "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": url, "stream": true, "contextTokens": 100000 },
            "mode": "agent",
            "config": { "permissions": { "verifyChanges": false, "checklist": "off" } },
        })
        .to_string(),
    )
    .unwrap();
    let out = dir.0.join("report.json");
    let argv: Vec<String> = [
        "--project",
        project.to_str().unwrap(),
        "--settings",
        settings.to_str().unwrap(),
        "--prompt",
        "Look at a.txt",
        "--out",
        out.to_str().unwrap(),
        "--timeout",
        "60",
        "--steer",
        "don't change a.txt@1",
        "--steer",
        "never sent@99",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let args = crate::parse_args(&argv).unwrap();
    crate::run(&args).await.unwrap();

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["outcome"], "done");
    let steers = report["steers"].as_array().unwrap();
    assert_eq!(steers.len(), 2);
    assert_eq!(steers[0]["text"], "don't change a.txt");
    assert_eq!(steers[0]["after"], 1);
    assert_eq!(steers[0]["reply"], "queued");
    assert_eq!(steers[0]["delivered"]["phase"], "delivered", "{steers:?}");
    assert_eq!(steers[0]["returned"], Value::Null);
    // Its run ended first: never sent.
    assert_eq!(steers[1]["reply"], Value::Null);
    // The model's next request had it.
    let requests = seen.lock().unwrap().clone();
    assert!(requests
        .iter()
        .any(|r| r.to_string().contains("don't change a.txt")));
    let run = report["runLog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["event"] == "run")
        .cloned()
        .unwrap();
    assert_eq!(run["steers"], 1, "{run}");
}

//! G00: a scripted task run by the headless runner (src/headless.rs) from
//! its command line, to its JSON report.

use super::*;

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
    let args = crate::headless::parse_args(&argv).unwrap();
    crate::headless::run(&args).await.unwrap();

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["outcome"], "done");
    assert_eq!(report["toolCalls"], 2);
    assert_eq!(report["filesWritten"], json!(["a.txt"]));
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
        Reply::Usage(400_000, 1_000, Box::new(read("r2"))),
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
    let args = crate::headless::parse_args(&argv).unwrap();
    let started = std::time::Instant::now();
    crate::headless::run(&args).await.unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(25));

    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["outcome"], "budget", "{report:#}");
    assert_eq!(report["usage"]["leadInputTokens"], 1_200_000);
    assert_eq!(report["usage"]["leadOutputTokens"], 3_000);
    assert_eq!(report["usage"]["peakLeadContext"], 400_000);
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
    let args = crate::headless::parse_args(&argv).unwrap();
    crate::headless::run(&args).await.unwrap();
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

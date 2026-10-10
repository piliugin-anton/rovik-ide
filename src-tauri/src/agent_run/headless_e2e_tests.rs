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
    assert_eq!(report["approvals"][0]["tool"], "bash");
    assert_eq!(report["approvals"][0]["decision"], "deny");
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

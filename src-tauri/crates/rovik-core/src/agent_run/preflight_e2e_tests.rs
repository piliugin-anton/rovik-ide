//! Completion preflight uses the real checks and scripted model review.

// Command environment is process-wide; keep host-toolchain tests from
// changing it between the before/after snapshots (also under cargo test).
#![allow(clippy::await_holding_lock)]

use super::*;

fn allow_checks() -> Permissions {
    Permissions {
        allow_any_command: true,
        ..checks_on()
    }
}

#[tokio::test]
async fn unavailable_preflight_tool_does_not_prevent_review() {
    let _environment = stable_check_environment();
    unavailable_preflight(false).await;
}

#[tokio::test]
async fn mixed_unavailable_and_failing_preflight_checks_keep_normal_review_order() {
    let _environment = stable_check_environment();
    unavailable_preflight(true).await;
}

async fn unavailable_preflight(with_actionable_failure: bool) {
    let h = harness(&[("a.txt", "old\n")]).await;
    let mut checks = vec![
        json!({ "kind": "lint", "command": "rovik_preflight_missing_program", "enabled": true, "reuseSuccess": true }),
    ];
    if with_actionable_failure {
        checks.push(json!({ "kind": "typecheck", "command": "false", "enabled": true, "reuseSuccess": true }));
    }
    h.set_checks(json!(checks));
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![
            write_call("w1", "a.txt", "hello\n"),
            Reply::Text("Done."),
            Reply::Text("The lint program is unavailable; I cannot verify it here."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    assert_eq!(requests_with(&seen, REVIEW).len(), 1);
    let order = record_order(&h.messages().await);
    assert_eq!(
        order.first().map(String::as_str),
        Some("verification:unverified"),
        "{order:?}"
    );
    let reviewed = order.iter().position(|s| s == "review:clean").unwrap();
    let failed = order
        .iter()
        .position(|s| s == "verification:failed")
        .unwrap();
    assert!(
        reviewed < failed,
        "the final gate still reports the unavailable check: {order:?}"
    );
}

#[tokio::test]
async fn a_review_fix_invalidates_the_successful_preflight() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(
        json!([{ "kind": "lint", "command": "true", "enabled": true, "reuseSuccess": true }]),
    );
    let resolved =
        "## Earlier findings\n\n- Earlier 1: resolved — a.txt says hello\n\n## Findings\n\nNone.";
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(FINDING_A), Reply::Text(resolved)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            answered("a1", "fixed", "The greeting is spelled hello."),
            Reply::Text("Fixed."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verified");
    assert_eq!(requests_with(&seen, REVIEW).len(), 2);
    let messages = h.messages().await;
    let records: Vec<_> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .collect();
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0]["preflight"], true);
    let lint = &records[1]["checks"][0];
    assert_eq!(lint["status"], "passed");
    assert!(
        lint["note"]
            .as_str()
            .is_none_or(|n| !n.starts_with("Reused unchanged results")),
        "reviewer fix must be checked again: {lint}"
    );
}

#[tokio::test]
async fn stopping_preflight_never_starts_the_reviewer() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(
        json!([{ "kind": "lint", "command": "sleep 30", "enabled": true, "reuseSuccess": true }]),
    );
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![write_call("w1", "a.txt", "hello\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    h.wait_for("running preflight", |e| {
        e.pointer("/patch/meta/verification/checks/0/status") == Some(&json!("running"))
    })
    .await;
    assert!(agent_stop(&h.core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "stopped");
    assert!(requests_with(&seen, REVIEW).is_empty());
}

#[tokio::test]
async fn checks_without_reuse_opt_in_keep_their_single_execution_after_review() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(json!([{ "kind": "typecheck", "command": "true", "enabled": true }]));
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![write_call("w1", "a.txt", "hello\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verified");
    assert_eq!(
        record_order(&h.messages().await),
        ["review:clean", "verification:passed"]
    );
    assert_eq!(requests_with(&seen, REVIEW).len(), 1);
}

#[tokio::test]
async fn introduced_preflight_failure_is_fixed_before_the_first_review() {
    let _environment = stable_check_environment();
    let h = harness(&[
        ("a.txt", "old\n"),
        (
            "check.sh",
            "if grep -q helo a.txt; then echo 'a.txt:1: error: misspelled greeting'; exit 1; fi\n",
        ),
    ])
    .await;
    h.set_checks(json!([{ "kind": "typecheck", "command": "sh check.sh", "enabled": true, "reuseSuccess": true }]));
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            Reply::Text("Fixed the greeting."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let order = record_order(&messages);
    let failed = order
        .iter()
        .position(|s| s == "verification:failed")
        .expect("preflight failure");
    let reviewed = order
        .iter()
        .position(|s| s == "review:clean")
        .expect("review after repair");
    assert!(failed < reviewed, "{order:?}");
    assert_eq!(
        requests_with(&seen, REVIEW).len(),
        1,
        "broken code spent no review turn"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\n"));
}

#[tokio::test]
async fn passing_preflight_precedes_review_and_full_verification_still_runs() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(json!([
        { "kind": "lint", "command": "true", "enabled": true, "reuseSuccess": true },
        { "kind": "test", "command": "true", "enabled": true, "reuseSuccess": true }
    ]));
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![write_call("w1", "a.txt", "hello\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let order = record_order(&messages);
    assert_eq!(
        order.first().map(String::as_str),
        Some("verification:passed"),
        "{order:?}"
    );
    assert_eq!(requests_with(&seen, REVIEW).len(), 1);
    let records: Vec<_> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .collect();
    let first = records.first().unwrap()["checks"].as_array().unwrap();
    assert!(first.iter().all(|c| c["kind"] == "lint"));
    assert!(records.last().unwrap()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["kind"] == "test" && c["status"] == "passed"));
    let final_checks = records.last().unwrap()["checks"].as_array().unwrap();
    let lint = final_checks.iter().find(|c| c["kind"] == "lint").unwrap();
    assert!(
        lint["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("Reused unchanged results")),
        "{lint}"
    );
    let test = final_checks.iter().find(|c| c["kind"] == "test").unwrap();
    assert!(
        test["note"]
            .as_str()
            .is_none_or(|n| !n.starts_with("Reused unchanged results")),
        "a lint result is not a test result: {test}"
    );
}

#[tokio::test]
async fn disabled_preflight_checks_do_not_run_or_delay_review() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(json!([{ "kind": "typecheck", "command": "exit 1", "enabled": false, "reuseSuccess": true }]));
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![write_call("w1", "a.txt", "hello\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", allow_checks(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let order = record_order(&h.messages().await);
    assert_eq!(
        order.first().map(String::as_str),
        Some("review:clean"),
        "{order:?}"
    );
    assert_eq!(requests_with(&seen, REVIEW).len(), 1);
}

#[tokio::test]
async fn refused_preflight_checks_leave_the_change_available_for_review() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "old\n")]).await;
    h.set_checks(
        json!([{ "kind": "lint", "command": "echo ran > check-ran.txt", "enabled": true, "reuseSuccess": true }]),
    );
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![write_call("w1", "a.txt", "hello\n"), Reply::Text("Done.")],
    )
    .await;
    let mut permissions = checks_on();
    permissions.allow_any_command = false;
    permissions.denied_commands = vec!["echo".into()];
    h.start(h.with_subagents(&url, "Greet", permissions, reviewer_on(json!({}))))
        .await;
    let end = h.run_end().await;
    assert_ne!(end, "verified");
    assert_eq!(requests_with(&seen, REVIEW).len(), 1);
    assert!(h.file("check-ran.txt").is_none());
    assert!(!record_order(&h.messages().await)
        .iter()
        .any(|s| s == "verification:failed"));
}

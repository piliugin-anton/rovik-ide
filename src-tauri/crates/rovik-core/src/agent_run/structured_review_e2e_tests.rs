//! Typed change reviews through the real request, dispatch and review gates.

use super::*;

fn submit(id: &'static str, args: Value) -> Reply {
    Reply::Call {
        id,
        name: "submit_review",
        args,
    }
}

fn greeting_finding() -> Value {
    json!({
        "severity": "high", "path": "a.txt", "line": 1,
        "title": "The greeting is wrong",
        "scenario": "Anyone reading a.txt sees helo.",
        "fix": "Spell it hello.", "confidence": "confirmed",
        "evidence": "a.txt:1 contains helo instead of hello.",
        "new_evidence": null
    })
}

#[tokio::test]
async fn a_typed_review_finishes_in_one_turn_and_settles_later_calls_without_running_them() {
    let h = harness(&[("a.txt", "old\n"), ("private.txt", "UNREAD-CONTENT\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Calls(vec![
                (
                    "final",
                    "submit_review",
                    json!({"findings": [], "verdicts": []}),
                ),
                ("late-read", "read", json!({"path": "private.txt"})),
                (
                    "duplicate",
                    "submit_review",
                    json!({"findings": [greeting_finding()], "verdicts": []}),
                ),
            ])],
        )],
        vec![write_call("w1", "a.txt", "new\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        ["review:clean", "verification:unverified"]
    );
    let requests = requests_with(&seen, REVIEW);
    assert_eq!(
        requests.len(),
        1,
        "a final tool submission needs no formatting turn"
    );
    let schema = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "submit_review")
        .unwrap();
    assert_eq!(schema["function"]["strict"], true);
    assert_eq!(
        schema["function"]["parameters"]["required"],
        json!(["findings", "verdicts"])
    );
    let lead = seen.lock().unwrap()[0].clone();
    assert!(!tool_names(&lead).iter().any(|name| name == "submit_review"));
    let rounds = reviews(&messages);
    let child = rounds[0]["childDialogId"].as_str().unwrap();
    let work = h.messages_of(child).await;
    for id in ["final", "late-read", "duplicate"] {
        let results: Vec<_> = work
            .iter()
            .filter(|m| m.tool_call_id.as_deref() == Some(id))
            .collect();
        assert_eq!(
            results.len(),
            1,
            "every tool call ID is answered exactly once"
        );
        if id == "final" {
            assert!(results[0].content.contains(NO_FINDINGS));
        } else {
            assert!(results[0].content.contains("already submitted"));
            assert!(!results[0].content.contains("UNREAD-CONTENT"));
        }
    }
    assert!(!work.iter().any(|message| message
        .meta
        .as_ref()
        .is_some_and(|meta| meta["harness"] == "subagentFormat")));
}

#[tokio::test]
async fn typed_findings_reach_the_lead_and_unknown_or_missing_verdicts_cannot_close_them() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![
            submit("finding", json!({"findings": [greeting_finding()], "verdicts": []})),
            submit("unknown", json!({"findings": [], "verdicts": [{"finding": 2, "resolved": true, "reason": "Fixed."}]})),
            submit("missing", json!({"findings": [], "verdicts": []})),
            submit("resolved", json!({"findings": [], "verdicts": [{"finding": 1, "resolved": true, "reason": "a.txt:1 now says hello."}]})),
        ])],
        vec![
            write_call("w1", "a.txt", "helo\n"), Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            answered("a1", "fixed", "It says hello now."), Reply::Text("Fixed it."),
        ],
    ).await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        [
            "review:findings",
            "note",
            "review:clean",
            "verification:unverified"
        ]
    );
    assert_eq!(requests_with(&seen, REVIEW).len(), 4);
    let rounds = reviews(&messages);
    assert_eq!(
        rounds[0]["findings"][0]["evidence"],
        "a.txt:1 contains helo instead of hello."
    );
    assert_eq!(rounds[0]["findings"][0]["severity"], "high");
    let work = h
        .messages_of(rounds[1]["childDialogId"].as_str().unwrap())
        .await;
    for (id, reason) in [
        ("unknown", "does not exist"),
        ("missing", "exactly one verdict"),
    ] {
        let result = work
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap();
        assert!(result.content.contains(reason), "{}", result.content);
        assert!(result.content.contains("nothing was accepted"));
    }
    let accepted = work
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("resolved"))
        .unwrap();
    assert!(accepted.content.contains("Earlier 1: resolved"));
}

#[tokio::test]
async fn a_missing_typed_report_is_recorded_as_skipped_when_no_valid_report_follows() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                submit("malformed", json!({"verdicts": []})),
                Reply::Text("Looks fine."),
                Reply::Text("Still fine."),
            ],
        )],
        vec![write_call("w1", "a.txt", "new\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        ["review:skipped", "verification:unverified"]
    );
    let rounds = reviews(&messages);
    let work = h
        .messages_of(rounds[0]["childDialogId"].as_str().unwrap())
        .await;
    let result = work
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("malformed"))
        .unwrap();
    assert!(result.content.contains("missing field `findings`"));
    assert!(result.content.contains("nothing was accepted"));
}

#[tokio::test]
async fn a_provider_rejecting_the_schema_keeps_the_legacy_review_format() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Status(
                    400,
                    r#"{"error":{"message":"Unsupported parameter: tools[7].function.strict"}}"#,
                ),
                // The common transport first retries without stream usage.
                // A persistent schema rejection reaches the reviewer gate.
                Reply::Status(
                    400,
                    r#"{"error":{"message":"Unsupported parameter: tools[7].function.strict"}}"#,
                ),
                write_call("forbidden", "a.txt", "reviewer-wrote\n"),
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![write_call("w1", "a.txt", "new\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    assert_eq!(
        record_order(&h.messages().await),
        ["review:clean", "verification:unverified"]
    );
    let requests = requests_with(&seen, REVIEW);
    assert_eq!(
        requests.len(),
        4,
        "one transport retry, then legacy inspection and a report"
    );
    assert!(tool_names(&requests[0])
        .iter()
        .any(|name| name == "submit_review"));
    assert!(tool_names(&requests[1])
        .iter()
        .any(|name| name == "submit_review"));
    assert!(tool_names(&requests[2]).iter().any(|name| name == "read"));
    assert!(!tool_names(&requests[2])
        .iter()
        .any(|name| name == "submit_review"));
    assert!(!tool_names(&requests[2]).iter().any(|name| name == "write"));
    assert!(!system_prompt(&requests[2]).contains(crate::agent_run::structured_report::INSTRUCTION));
    // The existing saved report parser still reads the persisted legacy reply.
    let rounds = reviews(&h.messages().await);
    let work = h
        .messages_of(rounds[0]["childDialogId"].as_str().unwrap())
        .await;
    let refused = work
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("forbidden"))
        .unwrap();
    assert!(refused.content.contains("isn't available"));
    assert_eq!(h.file("a.txt").as_deref(), Some("new\n"));
    let saved = work
        .iter()
        .find(|message| message.role == "assistant" && message.content == NO_FINDINGS)
        .unwrap();
    assert!(
        crate::agent_run::change_review::parse_findings(&saved.content)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn schema_rejection_after_the_legacy_retry_is_skipped_without_looping() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Status(
                400,
                r#"{"error":{"message":"Unsupported parameter: tools[7].function.strict"}}"#,
            )],
        )],
        vec![write_call("w1", "a.txt", "new\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    assert_eq!(
        record_order(&h.messages().await),
        ["review:skipped", "verification:unverified"]
    );
    let requests = requests_with(&seen, REVIEW);
    assert_eq!(
        requests.len(),
        4,
        "one schema turn, one legacy turn, each with the transport's usage retry"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| tool_names(request)
                .iter()
                .any(|name| name == "submit_review"))
            .count(),
        2
    );
}

#[tokio::test]
async fn a_lead_cannot_submit_a_reviewer_report() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        submit("wrong-role", json!({"findings": [], "verdicts": []})),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Inspect it"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("wrong-role").await;
    assert!(result
        .content
        .contains("only available to a change reviewer"));
    assert!(!tool_names(&seen.lock().unwrap()[0])
        .iter()
        .any(|name| name == "submit_review"));
    assert!(reviews(&h.messages().await).is_empty());
}

//! Implementation evidence is written by the real run on both button and
//! ordinary file-reference paths, and a stop cannot be mistaken for success.
use super::*;

const PLAN: &str =
    "# Contact form\n\n## Steps\n1. Add the submit button.\n\n## Validation\n- Check the form.\n";

fn button_request(h: &Harness) -> StartKind {
    StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Implement \"Contact form\" plan.".into(),
            meta: Some(json!({ "mode": "agent", "implementsPlan": true,
            "planPath": h.project.join(".rovik/plans/contact.md").to_string_lossy() })),
            attachments: vec![],
        },
    }
}

fn todo(status: &str) -> Reply {
    let args = json!({ "items": [{ "title": "Add the submit button", "status": status }] });
    Reply::Call {
        id: "todo",
        name: "todo",
        args,
    }
}

#[tokio::test]
async fn successful_button_implementation_persists_its_completed_checklist_and_revision() {
    let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
    let (url, _) = model(vec![todo("done"), Reply::Text("Done.")]).await;
    let run_id = h
        .start(h.request(&url, button_request(&h), quiet_permissions()))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");
    let rows = h.messages().await;
    let meta = rows
        .iter()
        .find(|m| m.role == "user")
        .unwrap()
        .meta
        .as_ref()
        .unwrap();
    assert_eq!(meta["planImplementation"]["status"], "implemented");
    assert_eq!(meta["planImplementation"]["runId"], run_id);
    assert_eq!(meta["planFingerprint"], plan::plan_fingerprint(PLAN));
    assert_eq!(meta["planKey"], ".rovik/plans/contact.md");
    assert!(h
        .events_of("messagePatched")
        .iter()
        .any(|e| e["patch"]["meta"]["planImplementation"]["status"] == "implementing"));
}

#[tokio::test]
async fn typed_and_at_referenced_plans_track_implementation_without_the_button() {
    for attached in [false, true] {
        let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
        let instruction = if attached {
            "Implement contact.md"
        } else {
            "Implement `.rovik/plans/contact.md`."
        };
        let associated = Reply::Call {
            id: "todo",
            name: "todo",
            args: json!({
                "implementation": { "planPath": ".rovik/plans/contact.md", "instruction": instruction },
                "items": [{ "title": "Add the submit button", "status": "done" }]
            }),
        };
        let (url, _) = model(vec![associated, Reply::Text("Done.")]).await;
        let start = if attached {
            StartKind::Send {
                message: UserMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    content: "Implement contact.md".into(),
                    meta: Some(
                        json!({ "mode": "agent", "sentContent": "The user explicitly referenced these project files:\n- .rovik/plans/contact.md\n\nImplement contact.md" }),
                    ),
                    attachments: vec![],
                },
            }
        } else {
            Harness::send("Implement `.rovik/plans/contact.md`.")
        };
        h.start(h.request(&url, start, quiet_permissions())).await;
        assert_eq!(h.run_end().await, "done");
        let rows = h.messages().await;
        let meta = rows
            .iter()
            .find(|m| m.role == "user")
            .unwrap()
            .meta
            .as_ref()
            .unwrap();
        assert_eq!(
            meta["planImplementation"]["status"], "implemented",
            "{meta}"
        );
    }
}

#[tokio::test]
async fn implementation_intent_preserves_the_users_original_language() {
    for instruction in [
        "Реализуй план contact.md",
        "実装してください: contact.md の計画",
        "Implementa el plan contact.md",
        "يرجى تنفيذ الخطة contact.md",
    ] {
        let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
        let (url, _) = model(vec![Reply::Call { id: "todo", name: "todo", args: json!({
            "implementation": { "planPath": ".rovik/plans/contact.md", "instruction": instruction },
            "items": [{ "title": "Add the submit button", "status": "done" }]
        }) }, Reply::Text("Done.")]).await;
        let start = StartKind::Send {
            message: UserMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content: instruction.into(),
                meta: Some(
                    json!({ "mode": "agent", "sentContent": format!("The user explicitly referenced these project files:\n- .rovik/plans/contact.md\n\n{instruction}") }),
                ),
                attachments: vec![],
            },
        };
        h.start(h.request(&url, start, quiet_permissions())).await;
        assert_eq!(h.run_end().await, "done");
        let rows = h.messages().await;
        let meta = rows
            .iter()
            .find(|m| m.role == "user")
            .unwrap()
            .meta
            .as_ref()
            .unwrap();
        assert_eq!(meta["planImplementation"]["status"], "implemented");
        assert_eq!(meta["planImplementationInstruction"], instruction);
    }
}

#[tokio::test]
async fn invented_implementation_instructions_and_unreferenced_plans_are_rejected() {
    for (path, instruction) in [
        (
            ".rovik/plans/contact.md",
            "An instruction the user did not write",
        ),
        (".rovik/plans/other.md", "Carry out the contact plan"),
    ] {
        let h = harness(&[
            (".rovik/plans/contact.md", PLAN),
            (".rovik/plans/other.md", PLAN),
        ])
        .await;
        let (url, _) = model(vec![
            Reply::Call {
                id: "todo",
                name: "todo",
                args: json!({
                    "implementation": { "planPath": path, "instruction": instruction },
                    "items": [{ "title": "Add the submit button", "status": "done" }]
                }),
            },
            Reply::Text("Done."),
        ])
        .await;
        h.start(h.request(
            &url,
            Harness::send("Carry out the contact plan `.rovik/plans/contact.md`."),
            quiet_permissions(),
        ))
        .await;
        h.run_end().await;
        let rows = h.messages().await;
        assert!(rows.iter().all(|m| m
            .meta
            .as_ref()
            .is_none_or(|m| m.get("planImplementation").is_none())));
        let result = rows
            .iter()
            .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("todo"))
            .unwrap();
        assert!(result.call_failed());
    }
}

#[tokio::test]
async fn reviewing_a_referenced_plan_does_not_mark_it_implemented() {
    for text in [
        "Review `.rovik/plans/contact.md`.",
        "Read `.rovik/plans/contact.md`.",
        "Implement another feature. Read `.rovik/plans/contact.md` only for context.",
    ] {
        let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
        let (url, _) = model(vec![
            Reply::Call {
                id: "read-plan",
                name: "read",
                args: json!({ "path": ".rovik/plans/contact.md" }),
            },
            todo("done"),
            Reply::Text("Reviewed the plan."),
        ])
        .await;
        h.start(h.request(&url, Harness::send(text), quiet_permissions()))
            .await;
        assert_eq!(h.run_end().await, "done");
        assert!(h.messages().await.iter().all(|m| m
            .meta
            .as_ref()
            .is_none_or(|meta| meta.get("planImplementation").is_none())));
    }
}

#[tokio::test]
async fn stopping_after_a_completed_checklist_does_not_mark_the_plan_implemented() {
    let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
    let (url, _) = model(vec![todo("done"), Reply::Hang(None)]).await;
    h.start(h.request(&url, button_request(&h), quiet_permissions()))
        .await;
    h.wait_for("completed checklist", |e| {
        e["type"] == "checklist" && e["items"][0]["status"] == "done"
    })
    .await;
    assert!(agent_stop(&h.core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "stopped");
    let rows = h.messages().await;
    let meta = rows
        .iter()
        .find(|m| m.role == "user")
        .unwrap()
        .meta
        .as_ref()
        .unwrap();
    assert_eq!(meta["planImplementation"]["status"], "interrupted");
}

#[tokio::test]
async fn a_successful_run_with_a_skipped_step_requires_review() {
    let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
    let (url, _) = model(vec![
        todo("skipped"),
        Reply::Text("The step was unnecessary."),
    ])
    .await;
    h.start(h.request(&url, button_request(&h), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let rows = h.messages().await;
    let meta = rows
        .iter()
        .find(|m| m.role == "user")
        .unwrap()
        .meta
        .as_ref()
        .unwrap();
    assert_eq!(meta["planImplementation"]["status"], "review-needed");
}

#[tokio::test]
async fn a_later_read_request_cannot_complete_an_earlier_interrupted_implementation() {
    let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
    let (url, _) = model(vec![todo("pending"), Reply::Hang(None)]).await;
    h.start(h.request(&url, button_request(&h), quiet_permissions()))
        .await;
    h.wait_for("pending checklist", |e| {
        e["type"] == "checklist" && e["items"][0]["status"] == "pending"
    })
    .await;
    agent_stop(&h.core, h.dialog_id.clone()).await;
    assert_eq!(h.run_end().await, "stopped");
    h.events.lock().unwrap().clear();

    let (url, _) = model(vec![todo("done"), Reply::Text("Read the plan.")]).await;
    h.start(h.request(
        &url,
        Harness::send("Read `.rovik/plans/contact.md` and explain it."),
        quiet_permissions(),
    ))
    .await;
    h.run_end().await;
    let rows = h.messages().await;
    let original = rows
        .iter()
        .find(|m| m.role == "user")
        .unwrap()
        .meta
        .as_ref()
        .unwrap();
    assert_eq!(original["planImplementation"]["status"], "interrupted");
    let latest = rows.iter().rev().find(|m| m.role == "user").unwrap();
    assert!(latest
        .meta
        .as_ref()
        .is_none_or(|m| m.get("planImplementation").is_none()));
}

#[tokio::test]
async fn a_button_request_keeps_its_explicit_intent_when_the_agent_quotes_the_expanded_plan_prompt()
{
    let h = harness(&[(".rovik/plans/contact.md", PLAN)]).await;
    let (url, _) = model(vec![Reply::Call { id: "todo", name: "todo", args: json!({
        "implementation": { "planPath": ".rovik/plans/contact.md", "instruction": "Implement this plan." },
        "items": [{ "title": "Add the submit button", "status": "done" }]
    }) }, Reply::Text("Done.")]).await;
    h.start(h.request(&url, button_request(&h), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let rows = h.messages().await;
    let result = rows
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("todo"))
        .unwrap();
    assert!(!result.call_failed(), "{}", result.content);
    let meta = rows
        .iter()
        .find(|m| m.role == "user")
        .unwrap()
        .meta
        .as_ref()
        .unwrap();
    assert_eq!(meta["planImplementation"]["status"], "implemented");
}

//! What a chat's stored rows say about a run the process ended (5.4).

use super::*;
use crate::chat_db::store::{self, MessageRow, NewDialog, NewMessage, ToolCall};
use crate::testing::TestCore;
use serde_json::{json, Value};

/// A chat with these messages, in a fresh core.
async fn chat(messages: Vec<NewMessage>) -> (TestCore, String) {
    let test = TestCore::new();
    let id = "chat".to_string();
    let dialog = NewDialog {
        id: id.clone(),
        title: "Chat".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
        folders: None,
    };
    let d = id.clone();
    test.core
        .db
        .write(move |c| {
            store::create_dialog(c, &dialog)?;
            store::append_messages(c, &d, &messages).map(|_| ())
        })
        .await
        .unwrap();
    (test, id)
}

fn message(id: &str, role: &str, content: &str) -> NewMessage {
    NewMessage {
        id: id.into(),
        role: role.into(),
        content: content.into(),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: Vec::new(),
        created_at: 1,
        finalized: true,
    }
}

/// A reply the process ended mid-stream, as its last checkpoint left it.
fn cut(id: &str, content: &str, calls: &[&str]) -> NewMessage {
    let mut m = step(id, content, calls);
    m.finalized = false;
    m
}

/// A finished reply that called these tools.
fn step(id: &str, content: &str, calls: &[&str]) -> NewMessage {
    let mut m = message(id, "assistant", content);
    m.tool_calls = (!calls.is_empty()).then(|| {
        calls
            .iter()
            .map(|call| ToolCall {
                id: call.to_string(),
                name: "bash".into(),
                arguments: r#"{"command":"sleep 1"#.into(),
                extra_content: None,
            })
            .collect()
    });
    m
}

fn result(call: &str) -> NewMessage {
    let mut m = message(&format!("r-{call}"), "tool", "ok");
    m.tool_call_id = Some(call.into());
    m
}

fn record(id: &str, meta: Value) -> NewMessage {
    let mut m = message(id, "assistant", "");
    m.meta = Some(meta);
    m
}

async fn rows(core: &Core, dialog_id: &str) -> Vec<MessageRow> {
    recent_messages(&core.db, dialog_id).await.unwrap()
}

async fn recovery(messages: Vec<NewMessage>) -> Recovery {
    let (test, id) = chat(messages).await;
    after_crash(&test.core, &id).await.unwrap()
}

#[tokio::test]
async fn a_cut_reply_with_text_is_kept_and_continued() {
    let (test, id) = chat(vec![
        message("u", "user", "Explain it"),
        cut("a", "The first half", &["c1"]),
    ])
    .await;
    let recovery = after_crash(&test.core, &id).await.unwrap();
    assert_eq!(recovery, Recovery::Start(StartKind::ContinueReply));
    let rows = rows(&test.core, &id).await;
    let reply = rows.last().unwrap();
    assert!(reply.finalized);
    assert_eq!(reply.content, "The first half");
    assert!(
        reply.tool_calls.as_ref().is_none_or(|c| c.is_empty()),
        "a call cut in half goes"
    );
    let interrupted = &reply.meta.as_ref().unwrap()["interrupted"];
    assert_eq!(interrupted["kind"], "restarted");
    assert_eq!(reply.error, None);
    // The chat sees it settle.
    let events = test.events.lock().unwrap().clone();
    let patched = events
        .iter()
        .find(|e| e["type"] == "messagePatched")
        .unwrap();
    assert_eq!(patched["messageId"], "a");
    assert_eq!(patched["patch"]["finalized"], true);
}

#[tokio::test]
async fn an_empty_cut_reply_is_marked_failed_and_retried() {
    let (test, id) = chat(vec![message("u", "user", "Hi"), cut("a", "", &[])]).await;
    let recovery = after_crash(&test.core, &id).await.unwrap();
    assert_eq!(recovery, Recovery::Start(StartKind::Retry));
    let rows = rows(&test.core, &id).await;
    let reply = rows.last().unwrap();
    assert!(reply.finalized);
    assert_eq!(reply.meta.as_ref().unwrap()["failure"]["kind"], "restarted");
    assert_eq!(
        reply.error.as_deref(),
        Some("The server restarted while the reply was being written.")
    );
}

#[tokio::test]
async fn tool_calls_without_results_resume() {
    let recovery = recovery(vec![
        message("u", "user", "Run them"),
        step("a", "", &["c1", "c2"]),
        result("c1"),
    ])
    .await;
    assert_eq!(recovery, Recovery::Start(StartKind::Resume));
}

#[tokio::test]
async fn a_drain_pause_continues_and_another_pause_waits() {
    let paused = |kind: &str| {
        vec![
            message("u", "user", "Go on"),
            step("a", "", &["c1"]),
            result("c1"),
            record("p", json!({ "paused": { "kind": kind, "toolCalls": 1 } })),
        ]
    };
    assert_eq!(
        recovery(paused(DRAIN)).await,
        Recovery::Start(StartKind::ContinueRun)
    );
    assert_eq!(recovery(paused("checkpoint")).await, Recovery::Paused);
    assert_eq!(recovery(paused("loop")).await, Recovery::Paused);
}

#[tokio::test]
async fn anything_else_was_interrupted() {
    let finished = vec![message("u", "user", "Hi"), step("a", "Done.", &[])];
    // The end-of-run checks were going.
    let mut checking = finished.clone();
    checking.push(record(
        "v",
        json!({ "verification": { "status": "running", "round": 1 } }),
    ));
    // Between two steps: every call had its result.
    let between = vec![
        message("u", "user", "Hi"),
        step("a", "", &["c1"]),
        result("c1"),
    ];
    // Before the first step: only the message was stored.
    let sent = vec![message("u", "user", "Hi")];
    for messages in [finished, checking, between, sent, Vec::new()] {
        assert_eq!(recovery(messages).await, Recovery::Interrupted);
    }
}

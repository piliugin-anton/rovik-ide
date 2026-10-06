//! Whole runs against a scripted provider: a mock Tauri app with the real
//! chat database, the real tools and the real request code, talking to a
//! local server that plays the model.

use super::*;
use crate::chat_db::store::{self, MessageRow, NewDialog};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::{Listener, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/*
 * The model
 */

/// One streamed reply.
enum Reply {
    Text(&'static str),
    Call { id: &'static str, name: &'static str, args: Value },
}

impl Reply {
    fn sse(&self) -> String {
        let chunk = |v: Value| format!("data: {v}\n\n");
        match self {
            Reply::Text(text) => {
                chunk(json!({ "choices": [{ "delta": { "content": text }, "finish_reason": null }] }))
                    + &chunk(json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }))
                    + "data: [DONE]\n\n"
            }
            Reply::Call { id, name, args } => {
                let delta = json!({ "tool_calls": [{
                    "index": 0, "id": id, "type": "function",
                    "function": { "name": name, "arguments": args.to_string() }
                }]});
                chunk(json!({ "choices": [{ "delta": delta, "finish_reason": null }] }))
                    + &chunk(json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }))
                    + "data: [DONE]\n\n"
            }
        }
    }
}

/// A local server answering each request with the next scripted reply (the
/// last one again once they run out). Returns its base URL and the request
/// bodies it saw.
async fn model(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let bodies: Vec<String> = replies.iter().map(Reply::sse).collect();
    tokio::spawn(async move {
        let mut n = 0;
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = read_request(&mut socket).await;
            log.lock().unwrap().push(body);
            let reply = &bodies[n.min(bodies.len() - 1)];
            n += 1;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(format!("{head}{reply}").as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (url, seen)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Value {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                return serde_json::from_slice(&buf[end + 4..end + 4 + length]).unwrap_or_default();
            }
        }
    }
    Value::Null
}

/*
 * The app
 */

struct Harness {
    app: tauri::App<MockRuntime>,
    project: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
    dialog_id: String,
    _dir: TempDir,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("agent-e2e-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

async fn harness(files: &[(&str, &str)]) -> Harness {
    let dir = temp_dir();
    let project = dir.0.join("project");
    for (path, content) in files {
        let p = project.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    std::fs::create_dir_all(&project).unwrap();
    let db = DbState::open(&dir.0.join("data")).unwrap();
    let app = mock_builder()
        .manage(db.clone())
        .manage(crate::vfs::VfsState::default())
        .manage(crate::browser_tools::BrowserState::default())
        .manage(AgentState::default())
        .build(mock_context(noop_assets()))
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    app.listen_any(events::EVENT, move |e| {
        sink.lock().unwrap().push(serde_json::from_str::<Value>(e.payload()).unwrap());
    });
    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog { id: dialog_id.clone(), title: "Test".into(), created_at: 0, updated_at: 0, project_id: None };
    db.write(move |c| store::create_dialog(c, &dialog)).await.unwrap();
    Harness { app, project, events, dialog_id, _dir: dir }
}

fn quiet_permissions() -> Permissions {
    Permissions { verify_changes: false, ..Permissions::default() }
}

impl Harness {
    fn request(&self, url: &str, start: StartKind, permissions: Permissions) -> StartRequest {
        StartRequest {
            dialog_id: self.dialog_id.clone(),
            start,
            profile: ModelProfile {
                id: "p::m".into(),
                label: "Test/m".into(),
                model: "m".into(),
                base_url: url.into(),
                api_key: String::new(),
                stream: true,
                vision: false,
                context_tokens: Some(100_000),
                no_tools: false,
                first_byte_timeout_ms: None,
            },
            options: None,
            mode: Some("agent".into()),
            context_mode: Some("trimming".into()),
            root: Some(self.project.to_string_lossy().into_owned()),
            current_user_content: None,
            local_date: Some("2026-10-06".into()),
            config: Some(AgentConfig { permissions, approval_profiles: Vec::new() }),
        }
    }

    async fn start(&self, request: StartRequest) -> Option<String> {
        let app = self.app.handle().clone();
        agent_start(app.clone(), app.state::<AgentState>(), app.state::<DbState>(), request).await.unwrap()
    }

    fn send(text: &str) -> StartKind {
        StartKind::Send {
            message: UserMessage { id: uuid::Uuid::new_v4().to_string(), content: text.into(), meta: None },
        }
    }

    /// Waits for an event the predicate accepts; returns it.
    async fn wait_for(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..600 {
            if let Some(e) = self.events.lock().unwrap().iter().find(|e| pred(e)) {
                return e.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no {what} event; got {:#?}", self.events.lock().unwrap());
    }

    async fn run_end(&self) -> String {
        let e = self.wait_for("runEnded", |e| e["type"] == "runEnded").await;
        e["outcome"].as_str().unwrap().to_string()
    }

    async fn messages(&self) -> Vec<MessageRow> {
        let id = self.dialog_id.clone();
        let db = self.app.state::<DbState>().inner().clone();
        db.read(move |c| store::get_messages(c, &id, None, 500)).await.unwrap().messages
    }

    fn events_of(&self, kind: &str) -> Vec<Value> {
        self.events.lock().unwrap().iter().filter(|e| e["type"] == kind).cloned().collect()
    }

    fn file(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.project.join(path)).ok()
    }
}

fn roles(messages: &[MessageRow]) -> Vec<&str> {
    messages.iter().map(|m| m.role.as_str()).collect()
}

/*
 * Runs
 */

#[tokio::test]
async fn a_run_calls_tools_until_the_model_is_done() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, seen) = model(vec![
        Reply::Call { id: "c1", name: "write_file", args: json!({ "path": "hello.txt", "content": "hi\n" }) },
        Reply::Text("Wrote hello.txt."),
    ])
    .await;
    let run = h.start(h.request(&url, Harness::send("Write hello"), quiet_permissions())).await;
    assert!(run.is_some());
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("hello.txt").as_deref(), Some("hi\n"));
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "tool", "assistant"]);
    assert_eq!(messages[1].tool_calls.as_ref().unwrap()[0].name, "write_file");
    assert!(messages.iter().all(|m| m.finalized), "every message is finished");
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(messages[2].content, "wrote 3 bytes to hello.txt");
    assert_eq!(messages[3].content, "Wrote hello.txt.");

    // The second request carried the tool's result, with the tools and the system prompt.
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let sent = requests[1]["messages"].as_array().unwrap();
    assert_eq!(sent[0]["role"], "system");
    assert!(sent.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == "c1"));
    let tools: Vec<&str> = requests[0]["tools"].as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str()).collect();
    assert!(tools.contains(&"write_file") && tools.contains(&"todo"));

    // The UI heard about it.
    assert!(h.events_of("messagesAdded").len() >= 4);
    assert_eq!(h.events_of("fileWritten")[0]["content"], "hi\n");
    assert_eq!(h.events_of("fileWritten")[0]["reviewed"], false);
    let settled = h.events_of("userMessageSettled");
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0]["ok"], true);
    assert!(h.events_of("runStarted").len() == 1);
}

#[tokio::test]
async fn a_denied_edit_is_reported_to_the_model() {
    let h = harness(&[("a.txt", "one\ntwo\n")]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "e1", name: "edit_file", args: json!({ "path": "a.txt", "old_text": "two", "new_text": "TWO" }) },
        Reply::Text("Okay, leaving it."),
    ])
    .await;
    let permissions = Permissions {
        edit_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::User },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Shout"), permissions)).await;

    let asked = h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;
    let request = &asked["request"];
    assert_eq!(request["toolCallId"], "e1");
    assert_eq!(request["summary"], "Edit a.txt");
    assert_eq!(request["edit"]["current"], "one\ntwo\n");
    assert_eq!(request["edit"]["proposed"], "one\nTWO\n");
    assert!(request["edit"]["absPath"].as_str().unwrap().ends_with("a.txt"));

    let app = h.app.handle().clone();
    assert!(agent_resolve_approval(app.clone(), app.state(), "e1".into(), ApprovalDecision::Deny));
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("a.txt").as_deref(), Some("one\ntwo\n"));
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert!(result.content.starts_with("Error: the user rejected every change to a.txt"), "{}", result.content);
    assert_eq!(result.meta.as_ref().unwrap()["approval"], json!({ "by": "user", "approved": false }));
    assert_eq!(h.events_of("approvalSettled").len(), 1);
}

#[tokio::test]
async fn a_partly_accepted_edit_writes_only_those_changes() {
    let h = harness(&[("a.txt", "one\ntwo\nthree\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write_file",
            args: json!({ "path": "a.txt", "content": "ONE\ntwo\nTHREE\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        edit_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::User },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Caps"), permissions)).await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;
    let app = h.app.handle().clone();
    let rejected = vec![diff::Hunk { orig_start: 3, deleted: vec!["three".into()], added: vec!["THREE".into()] }];
    agent_resolve_approval(
        app.clone(),
        app.state(),
        "w1".into(),
        ApprovalDecision::Partial { content: "ONE\ntwo\nthree\n".into(), total: 2, rejected },
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("ONE\ntwo\nthree\n"));
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert!(result.content.contains("accepted 1 of 2 changes"), "{}", result.content);
    // Reviewed by the user: not marked as an unreviewed change.
    assert_eq!(h.events_of("fileWritten")[0]["reviewed"], true);
}

#[tokio::test]
async fn stop_ends_a_run_waiting_for_approval() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Call { id: "r1", name: "run_command", args: json!({ "command": "ls" }) }]).await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::User },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("List"), permissions)).await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;

    // Another run in the same chat is refused meanwhile.
    let app = h.app.handle().clone();
    let again = agent_start(app.clone(), app.state(), app.state(), h.request(&url, StartKind::Retry, quiet_permissions())).await;
    assert!(again.is_err());

    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert_eq!(result.content, "Error: stopped by the user before this ran.");
    assert_eq!(seen.lock().unwrap().len(), 1, "no request after Stop");
    assert_eq!(h.events_of("approvalSettled").len(), 1);
}

#[tokio::test]
async fn a_paused_run_continues_where_it_stopped() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "r1", name: "read_file", args: json!({ "path": "a.txt" }) },
        Reply::Call { id: "r2", name: "list_dir", args: json!({}) },
        Reply::Text("All read."),
    ])
    .await;
    let permissions = Permissions { tool_call_checkpoint: Some(1), ..quiet_permissions() };
    h.start(h.request(&url, Harness::send("Read"), permissions.clone())).await;
    assert_eq!(h.run_end().await, "paused-checkpoint");
    let messages = h.messages().await;
    let paused = messages.last().unwrap();
    assert_eq!(paused.meta.as_ref().unwrap()["paused"], json!({ "kind": "checkpoint", "toolCalls": 1 }));

    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::ContinueRun, permissions)).await;
    // Counting goes on from 1: the next checkpoint is at 2.
    assert_eq!(h.run_end().await, "paused-checkpoint");
    let messages = h.messages().await;
    assert_eq!(messages.last().unwrap().meta.as_ref().unwrap()["paused"]["toolCalls"], 2);
}

#[tokio::test]
async fn failing_checks_go_back_to_the_model_until_they_pass() {
    // `npm test` passes once ok.txt says "ok".
    let h = harness(&[
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        ("check.js", "const fs = require('fs'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== 'ok') { console.error('src/ok.txt:1: error: not ok'); process.exit(1); }"),
        ("ok.txt", "no"),
    ])
    .await;
    if std::process::Command::new("npm").arg("--version").output().is_err() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, seen) = model(vec![
        Reply::Call { id: "w1", name: "write_file", args: json!({ "path": "ok.txt", "content": "still no" }) },
        Reply::Text("Done."),
        Reply::Call { id: "w2", name: "write_file", args: json!({ "path": "ok.txt", "content": "ok" }) },
        Reply::Text("Fixed."),
    ])
    .await;
    let permissions = Permissions::default();
    h.start(h.request(&url, Harness::send("Make the test pass"), permissions)).await;
    assert_eq!(h.run_end().await, "verified");

    let messages = h.messages().await;
    let records: Vec<&Value> =
        messages.iter().filter_map(|m| m.meta.as_ref()?.get("verification")).collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["status"], "failed");
    assert_eq!(records[0]["checks"][0]["command"], "npm test");
    assert_eq!(records[1]["status"], "passed");
    let report = messages.iter().find(|m| m.role == "user" && m.meta.as_ref().is_some_and(|x| x["harness"] == "verification")).unwrap();
    assert!(report.content.contains("Your changes are not done yet: a check fails (attempt 1 of 3)."), "{}", report.content);
    assert!(report.content.contains("not ok"));
    assert_eq!(seen.lock().unwrap().len(), 4);
    // The model was told about the project and its checks.
    let system = seen.lock().unwrap()[0]["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(system.contains("## Project") && system.contains("- Tests: `npm test`"), "{system}");
}

#[tokio::test]
async fn plan_mode_asks_and_saves_the_plan() {
    let h = harness(&[]).await;
    let plan = "# Users\n\nAdd a table.\n\n## Steps\n1. Schema\n2. API\n\n## Decisions\n- SQLite";
    let (url, _) = model(vec![
        Reply::Call {
            id: "q1",
            name: "ask_user",
            args: json!({ "questions": [{ "question": "Which DB?", "options": [{ "label": "SQLite" }, { "label": "Postgres" }] }] }),
        },
        Reply::Call { id: "p1", name: "submit_plan", args: json!({ "plan": plan }) },
        Reply::Text("Saved the plan."),
    ])
    .await;
    let mut request = h.request(
        &url,
        StartKind::Send {
            message: UserMessage { id: "u1".into(), content: "Add users".into(), meta: Some(json!({ "mode": "plan" })) },
        },
        quiet_permissions(),
    );
    request.mode = Some("agent".into()); // the message's mode wins
    h.start(request).await;

    let asked = h.wait_for("questionsAsked", |e| e["type"] == "questionsAsked").await;
    assert_eq!(asked["request"]["questions"][0]["question"], "Which DB?");
    let app = h.app.handle().clone();
    let outcome: plan::QuestionsOutcome =
        serde_json::from_value(json!({ "answers": [{ "kind": "option", "label": "SQLite" }] })).unwrap();
    assert!(agent_answer_questions(app.clone(), app.state(), "q1".into(), outcome));
    assert_eq!(h.run_end().await, "done");

    let saved = h.project.join(".rovik/plans/2026-10-06-add-users.md");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), format!("{plan}\n"));
    let messages = h.messages().await;
    let answer = messages.iter().find(|m| m.tool_call_id.as_deref() == Some("q1")).unwrap();
    assert!(answer.content.contains("→ SQLite"));
    assert_eq!(answer.meta.as_ref().unwrap()["questions"]["outcome"]["answers"][0]["label"], "SQLite");
    // The final reply links the plan, for "Implement".
    let last = messages.last().unwrap();
    assert_eq!(last.meta.as_ref().unwrap()["planFile"], saved.to_string_lossy().as_ref());
}

#[tokio::test]
async fn a_resumed_chat_asks_again_then_continues() {
    let h = harness(&[]).await;
    // A chat that ended with a tool call that never ran.
    let id = h.dialog_id.clone();
    let db = h.app.state::<DbState>().inner().clone();
    let stored = vec![
        run::new_message("user", "List files"),
        store::NewMessage {
            tool_calls: Some(vec![store::ToolCall { id: "l1".into(), name: "list_dir".into(), arguments: "{}".into(), extra_content: None }]),
            ..run::new_message("assistant", "")
        },
    ];
    db.write(move |c| store::append_messages(c, &id, &stored)).await.unwrap();

    let (url, _) = model(vec![Reply::Text("Nothing there.")]).await;
    let run = h.start(h.request(&url, StartKind::Resume, quiet_permissions())).await;
    assert!(run.is_some());
    // Asked again, whatever the settings say.
    let asked = h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;
    assert!(asked["request"]["note"].as_str().unwrap().starts_with("Interrupted"));
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "l1".into(), ApprovalDecision::Allow);
    assert_eq!(h.run_end().await, "done");
    assert_eq!(roles(&h.messages().await), ["user", "assistant", "tool", "assistant"]);

    // Nothing left to resume: no run.
    assert!(h.start(h.request(&url, StartKind::Resume, quiet_permissions())).await.is_none());
}

#[tokio::test]
async fn a_failed_first_reply_marks_the_message_failed() {
    let h = harness(&[]).await;
    // Nobody listens there.
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1", l.local_addr().unwrap())
    };
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.profile.first_byte_timeout_ms = Some(5_000);
    h.start(request).await;
    assert_eq!(h.run_end().await, "error");
    let messages = h.messages().await;
    assert!(messages[0].failed, "the user's message is marked failed");
    assert!(messages[1].error.is_some());
    assert_eq!(messages[1].meta.as_ref().unwrap()["failure"]["kind"], "network");
    assert_eq!(h.events_of("userMessageSettled")[0]["ok"], false);
}

#[tokio::test]
async fn the_ai_reviewer_decides_and_risky_commands_still_ask() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "r1", name: "run_command", args: json!({ "command": "ls" }) },
        Reply::Call { id: "r2", name: "run_command", args: json!({ "command": "git push" }) },
        Reply::Text("Done."),
    ])
    .await;
    let (reviewer, reviewed) = model(vec![Reply::Text("{\"decision\": \"approve\", \"reason\": \"Harmless.\"}")]).await;
    let mut request = h.request(
        &url,
        Harness::send("List, then push"),
        Permissions {
            allow_any_command: true,
            command_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::Ai },
            ..quiet_permissions()
        },
    );
    let mut profile = request.profile.clone();
    profile.base_url = reviewer;
    profile.label = "Reviewer".into();
    request.config.as_mut().unwrap().approval_profiles = vec![profile];
    h.start(request).await;

    // `git push` is never left to the reviewer.
    let asked = h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;
    assert_eq!(asked["request"]["toolCallId"], "r2");
    assert!(asked["request"]["note"].as_str().unwrap().starts_with("Commands that push"));
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "r2".into(), ApprovalDecision::Deny);
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let first = messages.iter().find(|m| m.tool_call_id.as_deref() == Some("r1")).unwrap();
    assert!(first.content.starts_with("$ ls\nexit code 0"), "{}", first.content);
    assert_eq!(
        first.meta.as_ref().unwrap()["approval"],
        json!({ "by": "ai", "approved": true, "model": "Reviewer", "reason": "Harmless." })
    );
    // The reviewer saw the request and the command.
    let review = reviewed.lock().unwrap()[0].clone();
    let prompt = review["messages"][1]["content"].as_str().unwrap();
    assert!(prompt.contains("User's request:\nList, then push") && prompt.contains("Action: Run `ls`"), "{prompt}");
    assert_eq!(review["max_tokens"], 300);
    // Its review was shown while it ran.
    let notes = h.events_of("toolNote");
    assert_eq!(notes[0]["note"], "Reviewing with Reviewer…");
    assert!(notes[1]["note"].is_null());
}

#[tokio::test]
async fn a_reply_repeating_itself_is_cut_and_the_model_told() {
    let h = harness(&[]).await;
    let looping: &'static str =
        Box::leak(format!("Let me think. {}", "Sigh. I'll just output it. ".repeat(80)).into_boxed_str());
    let (url, seen) = model(vec![Reply::Text(looping), Reply::Text("Sorry, here it is.")]).await;
    h.start(h.request(&url, Harness::send("Go"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "user", "assistant"]);
    assert!(messages[1].content.starts_with("Let me think. Sigh. I'll just output it.\n\n_[Stopped here"), "{}", messages[1].content);
    assert!(messages[2].content.contains("started repeating"));
    assert_eq!(messages[3].content, "Sorry, here it is.");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

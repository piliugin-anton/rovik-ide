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
    /// Some text (or nothing), then silence: the model stopped answering.
    Hang(Option<&'static str>),
    /// Several tool calls in one reply.
    Calls(Vec<(&'static str, &'static str, Value)>),
    /// An HTTP error with this body.
    Status(u16, &'static str),
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
                // A string is the arguments' raw text (to send broken JSON).
                let arguments = args.as_str().map(str::to_string).unwrap_or_else(|| args.to_string());
                let delta = json!({ "tool_calls": [{
                    "index": 0, "id": id, "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]});
                chunk(json!({ "choices": [{ "delta": delta, "finish_reason": null }] }))
                    + &chunk(json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }))
                    + "data: [DONE]\n\n"
            }
            Reply::Hang(text) => text
                .map(|t| chunk(json!({ "choices": [{ "delta": { "content": t }, "finish_reason": null }] })))
                .unwrap_or_default(),
            Reply::Calls(calls) => {
                let calls: Vec<Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(i, (id, name, args))| json!({
                        "index": i, "id": id, "type": "function",
                        "function": { "name": name, "arguments": args.to_string() }
                    }))
                    .collect();
                chunk(json!({ "choices": [{ "delta": { "tool_calls": calls }, "finish_reason": null }] }))
                    + &chunk(json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }))
                    + "data: [DONE]\n\n"
            }
            Reply::Status(..) => String::new(),
        }
    }

    /// The whole HTTP response.
    fn response(&self) -> String {
        match self {
            Reply::Status(code, body) => format!(
                "HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            _ => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                self.sse()
            ),
        }
    }

    fn hangs(&self) -> bool {
        matches!(self, Reply::Hang(_))
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
    let bodies: Vec<(String, bool)> = replies.iter().map(|r| (r.response(), r.hangs())).collect();
    tokio::spawn(async move {
        let mut n = 0;
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = read_request(&mut socket).await;
            log.lock().unwrap().push(body);
            let (reply, hangs) = bodies[n.min(bodies.len() - 1)].clone();
            n += 1;
            let _ = socket.write_all(reply.as_bytes()).await;
            if hangs {
                // Keeps the connection open, saying nothing more.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    drop(socket);
                });
                continue;
            }
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

fn effort(level: &str) -> Option<crate::agent::OpenAIChatOptions> {
    serde_json::from_value(json!({ "reasoning_effort": level, "reasoning_formats": ["reasoning_effort"] })).unwrap()
}

impl Harness {
    /// The effort picked in this chat's selector.
    fn pick_effort(&self, level: &str) -> usize {
        self.pick_effort_with(effort(level))
    }

    fn pick_effort_with(&self, options: Option<crate::agent::OpenAIChatOptions>) -> usize {
        let app = self.app.handle().clone();
        agent_set_effort(app.state(), "p::m".into(), options, Some(self.dialog_id.clone()))
    }
}

/// Waits until the model has been asked `n` times.
async fn requests_seen(seen: &Arc<Mutex<Vec<Value>>>, n: usize) {
    for _ in 0..400 {
        if seen.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the model was asked {} times, not {n}", seen.lock().unwrap().len());
}

#[tokio::test]
async fn a_stuck_request_is_asked_again_with_a_new_effort() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Hang(None), Reply::Text("Hello.")]).await;
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.options = effort("high");
    h.start(request).await;
    requests_seen(&seen, 1).await;

    let app = h.app.handle().clone();
    // Another model's effort changes nothing here; nor does picking it for
    // this model in another chat (that applies to the next request), nor
    // the same level again, sent in other forms.
    assert_eq!(agent_set_effort(app.state(), "other::model".into(), effort("low"), Some(h.dialog_id.clone())), 0);
    assert_eq!(agent_set_effort(app.state(), "p::m".into(), effort("medium"), Some("another-chat".into())), 1);
    let same_level = serde_json::from_value(json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort", "reasoning"] })).unwrap();
    h.pick_effort_with(Some(same_level));
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "nothing asked again yet");

    // Picked here: the request is long past "still waiting", so it's asked again.
    assert_eq!(h.pick_effort("low"), 1);
    assert_eq!(h.run_end().await, "done");

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["reasoning_effort"], "high");
    assert_eq!(requests[1]["reasoning_effort"], "low");
    // One reply, in the same message: the dropped attempt left nothing behind.
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant"]);
    assert_eq!(messages[1].content, "Hello.");
    assert!(messages[1].error.is_none() && !messages[0].failed);
    let statuses: Vec<Value> = h
        .events_of("messagePatched")
        .into_iter()
        .filter_map(|e| e["patch"].get("status").cloned())
        .collect();
    assert!(statuses.iter().any(|s| s["text"] == "Asking again with low effort…"), "{statuses:?}");
    assert_eq!(h.events_of("userMessageSettled")[0]["ok"], true);
}

#[tokio::test]
async fn an_effort_picked_early_waits_until_the_request_is_stuck() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Hang(None), Reply::Text("Hello.")]).await;
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.options = effort("high");
    h.start(request).await;
    requests_seen(&seen, 1).await;
    // Picked right away: the model may still answer, so it isn't cut short...
    h.pick_effort("low");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    // ...until it's past the usual wait.
    assert_eq!(h.run_end().await, "done");
    assert_eq!(seen.lock().unwrap()[1]["reasoning_effort"], "low");
}

#[tokio::test]
async fn an_effort_picked_mid_run_applies_to_the_next_request() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call { id: "r1", name: "run_command", args: json!({ "command": "ls" }) },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("List"),
        Permissions {
            command_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::User },
            ..quiet_permissions()
        },
    );
    request.options = effort("high");
    h.start(request).await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested").await;
    let app = h.app.handle().clone();
    h.pick_effort("low");
    agent_resolve_approval(app.clone(), app.state(), "r1".into(), ApprovalDecision::Allow);
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["reasoning_effort"], "high");
    assert_eq!(requests[1]["reasoning_effort"], "low");
}

#[tokio::test]
async fn a_reply_under_way_is_not_thrown_away_for_a_new_effort() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Hang(Some("Part of the answer"))]).await;
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.options = effort("high");
    h.start(request).await;
    h.wait_for("streamed text", |e| e["type"] == "messagePatched" && e["patch"]["content"] == "Part of the answer").await;

    // Silent since: stalled, but part of the reply is there.
    h.pick_effort("low");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "not asked again: the reply was coming");

    let app = h.app.handle().clone();
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");
    let messages = h.messages().await;
    assert_eq!(messages[1].content, "Part of the answer");
    assert_eq!(messages[1].meta.as_ref().unwrap()["interrupted"]["kind"], "cancelled");
}

/*
 * Edge cases
 */

fn plan_send(text: &str) -> StartKind {
    StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: text.into(),
            meta: Some(json!({ "mode": "plan" })),
        },
    }
}

fn result_of<'a>(messages: &'a [MessageRow], call: &str) -> &'a MessageRow {
    messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call))
        .unwrap_or_else(|| panic!("no result for {call}: {messages:#?}"))
}

fn harness_notes(messages: &[MessageRow]) -> Vec<&MessageRow> {
    messages.iter().filter(|m| m.role == "user" && plan::is_harness(m)).collect()
}

fn ask_approval(group: &str) -> Permissions {
    let rule = permissions::ApprovalRule { required: true, by: permissions::Approver::User };
    match group {
        "commands" => Permissions { command_approval: rule, ..quiet_permissions() },
        "edits" => Permissions { edit_approval: rule, ..quiet_permissions() },
        _ => Permissions { tool_approval: rule, ..quiet_permissions() },
    }
}

#[tokio::test]
async fn every_call_of_a_turn_gets_a_result_when_stopped_midway() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, seen) = model(vec![Reply::Calls(vec![
        ("c1", "read_file", json!({ "path": "a.txt" })),
        ("c2", "list_dir", json!({})),
        ("c3", "read_file", json!({ "path": "a.txt", "start_line": 1 })),
    ])])
    .await;
    h.start(h.request(&url, Harness::send("Look"), ask_approval("tools"))).await;
    h.wait_for("approval c1", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "c1").await;
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "c1".into(), ApprovalDecision::Allow);
    h.wait_for("approval c2", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "c2").await;
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");

    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "c1").content, "x");
    assert_eq!(result_of(&messages, "c2").content, "Error: stopped by the user before this ran.");
    assert_eq!(result_of(&messages, "c3").content, "Error: stopped by the user before this ran.");
    assert_eq!(seen.lock().unwrap().len(), 1);
    // The history sent next time answers every call: a new run goes on fine.
    let (url, seen) = model(vec![Reply::Text("Ok.")]).await;
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Go on"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let sent = seen.lock().unwrap()[0]["messages"].as_array().unwrap().clone();
    let results = sent.iter().filter(|m| m["role"] == "tool").count();
    assert_eq!(results, 3);
}

#[tokio::test]
async fn refused_and_broken_calls_are_reported_without_running() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "run_command", json!({ "command": "rm -rf /" })),
            ("c2", "no_such_tool", json!({})),
            ("c3", "write_file", json!({ "path": "../outside.txt", "content": "x" })),
            ("c4", "read_file", json!({})),
            ("c5", "run_command", json!({ "command": "echo $(whoami)" })),
        ]),
        Reply::Text("Understood."),
    ])
    .await;
    // Approval required: refused calls must never reach it.
    h.start(h.request(&url, Harness::send("Do things"), ask_approval("commands"))).await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("approvalRequested").is_empty());
    let messages = h.messages().await;
    let c1 = &result_of(&messages, "c1").content;
    assert!(c1.starts_with("Error: blocked by the user's permission settings. Not in the user's allowed commands: \"rm -rf /\""), "{c1}");
    assert_eq!(result_of(&messages, "c2").content, "Error: unknown tool \"no_such_tool\".");
    assert!(result_of(&messages, "c3").content.contains("must not contain '..'"));
    assert_eq!(result_of(&messages, "c4").content, "Error: \"path\" is required.");
    assert!(result_of(&messages, "c5").content.contains("Command substitution"));
    assert!(!h.project.parent().unwrap().join("outside.txt").exists());
}

#[tokio::test]
async fn arguments_that_arent_json_are_an_error_for_the_model() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "c1", name: "list_dir", args: json!("{not json") },
        Reply::Text("Oops."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("List"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(result_of(&messages, "c1").content.starts_with("Error: could not parse arguments as JSON"));
}

#[tokio::test]
async fn without_a_project_only_web_tools_are_offered() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call { id: "c1", name: "read_file", args: json!({ "path": "a.txt" }) },
        Reply::Text("No project."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Read"), Permissions::default());
    request.root = None;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let tools: Vec<String> = seen.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tools, ["web_search", "browse_url"]);
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "c1").content, "Error: \"read_file\" needs an open project folder.");
    // No project: no verification either.
    assert!(messages.iter().all(|m| m.meta.as_ref().is_none_or(|x| x.get("verification").is_none())));
}

#[tokio::test]
async fn a_model_without_tools_gets_none_and_no_tool_rules() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("Hi.")]).await;
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.profile.no_tools = true;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let body = seen.lock().unwrap()[0].clone();
    assert!(body.get("tools").is_none() && body.get("tool_choice").is_none());
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(!system.contains("## Tool use rules"));
}

#[tokio::test]
async fn plan_mode_refuses_changes_and_extra_questions() {
    let h = harness(&[("a.txt", "keep")]).await;
    let ask = |id: &'static str| Reply::Call {
        id,
        name: "ask_user",
        args: json!({ "questions": [{ "question": "Q?", "options": ["A"] }] }),
    };
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("w1", "write_file", json!({ "path": "a.txt", "content": "changed" })),
            ("r1", "run_command", json!({ "command": "ls" })),
        ]),
        ask("q1"),
        ask("q2"),
        ask("q3"),
        ask("q4"),
        Reply::Text("I'll decide."),
    ])
    .await;
    h.start(h.request(&url, plan_send("Plan it"), quiet_permissions())).await;
    let app = h.app.handle().clone();
    for id in ["q1", "q2", "q3"] {
        h.wait_for(id, |e| e["type"] == "questionsAsked" && e["request"]["toolCallId"] == id).await;
        let outcome: plan::QuestionsOutcome = serde_json::from_value(json!({ "reply": "whatever" })).unwrap();
        assert!(agent_answer_questions(app.clone(), app.state(), id.into(), outcome));
        // Answering twice does nothing.
        let again: plan::QuestionsOutcome = serde_json::from_value(json!({ "reply": "again" })).unwrap();
        assert!(!agent_answer_questions(app.clone(), app.state(), id.into(), again));
    }
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("a.txt").as_deref(), Some("keep"));
    let messages = h.messages().await;
    assert!(result_of(&messages, "w1").content.starts_with("Error: write_file isn't available in Plan mode"));
    assert!(result_of(&messages, "r1").content.starts_with("Error: run_command isn't available in Plan mode"));
    assert!(result_of(&messages, "q4").content.starts_with("You've already asked 3 times"));
    assert_eq!(h.events_of("questionsAsked").len(), 3, "the fourth isn't shown");
    // Plan mode offers no tool that changes anything.
    let tools = seen.lock().unwrap()[0]["tools"].to_string();
    assert!(!tools.contains("\"write_file\"") && tools.contains("\"submit_plan\""));
    assert!(seen.lock().unwrap()[0]["messages"][0]["content"].as_str().unwrap().contains("## Plan mode"));
}

#[tokio::test]
async fn ask_user_outside_plan_mode_is_refused() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "q1", name: "ask_user", args: json!({ "questions": [{ "question": "Q?", "options": [] }] }) },
        Reply::Text("Decided."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do it"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("questionsAsked").is_empty());
    assert!(result_of(&h.messages().await, "q1").content.starts_with("Error: asking the user is only available in Plan mode"));
}

#[tokio::test]
async fn stop_while_a_question_waits_ends_the_run() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Call {
        id: "q1",
        name: "ask_user",
        args: json!({ "questions": [{ "question": "Which?", "options": ["A", "B"] }] }),
    }])
    .await;
    h.start(h.request(&url, plan_send("Plan"), quiet_permissions())).await;
    h.wait_for("questionsAsked", |e| e["type"] == "questionsAsked").await;
    let app = h.app.handle().clone();
    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    assert_eq!(h.events_of("questionsSettled").len(), 1);
    let answer = result_of(&h.messages().await, "q1").clone();
    assert_eq!(answer.content, "Error: the user stopped the task instead of answering.");
    assert_eq!(answer.meta.unwrap()["questions"]["outcome"], json!({ "stopped": true }));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_bad_plan_is_sent_back_and_a_written_out_plan_nudged_once() {
    let h = harness(&[]).await;
    let written = "Here it is:\n\n## Steps\n1. One\n2. Two";
    let (url, _) = model(vec![
        Reply::Call { id: "p1", name: "submit_plan", args: json!({ "plan": "no title, no steps" }) },
        Reply::Text(written),
        Reply::Text(written),
    ])
    .await;
    h.start(h.request(&url, plan_send("Plan"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let rejected = &result_of(&messages, "p1").content;
    assert!(rejected.starts_with("Error: the plan wasn't saved.") && rejected.contains("\"# \" title"), "{rejected}");
    assert!(!h.project.join(".rovik/plans").exists());
    // Written in the reply instead: pointed out once, then the run ends.
    let notes = harness_notes(&messages);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(notes[0].content.contains("Submit it with submit_plan"));
    assert!(messages.last().unwrap().meta.as_ref().is_none_or(|m| m.get("planFile").is_none()));
}

#[tokio::test]
async fn a_revised_plan_replaces_the_chats_plan_file() {
    let h = harness(&[]).await;
    let plan = |title: &str| format!("# {title}\n\n## Steps\n1. a\n2. b");
    let first: &'static str = Box::leak(plan("First").into_boxed_str());
    let second: &'static str = Box::leak(plan("Second").into_boxed_str());
    let (url, _) = model(vec![
        Reply::Call { id: "p1", name: "submit_plan", args: json!({ "plan": first }) },
        Reply::Text("Saved."),
    ])
    .await;
    h.start(h.request(&url, plan_send("Make a plan"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let (url, _) = model(vec![
        Reply::Call { id: "p2", name: "submit_plan", args: json!({ "plan": second }) },
        Reply::Text("Revised."),
    ])
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, plan_send("Change it"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let plans: Vec<_> = std::fs::read_dir(h.project.join(".rovik/plans")).unwrap().flatten().collect();
    assert_eq!(plans.len(), 1, "the revision went to the same file");
    assert!(std::fs::read_to_string(plans[0].path()).unwrap().starts_with("# Second"));
}

#[tokio::test]
async fn unfinished_checklist_steps_are_pointed_out_once() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Write it", "status": "done" }, { "title": "Test it", "status": "pending" }] }),
        },
        Reply::Text("Done."),
        Reply::Text("Really done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "t1").content, "Checklist updated: 1 of 2 done.");
    let notes = harness_notes(&messages);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].content.contains("\"Test it\""));
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_loop_is_noted_then_paused() {
    let h = harness(&[("a.txt", "same")]).await;
    let read = |id: &'static str| Reply::Call { id, name: "read_file", args: json!({ "path": "a.txt" }) };
    let (url, _) = model(vec![read("r1"), read("r2"), read("r3"), read("r4"), read("r5"), read("r6"), Reply::Text("x")])
        .await;
    h.start(h.request(&url, Harness::send("Read"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "paused-loop");
    let messages = h.messages().await;
    assert!(result_of(&messages, "r3").content.contains("This looks like a loop"));
    assert!(result_of(&messages, "r6").content.contains("the run was paused here for the user"));
    let paused = messages.last().unwrap().meta.as_ref().unwrap()["paused"].clone();
    assert_eq!(paused["kind"], "loop");
    assert_eq!(paused["toolCalls"], 6);
    assert!(paused["reason"].as_str().unwrap().contains("read_file (a.txt)"));
}

#[tokio::test]
async fn edits_that_change_nothing_or_cant_apply_skip_approval() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("w1", "write_file", json!({ "path": "a.txt", "content": "one\n" })),
            ("e1", "edit_file", json!({ "path": "a.txt", "old_text": "missing", "new_text": "x" })),
            ("e2", "edit_file", json!({ "path": "nope.txt", "old_text": "a", "new_text": "b" })),
        ]),
        Reply::Text("Ok."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits"))).await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("approvalRequested").is_empty(), "nothing to approve");
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "w1").content, "wrote 4 bytes to a.txt");
    assert!(result_of(&messages, "e1").content.starts_with("Error:"));
    assert!(result_of(&messages, "e2").content.contains("no such file"));
    assert_eq!(h.file("a.txt").as_deref(), Some("one\n"));
}

#[tokio::test]
async fn a_new_file_is_approved_whole_and_an_allowed_edit_is_recorded_as_reviewed() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "w1", name: "write_file", args: json!({ "path": "dir/new.txt", "content": "hello" }) },
        Reply::Text("Created."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Create"), ask_approval("edits"))).await;
    let asked = h.wait_for("approval", |e| e["type"] == "approvalRequested").await;
    assert!(asked["request"].get("edit").is_none(), "nothing to diff against");
    assert!(asked["request"]["note"].as_str().unwrap().contains("New file"));
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "w1".into(), ApprovalDecision::Allow);
    // Unknown and repeated answers do nothing.
    assert!(!agent_resolve_approval(app.clone(), app.state(), "w1".into(), ApprovalDecision::Deny));
    assert!(!agent_resolve_approval(app.clone(), app.state(), "nope".into(), ApprovalDecision::Allow));
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("dir/new.txt").as_deref(), Some("hello"));
    let written = h.events_of("fileWritten");
    assert_eq!(written[0]["reviewed"], true);
    assert!(written[0]["before"].is_null());
}

#[tokio::test]
async fn the_ai_reviewer_can_reject_and_falls_back_to_the_user() {
    let h = harness(&[("a.txt", "x\n"), (".env", "SECRET=1\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("e1", "edit_file", json!({ "path": "a.txt", "old_text": "x", "new_text": "y" })),
            ("e2", "edit_file", json!({ "path": ".env", "old_text": "1", "new_text": "2" })),
            ("e3", "write_file", json!({ "path": "b.txt", "content": "b" })),
        ]),
        Reply::Text("Ok."),
    ])
    .await;
    // The reviewer rejects the first, then can't answer at all.
    let (rejecting, _) = model(vec![
        Reply::Text("{\"decision\": \"reject\", \"reason\": \"Not asked for.\"}"),
        Reply::Status(500, "{\"error\":{\"message\":\"down\"}}"),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Edit"),
        Permissions {
            edit_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::Ai },
            ..quiet_permissions()
        },
    );
    let mut reviewer = request.profile.clone();
    reviewer.base_url = rejecting;
    reviewer.label = "Reviewer".into();
    request.config.as_mut().unwrap().approval_profiles = vec![reviewer];
    h.start(request).await;

    let app = h.app.handle().clone();
    // .env: never left to the reviewer.
    let asked = h.wait_for("approval e2", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e2").await;
    assert!(asked["request"]["note"].as_str().unwrap().starts_with("Edits to credential or secret files"));
    agent_resolve_approval(app.clone(), app.state(), "e2".into(), ApprovalDecision::Deny);
    // The reviewer failed: the user decides.
    let asked = h.wait_for("approval e3", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e3").await;
    assert!(asked["request"]["note"].as_str().unwrap().contains("couldn’t answer"), "{asked}");
    agent_resolve_approval(app.clone(), app.state(), "e3".into(), ApprovalDecision::Allow);
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let e1 = result_of(&messages, "e1");
    assert!(e1.content.starts_with("Error: rejected by the user's AI reviewer (Reviewer): Not asked for."));
    assert_eq!(e1.meta.as_ref().unwrap()["approval"]["by"], "ai");
    assert_eq!(h.file("a.txt").as_deref(), Some("x\n"));
    assert_eq!(h.file(".env").as_deref(), Some("SECRET=1\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("b"));
}

#[tokio::test]
async fn no_reviewer_model_means_the_user_approves() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Call { id: "r1", name: "list_dir", args: json!({}) }, Reply::Text("Ok.")]).await;
    let permissions = Permissions {
        tool_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::Ai },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("List"), permissions)).await;
    let asked = h.wait_for("approval", |e| e["type"] == "approvalRequested").await;
    assert!(asked["request"]["note"].as_str().unwrap().starts_with("No AI reviewer model is set"));
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "r1".into(), ApprovalDecision::Allow);
    assert_eq!(h.run_end().await, "done");
}

#[tokio::test]
async fn a_repeating_model_ends_the_run_the_second_time() {
    let h = harness(&[]).await;
    let looping: &'static str =
        Box::leak(format!("Thinking. {}", "Let me try again now. ".repeat(80)).into_boxed_str());
    let (url, seen) = model(vec![Reply::Text(looping)]).await;
    h.start(h.request(&url, Harness::send("Go"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "repetition");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(h.events_of("notice")[0]["title"], "The model kept repeating itself");
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "user", "assistant"]);
    assert!(messages[3].content.contains("_[Stopped here"));
}

#[tokio::test]
async fn a_rejected_request_is_not_retried_and_reported() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Status(401, "{\"error\":{\"message\":\"bad key\"}}")]).await;
    h.start(h.request(&url, Harness::send("Hi"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "error");
    assert_eq!(seen.lock().unwrap().len(), 1, "auth errors aren't retried");
    let messages = h.messages().await;
    assert_eq!(messages[1].error.as_deref(), Some("The API key was rejected. Check it in Model Settings."));
    assert_eq!(messages[1].meta.as_ref().unwrap()["failure"]["kind"], "auth");
    assert!(messages[0].failed);
}

#[tokio::test]
async fn a_refused_reasoning_form_isnt_sent_again_in_the_run() {
    let h = harness(&[]).await;
    let refusal = "{\"error\":{\"message\":\"Unrecognized request argument supplied: chat_template_kwargs\"}}";
    let (url, seen) = model(vec![
        Reply::Status(400, refusal),
        Reply::Call { id: "c1", name: "list_dir", args: json!({}) },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("List"), quiet_permissions());
    request.options = serde_json::from_value(
        json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort", "chat_template_kwargs"] }),
    )
    .unwrap();
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].get("chat_template_kwargs").is_some());
    assert!(requests[1].get("chat_template_kwargs").is_none() && requests[1]["reasoning_effort"] == "high");
    assert!(requests[2].get("chat_template_kwargs").is_none(), "not tried again on the next turn");
    let learned = h.events_of("reasoningLearned");
    assert!(learned.iter().any(|l| l["rejected"] == json!(["chat_template_kwargs"])));
}

#[tokio::test]
async fn continue_and_retry_reuse_the_requests_wire_text() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("Answer.")]).await;
    let message = UserMessage {
        id: "u1".into(),
        content: "Fix it".into(),
        meta: Some(json!({ "mode": "agent", "sentContent": "Editor context\n\nFix it" })),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    h.events.lock().unwrap().clear();
    // No wire text from the frontend: the stored one is used.
    h.start(h.request(&url, StartKind::ContinueReply, quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    let user = |r: &Value| {
        r["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap()["content"].clone()
    };
    assert_eq!(user(&requests[0]), "Editor context\n\nFix it");
    assert_eq!(user(&requests[1]), "Editor context\n\nFix it");
    let last = requests[1]["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().starts_with("Your previous reply was interrupted"));
}

#[tokio::test]
async fn runs_in_two_chats_go_on_independently() {
    let a = harness(&[]).await;
    // The second chat in the same app and database.
    let b_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog { id: b_id.clone(), title: "B".into(), created_at: 0, updated_at: 0, project_id: None };
    let db = a.app.state::<DbState>().inner().clone();
    db.write(move |c| store::create_dialog(c, &dialog)).await.unwrap();

    let (hang, _) = model(vec![Reply::Hang(None)]).await;
    let (quick, _) = model(vec![Reply::Text("B done.")]).await;
    a.start(a.request(&hang, Harness::send("A"), quiet_permissions())).await;
    let mut b = a.request(&quick, Harness::send("B"), quiet_permissions());
    b.dialog_id = b_id.clone();
    a.start(b).await;
    a.wait_for("B ends", |e| e["type"] == "runEnded" && e["dialogId"] == b_id.as_str()).await;

    let app = a.app.handle().clone();
    let status = agent_status(app.state());
    assert_eq!(status.runs.len(), 1, "A still runs");
    assert!(!agent_stop(app.clone(), app.state(), b_id.clone()), "B is done: nothing to stop");
    assert!(agent_stop(app.clone(), app.state(), a.dialog_id.clone()));
    a.wait_for("A ends", |e| e["type"] == "runEnded" && e["dialogId"] == a.dialog_id.as_str()).await;
    assert!(agent_status(app.state()).runs.is_empty());
}

#[tokio::test]
async fn status_lists_what_a_run_waits_on() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Call { id: "r1", name: "list_dir", args: json!({}) }, Reply::Text("Ok.")]).await;
    h.start(h.request(&url, Harness::send("List"), ask_approval("tools"))).await;
    h.wait_for("approval", |e| e["type"] == "approvalRequested").await;
    let app = h.app.handle().clone();
    let status = agent_status(app.state());
    assert_eq!(status.runs[0].dialog_id, h.dialog_id);
    assert_eq!(status.approvals[0].tool_call_id, "r1");
    assert!(status.questions.is_empty());
    agent_resolve_approval(app.clone(), app.state(), "r1".into(), ApprovalDecision::Allow);
    h.run_end().await;
    let status = agent_status(app.state());
    assert!(status.runs.is_empty() && status.approvals.is_empty());
}

#[tokio::test]
async fn a_chat_that_doesnt_exist_cant_start() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("x")]).await;
    let mut request = h.request(&url, Harness::send("Hi"), quiet_permissions());
    request.dialog_id = "no-such-chat".into();
    let app = h.app.handle().clone();
    let started = agent_start(app.clone(), app.state(), app.state(), request).await;
    assert!(started.is_err());
    assert!(agent_status(app.state()).runs.is_empty(), "not left registered");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nothing_to_continue_or_check_starts_no_run() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("x")]).await;
    let none = h.start(h.request(&url, StartKind::RunChecks { record_id: "missing".into() }, Permissions::default())).await;
    assert!(none.is_none());
    assert!(h.events_of("runStarted").is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checks_wait_for_approval_then_run_when_started() {
    let h = harness(&[
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(0)\"" } }"#),
    ])
    .await;
    if std::process::Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call { id: "w1", name: "write_file", args: json!({ "path": "x.txt", "content": "x" }) },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule { required: true, by: permissions::Approver::User },
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Write"), permissions.clone())).await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let messages = h.messages().await;
    let record = messages.last().unwrap();
    let waiting = record.meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(waiting["status"], "needs-approval");
    assert_eq!(waiting["checks"][0]["status"], "pending");
    assert_eq!(waiting["changedFiles"], json!(["x.txt"]));

    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::RunChecks { record_id: record.id.clone() }, permissions)).await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let records: Vec<_> = messages.iter().filter_map(|m| m.meta.as_ref()?.get("verification")).collect();
    assert_eq!(records.len(), 1, "the waiting record became the result");
    assert_eq!(records[0]["status"], "passed");
}

#[tokio::test]
async fn checks_that_keep_failing_give_up_and_a_model_that_stops_fixing_is_unresolved() {
    let h = harness(&[("package.json", r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#)]).await;
    if std::process::Command::new("npm").arg("--version").output().is_err() {
        return;
    }
    let write = |id: &'static str| Reply::Call { id, name: "write_file", args: json!({ "path": "x.txt", "content": id }) };
    let (url, _) = model(vec![write("w1"), Reply::Text("Done."), write("w2"), Reply::Text("Done."), write("w3"), Reply::Text("Done.")])
        .await;
    h.start(h.request(&url, Harness::send("Fix"), Permissions::default())).await;
    assert_eq!(h.run_end().await, "verify-gave-up");
    let messages = h.messages().await;
    let statuses: Vec<_> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").map(|v| v["status"].clone()))
        .collect();
    assert_eq!(statuses, [json!("failed"), json!("failed"), json!("gave-up")]);

    // Another chat: the model gives up after the first failure without changing anything.
    let h = harness(&[("package.json", r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#)]).await;
    let (url, _) = model(vec![write("w1"), Reply::Text("Done."), Reply::Text("It fails anyway.")]).await;
    h.start(h.request(&url, Harness::send("Fix"), Permissions::default())).await;
    assert_eq!(h.run_end().await, "verify-unresolved");
}

#[tokio::test]
async fn a_project_without_checks_is_unverified_and_disallowed_checks_are_skipped() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "w1", name: "write_file", args: json!({ "path": "x.txt", "content": "x" }) },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write"), Permissions::default())).await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    let record = messages.last().unwrap().meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(record["reason"], "no checks are set up for this project");

    // A check the allowed commands don't cover isn't run.
    let h = harness(&[("package.json", r#"{ "name": "t", "scripts": { "lint": "node -e 1" } }"#)]).await;
    let (url, _) = model(vec![
        Reply::Call { id: "w1", name: "write_file", args: json!({ "path": "x.txt", "content": "x" }) },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions { allowed_commands: vec!["ls".into()], ..Permissions::default() };
    h.start(h.request(&url, Harness::send("Write"), permissions)).await;
    assert_eq!(h.run_end().await, "verify-unverified");
    let messages = h.messages().await;
    let record = messages.last().unwrap().meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(record["checks"][0]["status"], "skipped");
    assert_eq!(record["checks"][0]["reason"], "not in your allowed commands");
}

/*
 * Tool calls that run at the same time
 */

fn tool_results(messages: &[MessageRow]) -> Vec<(&str, &str)> {
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| (m.tool_call_id.as_deref().unwrap_or(""), m.content.as_str()))
        .collect()
}

fn ids(batches: &[Vec<String>]) -> Vec<Vec<&str>> {
    batches.iter().map(|b| b.iter().map(String::as_str).collect()).collect()
}

#[tokio::test]
async fn reads_in_one_turn_run_together_and_are_recorded_in_order() {
    let h = harness(&[("a.txt", "alpha\n"), ("b.txt", "beta needle\n"), ("sub/c.txt", "gamma\n")]).await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("c1", "read_file", json!({ "path": "a.txt" })),
            ("c2", "search_code", json!({ "query": "needle" })),
            ("c3", "list_dir", json!({ "path": "sub" })),
            ("c4", "read_file", json!({ "path": "b.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");

    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [["c1", "c2", "c3", "c4"]]);
    let messages = h.messages().await;
    let results = tool_results(&messages);
    let order: Vec<&str> = results.iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["c1", "c2", "c3", "c4"], "stored in the calls' order");
    assert_eq!(results[0].1, "alpha\n");
    assert!(results[1].1.starts_with("1 match in 1 file") && results[1].1.contains("b.txt:1: beta needle"), "{}", results[1].1);
    assert!(results[2].1.contains("sub/c.txt") || results[2].1.contains("c.txt"), "{}", results[2].1);
    assert_eq!(results[3].1, "beta needle\n");
    assert!(messages.iter().all(|m| m.finalized));

    // The next request answers the calls in the order they were made.
    let sent = seen.lock().unwrap()[1]["messages"].as_array().unwrap().clone();
    let sent_ids: Vec<&str> = sent.iter().filter(|m| m["role"] == "tool").map(|m| m["tool_call_id"].as_str().unwrap()).collect();
    assert_eq!(sent_ids, ["c1", "c2", "c3", "c4"]);
}

#[tokio::test]
async fn a_write_splits_the_reads_around_it_and_later_reads_see_it() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("r1", "read_file", json!({ "path": "a.txt" })),
            ("r2", "list_dir", json!({})),
            ("w1", "write_file", json!({ "path": "a.txt", "content": "new\n" })),
            ("r3", "read_file", json!({ "path": "a.txt" })),
            ("r4", "search_code", json!({ "query": "new" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Change it"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");

    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [vec!["r1", "r2"], vec!["r3", "r4"]]);
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "r1").content, "old\n", "read before the write");
    assert_eq!(result_of(&messages, "r3").content, "new\n", "read after the write sees it");
    assert!(result_of(&messages, "r4").content.starts_with("1 match"));
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["r1", "r2", "w1", "r3", "r4"]);
}

#[tokio::test]
async fn reads_that_need_approval_are_asked_one_at_a_time() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read_file", json!({ "path": "a.txt" })),
            ("c2", "read_file", json!({ "path": "a.txt", "start_line": 1 })),
            ("c3", "list_dir", json!({})),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), ask_approval("tools"))).await;
    let app = h.app.handle().clone();
    for id in ["c1", "c2", "c3"] {
        h.wait_for(id, |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == id).await;
        // Only this one is waiting: the next isn't asked before it's decided.
        let asked: Vec<Value> = h.events_of("approvalRequested");
        assert_eq!(asked.last().unwrap()["request"]["toolCallId"], id);
        agent_resolve_approval(app.clone(), app.state(), id.into(), ApprovalDecision::Allow);
    }
    assert_eq!(h.run_end().await, "done");
    assert!(parallel::batches_of(&h.dialog_id).is_empty());
    let messages = h.messages().await;
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["c1", "c2", "c3"]);
}

#[tokio::test]
async fn a_call_that_isnt_a_local_read_ends_a_batch() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read_file", json!({ "path": "a.txt" })),
            ("c2", "read_file", json!({ "path": "../outside.txt" })),
            ("c3", "read_file", json!({ "path": "a.txt" })),
            ("c4", "list_dir", json!({})),
            ("c5", "todo", json!({ "items": [{ "title": "one", "status": "pending" }] })),
            ("c6", "list_dir", json!({})),
            ("c7", "read_file", json!({ "path": "a.txt", "start_line": 1 })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    // The todo runs on its own between two batches. A read outside the
    // project is still a read: it runs with the others and gets its error.
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [vec!["c1", "c2", "c3", "c4"], vec!["c6", "c7"]]);
    let messages = h.messages().await;
    assert!(result_of(&messages, "c2").content.starts_with("Error: path must not contain"), "{}", result_of(&messages, "c2").content);
    assert!(result_of(&messages, "c5").content.contains("one"), "{}", result_of(&messages, "c5").content);
    assert_eq!(result_of(&messages, "c7").content, "x");
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["c1", "c2", "c3", "c4", "c5", "c6", "c7"]);
}

#[tokio::test]
async fn a_refused_command_between_reads_splits_them() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("r1", "read_file", json!({ "path": "a.txt" })),
            ("r2", "list_dir", json!({})),
            ("x1", "run_command", json!({ "command": "rm -rf /" })),
            ("r3", "read_file", json!({ "path": "a.txt" })),
            ("r4", "read_file", json!({ "path": "a.txt", "start_line": 1 })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [vec!["r1", "r2"], vec!["r3", "r4"]]);
    let messages = h.messages().await;
    assert!(result_of(&messages, "x1").content.starts_with("Error: blocked by the user's permission settings"));
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["r1", "r2", "x1", "r3", "r4"]);
}

#[tokio::test]
async fn bad_calls_in_a_batch_get_their_errors_and_the_rest_runs() {
    let h = harness(&[("a.txt", "fine\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read_file", json!({})),
            ("c2", "read_file", json!({ "path": "missing.txt" })),
            ("c3", "search_code", json!({ "query": "(unclosed", "regex": true })),
            ("c4", "read_file", json!({ "path": "a.txt", "start_line": 5, "end_line": 1 })),
            ("c5", "read_file", json!({ "path": "a.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [["c1", "c2", "c3", "c4", "c5"]]);
    let messages = h.messages().await;
    for id in ["c1", "c2", "c3", "c4"] {
        assert!(result_of(&messages, id).content.starts_with("Error:"), "{id}: {}", result_of(&messages, id).content);
    }
    assert_eq!(result_of(&messages, "c5").content, "fine\n");
}

#[tokio::test]
async fn more_reads_than_run_at_once_all_come_back_in_order() {
    let files: Vec<(String, String)> = (0..25).map(|i| (format!("f{i:02}.txt"), format!("content {i}"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let h = harness(&refs).await;
    let ids_owned: Vec<String> = (0..25).map(|i| format!("c{i:02}")).collect();
    let paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let calls: Vec<(&'static str, &'static str, Value)> = ids_owned
        .iter()
        .zip(&paths)
        .map(|(id, path)| (&*Box::leak(id.clone().into_boxed_str()), "read_file", json!({ "path": path })))
        .collect();
    let (url, _) = model(vec![Reply::Calls(calls), Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Read all"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(parallel::batches_of(&h.dialog_id).len(), 1);
    assert_eq!(parallel::batches_of(&h.dialog_id)[0].len(), 25);
    let messages = h.messages().await;
    let results = tool_results(&messages);
    for (i, (id, content)) in results.iter().enumerate() {
        assert_eq!(*id, format!("c{i:02}"));
        assert_eq!(*content, format!("content {i}"));
    }
}

#[tokio::test]
async fn the_guard_sees_a_batchs_calls_in_order() {
    let h = harness(&[("a.txt", "same\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read_file", json!({ "path": "a.txt" })),
            ("c2", "read_file", json!({ "path": "a.txt" })),
            ("c3", "read_file", json!({ "path": "a.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [["c1", "c2", "c3"]]);
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "c1").content, "same\n");
    assert_eq!(result_of(&messages, "c2").content, "same\n");
    // The third identical call is the one the guard notes, as when they ran one by one.
    assert!(result_of(&messages, "c3").content.contains("was called 3 times"), "{}", result_of(&messages, "c3").content);
}

#[tokio::test]
async fn stop_before_a_batch_records_every_call_as_stopped() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![Reply::Calls(vec![
        ("e1", "edit_file", json!({ "path": "a.txt", "old_text": "one", "new_text": "ONE" })),
        ("r1", "read_file", json!({ "path": "a.txt" })),
        ("r2", "list_dir", json!({})),
    ])])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits"))).await;
    h.wait_for("approval e1", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e1").await;
    let app = h.app.handle().clone();
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");
    assert!(parallel::batches_of(&h.dialog_id).is_empty(), "nothing runs after Stop");
    let messages = h.messages().await;
    for id in ["e1", "r1", "r2"] {
        assert_eq!(result_of(&messages, id).content, "Error: stopped by the user before this ran.");
    }
    assert_eq!(h.file("a.txt").as_deref(), Some("one\n"));
}

#[tokio::test]
async fn reads_after_an_approved_edit_run_together_and_see_the_edit() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("e1", "edit_file", json!({ "path": "a.txt", "old_text": "one", "new_text": "ONE" })),
            ("r1", "read_file", json!({ "path": "a.txt" })),
            ("r2", "search_code", json!({ "query": "ONE", "case_sensitive": true })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits"))).await;
    h.wait_for("approval e1", |e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e1").await;
    let app = h.app.handle().clone();
    agent_resolve_approval(app.clone(), app.state(), "e1".into(), ApprovalDecision::Allow);
    assert_eq!(h.run_end().await, "done");
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [["r1", "r2"]]);
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "r1").content, "ONE\n");
    assert!(result_of(&messages, "r2").content.starts_with("1 match"));
}

#[tokio::test]
async fn a_single_read_runs_as_before() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![Reply::Call { id: "c1", name: "read_file", args: json!({ "path": "a.txt" }) }, Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert!(parallel::batches_of(&h.dialog_id).is_empty());
    assert_eq!(result_of(&h.messages().await, "c1").content, "x");
}

/*
 * The repository map
 */

fn system_prompt(request: &Value) -> String {
    request["messages"][0]["content"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_system_prompt_carries_a_map_centred_on_the_request() {
    let h = harness(&[
        ("src/billing.rs", "pub fn compute_invoice_total() -> u32 { 0 }\n"),
        ("src/report.rs", "pub fn build_report() { compute_invoice_total(); }\n"),
        ("src/unrelated.rs", "pub fn something_else_entirely() {}\n"),
        ("notes.txt", "not code"),
    ])
    .await;
    let (url, seen) = model(vec![Reply::Text("Ok."), Reply::Text("Ok again.")]).await;
    h.start(h.request(&url, Harness::send("Why is src/report.rs slow?"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let prompt = system_prompt(&seen.lock().unwrap()[0]);
    let map = &prompt[prompt.find("## Repository map").expect("a map in the system prompt")..];
    let report = map.find("src/report.rs:").expect("the file the request names");
    let billing = map.find("src/billing.rs:").expect("what it uses");
    assert!(report < billing, "{map}");
    assert!(map.contains("pub fn compute_invoice_total() -> u32"), "{map}");
    assert!(!map.contains("notes.txt"));
}

#[tokio::test]
async fn a_project_without_code_has_no_map() {
    let h = harness(&[("a.txt", "hello"), ("data.json", "{\"a\": 1}")]).await;
    let (url, seen) = model(vec![Reply::Text("Ok.")]).await;
    h.start(h.request(&url, Harness::send("Hi"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    assert!(!system_prompt(&seen.lock().unwrap()[0]).contains("## Repository map"));
}

/*
 * Reading by symbol
 */

#[tokio::test]
async fn the_model_reads_one_declaration_by_symbol() {
    let src = "use std::fmt;\n\npub struct Parser;\n\nimpl Parser {\n    /// Parses it.\n    pub fn parse(&self) -> u8 {\n        1\n    }\n\n    pub fn other(&self) {}\n}\n";
    let h = harness(&[("src/parser.rs", src)]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("s1", "read_file", json!({ "path": "src/parser.rs", "symbol": "Parser::parse" })),
            ("s2", "read_file", json!({ "path": "src/parser.rs", "symbol": "missing_fn" })),
            ("s3", "read_file", json!({ "path": "src/parser.rs", "symbol": "parse", "start_line": 1 })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Show parse"), quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let s1 = result_of(&messages, "s1");
    assert_eq!(
        s1.content,
        "[Symbol read: Parser > parse, lines 6-9 of src/parser.rs]\n    /// Parses it.\n    pub fn parse(&self) -> u8 {\n        1\n    }"
    );
    assert_eq!(s1.meta.as_ref().unwrap()["range"], json!([6, 9]));
    assert!(result_of(&messages, "s2").content.starts_with("Error: No declaration \"missing_fn\" in src/parser.rs."));
    assert_eq!(result_of(&messages, "s3").content, "Error: give either \"symbol\" or start_line/end_line, not both");
}

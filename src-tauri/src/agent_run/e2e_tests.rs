//! Whole runs against a scripted provider: a mock Tauri app with the real
//! chat database, the real tools and the real request code, talking to a
//! local server that plays the model.

use super::permissions::ChecklistMode;
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
    Call {
        id: &'static str,
        name: &'static str,
        args: Value,
    },
    /// Some text (or nothing), then silence: the model stopped answering.
    Hang(Option<&'static str>),
    /// Several tool calls in one reply.
    Calls(Vec<(&'static str, &'static str, Value)>),
    /// An HTTP error with this body.
    Status(u16, &'static str),
    /// Reasoning (`reasoning` text, and `reasoning_details` fragments, one
    /// per chunk, as OpenRouter streams them), then the reply.
    Thought(&'static str, Vec<Value>, Box<Reply>),
    /// The reply, after this many milliseconds.
    Slow(u64, Box<Reply>),
    /// 429 with a Retry-After of this many seconds.
    RateLimited(u64),
    /// The reply, then the usage the provider reports (prompt, completion);
    /// before a reply that hangs, its usage, then silence.
    Usage(u64, u64, Box<Reply>),
    /// The reply, then this usage object, as the provider sends it.
    UsageAs(Value, Box<Reply>),
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
            Reply::Thought(text, fragments, then) => {
                let mut out = chunk(json!({ "choices": [{ "delta": { "reasoning": text }, "finish_reason": null }] }));
                for fragment in fragments {
                    let delta = json!({ "reasoning_details": [fragment] });
                    out += &chunk(json!({ "choices": [{ "delta": delta, "finish_reason": null }] }));
                }
                out + &then.sse()
            }
            Reply::Slow(_, then) => then.sse(),
            Reply::RateLimited(_) => String::new(),
            Reply::Usage(prompt, completion, then) => Reply::usage_sse(
                json!({ "prompt_tokens": prompt, "completion_tokens": completion }),
                then,
            ),
            Reply::UsageAs(usage, then) => Reply::usage_sse(usage.clone(), then),
        }
    }

    /// The whole HTTP response.
    fn response(&self) -> String {
        match self {
            Reply::Status(code, body) => format!(
                "HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            Reply::Slow(_, then) => then.response(),
            Reply::Usage(..) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                self.sse()
            ),
            Reply::RateLimited(secs) => {
                let body = r#"{"error":{"message":"rate limited"}}"#;
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {secs}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            }
            _ => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                self.sse()
            ),
        }
    }

    fn hangs(&self) -> bool {
        match self {
            Reply::Hang(_) => true,
            Reply::Slow(_, then) => then.hangs(),
            Reply::Usage(_, _, then) | Reply::UsageAs(_, then) => then.hangs(),
            _ => false,
        }
    }

    fn delay(&self) -> Duration {
        match self {
            Reply::Slow(ms, _) => Duration::from_millis(*ms),
            _ => Duration::ZERO,
        }
    }

    /// The usage in a chunk of its own, as providers send it: just before
    /// [DONE]; for a reply that hangs, after what it sends.
    fn usage_sse(usage: Value, then: &Reply) -> String {
        let usage = format!("data: {}\n\n", json!({ "choices": [], "usage": usage }));
        if then.hangs() {
            return then.sse() + &usage;
        }
        then.sse()
            .replace("data: [DONE]\n\n", &format!("{usage}data: [DONE]\n\n"))
    }
}

/// A local server answering each request with the next scripted reply (the
/// last one again once they run out). Returns its base URL and the request
/// bodies it saw.
async fn model(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Value>>>) {
    model_routed(Vec::new(), replies).await
}

/// The script a request is answered from: the first route whose marker is
/// in the request's first message after the system prompt (a subagent's
/// brief, or the user's request), else the default script (K05: parallel
/// runs each get their own replies, in their own order).
fn route_of(body: &Value, markers: &[&str]) -> Option<usize> {
    let first = body["messages"]
        .as_array()?
        .iter()
        .find(|m| m["role"] != "system")?;
    let text = match &first["content"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    markers.iter().position(|m| text.contains(m))
}

/// `model`, with one script per run: `routes` pairs a marker (found in the
/// run's first message) with that run's replies; anything else is answered
/// from `default`. Each script is answered in order, the last reply again
/// once it runs out.
async fn model_routed(
    routes: Vec<(&'static str, Vec<Reply>)>,
    default: Vec<Reply>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let script = |replies: &[Reply]| -> Vec<(String, bool, Duration)> {
        replies
            .iter()
            .map(|r| (r.response(), r.hangs(), r.delay()))
            .collect()
    };
    let markers: Vec<&'static str> = routes.iter().map(|(m, _)| *m).collect();
    let mut scripts: Vec<Vec<(String, bool, Duration)>> =
        routes.iter().map(|(_, r)| script(r)).collect();
    scripts.push(script(&default));
    let next = Arc::new(Mutex::new(vec![0usize; scripts.len()]));
    let scripts = Arc::new(scripts);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (log, next, scripts, markers) =
                (log.clone(), next.clone(), scripts.clone(), markers.clone());
            // Each connection answered on its own: a slow reply to one run
            // doesn't hold up another's.
            tokio::spawn(async move {
                let body = read_request(&mut socket).await;
                let which = route_of(&body, &markers).unwrap_or(scripts.len() - 1);
                log.lock().unwrap().push(body);
                let (reply, hangs, delay) = {
                    let mut next = next.lock().unwrap();
                    let bodies = &scripts[which];
                    let n = next[which];
                    next[which] += 1;
                    bodies[n.min(bodies.len() - 1)].clone()
                };
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let _ = socket.write_all(reply.as_bytes()).await;
                if hangs {
                    // Keeps the connection open, saying nothing more.
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    drop(socket);
                    return;
                }
                let _ = socket.shutdown().await;
            });
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
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                })
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                return serde_json::from_slice(&buf[end + 4..end + 4 + length]).unwrap_or_default();
            }
        }
    }
    Value::Null
}

#[cfg(test)]
#[path = "subagent_e2e_tests.rs"]
mod subagent_tests;

#[cfg(test)]
#[path = "repair_e2e_tests.rs"]
mod repair_tests;

#[cfg(test)]
#[path = "headless_e2e_tests.rs"]
mod headless_tests;

#[cfg(test)]
#[path = "context_e2e_tests.rs"]
mod context_tests;

/*
 * The app
 */

struct Harness {
    app: tauri::App<MockRuntime>,
    project: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
    dialog_id: String,
    _dir: TempDir,
    /// An isolated chat's tests hold this: the overlays' data folder is
    /// the process's, so they run one at a time.
    _isolated: Option<tokio::sync::OwnedMutexGuard<()>>,
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
    let dir = std::env::temp_dir().join(format!(
        "agent-e2e-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
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
        sink.lock()
            .unwrap()
            .push(serde_json::from_str::<Value>(e.payload()).unwrap());
    });
    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: dialog_id.clone(),
        title: "Test".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
    };
    db.write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    Harness {
        app,
        project,
        events,
        dialog_id,
        _dir: dir,
        _isolated: None,
    }
}

impl Harness {
    /// Isolated chats: the overlays' data folder is this test's, and no
    /// other isolated test runs meanwhile.
    async fn isolate(&mut self) {
        self._isolated = Some(crate::overlay::chats::TEST_DATA.clone().lock_owned().await);
        crate::overlay::chats::init(&self._dir.0.join("data"));
    }
}

fn quiet_permissions() -> Permissions {
    Permissions {
        verify_changes: false,
        ..Permissions::default()
    }
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
                pdf: false,
                context_tokens: Some(100_000),
                no_tools: false,
                first_byte_timeout_ms: None,
                api_route: None,
            },
            options: None,
            mode: Some("agent".into()),
            context_mode: Some("trimming".into()),
            root: Some(self.project.to_string_lossy().into_owned()),
            current_user_content: None,
            local_date: Some("2026-10-06".into()),
            config: Some(AgentConfig {
                permissions,
                approval_profiles: Vec::new(),
                subagents: Default::default(),
            }),
            isolate: Some(false),
        }
    }

    async fn start(&self, request: StartRequest) -> Option<String> {
        let app = self.app.handle().clone();
        agent_start(
            app.clone(),
            app.state::<AgentState>(),
            app.state::<DbState>(),
            request,
        )
        .await
        .unwrap()
    }

    fn send(text: &str) -> StartKind {
        StartKind::Send {
            message: UserMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content: text.into(),
                meta: None,
                attachments: Vec::new(),
            },
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
        db.read(move |c| store::get_messages(c, &id, None, 500))
            .await
            .unwrap()
            .messages
    }

    fn events_of(&self, kind: &str) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == kind)
            .cloned()
            .collect()
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
        Reply::Call {
            id: "c1",
            name: "write",
            args: json!({ "path": "hello.txt", "content": "hi\n" }),
        },
        Reply::Text("Wrote hello.txt."),
    ])
    .await;
    let run = h
        .start(h.request(&url, Harness::send("Write hello"), quiet_permissions()))
        .await;
    assert!(run.is_some());
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("hello.txt").as_deref(), Some("hi\n"));
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "tool", "assistant"]);
    assert_eq!(messages[1].tool_calls.as_ref().unwrap()[0].name, "write");
    assert!(
        messages.iter().all(|m| m.finalized),
        "every message is finished"
    );
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(
        messages[2].content,
        "wrote 3 bytes to hello.txt (+1 -0 lines)"
    );
    assert_eq!(messages[3].content, "Wrote hello.txt.");

    // The second request carried the tool's result, with the tools and the system prompt.
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let sent = requests[1]["messages"].as_array().unwrap();
    assert_eq!(sent[0]["role"], "system");
    assert!(sent
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_call_id"] == "c1"));
    let tools: Vec<&str> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(tools.contains(&"write") && tools.contains(&"todo"));

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
        Reply::Call {
            id: "e1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "two", "content": "TWO" }),
        },
        Reply::Text("Okay, leaving it."),
    ])
    .await;
    let permissions = Permissions {
        edit_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Shout"), permissions))
        .await;

    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let request = &asked["request"];
    assert_eq!(request["toolCallId"], "e1");
    assert_eq!(request["summary"], "Edit a.txt");
    assert_eq!(request["edit"]["current"], "one\ntwo\n");
    assert_eq!(request["edit"]["proposed"], "one\nTWO\n");
    assert!(request["edit"]["absPath"]
        .as_str()
        .unwrap()
        .ends_with("a.txt"));

    let app = h.app.handle().clone();
    assert!(agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "e1".into(),
        ApprovalDecision::Deny
    ));
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("a.txt").as_deref(), Some("one\ntwo\n"));
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert!(
        result
            .content
            .starts_with("Error: the user rejected every change to a.txt"),
        "{}",
        result.content
    );
    assert_eq!(
        result.meta.as_ref().unwrap()["approval"],
        json!({ "by": "user", "approved": false })
    );
    assert_eq!(h.events_of("approvalSettled").len(), 1);
}

#[tokio::test]
async fn a_partly_accepted_edit_writes_only_those_changes() {
    let h = harness(&[("a.txt", "one\ntwo\nthree\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "ONE\ntwo\nTHREE\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        edit_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Caps"), permissions))
        .await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let app = h.app.handle().clone();
    let rejected = vec![diff::Hunk {
        orig_start: 3,
        deleted: vec!["three".into()],
        added: vec!["THREE".into()],
    }];
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Partial {
            content: "ONE\ntwo\nthree\n".into(),
            total: 2,
            rejected,
        },
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("ONE\ntwo\nthree\n"));
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert!(
        result.content.contains("accepted 1 of 2 changes"),
        "{}",
        result.content
    );
    // Reviewed by the user: not marked as an unreviewed change.
    assert_eq!(h.events_of("fileWritten")[0]["reviewed"], true);
}

#[tokio::test]
async fn a_whole_write_over_another_chats_unseen_change_is_refused() {
    let h = harness(&[("a.txt", "one\n")]).await;
    // Another chat changed the file; this one never read it.
    let other = crate::file_guard::Seen::new("other-chat");
    other.record(&h.project.join("a.txt"), b"one\n");
    crate::file_guard::write_for_chat(&h.project.join("a.txt"), b"other's change\n", &other)
        .unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "mine\n" }),
        },
        // The refusal showed the whole file: it counts as read.
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "a.txt", "content": "other's change\nmine\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let refused = result_of(&messages, "w1");
    assert!(
        refused.content.starts_with("Error: a.txt changed since you last read it (by another chat); your write was not applied."),
        "{}",
        refused.content
    );
    assert!(
        refused.content.contains("other's change"),
        "{}",
        refused.content
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("other's change\nmine\n"));
}

#[tokio::test]
async fn an_edit_after_an_unrelated_change_still_applies() {
    let h = harness(&[("a.txt", "one\ntwo\n")]).await;
    let other = crate::file_guard::Seen::new("other-chat");
    other.record(&h.project.join("a.txt"), b"one\ntwo\n");
    crate::file_guard::write_for_chat(&h.project.join("a.txt"), b"one\ntwo\nthree\n", &other)
        .unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "e1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "one", "content": "ONE" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("ONE\ntwo\nthree\n"));
}

fn user_approves_edits() -> Permissions {
    Permissions {
        edit_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    }
}

#[tokio::test]
async fn an_approved_edit_merges_with_a_change_made_while_it_waited() {
    let h = harness(&[("a.txt", "one\ntwo\nthree\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "ONE\ntwo\nthree\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Caps"), user_approves_edits()))
        .await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    // Another chat changes another line while the user reviews.
    std::fs::write(h.project.join("a.txt"), "one\ntwo\nTHREE\n").unwrap();
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("ONE\ntwo\nTHREE\n"));
    let messages = h.messages().await;
    assert!(result_of(&messages, "w1")
        .content
        .contains("changed while this edit waited for approval"));
}

#[tokio::test]
async fn a_partly_accepted_edit_keeps_a_change_made_while_it_waited() {
    let h = harness(&[("a.txt", "one\ntwo\nthree\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "one", "content": "ONE" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Caps"), user_approves_edits()))
        .await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    std::fs::write(h.project.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Partial {
            content: "ONE\ntwo\nthree\n".into(),
            total: 1,
            rejected: Vec::new(),
        },
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("ONE\ntwo\nthree\nfour\n"));
}

#[tokio::test]
async fn an_approved_edit_overlapping_a_new_change_is_reviewed_again() {
    let h = harness(&[("a.txt", "one\ntwo\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "ONE\ntwo\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Caps"), user_approves_edits()))
        .await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    std::fs::write(h.project.join("a.txt"), "uno\ntwo\n").unwrap();
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Allow,
    );
    // The same call is asked again, against the file as it is now.
    for _ in 0..200 {
        if h.events_of("approvalRequested").len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let again = h.events_of("approvalRequested");
    assert_eq!(again.len(), 2, "{again:#?}");
    assert_eq!(again[1]["request"]["edit"]["current"], "uno\ntwo\n");
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Deny,
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("uno\ntwo\n"));
}

#[tokio::test]
async fn a_heavy_command_waits_for_a_cpu_slot_with_a_note() {
    let _serial = crate::workload::queue::test_support::SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::workload::queue::test_reset(4, 16 << 30, 8 << 30);
    let h = harness(&[]).await;
    // Another chat holds the only slot.
    let holder = crate::workload::begin(
        crate::workload::JobKind::Agent {
            owner: "other".into(),
        },
        "cargo build",
        None,
    );
    assert!(holder.wait_for_slot(&|| false, &|_| {}).is_some());
    let (url, _) = model(vec![
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "make --version" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Build"), permissions))
        .await;
    let note = h
        .wait_for("a waiting note", |e| {
            e["type"] == "toolNote" && e["note"].is_string()
        })
        .await;
    assert!(
        note["note"]
            .as_str()
            .unwrap()
            .starts_with("Waiting for a CPU slot: another chat is running `cargo build`"),
        "{note}"
    );
    assert_eq!(
        h.messages()
            .await
            .iter()
            .filter(|m| m.role == "tool")
            .count(),
        0,
        "not run yet"
    );
    drop(holder);
    assert_eq!(h.run_end().await, "done");
    // The note is cleared once it runs.
    assert!(h.events_of("toolNote").last().unwrap()["note"].is_null());
}

/// An isolated chat: its writes and commands change its overlay, not the
/// project. Needs FUSE (skipped without it).
#[tokio::test]
async fn an_isolated_chat_changes_only_its_overlay() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("src/a.txt", "one\ntwo\n"), ("README.md", "hi\n")]).await;
    h.isolate().await;
    let (url, _) = model(vec![
        Reply::Call { id: "w1", name: "write", args: json!({ "path": "src/a.txt", "old_text": "one", "content": "ONE" }) },
        Reply::Call { id: "b1", name: "bash", args: json!({ "command": "sed -i 's/hi/hello/' README.md && cat README.md && echo new > new.txt" }) },
        Reply::Call { id: "b2", name: "bash", args: json!({ "command": "git commit -am x" }) },
        // The project's own path: the chat's view, not the project.
        Reply::Call { id: "b3", name: "bash", args: json!({ "command": format!("echo leak > {}/leak.txt && ls {}", h.project.display(), h.project.display()) }) },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Change things"),
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.isolate = Some(true);
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(
        result_of(&messages, "b1").content.contains("hello"),
        "{}",
        result_of(&messages, "b1").content
    );
    assert!(result_of(&messages, "b2")
        .content
        .starts_with("Error: `git commit` changes the repository"));
    let b3 = &result_of(&messages, "b3").content;
    assert!(
        b3.contains("new.txt"),
        "the listing is the chat's view: {b3}"
    );
    // The run finished cleanly: its changes landed, and the overlay is gone.
    assert_eq!(h.file("src/a.txt").as_deref(), Some("ONE\ntwo\n"));
    assert_eq!(h.file("README.md").as_deref(), Some("hello\n"));
    assert_eq!(h.file("new.txt").as_deref(), Some("new\n"));
    assert!(crate::overlay::chats::changes(&h.dialog_id).is_empty());
    assert!(
        h.events_of("notice").iter().any(|e| e["title"] == "Landed"),
        "{:#?}",
        h.events_of("notice")
    );
    let last = crate::overlay::chats::last_landing(&h.dialog_id).unwrap();
    // leak.txt came through the chat's view, so it landed with the rest.
    assert_eq!(last.paths.len(), 4, "{:?}", last.paths);
    assert_eq!(h.file("leak.txt").as_deref(), Some("leak\n"));
}

/// Changes that conflict with the project's own wait for the user.
#[tokio::test]
async fn an_isolated_chats_conflicting_changes_wait() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("a.txt", "one\n")]).await;
    h.isolate().await;
    let project_file = h.project.join("a.txt");
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "one", "content": "chat" }),
        },
        // Time for the user to edit the same line in the project.
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "sleep 1" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Edit"),
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.isolate = Some(true);
    h.start(request).await;
    // The user's edit, once the chat has made its change.
    for _ in 0..200 {
        if crate::overlay::chats::changes(&h.dialog_id)
            .iter()
            .any(|c| c.path() == "a.txt")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(&project_file, "user\n").unwrap();
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("a.txt").as_deref(), Some("user\n"));
    let pending = crate::overlay::chats::pending(&h.dialog_id).expect("a reason it didn't land");
    assert_eq!(pending.conflicts.len(), 1, "{pending:?}");
    // Land anyway: the chat's version.
    crate::overlay::chats::land_now(&h.dialog_id, true).unwrap();
    assert_eq!(h.file("a.txt").as_deref(), Some("chat\n"));
}

/// The project changed a file the chat changed too, and the merged result
/// fails the checks: the chat gets the failures and fixes them before its
/// changes land. Needs FUSE and npm (skipped without them).
#[tokio::test]
async fn checks_failing_on_the_projects_merged_changes_go_back_to_the_chat() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        ("check.js", "if (require('fs').readFileSync('a.txt', 'utf8').includes('BAD')) { console.error('a.txt: BAD'); process.exit(1); }"),
        ("a.txt", "1\n2\n3\n4\n5\n"),
    ])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    h.isolate().await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "chat\n2\n3\n4\n5\n" }),
        },
        // Time for the user to change another line in the project.
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "sleep 1" }),
        },
        Reply::Text("Done."),
        // The report says the file changed: read it again, then fix it.
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "a.txt", "content": "chat\n2\n3\n4\nGOOD\n" }),
        },
        Reply::Text("Fixed."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Edit"),
        Permissions {
            allow_any_command: true,
            ..Permissions::default()
        },
    );
    request.isolate = Some(true);
    h.start(request).await;
    for _ in 0..200 {
        if crate::overlay::chats::changes(&h.dialog_id)
            .iter()
            .any(|c| c.path() == "a.txt")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(h.project.join("a.txt"), "1\n2\n3\n4\nBAD\n").unwrap();
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let report = messages
        .iter()
        .find(|m| {
            m.role == "user"
                && m.meta
                    .as_ref()
                    .is_some_and(|x| x["harness"] == "verification")
        })
        .expect("the failures went back to the chat");
    assert!(
        report
            .content
            .contains("The project changed meanwhile (a.txt)"),
        "{}",
        report.content
    );
    assert!(
        result_of(&messages, "r1").content.contains("BAD"),
        "the merged file is in the chat's view"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("chat\n2\n3\n4\nGOOD\n"));
}

/// A conflict found while the chat runs goes to it: markers in its file and
/// a note; it combines both changes and they land. Needs FUSE
/// (skipped without it).
#[tokio::test]
async fn a_running_chat_resolves_its_own_conflict() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("a.txt", "one\n")]).await;
    h.isolate().await;
    crate::overlay::chats::set_chat_resolves(true);
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "one", "content": "chat" }),
        },
        // Time for the user to edit the same line in the project.
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "sleep 1" }),
        },
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "a.txt", "content": "user and chat\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Edit"),
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.isolate = Some(true);
    h.start(request).await;
    for _ in 0..200 {
        if crate::overlay::chats::changes(&h.dialog_id)
            .iter()
            .any(|c| c.path() == "a.txt")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(h.project.join("a.txt"), "user\n").unwrap();
    // What the project's watcher does when the file changes.
    crate::overlay::chats::project_changed(&h.project, &[h.project.join("a.txt")], false);
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let b1 = &result_of(&messages, "b1").content;
    assert!(b1.contains("conflict markers"), "the chat was told: {b1}");
    assert!(result_of(&messages, "r1")
        .content
        .contains("<<<<<<< project (now)"));
    assert_eq!(
        h.file("a.txt").as_deref(),
        Some("user and chat\n"),
        "landed once resolved"
    );
}

/// While many files change in the project (a branch switch), an isolated
/// run waits before its next step. Needs FUSE (skipped without it).
#[tokio::test]
async fn an_isolated_run_waits_while_the_project_switches_branches() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("a.txt", "one\n")]).await;
    h.isolate().await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "cat a.txt" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let switching = crate::overlay::chats::hold_busy(&h.project);
    let mut request = h.request(
        &url,
        Harness::send("Look"),
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.isolate = Some(true);
    let started = std::time::Instant::now();
    h.start(request).await;
    h.wait_for("notice", |e| {
        e["type"] == "notice" && e["title"] == "Waiting for the project"
    })
    .await;
    // The switch: the file changes, then the project settles.
    std::fs::write(h.project.join("a.txt"), "switched\n").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(switching);
    assert_eq!(h.run_end().await, "done");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "waited for the project to settle"
    );
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "b1").content.lines().last(),
        Some("switched"),
        "{}",
        result_of(&messages, "b1").content
    );
}

#[tokio::test]
async fn stop_ends_a_run_waiting_for_approval() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Call {
        id: "r1",
        name: "bash",
        args: json!({ "command": "ls" }),
    }])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("List"), permissions))
        .await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;

    // Another run in the same chat is refused meanwhile.
    let app = h.app.handle().clone();
    let again = agent_start(
        app.clone(),
        app.state(),
        app.state(),
        h.request(&url, StartKind::Retry, quiet_permissions()),
    )
    .await;
    assert!(again.is_err());

    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    let messages = h.messages().await;
    let result = messages.iter().find(|m| m.role == "tool").unwrap();
    assert_eq!(
        result.content,
        "Error: stopped by the user before this ran."
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "no request after Stop");
    assert_eq!(h.events_of("approvalSettled").len(), 1);
}

#[tokio::test]
async fn a_paused_run_continues_where_it_stopped() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        Reply::Call {
            id: "r2",
            name: "list_dir",
            args: json!({}),
        },
        Reply::Text("All read."),
    ])
    .await;
    let permissions = Permissions {
        tool_call_checkpoint: Some(1),
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Read"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "paused-checkpoint");
    let messages = h.messages().await;
    let paused = messages.last().unwrap();
    assert_eq!(
        paused.meta.as_ref().unwrap()["paused"],
        json!({ "kind": "checkpoint", "toolCalls": 1 })
    );

    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::ContinueRun, permissions))
        .await;
    // Counting goes on from 1: the next checkpoint is at 2.
    assert_eq!(h.run_end().await, "paused-checkpoint");
    let messages = h.messages().await;
    assert_eq!(
        messages.last().unwrap().meta.as_ref().unwrap()["paused"]["toolCalls"],
        2
    );
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
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "ok.txt", "content": "still no" }),
        },
        Reply::Text("Done."),
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "ok.txt", "content": "ok" }),
        },
        Reply::Text("Fixed."),
    ])
    .await;
    let permissions = Permissions::default();
    h.start(h.request(&url, Harness::send("Make the test pass"), permissions))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let messages = h.messages().await;
    let records: Vec<&Value> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["status"], "failed");
    assert_eq!(records[0]["checks"][0]["command"], "npm test");
    assert_eq!(records[1]["status"], "passed");
    let report = messages
        .iter()
        .find(|m| {
            m.role == "user"
                && m.meta
                    .as_ref()
                    .is_some_and(|x| x["harness"] == "verification")
        })
        .unwrap();
    assert!(
        report
            .content
            .contains("Your changes are not done yet: a check fails (attempt 1 of 3)."),
        "{}",
        report.content
    );
    assert!(report.content.contains("not ok"));
    assert_eq!(seen.lock().unwrap().len(), 4);
    // The model was told about the project and its checks.
    let system = seen.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        system.contains("## Project") && system.contains("- Tests: `npm test`"),
        "{system}"
    );
}

#[tokio::test]
async fn plan_mode_asks_and_saves_the_plan() {
    let h = harness(&[("src/db.rs", ""), ("src/api.rs", "")]).await;
    let plan = "# Users\n\nAdd a table.\n\n## Steps\n\
        1. Schema\n   - Files: `src/db.rs`\n   - Add a `users` table with `id` and `email`.\n\
        2. API\n   - Files: `src/api.rs`\n   - Add `fn list_users()` returning every user.\n\n\
        ## Decisions\n- SQLite\n\n## Validation\n- `cargo test`";
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
            message: UserMessage {
                id: "u1".into(),
                content: "Add users".into(),
                meta: Some(json!({ "mode": "plan" })),
                attachments: Vec::new(),
            },
        },
        quiet_permissions(),
    );
    request.mode = Some("agent".into()); // the message's mode wins
    h.start(request).await;

    let asked = h
        .wait_for("questionsAsked", |e| e["type"] == "questionsAsked")
        .await;
    assert_eq!(asked["request"]["questions"][0]["question"], "Which DB?");
    let app = h.app.handle().clone();
    let outcome: plan::QuestionsOutcome =
        serde_json::from_value(json!({ "answers": [{ "kind": "option", "label": "SQLite" }] }))
            .unwrap();
    assert!(agent_answer_questions(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "q1".into(),
        outcome
    ));
    assert_eq!(h.run_end().await, "done");

    let saved = h
        .project
        .join(".rovik")
        .join("plans")
        .join("2026-10-06-add-users.md");
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        format!("{plan}\n")
    );
    let messages = h.messages().await;
    let answer = messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("q1"))
        .unwrap();
    assert!(answer.content.contains("→ SQLite"));
    assert_eq!(
        answer.meta.as_ref().unwrap()["questions"]["outcome"]["answers"][0]["label"],
        "SQLite"
    );
    // The final reply links the plan, for "Implement".
    let last = messages.last().unwrap();
    assert_eq!(
        last.meta.as_ref().unwrap()["planFile"],
        saved.to_string_lossy().as_ref()
    );
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
            tool_calls: Some(vec![store::ToolCall {
                id: "l1".into(),
                name: "list_dir".into(),
                arguments: "{}".into(),
                extra_content: None,
            }]),
            ..run::new_message("assistant", "")
        },
    ];
    db.write(move |c| store::append_messages(c, &id, &stored))
        .await
        .unwrap();

    let (url, _) = model(vec![Reply::Text("Nothing there.")]).await;
    let run = h
        .start(h.request(&url, StartKind::Resume, quiet_permissions()))
        .await;
    assert!(run.is_some());
    // Asked again, whatever the settings say.
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert!(asked["request"]["note"]
        .as_str()
        .unwrap()
        .starts_with("Interrupted"));
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "l1".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        roles(&h.messages().await),
        ["user", "assistant", "tool", "assistant"]
    );

    // Nothing left to resume: no run.
    assert!(h
        .start(h.request(&url, StartKind::Resume, quiet_permissions()))
        .await
        .is_none());
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
    assert_eq!(
        messages[1].meta.as_ref().unwrap()["failure"]["kind"],
        "network"
    );
    assert_eq!(h.events_of("userMessageSettled")[0]["ok"], false);
}

#[tokio::test]
async fn the_ai_reviewer_decides_and_risky_commands_still_ask() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "bash",
            args: json!({ "command": "echo hi" }),
        },
        Reply::Call {
            id: "r2",
            name: "bash",
            args: json!({ "command": "git push" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let (reviewer, reviewed) = model(vec![Reply::Text(
        "{\"decision\": \"approve\", \"reason\": \"Harmless.\"}",
    )])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("List, then push"),
        Permissions {
            allow_any_command: true,
            command_approval: permissions::ApprovalRule {
                required: true,
                by: permissions::Approver::Ai,
            },
            ..quiet_permissions()
        },
    );
    let mut profile = request.profile.clone();
    profile.base_url = reviewer;
    profile.label = "Reviewer".into();
    request.config.as_mut().unwrap().approval_profiles = vec![profile];
    h.start(request).await;

    // `git push` is never left to the reviewer.
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert_eq!(asked["request"]["toolCallId"], "r2");
    assert!(asked["request"]["note"]
        .as_str()
        .unwrap()
        .starts_with("Commands that push"));
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "r2".into(),
        ApprovalDecision::Deny,
    );
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let first = messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("r1"))
        .unwrap();
    assert!(
        first.content.starts_with("$ echo hi\nexit code 0"),
        "{}",
        first.content
    );
    assert_eq!(
        first.meta.as_ref().unwrap()["approval"],
        json!({ "by": "ai", "approved": true, "model": "Reviewer", "reason": "Harmless." })
    );
    // The reviewer saw the request and the command.
    let review = reviewed.lock().unwrap()[0].clone();
    let prompt = review["messages"][1]["content"].as_str().unwrap();
    assert!(
        prompt.contains("User's request:\nList, then push")
            && prompt.contains("Action: Run `echo hi`"),
        "{prompt}"
    );
    assert_eq!(review["max_tokens"], 300);
    // Its review was shown while it ran.
    let notes = h.events_of("toolNote");
    assert_eq!(notes[0]["note"], "Reviewing with Reviewer…");
    assert!(notes[1]["note"].is_null());
}

#[tokio::test]
async fn a_reply_repeating_itself_is_cut_and_the_model_told() {
    let h = harness(&[]).await;
    let looping: &'static str = Box::leak(
        format!("Let me think. {}", "Sigh. I'll just output it. ".repeat(80)).into_boxed_str(),
    );
    let (url, seen) = model(vec![
        Reply::Text(looping),
        Reply::Text("Sorry, here it is."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Go"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "user", "assistant"]);
    assert!(
        messages[1]
            .content
            .starts_with("Let me think. Sigh. I'll just output it.\n\n_[Stopped here"),
        "{}",
        messages[1].content
    );
    assert!(messages[2].content.contains("started repeating"));
    assert_eq!(messages[3].content, "Sorry, here it is.");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

fn effort(level: &str) -> Option<crate::agent::OpenAIChatOptions> {
    serde_json::from_value(
        json!({ "reasoning_effort": level, "reasoning_formats": ["reasoning_effort"] }),
    )
    .unwrap()
}

impl Harness {
    /// The effort picked in this chat's selector.
    fn pick_effort(&self, level: &str) -> usize {
        self.pick_effort_with(effort(level))
    }

    fn pick_effort_with(&self, options: Option<crate::agent::OpenAIChatOptions>) -> usize {
        let app = self.app.handle().clone();
        agent_set_effort(
            app.state(),
            "p::m".into(),
            options,
            Some(self.dialog_id.clone()),
        )
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
    panic!(
        "the model was asked {} times, not {n}",
        seen.lock().unwrap().len()
    );
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
    assert_eq!(
        agent_set_effort(
            app.state(),
            "other::model".into(),
            effort("low"),
            Some(h.dialog_id.clone())
        ),
        0
    );
    assert_eq!(
        agent_set_effort(
            app.state(),
            "p::m".into(),
            effort("medium"),
            Some("another-chat".into())
        ),
        1
    );
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
    assert!(
        statuses
            .iter()
            .any(|s| s["text"] == "Asking again with low effort…"),
        "{statuses:?}"
    );
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
        Reply::Call {
            id: "r1",
            name: "bash",
            args: json!({ "command": "ls" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("List"),
        Permissions {
            command_approval: permissions::ApprovalRule {
                required: true,
                by: permissions::Approver::User,
            },
            ..quiet_permissions()
        },
    );
    request.options = effort("high");
    h.start(request).await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let app = h.app.handle().clone();
    h.pick_effort("low");
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "r1".into(),
        ApprovalDecision::Allow,
    );
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
    h.wait_for("streamed text", |e| {
        e["type"] == "messagePatched" && e["patch"]["content"] == "Part of the answer"
    })
    .await;

    // Silent since: stalled, but part of the reply is there.
    h.pick_effort("low");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "not asked again: the reply was coming"
    );

    let app = h.app.handle().clone();
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");
    let messages = h.messages().await;
    assert_eq!(messages[1].content, "Part of the answer");
    assert_eq!(
        messages[1].meta.as_ref().unwrap()["interrupted"]["kind"],
        "cancelled"
    );
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
            attachments: Vec::new(),
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
    messages
        .iter()
        .filter(|m| m.role == "user" && plan::is_harness(m))
        .collect()
}

fn ask_approval(group: &str) -> Permissions {
    let rule = permissions::ApprovalRule {
        required: true,
        by: permissions::Approver::User,
    };
    match group {
        "commands" => Permissions {
            command_approval: rule,
            ..quiet_permissions()
        },
        "edits" => Permissions {
            edit_approval: rule,
            ..quiet_permissions()
        },
        _ => Permissions {
            tool_approval: rule,
            ..quiet_permissions()
        },
    }
}

#[tokio::test]
async fn every_call_of_a_turn_gets_a_result_when_stopped_midway() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, seen) = model(vec![Reply::Calls(vec![
        ("c1", "read", json!({ "path": "a.txt" })),
        ("c2", "list_dir", json!({})),
        ("c3", "read", json!({ "path": "a.txt", "start_line": 1 })),
    ])])
    .await;
    h.start(h.request(&url, Harness::send("Look"), ask_approval("tools")))
        .await;
    h.wait_for("approval c1", |e| {
        e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "c1"
    })
    .await;
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "c1".into(),
        ApprovalDecision::Allow,
    );
    h.wait_for("approval c2", |e| {
        e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "c2"
    })
    .await;
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");

    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "c1").content, "x");
    assert_eq!(
        result_of(&messages, "c2").content,
        "Error: stopped by the user before this ran."
    );
    assert_eq!(
        result_of(&messages, "c3").content,
        "Error: stopped by the user before this ran."
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    // The history sent next time answers every call: a new run goes on fine.
    let (url, seen) = model(vec![Reply::Text("Ok.")]).await;
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Go on"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let sent = seen.lock().unwrap()[0]["messages"]
        .as_array()
        .unwrap()
        .clone();
    let results = sent.iter().filter(|m| m["role"] == "tool").count();
    assert_eq!(results, 3);
}

#[tokio::test]
async fn refused_and_broken_calls_are_reported_without_running() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "bash", json!({ "command": "rm -rf /" })),
            ("c2", "no_such_tool", json!({})),
            (
                "c3",
                "write",
                json!({ "path": "../outside.txt", "content": "x" }),
            ),
            ("c4", "read", json!({})),
            ("c5", "bash", json!({ "command": "echo $(whoami)" })),
        ]),
        Reply::Text("Understood."),
    ])
    .await;
    // Approval required: refused calls must never reach it.
    h.start(h.request(&url, Harness::send("Do things"), ask_approval("commands")))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("approvalRequested").is_empty());
    let messages = h.messages().await;
    let c1 = &result_of(&messages, "c1").content;
    assert!(c1.starts_with("Error: blocked by the user's permission settings. Not in the user's allowed commands: \"rm -rf /\""), "{c1}");
    assert_eq!(
        result_of(&messages, "c2").content,
        "Error: unknown tool \"no_such_tool\"."
    );
    assert!(result_of(&messages, "c3")
        .content
        .contains("must not contain '..'"));
    assert_eq!(
        result_of(&messages, "c4").content,
        "Error: \"path\" is required."
    );
    assert!(result_of(&messages, "c5")
        .content
        .contains("Command substitution"));
    assert!(!h.project.parent().unwrap().join("outside.txt").exists());
}

#[tokio::test]
async fn arguments_that_arent_json_are_an_error_for_the_model() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "list_dir",
            args: json!("{not json"),
        },
        Reply::Text("Oops."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("List"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(result_of(&messages, "c1")
        .content
        .starts_with("Error: could not parse arguments as JSON"));
}

#[tokio::test]
async fn without_a_project_only_web_tools_are_offered() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
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
    assert_eq!(
        result_of(&messages, "c1").content,
        "Error: \"read\" needs an open project folder."
    );
    // No project: no verification either.
    assert!(messages.iter().all(|m| m
        .meta
        .as_ref()
        .is_none_or(|x| x.get("verification").is_none())));
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
            (
                "w1",
                "write",
                json!({ "path": "a.txt", "content": "changed" }),
            ),
            ("r1", "bash", json!({ "command": "ls" })),
        ]),
        ask("q1"),
        ask("q2"),
        ask("q3"),
        ask("q4"),
        Reply::Text("I'll decide."),
    ])
    .await;
    h.start(h.request(&url, plan_send("Plan it"), quiet_permissions()))
        .await;
    let app = h.app.handle().clone();
    for id in ["q1", "q2", "q3"] {
        h.wait_for(id, |e| {
            e["type"] == "questionsAsked" && e["request"]["toolCallId"] == id
        })
        .await;
        let outcome: plan::QuestionsOutcome =
            serde_json::from_value(json!({ "reply": "whatever" })).unwrap();
        assert!(agent_answer_questions(
            app.clone(),
            app.state(),
            h.dialog_id.clone(),
            id.into(),
            outcome
        ));
        // Answering twice does nothing.
        let again: plan::QuestionsOutcome =
            serde_json::from_value(json!({ "reply": "again" })).unwrap();
        assert!(!agent_answer_questions(
            app.clone(),
            app.state(),
            h.dialog_id.clone(),
            id.into(),
            again
        ));
    }
    assert_eq!(h.run_end().await, "done");

    assert_eq!(h.file("a.txt").as_deref(), Some("keep"));
    let messages = h.messages().await;
    assert!(result_of(&messages, "w1")
        .content
        .starts_with("Error: write isn't available in Plan mode"));
    assert!(result_of(&messages, "r1")
        .content
        .starts_with("Error: bash isn't available in Plan mode"));
    assert!(result_of(&messages, "q4")
        .content
        .starts_with("You've already asked 3 times"));
    assert_eq!(
        h.events_of("questionsAsked").len(),
        3,
        "the fourth isn't shown"
    );
    // Plan mode offers no tool that changes anything.
    let tools = seen.lock().unwrap()[0]["tools"].to_string();
    assert!(!tools.contains("\"write\"") && tools.contains("\"submit_plan\""));
    assert!(seen.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("## Plan mode"));
}

#[tokio::test]
async fn ask_user_outside_plan_mode_is_refused() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "q1",
            name: "ask_user",
            args: json!({ "questions": [{ "question": "Q?", "options": [] }] }),
        },
        Reply::Text("Decided."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do it"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("questionsAsked").is_empty());
    assert!(result_of(&h.messages().await, "q1")
        .content
        .starts_with("Error: asking the user is only available in Plan mode"));
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
    h.start(h.request(&url, plan_send("Plan"), quiet_permissions()))
        .await;
    h.wait_for("questionsAsked", |e| e["type"] == "questionsAsked")
        .await;
    let app = h.app.handle().clone();
    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    assert_eq!(h.events_of("questionsSettled").len(), 1);
    let answer = result_of(&h.messages().await, "q1").clone();
    assert_eq!(
        answer.content,
        "Error: the user stopped the task instead of answering."
    );
    assert_eq!(
        answer.meta.unwrap()["questions"]["outcome"],
        json!({ "stopped": true })
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_bad_plan_is_sent_back_and_a_written_out_plan_nudged_once() {
    let h = harness(&[]).await;
    let written = "Here it is:\n\n# Plan\n\n## Steps\n1. One\n2. Two\n\n## Validation\n- tests";
    let (url, _) = model(vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": "no title, no steps" }),
        },
        Reply::Text(written),
        Reply::Text(written),
    ])
    .await;
    h.start(h.request(&url, plan_send("Plan"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let rejected = &result_of(&messages, "p1").content;
    assert!(
        rejected.starts_with("Error: the plan wasn't saved.") && rejected.contains("\"# \" title"),
        "{rejected}"
    );
    assert!(!h.project.join(".rovik/plans").exists());
    // Written in the reply instead: pointed out once, then the run ends.
    let notes = harness_notes(&messages);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(notes[0].content.contains("Submit it with submit_plan"));
    assert!(messages
        .last()
        .unwrap()
        .meta
        .as_ref()
        .is_none_or(|m| m.get("planFile").is_none()));
}

#[tokio::test]
async fn a_revised_plan_replaces_the_chats_plan_file() {
    let h = harness(&[("src/a.rs", ""), ("src/b.rs", "")]).await;
    let plan = |title: &str| {
        format!(
            "# {title}\n\n## Steps\n1. A\n   - Files: `src/a.rs`\n   - Add `fn a()` that returns 1.\n\
             2. B\n   - Files: `src/b.rs`\n   - Add `fn b()` that calls `a()`.\n\n## Validation\n- `cargo test`"
        )
    };
    let first: &'static str = Box::leak(plan("First").into_boxed_str());
    let second: &'static str = Box::leak(plan("Second").into_boxed_str());
    let (url, _) = model(vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": first }),
        },
        Reply::Text("Saved."),
    ])
    .await;
    h.start(h.request(&url, plan_send("Make a plan"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let (url, _) = model(vec![
        Reply::Call {
            id: "p2",
            name: "submit_plan",
            args: json!({ "plan": second }),
        },
        Reply::Text("Revised."),
    ])
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, plan_send("Change it"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let plans: Vec<_> = std::fs::read_dir(h.project.join(".rovik/plans"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(plans.len(), 1, "the revision went to the same file");
    assert!(std::fs::read_to_string(plans[0].path())
        .unwrap()
        .starts_with("# Second"));
}

#[tokio::test]
async fn unfinished_checklist_steps_are_pointed_out_twice_then_left_open() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Write it", "status": "done" }, { "title": "Test it", "status": "pending" }] }),
        },
        Reply::Text("Done."),
        Reply::Text("Really done."),
        Reply::Text("Truly done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "unfinished-checklist");
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "t1").content,
        "Checklist updated: 1 of 2 done."
    );
    let notes = harness_notes(&messages);
    assert_eq!(notes.len(), 2);
    assert!(notes[0].content.contains("\"Test it\""));
    assert!(notes[1].content.contains("last reminder"));
    // The final message carries `meta.checklistOpen` for the UI's Continue.
    let last = messages.last().unwrap();
    assert_eq!(
        last.meta
            .as_ref()
            .and_then(|m| m.get("checklistOpen"))
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(seen.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn work_without_a_checklist_is_refused_once_then_let_through() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "y\n" }),
        },
        // Two files: still within the "3 or more steps" rule.
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "b.txt", "content": "new\n" }),
        },
        // A third file with no checklist: refused, not run.
        Reply::Call {
            id: "w3",
            name: "write",
            args: json!({ "path": "c.txt", "content": "new\n" }),
        },
        // The repeat goes through: refuse-once-per-run, no stuck loop.
        Reply::Call {
            id: "w4",
            name: "write",
            args: json!({ "path": "c.txt", "content": "new\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Write three files"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(
        !result_of(&messages, "w2").content.contains("checklist"),
        "{}",
        result_of(&messages, "w2").content
    );
    let refused = &result_of(&messages, "w3").content;
    assert!(
        refused.contains("no checklist") && refused.contains("3 files changed"),
        "{refused}"
    );
    assert!(refused.contains("already finished as done"), "{refused}");
    assert_eq!(h.file("c.txt").as_deref(), Some("new\n"));
}

#[tokio::test]
async fn a_two_step_task_is_not_asked_for_a_checklist() {
    // Edit one file, then run a command: below the gate's thresholds.
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "y\n" }),
        },
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "ls" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Fix a.txt and check"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(
        !result_of(&messages, "b1").content.contains("checklist"),
        "{}",
        result_of(&messages, "b1").content
    );
}

#[tokio::test]
async fn checklist_off_never_refuses() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "1\n" }),
        },
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "b.txt", "content": "2\n" }),
        },
        Reply::Call {
            id: "w3",
            name: "write",
            args: json!({ "path": "c.txt", "content": "3\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        checklist: ChecklistMode::Off,
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Write three files"), permissions))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(
        !result_of(&messages, "w3").content.contains("checklist"),
        "{}",
        result_of(&messages, "w3").content
    );
    assert_eq!(seen.lock().unwrap()[0]["tool_choice"], json!("auto"));
}

#[tokio::test]
async fn checklist_always_forces_todo_on_every_message() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Answer", "status": "done" }] }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        checklist: ChecklistMode::Always,
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("A small change"), permissions))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests[0]["tool_choice"],
        json!({ "type": "function", "function": { "name": "todo" } })
    );
    assert_eq!(requests[1]["tool_choice"], json!("auto"));
}

#[tokio::test]
async fn plan_implementation_forces_todo_on_the_first_turn() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Step one", "status": "done" }] }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan above.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests[0]["tool_choice"],
        json!({ "type": "function", "function": { "name": "todo" } }),
        "{}",
        requests[0]["tool_choice"]
    );
    assert_eq!(requests[1]["tool_choice"], json!("auto"));
}

/// An image pasted into the chat input: stored with the message, and sent
/// in it, as the PNG it was made, to a model that takes images.
#[tokio::test]
async fn a_pasted_image_is_stored_with_the_message_and_sent_in_it() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("The theme makes it blue.")]).await;
    let db = h.app.state::<DbState>().inner().clone();
    let mut webp = std::io::Cursor::new(Vec::new());
    image::DynamicImage::from(image::RgbImage::from_pixel(
        2000,
        1000,
        image::Rgb([30, 90, 220]),
    ))
    .write_to(&mut webp, image::ImageFormat::WebP)
    .unwrap();
    let image = crate::chat_db::attachments::store_attached(&db, webp.into_inner())
        .await
        .unwrap()
        .id;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Why is this button blue?".into(),
        meta: None,
        // Pasted twice: sent once.
        attachments: vec![image.clone(), image.clone()],
    };
    let message_id = message.id.clone();
    let mut request = h.request(&url, StartKind::Send { message }, quiet_permissions());
    request.profile.vision = true;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");

    let dialog_id = h.dialog_id.clone();
    let page = db
        .read(move |c| store::get_messages(c, &dialog_id, None, 10))
        .await
        .unwrap();
    let stored = page.messages.iter().find(|m| m.id == message_id).unwrap();
    assert_eq!(stored.attachments, [image]);

    let requests = seen.lock().unwrap().clone();
    let sent = requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()
        .clone();
    let parts = sent["content"].as_array().unwrap();
    assert!(parts
        .last()
        .and_then(|p| p["text"].as_str())
        .unwrap()
        .contains("Why is this button blue?"));
    let images: Vec<&str> = parts
        .iter()
        .filter(|p| p["type"] == "image_url")
        .map(|p| p["image_url"]["url"].as_str().unwrap())
        .collect();
    assert_eq!(images.len(), 1, "{sent}");
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(images[0].strip_prefix("data:image/png;base64,").unwrap())
        .unwrap();
    let sent_image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
    assert_eq!((sent_image.width(), sent_image.height()), (768, 384));
}

/// A PDF attached in the chat input: stored with the message, named by its
/// meta, and sent as a file to a model that takes PDFs; another model is
/// told it was there.
#[tokio::test]
async fn an_attached_pdf_is_sent_as_a_file_to_a_model_that_takes_pdfs() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Text("It's about parsing."),
        Reply::Text("Can't see it."),
    ])
    .await;
    let db = h.app.state::<DbState>().inner().clone();
    let pdf =
        b"%PDF-1.4\n1 0 obj << /Type /Page >> endobj\n2 0 obj << /Type /Page >> endobj\n%%EOF";
    let stored = crate::chat_db::attachments::store_attached(&db, pdf.to_vec())
        .await
        .unwrap();
    assert_eq!(stored.mime, "application/pdf");
    assert_eq!(stored.pages, Some(2));

    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "What is this about?".into(),
        meta: Some(json!({
            "mode": "agent",
            "pdfs": [{ "id": stored.id, "name": "paper.pdf", "size": pdf.len(), "pages": 2 }],
        })),
        attachments: vec![stored.id.clone()],
    };
    let mut request = h.request(&url, StartKind::Send { message }, quiet_permissions());
    request.profile.pdf = true;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");

    let user_message = |request: &serde_json::Value| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap()
            .clone()
    };
    let sent = user_message(&seen.lock().unwrap()[0]);
    let file = &sent["content"][0];
    assert_eq!(file["type"], "file", "{sent}");
    assert_eq!(file["file"]["filename"], "paper.pdf");
    assert!(file["file"]["file_data"]
        .as_str()
        .unwrap()
        .starts_with("data:application/pdf;base64,"));

    // The same chat with a model not set to take PDFs.
    h.events.lock().unwrap().clear();
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "And now?".into(),
        meta: None,
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let sent = user_message(&seen.lock().unwrap()[1]);
    assert_eq!(
        sent["content"],
        "What is this about?\n[The PDF paper.pdf the user attached here wasn't sent: \
         this model isn't set up to accept PDFs.]"
    );
}

/// An image gone from the store (cleaned up) can't be sent: the message is
/// refused with a reason, before anything is stored or started.
#[tokio::test]
async fn a_message_with_a_missing_image_is_refused() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("unused")]).await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Look".into(),
        meta: None,
        attachments: vec!["f".repeat(64)],
    };
    let app = h.app.handle().clone();
    let err = agent_start(
        app.clone(),
        app.state::<AgentState>(),
        app.state::<DbState>(),
        h.request(&url, StartKind::Send { message }, quiet_permissions()),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no longer available"), "{err}");
    assert!(!app.state::<AgentState>().is_running(&h.dialog_id));
    let dialog_id = h.dialog_id.clone();
    let page = app
        .state::<DbState>()
        .read(move |c| store::get_messages(c, &dialog_id, None, 10))
        .await
        .unwrap();
    assert!(page.messages.is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

/// B07b: a plan edited by hand after it was checked: the agent hears what's
/// wrong with it before its first turn, and the user sees it.
#[tokio::test]
async fn a_plan_edited_to_name_a_missing_file_is_flagged_at_implementation() {
    let plan = "# Greet\n\nSay hello.\n\n## Steps\n\
        1. **Greeting**\n   - Files: `src/greet.rs`\n   - Change `fn greet()` to return \"hello\".\n\
        2. **Use it**\n   - Files: `src/main.rs`\n   - Call `greet()` and print the result.\n\n\
        ## Validation\n- `cargo test`\n";
    let h = harness(&[
        ("src/main.rs", "fn main() {}\n"),
        (".rovik/plans/2026-10-06-greet.md", plan),
    ])
    .await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Greeting", "status": "done" }] }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/2026-10-06-greet.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let note = messages
        .iter()
        .find(|m| m.meta.as_ref().is_some_and(|m| m["harness"] == "planCheck"))
        .expect("a plan check note");
    // The user sees it: the chat shows `planCheck` as a card.
    let check = &note.meta.as_ref().unwrap()["planCheck"];
    assert_eq!(check["file"], ".rovik/plans/2026-10-06-greet.md");
    assert!(
        check["problems"][0]
            .as_str()
            .unwrap()
            .starts_with("Step 1 names `src/greet.rs`, which isn't in the project"),
        "{check}"
    );
    // And the agent has it on its first turn.
    let first = seen.lock().unwrap()[0].clone();
    let text = first["messages"].to_string();
    assert!(text.contains("Step 1 names `src/greet.rs`"), "{text}");
    assert!(text.contains("may have been edited since it was written"));

    // A plan that holds up: no note.
    let h = harness(&[
        ("src/main.rs", "fn main() {}\n"),
        ("src/greet.rs", "\n"),
        (".rovik/plans/p.md", plan),
    ])
    .await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/p.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    h.run_end().await;
    assert!(!h
        .messages()
        .await
        .iter()
        .any(|m| m.meta.as_ref().is_some_and(|m| m["harness"] == "planCheck")));
}

#[tokio::test]
async fn restart_followed_by_continue_carries_the_checklist() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Write it", "status": "done" }, { "title": "Test it", "status": "pending" }] }),
        },
        Reply::Text("Done."),
        Reply::Text("Really done."),
        Reply::Text("Truly done."),
        Reply::Call { id: "w1", name: "write", args: json!({ "path": "t.txt", "content": "ok\n" }) },
        Reply::Text("Still going."),
        Reply::Text("Still going again."),
        Reply::Text("Still going a third time."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "unfinished-checklist");
    // A restart with a short "continue": the app carries the unfinished
    // checklist itself, and since this run works on the task (it changes a
    // file), the end-of-run reminders fire again.
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("continue"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "unfinished-checklist");
    let messages = h.messages().await;
    let carried: Vec<&MessageRow> = messages
        .iter()
        .filter(|m| {
            m.role == "user"
                && m.meta
                    .as_ref()
                    .and_then(|meta| meta.get("harness"))
                    .and_then(|v| v.as_str())
                    == Some("checklistCarried")
        })
        .collect();
    assert_eq!(carried.len(), 1, "{messages:#?}");
    assert!(
        carried[0].content.contains("Unfinished checklist"),
        "{}",
        carried[0].content
    );
    assert!(
        carried[0].content.contains("Test it"),
        "{}",
        carried[0].content
    );
    let notes = harness_notes(&messages);
    assert!(
        notes.iter().any(|n| n.content.contains("last reminder")),
        "{messages:#?}"
    );
}

#[tokio::test]
async fn a_quick_question_after_an_unfinished_run_is_just_answered() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Write it", "status": "done" }, { "title": "Test it", "status": "pending" }] }),
        },
        Reply::Text("Done."),
        Reply::Text("Really done."),
        Reply::Text("Truly done."),
        // The follow-up: answered in words, no work.
        Reply::Text("It parses the config."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Do"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "unfinished-checklist");
    let before = harness_notes(&h.messages().await).len();
    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        Harness::send("What does main.rs do?"),
        quiet_permissions(),
    ))
    .await;
    // Told about the old checklist (the carry note), but not pushed back
    // into it: no end-of-run reminders for work this run never touched.
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let new_notes: Vec<_> = harness_notes(&messages).into_iter().skip(before).collect();
    assert!(
        new_notes
            .iter()
            .all(|n| !n.content.contains("still has unfinished steps")),
        "{new_notes:#?}"
    );
}

#[tokio::test]
async fn named_tool_choice_falls_back_to_auto_when_rejected() {
    let h = harness(&[]).await;
    let refusal =
        "{\"error\":{\"message\":\"Unrecognized request argument supplied: tool_choice\"}}";
    let (url, seen) = model(vec![
        Reply::Status(400, refusal),
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Step one", "status": "done" }] }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan above.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert!(requests.len() >= 2, "{requests:?}");
    assert_eq!(
        requests[0]["tool_choice"],
        json!({ "type": "function", "function": { "name": "todo" } }),
        "{:?}",
        requests[0]
    );
    assert_eq!(
        requests[1]["tool_choice"],
        json!("auto"),
        "{:?}",
        requests[1]
    );
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "t1").content,
        "Checklist updated: 1 of 1 done."
    );
}

#[tokio::test]
async fn task_card_guidance_then_write_shows_through_card_note() {
    // Scripted model, CLM file tools directly: first read gets guidance as a
    // normal result, then the written card shows through card_note.
    let dir = std::env::temp_dir().join(format!(
        "agent-e2e-task-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::chat_db::DbState::open(&dir.join("data")).unwrap();
    let vfs = crate::vfs::VfsState::default();
    let dialog_id = "chat";
    db.write(|conn| {
        crate::chat_db::store::create_dialog(
            conn,
            &store::NewDialog {
                id: dialog_id.into(),
                title: "t".into(),
                created_at: 0,
                updated_at: 0,
                project_id: None,
            },
        )
    })
    .await
    .unwrap();
    db.write(|conn| {
        crate::chat_db::store::append_messages(
            conn,
            dialog_id,
            &[crate::chat_db::store::NewMessage {
                id: "u1".into(),
                role: "user".into(),
                content: "Do the thing".into(),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
                error: None,
                failed: false,
                attachments: vec![],
                created_at: 0,
                finalized: true,
            }],
        )
        .map(|_| ())
    })
    .await
    .unwrap();
    // First call: read before the card exists is guidance, not an error.
    let guidance = crate::clm::tool_read(&db, &vfs, dialog_id, "@context/TASK.md", None, None)
        .await
        .unwrap();
    assert!(guidance.contains("no task card yet"), "{guidance}");
    // The model writes the card, and the next request shows it.
    crate::clm::tool_write(
        &vfs,
        dialog_id,
        "@context/TASK.md",
        "Goal: do it\nNEXT: ship",
    )
    .unwrap();
    let cost = crate::chat_db::history::Cost {
        accepts: Default::default(),
        chars_per_token: 3.0,
    };
    let prepared = crate::clm::prepare(
        &db,
        &vfs,
        dialog_id.into(),
        100_000,
        cost,
        None,
        None,
        String::new(),
    )
    .await
    .unwrap();
    assert!(
        prepared.notes.join("\n").contains("Goal: do it"),
        "{:?}",
        prepared.notes
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clm_step_done_note_appears_at_the_threshold() {
    // Phase D: a finished step asks for a compaction of its span at/above
    // CLM_STEP_COMPACT_MIN, and only a TASK.md update below it. The
    // threshold itself is the contract the scripted-model runs rely on.
    assert_eq!(super::run::CLM_STEP_COMPACT_MIN, 35);
    assert!(crate::clm::prompt::percent(35, 100) >= super::run::CLM_STEP_COMPACT_MIN);
    assert!(crate::clm::prompt::percent(34, 100) < super::run::CLM_STEP_COMPACT_MIN);
}

#[test]
fn context_only_turns_on_at_90_and_lifts_after_an_edit() {
    // Phase E: 90%+ restricts a turn to context tools until an edit lands;
    // below 75% the restriction lifts. Full mounted-CLM runs need FUSE, so
    // the file-level contract is pinned here: the urgent threshold, the
    // lift threshold, and the restricted toolset itself.
    assert_eq!(crate::clm::prompt::URGENT, 90);
    assert_eq!(super::run::CLM_RESTRICT_LIFT_BELOW, 75);
    let only = super::tools::tools_for(true, false, false, &quiet_permissions(), false, true, true);
    let names: Vec<&str> = only
        .iter()
        .map(|t| {
            t.pointer("/function/name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
        })
        .collect();
    assert!(names.contains(&"todo"), "{names:?}");
    assert!(
        names.iter().all(|n| [
            "read",
            "write",
            "list_dir",
            "restore_context",
            "todo",
            "ask_user",
            "submit_plan"
        ]
        .contains(n)),
        "{names:?}"
    );
    assert!(
        !names.contains(&"bash"),
        "project work is not offered: {names:?}"
    );
}

#[test]
fn non_compliance_safety_valve_lifts_the_restriction() {
    // Weak models must not get stuck: after CLM_MAX_RESTRICTED_STALL
    // restricted turns with no edit the restriction lifts for good.
    assert_eq!(super::run::CLM_MAX_RESTRICTED_STALL, 2);
    assert_eq!(super::run::CLM_ESCALATE_AFTER_CALLS, 4);
}

#[tokio::test]
async fn a_loop_is_noted_then_paused() {
    let h = harness(&[("a.txt", "same")]).await;
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "a.txt" }),
    };
    let summary = "- Tried to read a.txt; it says \"same\" every time.";
    let (url, seen) = model(vec![
        read("r1"),
        read("r2"),
        read("r3"),
        read("r4"),
        read("r5"),
        read("r6"),
        Reply::Text(summary),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Read"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "paused-loop");
    let messages = h.messages().await;
    assert!(result_of(&messages, "r3")
        .content
        .contains("This looks like a loop"));
    assert!(result_of(&messages, "r6")
        .content
        .contains("the run was paused here for the user"));
    let paused = messages.last().unwrap().meta.as_ref().unwrap()["paused"].clone();
    assert_eq!(paused["kind"], "loop");
    assert_eq!(paused["toolCalls"], 6);
    assert!(paused["reason"].as_str().unwrap().contains("read (a.txt)"));

    // Before the pause: the model was asked, without tools, where it got to.
    let notes = harness_notes(&messages);
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].content.contains("Don't call any tools"),
        "{}",
        notes[0].content
    );
    let reply = &messages[messages.len() - 2];
    assert_eq!(
        (reply.role.as_str(), reply.content.as_str()),
        ("assistant", summary)
    );
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 7);
    assert_eq!(requests[5]["tool_choice"], "auto");
    assert_eq!(requests[6]["tool_choice"], "none");
    assert!(
        requests[6]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "the history refers to them"
    );
}

#[tokio::test]
async fn a_blocked_command_tried_again_pauses_the_run() {
    let h = harness(&[]).await;
    let rm = |id: &'static str| Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": "rm -rf build" }),
    };
    let (url, _) = model(vec![
        rm("b1"),
        rm("b2"),
        Reply::Text("- I tried to delete build/ twice; it's not allowed."),
    ])
    .await;
    let permissions = Permissions {
        allow_any_command: false,
        allowed_commands: vec!["git".into()],
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("Clean up"), permissions))
        .await;
    assert_eq!(h.run_end().await, "paused-loop");
    let messages = h.messages().await;
    assert!(result_of(&messages, "b1")
        .content
        .starts_with("Error: blocked by the user's permission settings"));
    let second = &result_of(&messages, "b2").content;
    assert!(
        second.contains("the run was paused here for the user"),
        "{second}"
    );
    let paused = messages.last().unwrap().meta.as_ref().unwrap()["paused"].clone();
    assert!(paused["reason"]
        .as_str()
        .unwrap()
        .contains("was made again after it was rejected"));
}

#[tokio::test]
async fn a_pattern_sent_as_literal_text_is_searched_as_a_regex() {
    let css = ".cursor-follower { left: 0; }\n.btn { color: red; }\n.link-hover:hover { transform: rotate(45deg); }\n";
    let h = harness(&[("src/styles.css", css)]).await;
    let search = |id: &'static str, query: &'static str| Reply::Call {
        id,
        name: "search_code",
        args: json!({ "context_lines": 2, "path_glob": "styles.css", "query": query }),
    };
    let (url, _) = model(vec![
        search("s1", r"cursor-follower|link-hover|rotate\(45deg\)"),
        search("s2", r"nothing-here|absent\(x\)"),
        Reply::Text("Found them."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Find the cursor styles"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let found = result_of(&messages, "s1");
    // Lines 1 and 3 (link-hover and rotate(45deg) share a line); not .btn.
    assert!(
        found.content.starts_with("2 matches in 1 file"),
        "{}",
        found.content
    );
    assert!(
        found.content.contains("searched as a regular expression"),
        "{}",
        found.content
    );
    assert!(
        !found.content.contains(".btn") || found.content.contains("2- .btn"),
        "{}",
        found.content
    );
    assert_eq!(found.meta.as_ref().unwrap()["count"], 2);
    let none = result_of(&messages, "s2");
    assert!(none.content.starts_with("No matches."), "{}", none.content);
    assert!(
        none.content.contains("as a regular expression either"),
        "{}",
        none.content
    );
    assert_eq!(none.meta.as_ref().unwrap()["count"], 0);
}

/// The assistant messages of a request body that called tools.
fn steps(request: &Value) -> Vec<Value> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .cloned()
        .collect()
}

#[tokio::test]
async fn reasoning_goes_back_with_the_steps_of_the_current_request() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let signed = |t: Value| -> Value {
        let mut f =
            json!({ "type": "reasoning.text", "format": "anthropic-claude-v1", "index": 0 });
        f.as_object_mut()
            .unwrap()
            .extend(t.as_object().unwrap().clone());
        f
    };
    let thought = Reply::Thought(
        "Let me read it.",
        vec![
            signed(json!({ "text": "Let me " })),
            signed(json!({ "text": "read it." })),
            signed(json!({ "signature": "sig" })),
        ],
        Box::new(Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        }),
    );
    let (url, seen) = model(vec![
        thought,
        Reply::Text("It says one."),
        Reply::Text("Still one."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Read a.txt"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");

    let whole = json!([{ "type": "reasoning.text", "format": "anthropic-claude-v1", "index": 0, "text": "Let me read it.", "signature": "sig" }]);
    // Stored once, whole, with the model that wrote it; shown as text.
    let messages = h.messages().await;
    let step = messages.iter().find(|m| m.tool_calls.is_some()).unwrap();
    let stored = &step.meta.as_ref().unwrap()["reasoning"];
    assert_eq!(
        (
            stored["details"].clone(),
            stored["model"].clone(),
            stored["text"].clone()
        ),
        (whole.clone(), json!("m"), json!("Let me read it."))
    );
    // The next step of the same request carries it back, unchanged.
    let after_tool = steps(&seen.lock().unwrap()[1]);
    assert_eq!(after_tool[0]["reasoning_details"], whole);
    assert!(after_tool[0].get("reasoning_content").is_none());

    // A later request doesn't: the model's earlier turns are finished.
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Again?"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let later = steps(&seen.lock().unwrap()[2]);
    assert!(
        later[0].get("reasoning_details").is_none(),
        "{:?}",
        later[0]
    );
}

#[tokio::test]
async fn deepseek_keeps_its_tool_steps_reasoning_and_other_models_get_none() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let thought = Reply::Thought(
        "Read it first.",
        Vec::new(),
        Box::new(Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        }),
    );
    let (url, seen) = model(vec![
        thought,
        Reply::Text("one"),
        Reply::Text("two"),
        Reply::Text("three"),
    ])
    .await;
    let as_model = |name: &str, text: &str| {
        let mut request = h.request(&url, Harness::send(text), quiet_permissions());
        request.profile.model = name.into();
        request
    };
    h.start(as_model("deepseek-v4", "Read a.txt")).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        steps(&seen.lock().unwrap()[1])[0]["reasoning_content"],
        "Read it first."
    );
    // DeepSeek refuses later requests without it: it stays.
    h.events.lock().unwrap().clear();
    h.start(as_model("deepseek-v4", "And now?")).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        steps(&seen.lock().unwrap()[2])[0]["reasoning_content"],
        "Read it first."
    );
    // Another model never gets reasoning DeepSeek wrote.
    h.events.lock().unwrap().clear();
    h.start(as_model("other-model", "And you?")).await;
    assert_eq!(h.run_end().await, "done");
    let other = steps(&seen.lock().unwrap()[3]);
    assert!(
        other[0].get("reasoning_content").is_none() && other[0].get("reasoning_details").is_none()
    );
}

#[tokio::test]
async fn a_server_refusing_reasoning_sent_back_gets_none() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let thought = Reply::Thought(
        "Read it first.",
        Vec::new(),
        Box::new(Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        }),
    );
    let refusal = r#"{"error":{"message":"Extra inputs are not permitted: messages.1.reasoning_content","type":"invalid_request_error"}}"#;
    let (url, seen) = model(vec![
        thought,
        Reply::Status(400, refusal),
        Reply::Text("one"),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Read a.txt"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        steps(&requests[1])[0]["reasoning_content"],
        "Read it first."
    );
    assert!(
        steps(&requests[2])[0].get("reasoning_content").is_none(),
        "sent again without it"
    );
}

#[tokio::test]
async fn an_empty_overwrite_is_refused_without_asking() {
    let h = harness(&[("a.txt", "keep\n")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "" }),
        },
        Reply::Text("Ok."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Clear it"), ask_approval("edits")))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(
        h.events_of("approvalRequested").is_empty(),
        "nothing to approve"
    );
    let messages = h.messages().await;
    let result = &result_of(&messages, "w1").content;
    assert!(
        result.starts_with("Error: refusing to replace a.txt"),
        "{result}"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("keep\n"));
}

#[tokio::test]
async fn edits_that_change_nothing_or_cant_apply_skip_approval() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            (
                "w1",
                "write",
                json!({ "path": "a.txt", "content": "one\n" }),
            ),
            (
                "e1",
                "write",
                json!({ "path": "a.txt", "old_text": "missing", "content": "x" }),
            ),
            (
                "e2",
                "write",
                json!({ "path": "nope.txt", "old_text": "a", "content": "b" }),
            ),
        ]),
        Reply::Text("Ok."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits")))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(
        h.events_of("approvalRequested").is_empty(),
        "nothing to approve"
    );
    let messages = h.messages().await;
    assert!(result_of(&messages, "w1")
        .content
        .starts_with("wrote 4 bytes to a.txt ("));
    assert!(result_of(&messages, "e1").content.starts_with("Error:"));
    assert!(result_of(&messages, "e2").content.contains("no such file"));
    assert_eq!(h.file("a.txt").as_deref(), Some("one\n"));
}

#[tokio::test]
async fn a_new_file_is_approved_whole_and_an_allowed_edit_is_recorded_as_reviewed() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "dir/new.txt", "content": "hello" }),
        },
        Reply::Text("Created."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Create"), ask_approval("edits")))
        .await;
    let asked = h
        .wait_for("approval", |e| e["type"] == "approvalRequested")
        .await;
    assert!(
        asked["request"].get("edit").is_none(),
        "nothing to diff against"
    );
    assert!(asked["request"]["note"]
        .as_str()
        .unwrap()
        .contains("New file"));
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Allow,
    );
    // Unknown and repeated answers do nothing.
    assert!(!agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Deny
    ));
    assert!(!agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "nope".into(),
        ApprovalDecision::Allow
    ));
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
            (
                "e1",
                "write",
                json!({ "path": "a.txt", "old_text": "x", "content": "y" }),
            ),
            (
                "e2",
                "write",
                json!({ "path": ".env", "old_text": "1", "content": "2" }),
            ),
            ("e3", "write", json!({ "path": "b.txt", "content": "b" })),
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
            edit_approval: permissions::ApprovalRule {
                required: true,
                by: permissions::Approver::Ai,
            },
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
    let asked = h
        .wait_for("approval e2", |e| {
            e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e2"
        })
        .await;
    assert!(asked["request"]["note"]
        .as_str()
        .unwrap()
        .starts_with("Edits to credential or secret files"));
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "e2".into(),
        ApprovalDecision::Deny,
    );
    // The reviewer failed: the user decides.
    let asked = h
        .wait_for("approval e3", |e| {
            e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e3"
        })
        .await;
    assert!(
        asked["request"]["note"]
            .as_str()
            .unwrap()
            .contains("couldn’t answer"),
        "{asked}"
    );
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "e3".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let e1 = result_of(&messages, "e1");
    assert!(e1
        .content
        .starts_with("Error: rejected by the user's AI reviewer (Reviewer): Not asked for."));
    assert_eq!(e1.meta.as_ref().unwrap()["approval"]["by"], "ai");
    assert_eq!(h.file("a.txt").as_deref(), Some("x\n"));
    assert_eq!(h.file(".env").as_deref(), Some("SECRET=1\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("b"));
}

#[tokio::test]
async fn no_reviewer_model_means_the_user_approves() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "list_dir",
            args: json!({}),
        },
        Reply::Text("Ok."),
    ])
    .await;
    let permissions = Permissions {
        tool_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::Ai,
        },
        ..quiet_permissions()
    };
    h.start(h.request(&url, Harness::send("List"), permissions))
        .await;
    let asked = h
        .wait_for("approval", |e| e["type"] == "approvalRequested")
        .await;
    assert!(asked["request"]["note"]
        .as_str()
        .unwrap()
        .starts_with("No AI reviewer model is set"));
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "r1".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");
}

#[tokio::test]
async fn a_repeating_model_ends_the_run_the_second_time() {
    let h = harness(&[]).await;
    let looping: &'static str =
        Box::leak(format!("Thinking. {}", "Let me try again now. ".repeat(80)).into_boxed_str());
    let (url, seen) = model(vec![Reply::Text(looping)]).await;
    h.start(h.request(&url, Harness::send("Go"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "repetition");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(
        h.events_of("notice")[0]["title"],
        "The model kept repeating itself"
    );
    let messages = h.messages().await;
    assert_eq!(roles(&messages), ["user", "assistant", "user", "assistant"]);
    assert!(messages[3].content.contains("_[Stopped here"));
}

#[tokio::test]
async fn a_rejected_request_is_not_retried_and_reported() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Status(
        401,
        "{\"error\":{\"message\":\"bad key\"}}",
    )])
    .await;
    h.start(h.request(&url, Harness::send("Hi"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "error");
    assert_eq!(seen.lock().unwrap().len(), 1, "auth errors aren't retried");
    let messages = h.messages().await;
    assert_eq!(
        messages[1].error.as_deref(),
        Some("The API key was rejected. Check it in Model Settings.")
    );
    assert_eq!(
        messages[1].meta.as_ref().unwrap()["failure"]["kind"],
        "auth"
    );
    assert!(messages[0].failed);
}

#[tokio::test]
async fn a_refused_reasoning_form_isnt_sent_again_in_the_run() {
    let h = harness(&[]).await;
    let refusal = "{\"error\":{\"message\":\"Unrecognized request argument supplied: chat_template_kwargs\"}}";
    let (url, seen) = model(vec![
        Reply::Status(400, refusal),
        Reply::Call {
            id: "c1",
            name: "list_dir",
            args: json!({}),
        },
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
    assert!(
        requests[1].get("chat_template_kwargs").is_none()
            && requests[1]["reasoning_effort"] == "high"
    );
    assert!(
        requests[2].get("chat_template_kwargs").is_none(),
        "not tried again on the next turn"
    );
    let learned = h.events_of("reasoningLearned");
    assert!(learned
        .iter()
        .any(|l| l["rejected"] == json!(["chat_template_kwargs"])));
}

#[tokio::test]
async fn continue_and_retry_reuse_the_requests_wire_text() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("Answer.")]).await;
    let message = UserMessage {
        id: "u1".into(),
        content: "Fix it".into(),
        meta: Some(json!({ "mode": "agent", "sentContent": "Editor context\n\nFix it" })),
        attachments: Vec::new(),
    };
    h.start(h.request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    h.events.lock().unwrap().clear();
    // No wire text from the frontend: the stored one is used.
    h.start(h.request(&url, StartKind::ContinueReply, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    let user = |r: &Value| {
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .clone()
    };
    assert_eq!(user(&requests[0]), "Editor context\n\nFix it");
    assert_eq!(user(&requests[1]), "Editor context\n\nFix it");
    let last = requests[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert!(last["content"]
        .as_str()
        .unwrap()
        .starts_with("Your previous reply was interrupted"));
}

#[tokio::test]
async fn runs_in_two_chats_go_on_independently() {
    let a = harness(&[]).await;
    // The second chat in the same app and database.
    let b_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: b_id.clone(),
        title: "B".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
    };
    let db = a.app.state::<DbState>().inner().clone();
    db.write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();

    let (hang, _) = model(vec![Reply::Hang(None)]).await;
    let (quick, _) = model(vec![Reply::Text("B done.")]).await;
    a.start(a.request(&hang, Harness::send("A"), quiet_permissions()))
        .await;
    let mut b = a.request(&quick, Harness::send("B"), quiet_permissions());
    b.dialog_id = b_id.clone();
    a.start(b).await;
    a.wait_for("B ends", |e| {
        e["type"] == "runEnded" && e["dialogId"] == b_id.as_str()
    })
    .await;

    let app = a.app.handle().clone();
    let status = agent_status(app.state());
    assert_eq!(status.runs.len(), 1, "A still runs");
    assert!(
        !agent_stop(app.clone(), app.state(), b_id.clone()),
        "B is done: nothing to stop"
    );
    assert!(agent_stop(app.clone(), app.state(), a.dialog_id.clone()));
    a.wait_for("A ends", |e| {
        e["type"] == "runEnded" && e["dialogId"] == a.dialog_id.as_str()
    })
    .await;
    assert!(agent_status(app.state()).runs.is_empty());
}

#[tokio::test]
async fn status_lists_what_a_run_waits_on() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "list_dir",
            args: json!({}),
        },
        Reply::Text("Ok."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("List"), ask_approval("tools")))
        .await;
    h.wait_for("approval", |e| e["type"] == "approvalRequested")
        .await;
    let app = h.app.handle().clone();
    let status = agent_status(app.state());
    assert_eq!(status.runs[0].dialog_id, h.dialog_id);
    assert_eq!(status.approvals[0].tool_call_id, "r1");
    assert!(status.questions.is_empty());
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "r1".into(),
        ApprovalDecision::Allow,
    );
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
    assert!(
        agent_status(app.state()).runs.is_empty(),
        "not left registered"
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nothing_to_continue_or_check_starts_no_run() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("x")]).await;
    let none = h
        .start(h.request(
            &url,
            StartKind::RunChecks {
                record_id: "missing".into(),
            },
            Permissions::default(),
        ))
        .await;
    assert!(none.is_none());
    assert!(h.events_of("runStarted").is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

/// P1.7, P1.8: a newer run's waiting checks make an older run's stale:
/// starting those is refused and doesn't touch the newer run's state;
/// checks with no state left say so; deleting the chat frees its state.
#[tokio::test]
async fn an_older_waiting_check_is_superseded_and_carries_are_freed() {
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(0)\"" } }"#,
    )])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "y.txt", "content": "y" }),
        },
        Reply::Text("Done."),
        Reply::Call {
            id: "w3",
            name: "write",
            args: json!({ "path": "z.txt", "content": "z" }),
        },
        Reply::Text("Done."),
        Reply::Call {
            id: "w4",
            name: "write",
            args: json!({ "path": "w.txt", "content": "w" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..Permissions::default()
    };
    let waiting = |messages: &[MessageRow], status: &str| -> Vec<String> {
        messages
            .iter()
            .filter(|m| {
                m.meta
                    .as_ref()
                    .is_some_and(|m| m["verification"]["status"] == status)
            })
            .map(|m| m.id.clone())
            .collect()
    };
    let state = h.app.state::<AgentState>();
    h.start(h.request(&url, Harness::send("Write x"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let first = waiting(&h.messages().await, "needs-approval")[0].clone();
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Write y"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let messages = h.messages().await;
    assert_eq!(
        waiting(&messages, "superseded"),
        std::slice::from_ref(&first)
    );
    let second = waiting(&messages, "needs-approval")[0].clone();
    assert!(state.has_carry(&h.dialog_id));

    // The older record's checks don't start, and the newer state stays.
    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: first.clone(),
        },
        permissions.clone(),
    ))
    .await;
    assert!(h.events_of("runStarted").is_empty());
    assert!(state.has_carry(&h.dialog_id));

    // The newer one's run with its state: nothing says it was lost.
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: second.clone(),
        },
        permissions.clone(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");
    let record = h
        .messages()
        .await
        .into_iter()
        .find(|m| m.id == second)
        .unwrap();
    assert!(record.meta.unwrap()["verification"]
        .get("stateRestored")
        .is_none());
    assert!(!state.has_carry(&h.dialog_id));

    // A third waits; its state is gone (as after a restart): it says so.
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Write z"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let third = waiting(&h.messages().await, "needs-approval")[0].clone();
    state.drop_carry(&h.dialog_id);
    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: third.clone(),
        },
        permissions.clone(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");
    let record = h
        .messages()
        .await
        .into_iter()
        .find(|m| m.id == third)
        .unwrap();
    assert_eq!(record.meta.unwrap()["verification"]["stateRestored"], false);

    // Deleting a chat frees what it carried.
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Again"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert!(state.has_carry(&h.dialog_id));
    let app = h.app.handle().clone();
    crate::chat_db::commands::chat_delete_dialog(
        app.clone(),
        app.state::<AgentState>(),
        app.state::<crate::vfs::VfsState>(),
        app.state::<DbState>(),
        h.dialog_id.clone(),
    )
    .await
    .unwrap();
    assert!(!state.has_carry(&h.dialog_id));
}

/// Stores verification records as earlier runs left them; their ids, in
/// order.
async fn store_records(h: &Harness, records: &[Value]) -> Vec<String> {
    let id = h.dialog_id.clone();
    let db = h.app.state::<DbState>().inner().clone();
    let mut stored = vec![run::new_message("user", "Earlier")];
    for r in records {
        stored.push(store::NewMessage {
            meta: Some(json!({ "verification": r })),
            ..run::new_message("assistant", "")
        });
    }
    let ids: Vec<String> = stored[1..].iter().map(|m| m.id.clone()).collect();
    db.write(move |c| store::append_messages(c, &id, &stored))
        .await
        .unwrap();
    ids
}

fn verification_of(messages: &[MessageRow], id: &str) -> Value {
    messages
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.meta.as_ref())
        .map(|m| m["verification"].clone())
        .unwrap()
}

/// Round 2 (§4.2): a new run supersedes only records waiting for "Run
/// checks"; results stay as they were, and the superseded record keeps
/// its own round limit (not the new run's).
#[tokio::test]
async fn superseding_marks_only_waiting_records() {
    let h = harness(&[]).await;
    let ids = store_records(
        &h,
        &[
            json!({ "status": "passed", "maxRounds": 4 }),
            json!({ "status": "failed", "maxRounds": 4 }),
            json!({ "status": "flagged", "maxRounds": 4 }),
            json!({ "status": "needs-approval", "maxRounds": 4, "changedFiles": ["a.txt"] }),
        ],
    )
    .await;
    let (url, _) = model(vec![Reply::Text("Hi.")]).await;
    h.start(h.request(&url, Harness::send("Hello"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    for (id, status) in ids
        .iter()
        .zip(["passed", "failed", "flagged", "superseded"])
    {
        let v = verification_of(&messages, id);
        assert_eq!(v["status"], status, "{v}");
        assert_eq!(v["maxRounds"], 4, "{v}");
    }

    // Its checks don't start any more: no run.
    h.events.lock().unwrap().clear();
    let none = h
        .start(h.request(
            &url,
            StartKind::RunChecks {
                record_id: ids[3].clone(),
            },
            quiet_permissions(),
        ))
        .await;
    assert!(none.is_none());
    assert!(h.events_of("runStarted").is_empty());
}

/// Round 2 (§4.2): a chat resumed after a restart is a new run too: the
/// checks it left waiting are superseded.
#[tokio::test]
async fn a_resume_after_restart_supersedes_waiting_checks() {
    let h = harness(&[]).await;
    let ids = store_records(
        &h,
        &[json!({ "status": "needs-approval", "changedFiles": ["a.txt"] })],
    )
    .await;
    let id = h.dialog_id.clone();
    let db = h.app.state::<DbState>().inner().clone();
    let stored = vec![store::NewMessage {
        tool_calls: Some(vec![store::ToolCall {
            id: "l1".into(),
            name: "list_dir".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        ..run::new_message("assistant", "")
    }];
    db.write(move |c| store::append_messages(c, &id, &stored))
        .await
        .unwrap();
    let (url, _) = model(vec![Reply::Text("Nothing there.")]).await;
    assert!(h
        .start(h.request(&url, StartKind::Resume, quiet_permissions()))
        .await
        .is_some());
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "l1".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");
    let v = verification_of(&h.messages().await, &ids[0]);
    assert_eq!(v["status"], "superseded", "{v}");
}

/// Round 2 (§4.2): checks started with their state lost tell the agent so
/// once, in the first round's note, not in every round after.
#[tokio::test]
async fn the_lead_is_told_once_that_state_was_lost() {
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(require('fs').existsSync('ok.txt') ? 0 : 1)\"" } }"#,
    )])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
        // The checks' rounds, after "Run checks".
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "not-yet.txt", "content": "y" }),
        },
        Reply::Text("Fixed."),
        Reply::Call {
            id: "w3",
            name: "write",
            args: json!({ "path": "ok.txt", "content": "ok" }),
        },
        Reply::Text("Fixed."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Write x"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let record = h.messages().await.last().unwrap().id.clone();
    // As after a restart.
    h.app.state::<AgentState>().drop_carry(&h.dialog_id);
    // Each fix waits for approval again: "Run checks" until they pass.
    let mut record = record;
    let mut runs = 0;
    loop {
        runs += 1;
        h.events.lock().unwrap().clear();
        h.start(h.request(
            &url,
            StartKind::RunChecks { record_id: record },
            permissions.clone(),
        ))
        .await;
        match h.run_end().await.as_str() {
            "verified" => break,
            "verify-needs-approval" if runs < 4 => {
                record = h.messages().await.last().unwrap().id.clone();
            }
            other => panic!("{other}"),
        }
    }
    assert_eq!(runs, 3, "two failed rounds went back to the agent");
    let lost = "didn't carry over to these";
    let messages = h.messages().await;
    let told: Vec<bool> = messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.contains(lost))
        .collect();
    assert_eq!(told.iter().filter(|t| **t).count(), 1, "{told:?}");
}

fn needs_user_approval() -> Permissions {
    Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..Permissions::default()
    }
}

fn has_npm() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

const PASSING_TESTS: &str =
    r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(0)\"" } }"#;

/// Round 2 (§4.2): a carry past its time isn't taken up: the checks run
/// afresh, and the record says the state wasn't restored.
#[tokio::test]
async fn an_expired_carry_runs_afresh_and_says_so() {
    let h = harness(&[("package.json", PASSING_TESTS)]).await;
    if !has_npm() {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write x"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let record = h.messages().await.last().unwrap().id.clone();
    let state = h.app.state::<AgentState>();
    state.age_carry(&h.dialog_id, verify::CARRY_TTL);
    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: record.clone(),
        },
        needs_user_approval(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");
    let v = verification_of(&h.messages().await, &record);
    assert_eq!(v["status"], "passed", "{v}");
    assert_eq!(v["stateRestored"], false, "{v}");
    assert!(!state.has_carry(&h.dialog_id));
}

/// Round 2 (§4.2): discarding a chat's changes, or deleting the chat,
/// frees its carry and the folder its large originals spilled to.
#[tokio::test]
async fn discard_and_delete_free_the_carry_and_spill_dir() {
    let big = "a".repeat(verify::CARRY_MEMORY_BYTES + 1);
    let h = harness(&[("package.json", PASSING_TESTS), ("big.txt", big.as_str())]).await;
    if !has_npm() {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "big.txt", "content": "small" }),
        },
        Reply::Text("Done."),
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "big.txt", "content": "small" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let state = h.app.state::<AgentState>();
    let spill_dir = check_output::carry_dir(&h.dialog_id).unwrap();
    let app = h.app.handle().clone();

    h.start(h.request(&url, Harness::send("Shrink"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert!(state.has_carry(&h.dialog_id));
    assert!(spill_dir.read_dir().unwrap().next().is_some(), "spilled");
    let _ = crate::overlay::chats::overlay_discard(app.state(), h.dialog_id.clone()).await;
    assert!(!state.has_carry(&h.dialog_id));
    assert!(!spill_dir.exists(), "{}", spill_dir.display());

    std::fs::write(h.project.join("big.txt"), &big).unwrap();
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Shrink again"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert!(spill_dir.read_dir().unwrap().next().is_some(), "spilled");
    crate::chat_db::commands::chat_delete_dialog(
        app.clone(),
        app.state::<AgentState>(),
        app.state::<crate::vfs::VfsState>(),
        app.state::<DbState>(),
        h.dialog_id.clone(),
    )
    .await
    .unwrap();
    assert!(!state.has_carry(&h.dialog_id));
    assert!(!spill_dir.exists(), "{}", spill_dir.display());
}

/// Round 2 (§4.2): a lead's run that ends in an error, or is stopped,
/// leaves no checks waiting: the older run's state goes.
#[tokio::test]
async fn a_lead_that_errors_or_is_stopped_drops_the_carry() {
    let h = harness(&[("package.json", PASSING_TESTS)]).await;
    if !has_npm() {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
        Reply::Status(401, "{\"error\":{\"message\":\"bad key\"}}"),
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "y.txt", "content": "y" }),
        },
        Reply::Text("Done."),
        Reply::Hang(None),
    ])
    .await;
    let state = h.app.state::<AgentState>();
    h.start(h.request(&url, Harness::send("Write x"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert!(state.has_carry(&h.dialog_id));
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("And?"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "error");
    assert!(!state.has_carry(&h.dialog_id));

    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Write y"), needs_user_approval()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert!(state.has_carry(&h.dialog_id));
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, Harness::send("Wait"), needs_user_approval()))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let app = h.app.handle().clone();
    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    assert!(!state.has_carry(&h.dialog_id));
}

/// Round 2 (§4.2): a waiting record from before the hashes were kept,
/// after a restart: nothing to rebuild from, so the checks pass but the
/// run isn't verified, and the record says integrity wasn't checked.
#[tokio::test]
async fn rebuild_without_baseline_hashes_is_unchecked() {
    let h = harness(&[("package.json", PASSING_TESTS), ("x.txt", "x")]).await;
    if !has_npm() {
        return;
    }
    let ids = store_records(
        &h,
        &[json!({ "status": "needs-approval", "round": 0, "changedFiles": ["x.txt"] })],
    )
    .await;
    let (url, _) = model(vec![Reply::Text("Fine.")]).await;
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: ids[0].clone(),
        },
        needs_user_approval(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verify-unchecked");
    let v = verification_of(&h.messages().await, &ids[0]);
    assert_eq!(v["status"], "passed", "{v}");
    assert_eq!(v["stateRestored"], false, "{v}");
    assert_eq!(v["integrityChecked"], false, "{v}");
}

/// Round 2 (§4.2): checks waiting at round 3 of 4 (a review after green
/// granted a round), after a restart: the round they run is the record's
/// last; failing, it gives up, and says 4 of 4, not 4 of 3.
#[tokio::test]
async fn restart_with_persisted_baseline_and_bonus_round() {
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#,
    )])
    .await;
    if !has_npm() {
        return;
    }
    let ids = store_records(
        &h,
        &[json!({
            "status": "needs-approval",
            "round": 3,
            "maxRounds": 4,
            "changedFiles": ["x.txt"],
            "baselineHashes": [{ "path": "x.txt", "hash": null }],
        })],
    )
    .await;
    std::fs::write(h.project.join("x.txt"), "x").unwrap();
    let (url, _) = model(vec![Reply::Text("Hmm.")]).await;
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: ids[0].clone(),
        },
        needs_user_approval(),
    ))
    .await;
    let outcome = h.run_end().await;
    let v = verification_of(&h.messages().await, &ids[0]);
    assert_eq!(outcome, "verify-gave-up", "{v}");
    assert_eq!(v["round"], 4, "{v}");
    assert_eq!(v["maxRounds"], 4, "{v}");
}

#[tokio::test]
async fn checks_wait_for_approval_then_run_when_started() {
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(0)\"" } }"#,
    )])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Write"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let messages = h.messages().await;
    let record = messages.last().unwrap();
    let waiting = record.meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(waiting["status"], "needs-approval");
    assert_eq!(waiting["checks"][0]["status"], "pending");
    assert_eq!(waiting["changedFiles"], json!(["x.txt"]));

    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: record.id.clone(),
        },
        permissions,
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let records: Vec<_> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .collect();
    assert_eq!(records.len(), 1, "the waiting record became the result");
    assert_eq!(records[0]["status"], "passed");
}

#[tokio::test]
async fn checks_that_keep_failing_give_up_and_a_model_that_stops_fixing_is_unresolved() {
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#,
    )])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let write = |id: &'static str| Reply::Call {
        id,
        name: "write",
        args: json!({ "path": "x.txt", "content": id }),
    };
    let (url, _) = model(vec![
        write("w1"),
        Reply::Text("Done."),
        write("w2"),
        Reply::Text("Done."),
        write("w3"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix"), Permissions::default()))
        .await;
    assert_eq!(h.run_end().await, "verify-gave-up");
    let messages = h.messages().await;
    let statuses: Vec<_> = messages
        .iter()
        .filter_map(|m| {
            m.meta
                .as_ref()?
                .get("verification")
                .map(|v| v["status"].clone())
        })
        .collect();
    assert_eq!(
        statuses,
        [json!("failed"), json!("failed"), json!("gave-up")]
    );

    // Another chat: the model gives up after the first failure without changing anything.
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#,
    )])
    .await;
    let (url, _) = model(vec![
        write("w1"),
        Reply::Text("Done."),
        Reply::Text("It fails anyway."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix"), Permissions::default()))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
}

#[tokio::test]
async fn a_project_without_checks_is_unverified_and_disallowed_checks_are_skipped() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write"), Permissions::default()))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    let record = messages.last().unwrap().meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(record["reason"], "no checks are set up for this project");

    // A check the allowed commands don't cover isn't run.
    let h = harness(&[(
        "package.json",
        r#"{ "name": "t", "scripts": { "lint": "node -e 1" } }"#,
    )])
    .await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        allowed_commands: vec!["ls".into()],
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Write"), permissions))
        .await;
    assert_eq!(h.run_end().await, "verify-unverified");
    let messages = h.messages().await;
    let record = messages.last().unwrap().meta.as_ref().unwrap()["verification"].clone();
    assert_eq!(record["checks"][0]["status"], "skipped");
    assert_eq!(
        record["checks"][0]["reason"],
        "not in your allowed commands"
    );
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
    batches
        .iter()
        .map(|b| b.iter().map(String::as_str).collect())
        .collect()
}

#[tokio::test]
async fn reads_in_one_turn_run_together_and_are_recorded_in_order() {
    let h = harness(&[
        ("a.txt", "alpha\n"),
        ("b.txt", "beta needle\n"),
        ("sub/c.txt", "gamma\n"),
    ])
    .await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("c1", "read", json!({ "path": "a.txt" })),
            ("c2", "search_code", json!({ "query": "needle" })),
            ("c3", "list_dir", json!({ "path": "sub" })),
            ("c4", "read", json!({ "path": "b.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");

    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [["c1", "c2", "c3", "c4"]]
    );
    let messages = h.messages().await;
    let results = tool_results(&messages);
    let order: Vec<&str> = results.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        order,
        ["c1", "c2", "c3", "c4"],
        "stored in the calls' order"
    );
    assert_eq!(results[0].1, "alpha\n");
    assert!(
        results[1].1.starts_with("1 match in 1 file")
            && results[1].1.contains("b.txt (1 match)\n  1: beta needle"),
        "{}",
        results[1].1
    );
    assert!(
        results[2].1.contains("sub/c.txt") || results[2].1.contains("c.txt"),
        "{}",
        results[2].1
    );
    assert_eq!(results[3].1, "beta needle\n");
    assert!(messages.iter().all(|m| m.finalized));

    // The next request answers the calls in the order they were made.
    let sent = seen.lock().unwrap()[1]["messages"]
        .as_array()
        .unwrap()
        .clone();
    let sent_ids: Vec<&str> = sent
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(sent_ids, ["c1", "c2", "c3", "c4"]);
}

#[tokio::test]
async fn a_write_splits_the_reads_around_it_and_later_reads_see_it() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("r1", "read", json!({ "path": "a.txt" })),
            ("r2", "list_dir", json!({})),
            (
                "w1",
                "write",
                json!({ "path": "a.txt", "content": "new\n" }),
            ),
            ("r3", "read", json!({ "path": "a.txt" })),
            ("r4", "search_code", json!({ "query": "new" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Change it"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");

    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [vec!["r1", "r2"], vec!["r3", "r4"]]
    );
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "r1").content,
        "old\n",
        "read before the write"
    );
    assert_eq!(
        result_of(&messages, "r3").content,
        "new\n",
        "read after the write sees it"
    );
    assert!(result_of(&messages, "r4").content.starts_with("1 match"));
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["r1", "r2", "w1", "r3", "r4"]);
}

#[tokio::test]
async fn reads_that_need_approval_are_asked_one_at_a_time() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read", json!({ "path": "a.txt" })),
            ("c2", "read", json!({ "path": "a.txt", "start_line": 1 })),
            ("c3", "list_dir", json!({})),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), ask_approval("tools")))
        .await;
    let app = h.app.handle().clone();
    for id in ["c1", "c2", "c3"] {
        h.wait_for(id, |e| {
            e["type"] == "approvalRequested" && e["request"]["toolCallId"] == id
        })
        .await;
        // Only this one is waiting: the next isn't asked before it's decided.
        let asked: Vec<Value> = h.events_of("approvalRequested");
        assert_eq!(asked.last().unwrap()["request"]["toolCallId"], id);
        agent_resolve_approval(
            app.clone(),
            app.state(),
            h.dialog_id.clone(),
            id.into(),
            ApprovalDecision::Allow,
        );
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
            ("c1", "read", json!({ "path": "a.txt" })),
            ("c2", "read", json!({ "path": "../outside.txt" })),
            ("c3", "read", json!({ "path": "a.txt" })),
            ("c4", "list_dir", json!({})),
            (
                "c5",
                "todo",
                json!({ "items": [{ "title": "one", "status": "done" }] }),
            ),
            ("c6", "list_dir", json!({})),
            ("c7", "read", json!({ "path": "a.txt", "start_line": 1 })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    // The todo runs on its own between two batches. A read outside the
    // project is still a read: it runs with the others and gets its error.
    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [vec!["c1", "c2", "c3", "c4"], vec!["c6", "c7"]]
    );
    let messages = h.messages().await;
    assert!(
        result_of(&messages, "c2")
            .content
            .starts_with("Error: path must not contain"),
        "{}",
        result_of(&messages, "c2").content
    );
    assert!(
        result_of(&messages, "c5").content.contains("1 of 1 done"),
        "{}",
        result_of(&messages, "c5").content
    );
    assert_eq!(result_of(&messages, "c7").content, "x");
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["c1", "c2", "c3", "c4", "c5", "c6", "c7"]);
}

#[tokio::test]
async fn a_refused_command_between_reads_splits_them() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("r1", "read", json!({ "path": "a.txt" })),
            ("r2", "list_dir", json!({})),
            ("x1", "bash", json!({ "command": "rm -rf /" })),
            ("r3", "read", json!({ "path": "a.txt" })),
            ("r4", "read", json!({ "path": "a.txt", "start_line": 1 })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [vec!["r1", "r2"], vec!["r3", "r4"]]
    );
    let messages = h.messages().await;
    assert!(result_of(&messages, "x1")
        .content
        .starts_with("Error: blocked by the user's permission settings"));
    let order: Vec<&str> = tool_results(&messages).iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["r1", "r2", "x1", "r3", "r4"]);
}

#[tokio::test]
async fn bad_calls_in_a_batch_get_their_errors_and_the_rest_runs() {
    let h = harness(&[("a.txt", "fine\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "read", json!({})),
            ("c2", "read", json!({ "path": "missing.txt" })),
            (
                "c3",
                "search_code",
                json!({ "query": "(unclosed", "regex": true }),
            ),
            (
                "c4",
                "read",
                json!({ "path": "a.txt", "start_line": 5, "end_line": 1 }),
            ),
            ("c5", "read", json!({ "path": "a.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [["c1", "c2", "c3", "c4", "c5"]]
    );
    let messages = h.messages().await;
    for id in ["c1", "c2", "c3", "c4"] {
        assert!(
            result_of(&messages, id).content.starts_with("Error:"),
            "{id}: {}",
            result_of(&messages, id).content
        );
    }
    assert_eq!(result_of(&messages, "c5").content, "fine\n");
}

#[tokio::test]
async fn more_reads_than_run_at_once_all_come_back_in_order() {
    let files: Vec<(String, String)> = (0..25)
        .map(|i| (format!("f{i:02}.txt"), format!("content {i}")))
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let h = harness(&refs).await;
    let ids_owned: Vec<String> = (0..25).map(|i| format!("c{i:02}")).collect();
    let paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let calls: Vec<(&'static str, &'static str, Value)> = ids_owned
        .iter()
        .zip(&paths)
        .map(|(id, path)| {
            (
                &*Box::leak(id.clone().into_boxed_str()),
                "read",
                json!({ "path": path }),
            )
        })
        .collect();
    let (url, _) = model(vec![Reply::Calls(calls), Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Read all"), quiet_permissions()))
        .await;
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
            ("c1", "read", json!({ "path": "a.txt" })),
            ("c2", "read", json!({ "path": "a.txt" })),
            ("c3", "read", json!({ "path": "a.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [["c1", "c2", "c3"]]
    );
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "c1").content, "same\n");
    assert_eq!(result_of(&messages, "c2").content, "same\n");
    // The third identical call is the one the guard notes, as when they ran one by one.
    assert!(
        result_of(&messages, "c3")
            .content
            .contains("was called 3 times"),
        "{}",
        result_of(&messages, "c3").content
    );
}

#[tokio::test]
async fn stop_before_a_batch_records_every_call_as_stopped() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![Reply::Calls(vec![
        (
            "e1",
            "write",
            json!({ "path": "a.txt", "old_text": "one", "content": "ONE" }),
        ),
        ("r1", "read", json!({ "path": "a.txt" })),
        ("r2", "list_dir", json!({})),
    ])])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits")))
        .await;
    h.wait_for("approval e1", |e| {
        e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e1"
    })
    .await;
    let app = h.app.handle().clone();
    agent_stop(app.clone(), app.state(), h.dialog_id.clone());
    assert_eq!(h.run_end().await, "stopped");
    assert!(
        parallel::batches_of(&h.dialog_id).is_empty(),
        "nothing runs after Stop"
    );
    let messages = h.messages().await;
    for id in ["e1", "r1", "r2"] {
        assert_eq!(
            result_of(&messages, id).content,
            "Error: stopped by the user before this ran."
        );
    }
    assert_eq!(h.file("a.txt").as_deref(), Some("one\n"));
}

#[tokio::test]
async fn reads_after_an_approved_edit_run_together_and_see_the_edit() {
    let h = harness(&[("a.txt", "one\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            (
                "e1",
                "write",
                json!({ "path": "a.txt", "old_text": "one", "content": "ONE" }),
            ),
            ("r1", "read", json!({ "path": "a.txt" })),
            (
                "r2",
                "search_code",
                json!({ "query": "ONE", "case_sensitive": true }),
            ),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Edit"), ask_approval("edits")))
        .await;
    h.wait_for("approval e1", |e| {
        e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "e1"
    })
    .await;
    let app = h.app.handle().clone();
    agent_resolve_approval(
        app.clone(),
        app.state(),
        h.dialog_id.clone(),
        "e1".into(),
        ApprovalDecision::Allow,
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(ids(&parallel::batches_of(&h.dialog_id)), [["r1", "r2"]]);
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "r1").content, "ONE\n");
    assert!(result_of(&messages, "r2").content.starts_with("1 match"));
}

#[tokio::test]
async fn a_single_read_runs_as_before() {
    let h = harness(&[("a.txt", "x")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(parallel::batches_of(&h.dialog_id).is_empty());
    assert_eq!(result_of(&h.messages().await, "c1").content, "x");
}

/*
 * The repository map
 */

fn system_prompt(request: &Value) -> String {
    request["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The text of a request's last message: where what changes between steps
/// (the repository map) is sent, after the cacheable prefix.
fn tail_text(request: &Value) -> String {
    let last = request["messages"].as_array().and_then(|m| m.last());
    match last.map(|m| &m["content"]) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// How many leading messages two requests share, byte for byte: what a
/// provider's prompt cache can serve the second one.
fn shared_messages(a: &Value, b: &Value) -> usize {
    match (a["messages"].as_array(), b["messages"].as_array()) {
        (Some(a), Some(b)) => a.iter().zip(b).take_while(|(x, y)| x == y).count(),
        _ => 0,
    }
}

#[tokio::test]
async fn the_request_ends_with_a_map_centred_on_the_request() {
    let h = harness(&[
        (
            "src/billing.rs",
            "pub fn compute_invoice_total() -> u32 { 0 }\n",
        ),
        (
            "src/report.rs",
            "pub fn build_report() { compute_invoice_total(); }\n",
        ),
        ("src/unrelated.rs", "pub fn something_else_entirely() {}\n"),
        ("notes.txt", "not code"),
    ])
    .await;
    let (url, seen) = model(vec![Reply::Text("Ok."), Reply::Text("Ok again.")]).await;
    h.start(h.request(
        &url,
        Harness::send("Why is src/report.rs slow?"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let request = seen.lock().unwrap()[0].clone();
    // The map changes mid-run, so it isn't in the system prompt (which
    // providers cache): it ends the request, after the user's message.
    assert!(!system_prompt(&request).contains("## Repository map"));
    let tail = tail_text(&request);
    assert!(tail.starts_with("Why is src/report.rs slow?"), "{tail}");
    let map = &tail[tail
        .find("## Repository map")
        .expect("a map at the end of the request")..];
    let report = map
        .find("src/report.rs:")
        .expect("the file the request names");
    let billing = map.find("src/billing.rs:").expect("what it uses");
    assert!(report < billing, "{map}");
    assert!(
        map.contains("pub fn compute_invoice_total() -> u32"),
        "{map}"
    );
    assert!(!map.contains("notes.txt"));
}

#[tokio::test]
async fn the_map_refreshes_mid_run_after_a_write_without_moving_the_prefix() {
    let h = harness(&[("src/a.rs", "pub fn alpha_one() {}\n")]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "write",
            args: json!({ "path": "src/b.rs", "content": "pub fn beta_two_fresh() {}\n" }),
        },
        Reply::Text("Added it."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Add beta"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    // The first turn's map can't know the new file; the second turn's must.
    let first = tail_text(&requests[0]);
    assert!(first.contains("## Repository map"), "{first}");
    assert!(!first.contains("beta_two_fresh"), "{first}");
    let second = tail_text(&requests[1]);
    let map = &second[second
        .find("## Repository map")
        .expect("a refreshed map in the second turn")..];
    assert!(map.contains("beta_two_fresh"), "{map}");

    // The two-zone shape: the refreshed map changed nothing before the
    // last message. The system prompt, the tools and every message but the
    // previous request's last one are byte-identical, so a provider's
    // prompt cache serves them.
    assert_eq!(system_prompt(&requests[0]), system_prompt(&requests[1]));
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
    let n = requests[0]["messages"].as_array().unwrap().len();
    assert_eq!(shared_messages(&requests[0], &requests[1]), n - 1);
    // The user's request is sent the same way on every step: only the map
    // that rode on it moved to the new last message.
    let user = &requests[1]["messages"][n - 1];
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"], "Add beta");
}

#[tokio::test]
async fn a_project_without_code_has_no_map() {
    let h = harness(&[("a.txt", "hello"), ("data.json", "{\"a\": 1}")]).await;
    let (url, seen) = model(vec![Reply::Text("Ok.")]).await;
    h.start(h.request(&url, Harness::send("Hi"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let request = seen.lock().unwrap()[0].clone();
    assert!(!system_prompt(&request).contains("## Repository map"));
    assert_eq!(tail_text(&request), "Hi");
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
            (
                "s1",
                "read",
                json!({ "path": "src/parser.rs", "symbol": "Parser::parse" }),
            ),
            (
                "s2",
                "read",
                json!({ "path": "src/parser.rs", "symbol": "missing_fn" }),
            ),
            (
                "s3",
                "read",
                json!({ "path": "src/parser.rs", "symbol": "parse", "start_line": 1 }),
            ),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Show parse"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let s1 = result_of(&messages, "s1");
    assert_eq!(
        s1.content,
        "[Symbol read: Parser > parse, lines 6-9 of src/parser.rs]\n    /// Parses it.\n    pub fn parse(&self) -> u8 {\n        1\n    }"
    );
    assert_eq!(s1.meta.as_ref().unwrap()["range"], json!([6, 9]));
    assert!(result_of(&messages, "s2")
        .content
        .starts_with("Error: No declaration \"missing_fn\" in src/parser.rs."));
    assert_eq!(
        result_of(&messages, "s3").content,
        "Error: give either \"symbol\" or start_line/end_line, not both"
    );
}

/*
 * Search results by declaration
 */

#[tokio::test]
async fn search_results_name_the_declaration_each_match_is_in() {
    let src = "pub struct Parser;\n\nimpl Parser {\n    pub fn parse(&self) {\n        build_tree();\n    }\n}\n\nfn build_tree() {}\n";
    let h = harness(&[("src/parser.rs", src), ("notes.txt", "build_tree notes\n")]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "s1",
            name: "search_code",
            args: json!({ "query": "build_tree" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Where is build_tree used?"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "s1").content,
        "3 matches in 2 files
notes.txt (1 match)
  1: build_tree notes
src/parser.rs (2 matches)
  Parser > parse [4-6]:
    5: build_tree();
  build_tree [9]:
    9: fn build_tree() {}   (declaration)"
    );

    // What the model was told about it, in the first request.
    let request = seen.lock().unwrap()[0].clone();
    let search = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "search_code")
        .unwrap();
    assert!(search["function"]["description"]
        .as_str()
        .unwrap()
        .contains("works as read's symbol"));
    let prompt = request["messages"][0]["content"].as_str().unwrap();
    assert!(
        prompt.contains("results are grouped by the declaration each match is in"),
        "{prompt}"
    );
    assert!(prompt.contains("call read with the group's name as `symbol`"));
}

#[tokio::test]
async fn the_code_tools_run_together_and_word_their_errors_once() {
    let h = harness(&[
        (
            "src/tax.rs",
            "pub fn compute_tax_total(x: f64) -> f64 { x }\n",
        ),
        (
            "src/invoice.rs",
            "pub fn invoice_sum() -> f64 { compute_tax_total(1.0) }\n",
        ),
    ])
    .await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("g1", "neighbours", json!({ "path": "src/invoice.rs" })),
            ("g2", "callers", json!({ "symbol": "compute_tax_total" })),
            ("g3", "outline", json!({ "path": "src/tax.rs" })),
            ("g4", "diagnostics", json!({ "path": "src/tax.rs" })),
            ("g5", "callers", json!({ "symbol": "no_such_name_xyz" })),
            ("g6", "neighbours", json!({})),
        ]),
        Reply::Text("Looked."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look around"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    // Local reads: one parallel batch.
    assert_eq!(
        ids(&parallel::batches_of(&h.dialog_id)),
        [vec!["g1", "g2", "g3", "g4", "g5", "g6"]]
    );
    let n = &result_of(&messages, "g1").content;
    assert!(
        n.contains("## Neighbours of src/invoice.rs") && n.contains("src/tax.rs"),
        "{n}"
    );
    let c = &result_of(&messages, "g2").content;
    assert!(c.contains("src/invoice.rs"), "{c}");
    assert!(
        result_of(&messages, "g3")
            .content
            .contains("compute_tax_total"),
        "{}",
        result_of(&messages, "g3").content
    );
    assert!(
        result_of(&messages, "g4").content.contains("parses clean"),
        "{}",
        result_of(&messages, "g4").content
    );
    // A tool's own error text, and a missing argument: "Error:" once.
    let unknown = &result_of(&messages, "g5").content;
    assert!(
        unknown.starts_with("Error: no declaration named"),
        "{unknown}"
    );
    assert!(!unknown.contains("Error: Error:"), "{unknown}");
    assert_eq!(
        result_of(&messages, "g6").content,
        "Error: \"path\" is required."
    );
}

#[tokio::test]
async fn two_chats_waiting_on_the_same_call_id_are_answered_apart() {
    // Gemini numbers calls per reply: two chats both get "function-call-1".
    let h = harness(&[]).await;
    let app = h.app.handle().clone();
    let state = app.state::<AgentState>().inner().clone();
    let request = |dialog: &str| ApprovalRequest {
        tool_call_id: "function-call-1".into(),
        dialog_id: dialog.into(),
        tool_name: "bash".into(),
        summary: "Run `ls`".into(),
        command: Some("ls".into()),
        note: None,
        edit: None,
        parent_dialog_id: None,
        parent_call_id: None,
    };
    let wait = |dialog: &str| {
        let (state, app, request) = (state.clone(), app.clone(), request(dialog));
        tokio::spawn(async move {
            state
                .wait_for_approval(&app, request, &StopSignal::new())
                .await
        })
    };
    let a = wait("chat-a");
    let b = wait("chat-b");
    while state.lock().approvals.len() < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(agent_resolve_approval(
        app.clone(),
        app.state(),
        "chat-a".into(),
        "function-call-1".into(),
        ApprovalDecision::Allow
    ));
    assert!(matches!(a.await.unwrap(), ApprovalDecision::Allow));
    assert!(!b.is_finished(), "the other chat still waits");
    assert!(agent_resolve_approval(
        app.clone(),
        app.state(),
        "chat-b".into(),
        "function-call-1".into(),
        ApprovalDecision::Deny
    ));
    assert!(matches!(b.await.unwrap(), ApprovalDecision::Deny));
}

//! Test support, for this crate's tests and other crates' (the `testing`
//! feature): a local server that plays the model, folders that go when a
//! test is done, and a `Core` on one of them that keeps its events.

use crate::agent::OpenAIChatOptions;
use crate::agent_run::events::AgentEvent;
use crate::core::{Core, EventSink, Paths};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/*
 * The model
 */

/// One streamed reply.
pub enum Reply {
    Text(&'static str),
    Call {
        id: &'static str,
        name: &'static str,
        args: Value,
    },
    /// A call the output limit cut off: its arguments as far as they got,
    /// then `finish_reason: "length"`.
    CutOff {
        id: &'static str,
        name: &'static str,
        args: &'static str,
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
    /// Text a piece at a time, this many milliseconds apart: a reply that
    /// streams for a while.
    Trickle(Vec<&'static str>, u64),
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
                Reply::call_sse(id, name, &arguments, "tool_calls")
            }
            Reply::CutOff { id, name, args } => Reply::call_sse(id, name, args, "length"),
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
            Reply::Trickle(pieces, _) => {
                let mut out = String::new();
                for piece in pieces {
                    out += &chunk(json!({ "choices": [{ "delta": { "content": piece }, "finish_reason": null }] }));
                }
                out + &Reply::Text("").sse()
            }
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

    /// What the server writes, each piece after its wait: a trickle's text
    /// a piece at a time, any other reply at once.
    fn pieces(&self) -> Vec<(Duration, String)> {
        let Reply::Trickle(pieces, ms) = self else {
            return vec![(self.delay(), self.response())];
        };
        let chunk = |v: Value| format!("data: {v}\n\n");
        let mut out = vec![(
            Duration::ZERO,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                .to_string(),
        )];
        for piece in pieces {
            let delta =
                json!({ "choices": [{ "delta": { "content": piece }, "finish_reason": null }] });
            out.push((Duration::from_millis(*ms), chunk(delta)));
        }
        out.push((Duration::ZERO, Reply::Text("").sse()));
        out
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

    /// One call with these arguments (raw text), ended by `finish_reason`.
    fn call_sse(id: &str, name: &str, arguments: &str, finish_reason: &str) -> String {
        let chunk = |v: Value| format!("data: {v}\n\n");
        let delta = json!({ "tool_calls": [{
            "index": 0, "id": id, "type": "function",
            "function": { "name": name, "arguments": arguments }
        }]});
        chunk(json!({ "choices": [{ "delta": delta, "finish_reason": null }] }))
            + &chunk(json!({ "choices": [{ "delta": {}, "finish_reason": finish_reason }] }))
            + "data: [DONE]\n\n"
    }
}

/// What a provider without the Responses API answers on `/responses`,
/// where a model is asked first (`chat_request::responses`).
pub const NO_RESPONSES_API: &str =
    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// A local server answering each chat completions request with the next
/// scripted reply (the last one again once they run out). It has no
/// Responses API (`NO_RESPONSES_API`). Returns its base URL and the chat
/// completions request bodies it saw.
pub async fn model(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Value>>>) {
    model_routed(Vec::new(), replies).await
}

/// The script a request is answered from: the first route whose marker is
/// in the request's first message after the system prompt (a subagent's
/// brief, or the user's request), else the default script (K05: parallel
/// runs each get their own replies, in their own order). A marker that
/// starts with `last:` is looked for in the request's last message instead
/// (a turn the IDE asks for at the end of a chat, as a task's report).
pub fn route_of(body: &Value, markers: &[&str]) -> Option<usize> {
    let messages = body["messages"].as_array()?;
    let text_of = |m: &Value| match &m["content"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let first = text_of(messages.iter().find(|m| m["role"] != "system")?);
    let last = messages.last().map(text_of).unwrap_or_default();
    markers.iter().position(|m| match m.strip_prefix("last:") {
        Some(marker) => last.contains(marker),
        None => first.contains(m),
    })
}

/// `model`, with one script per run: `routes` pairs a marker (found in the
/// run's first message) with that run's replies; anything else is answered
/// from `default`. Each script is answered in order, the last reply again
/// once it runs out.
pub async fn model_routed(
    routes: Vec<(&'static str, Vec<Reply>)>,
    default: Vec<Reply>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let script = |replies: &[Reply]| -> Vec<(Vec<(Duration, String)>, bool)> {
        replies.iter().map(|r| (r.pieces(), r.hangs())).collect()
    };
    let markers: Vec<&'static str> = routes.iter().map(|(m, _)| *m).collect();
    // Each route's replies, each in its pieces, and whether it hangs.
    type Script = Vec<(Vec<(Duration, String)>, bool)>;
    let mut scripts: Vec<Script> = routes.iter().map(|(_, r)| script(r)).collect();
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
                let (path, body) = read_request(&mut socket).await;
                if path.ends_with("/responses") {
                    let _ = socket.write_all(NO_RESPONSES_API.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }
                let which = route_of(&body, &markers).unwrap_or(scripts.len() - 1);
                log.lock().unwrap().push(body);
                let (pieces, hangs) = {
                    let mut next = next.lock().unwrap();
                    let bodies = &scripts[which];
                    let n = next[which];
                    next[which] += 1;
                    bodies[n.min(bodies.len() - 1)].clone()
                };
                for (wait, piece) in pieces {
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                    if socket.write_all(piece.as_bytes()).await.is_err() {
                        return;
                    }
                }
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

/// A System One endpoint (`system_one.rs`, the typed AI reviewer) that
/// answers every question it's asked with the probability `noul`: 0.5
/// leaves the reviewer unsure (review.rs `decide`). The requests it got
/// are kept.
pub async fn system_one(noul: f64) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let (_, body) = read_request(&mut socket).await;
                let answers: serde_json::Map<String, Value> = body["questions"]
                    .as_object()
                    .map(|asked| {
                        asked
                            .keys()
                            .map(|key| (key.clone(), json!({ "noul": noul })))
                            .collect()
                    })
                    .unwrap_or_default();
                log.lock().unwrap().push(body);
                let text = json!({ "answers": answers }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (url, seen)
}

/// A request's path and JSON body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let (head, body) = read_raw(socket).await;
    (path_of(&head), body)
}

/// A request's head (its first line and its headers) and JSON body (null
/// without one).
async fn read_raw(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    // Where the body starts and how long it is, once the headers are in:
    // read once, so a request of megabytes isn't searched again per chunk.
    let mut body: Option<(usize, usize)> = None;
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if body.is_none() {
            body = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|end| {
                let length = String::from_utf8_lossy(&buf[..end])
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().to_string())
                    })
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                (end + 4, length)
            });
        }
        if let Some((start, length)) = body {
            if buf.len() >= start + length {
                let head = String::from_utf8_lossy(&buf[..start]).into_owned();
                let body = serde_json::from_slice(&buf[start..start + length]).unwrap_or_default();
                return (head, body);
            }
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), Value::Null)
}

/// The path on a request's first line ("POST /v1/chat/completions HTTP/1.1").
fn path_of(head: &str) -> String {
    let line = head.lines().next().unwrap_or_default();
    line.split_whitespace().nth(1).unwrap_or("").to_string()
}

/*
 * Folders and cores
 */

/// A folder of the test's own, removed when it goes.
pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A new, empty folder for the test, in the system's temporary folder.
pub fn temp_dir() -> TempDir {
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

/*
 * A test in a process of its own
 */

/// Runs `test` (its full name), an ignored test of this test binary, in a
/// process of its own, and checks it passed: for a test that changes what
/// the process keeps (the allowed hosts' list), which the tests running
/// beside it in this process mustn't see.
pub fn in_a_process_of_its_own(test: &str) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--ignored", "--test-threads=1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 passed"), "{stdout}{stderr}");
}

/*
 * A command that runs until it's stopped
 */

/// A POSIX shell command that prints `first`, then leaves a loop running in
/// the background of its shell that adds a line to `beat` in its folder
/// every 50 ms until it's killed: a server, say, whose life shows from
/// outside any sandbox (a pid in bubblewrap's namespace isn't the test's).
pub fn beating(first: &str) -> String {
    format!("echo {first}; while :; do echo . >> beat; sleep 0.05; done & wait")
}

/// Whether the loop of [`beating`] in `dir` still runs: its file grows
/// within 400 ms, which this waits out.
pub fn beats(dir: &std::path::Path) -> bool {
    let size = || {
        std::fs::metadata(dir.join("beat"))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let before = size();
    std::thread::sleep(Duration::from_millis(400));
    size() > before
}

/// Keeps every event, as the JSON the frontend gets.
pub struct Recorder(pub Arc<Mutex<Vec<Value>>>);

impl EventSink for Recorder {
    fn agent(&self, event: &AgentEvent) {
        let json = serde_json::to_value(event).unwrap();
        self.0.lock().unwrap().push(json);
    }
}

/// A core on a temporary folder (its `data` and `config`), as one test
/// process holds many (`Core::open`), with the events it sends.
pub struct TestCore {
    pub core: Core,
    pub events: Arc<Mutex<Vec<Value>>>,
    /// Goes after the core, with its files.
    pub dir: TempDir,
}

impl TestCore {
    pub fn new() -> TestCore {
        let dir = temp_dir();
        let events = Arc::new(Mutex::new(Vec::new()));
        let paths = Paths::new(dir.0.join("data"), dir.0.join("config"));
        let core = Core::open(paths, Arc::new(Recorder(events.clone()))).unwrap();
        TestCore { core, events, dir }
    }
}

impl Default for TestCore {
    fn default() -> Self {
        Self::new()
    }
}

/// A reasoning effort as the frontend sends it (src/lib/reasoning.ts).
pub fn effort(level: &str) -> Option<OpenAIChatOptions> {
    serde_json::from_value(
        json!({ "reasoning_effort": level, "reasoning_formats": ["reasoning_effort"] }),
    )
    .unwrap()
}

/*
 * An MCP server on the old HTTP+SSE transport
 */

/// A request the [`SseServer`] got: its method, path and `Authorization`
/// header, and its JSON body (null for a GET).
#[derive(Clone, Debug)]
pub struct SseRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Value,
}

/// What an [`SseServer`]'s connections share.
#[derive(Default)]
struct SseShared {
    seen: Mutex<Vec<SseRequest>>,
    /// Each open stream's messages to write, by its session.
    streams: Mutex<std::collections::HashMap<u64, tokio::sync::mpsc::UnboundedSender<String>>>,
    next: std::sync::atomic::AtomicU64,
    /// Its own requests waiting for the client's answers, by id.
    asked: Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// The endpoint its streams name, when not their own.
    endpoint: Option<String>,
}

/// An MCP server on the old HTTP+SSE transport (2024-11-05), on 127.0.0.1
/// (mcp-plan-2.md, Q13). A GET of `/sse` opens an event stream whose first
/// event names `/messages?session=<n>` to post to, and what's posted there
/// is answered on that stream: `initialize` with version 2024-11-05, then
/// its tools. `echo` answers `echo: <text>` once the client has answered a
/// `ping` of the server's own; `slow` never answers. A POST to `/sse`, as a
/// Streamable HTTP client makes first, is refused 405.
pub struct SseServer {
    /// The stream's URL: `http://127.0.0.1:<port>/sse`.
    pub url: String,
    shared: Arc<SseShared>,
}

impl SseServer {
    /// The server, its streams naming `endpoint` to post to when it's given
    /// (another origin's), else their own.
    pub async fn start(endpoint: Option<&str>) -> SseServer {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        let shared = Arc::new(SseShared {
            endpoint: endpoint.map(str::to_string),
            ..SseShared::default()
        });
        let served = shared.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(sse_connection(socket, served.clone()));
            }
        });
        SseServer { url, shared }
    }

    /// The requests it got, in order.
    pub fn seen(&self) -> Vec<SseRequest> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// Ends its open streams, as a server that went away.
    pub fn end_streams(&self) {
        self.shared.streams.lock().unwrap().clear();
    }
}

/// One connection to the [`SseServer`]: a stream, a message or a refusal.
async fn sse_connection(mut socket: tokio::net::TcpStream, shared: Arc<SseShared>) {
    let (head, body) = read_raw(&mut socket).await;
    let first = head.lines().next().unwrap_or_default().to_string();
    let method = first.split_whitespace().next().unwrap_or("").to_string();
    let path = path_of(&head);
    let authorization = (head.lines())
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim().to_string());
    shared.seen.lock().unwrap().push(SseRequest {
        method: method.clone(),
        path: path.clone(),
        authorization,
        body: body.clone(),
    });
    let reply = |status: &str| {
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    };
    let (route, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
    match (method.as_str(), route) {
        ("GET", "/sse") => {
            let n = shared
                .next
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (to_stream, mut messages) = tokio::sync::mpsc::unbounded_channel();
            shared.streams.lock().unwrap().insert(n, to_stream);
            let endpoint = (shared.endpoint.clone()).unwrap_or(format!("/messages?session={n}"));
            let opened = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\nevent: endpoint\ndata: {endpoint}\n\n"
            );
            if socket.write_all(opened.as_bytes()).await.is_err() {
                return;
            }
            while let Some(message) = messages.recv().await {
                let event = format!("event: message\ndata: {message}\n\n");
                if socket.write_all(event.as_bytes()).await.is_err() {
                    return;
                }
            }
        }
        ("POST", "/sse") => {
            let _ = socket
                .write_all(reply("405 Method Not Allowed").as_bytes())
                .await;
        }
        ("POST", "/messages") => {
            let session = query.strip_prefix("session=").and_then(|n| n.parse().ok());
            let stream = session.and_then(|n| shared.streams.lock().unwrap().get(&n).cloned());
            let Some(stream) = stream else {
                let _ = socket.write_all(reply("404 Not Found").as_bytes()).await;
                return;
            };
            let _ = socket.write_all(reply("202 Accepted").as_bytes()).await;
            tokio::spawn(sse_answer(body, stream, shared));
        }
        _ => {
            let _ = socket.write_all(reply("404 Not Found").as_bytes()).await;
        }
    }
}

/// The [`SseServer`]'s answer to `message`, on its session's stream.
async fn sse_answer(
    message: Value,
    stream: tokio::sync::mpsc::UnboundedSender<String>,
    shared: Arc<SseShared>,
) {
    let send = |message: Value| {
        let _ = stream.send(message.to_string());
    };
    let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
        return;
    };
    let result = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
    match message["method"].as_str() {
        // The client's answer to the server's own request.
        None => {
            if let Some(answered) = shared.asked.lock().unwrap().remove(&id.to_string()) {
                let _ = answered.send(());
            }
        }
        Some("initialize") => send(result(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "old-sse", "version": "0.9" },
            "instructions": "Old tools.",
        }))),
        Some("tools/list") => send(result(json!({ "tools": [
            { "name": "echo", "description": "Echo the text.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } },
            { "name": "slow", "description": "Never answers.", "inputSchema": { "type": "object" } },
        ] }))),
        Some("tools/call") if message["params"]["name"] == "slow" => {}
        Some("tools/call") => {
            let ping = json!(format!("server-ping-{id}"));
            let (answered, answer) = tokio::sync::oneshot::channel();
            shared
                .asked
                .lock()
                .unwrap()
                .insert(ping.to_string(), answered);
            send(json!({ "jsonrpc": "2.0", "id": ping, "method": "ping" }));
            if tokio::time::timeout(Duration::from_secs(5), answer)
                .await
                .is_err()
            {
                return send(
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "no answer to ping" } }),
                );
            }
            let text = message["params"]["arguments"]["text"]
                .as_str()
                .unwrap_or("");
            send(result(
                json!({ "content": [{ "type": "text", "text": format!("echo: {text}") }] }),
            ));
        }
        Some(other) => send(
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no {other}") } }),
        ),
    }
}

/*
 * An authorization server, and an MCP server that needs its sign-in
 */

/// How an [`OAuthServer`] behaves.
#[derive(Clone, Debug)]
pub struct OAuthOptions {
    /// It takes Client ID Metadata Documents.
    pub metadata_documents: bool,
    /// It registers clients (Dynamic Client Registration).
    pub registration: bool,
    /// The client registered with it by hand, and its secret: a token
    /// request must prove it (HTTP Basic).
    pub client: Option<(String, String)>,
    /// The `iss` its sign-in answers with: its own, unless this says another.
    pub iss: Option<String>,
    /// How long its access tokens last, in seconds.
    pub expires_in: u64,
    /// A scope a call needs beyond the sign-in's: a 403 says so.
    pub call_needs: Option<String>,
}

impl Default for OAuthOptions {
    fn default() -> Self {
        OAuthOptions {
            metadata_documents: false,
            registration: true,
            client: None,
            iss: None,
            expires_in: 3600,
            call_needs: None,
        }
    }
}

/// A request the [`OAuthServer`] got: its method, path (with its query),
/// `Authorization` header and body as text.
#[derive(Clone, Debug)]
pub struct OAuthRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

/// The sign-in an [`OAuthServer`] sent the browser on, which its code is
/// exchanged against.
struct OAuthAsked {
    code: String,
    /// The PKCE challenge.
    challenge: String,
    redirect: String,
    client: String,
    scope: String,
}

/// What an [`OAuthServer`]'s connections share.
#[derive(Default)]
struct OAuthShared {
    options: OAuthOptions,
    base: String,
    seen: Mutex<Vec<OAuthRequest>>,
    /// The sign-in the browser was sent on.
    asked: Mutex<Option<OAuthAsked>>,
    /// The access tokens it takes, with their scopes; the refresh tokens
    /// it takes.
    access: Mutex<std::collections::HashMap<String, String>>,
    refresh: Mutex<std::collections::HashMap<String, String>>,
    next: std::sync::atomic::AtomicU64,
}

/// An authorization server on 127.0.0.1 (tests, mcp-plan-2.md Q14), and on
/// the same origin a legacy MCP server at `/mcp` that takes only its
/// tokens: one without answers 401, naming its protected resource
/// metadata and the scope `mcp:read`. It publishes RFC 9728's and RFC
/// 8414's metadata, registers clients, plays the user's browser (`browse`:
/// the sign-in is granted at once), and gives and refreshes tokens, PKCE
/// (S256) checked and refresh tokens rotated. Its MCP tool `whoami` says
/// which token it got.
pub struct OAuthServer {
    pub base: String,
    /// The MCP server: `<base>/mcp`.
    pub mcp_url: String,
    /// The authorization server: `<base>/auth`.
    pub issuer: String,
    shared: Arc<OAuthShared>,
}

impl OAuthServer {
    pub async fn start(options: OAuthOptions) -> OAuthServer {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(OAuthShared {
            options,
            base: base.clone(),
            ..OAuthShared::default()
        });
        let served = shared.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(oauth_connection(socket, served.clone()));
            }
        });
        OAuthServer {
            mcp_url: format!("{base}/mcp"),
            issuer: format!("{base}/auth"),
            base,
            shared,
        }
    }

    /// The requests it got, in order.
    pub fn seen(&self) -> Vec<OAuthRequest> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// The user's browser on the sign-in page `url`: the sign-in is
    /// granted, and the browser sent back to its redirect with a code, its
    /// state and the issuer. What the page was asked: its query.
    pub fn browse(&self, url: &str) -> Result<Vec<(String, String)>, String> {
        let page = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        let query: Vec<(String, String)> = page.query_pairs().into_owned().collect();
        let get = |key: &str| {
            (query.iter())
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let n = self
            .shared
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let code = format!("code-{n}");
        *self.shared.asked.lock().unwrap() = Some(OAuthAsked {
            code: code.clone(),
            challenge: get("code_challenge"),
            redirect: get("redirect_uri"),
            client: get("client_id"),
            scope: get("scope"),
        });
        let mut back = reqwest::Url::parse(&get("redirect_uri")).map_err(|e| e.to_string())?;
        let iss = (self.shared.options.iss.clone()).unwrap_or(format!("{}/auth", self.shared.base));
        back.query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &get("state"))
            .append_pair("iss", &iss);
        tokio::spawn(async move {
            let _ = crate::net::client().await.get(back).send().await;
        });
        Ok(query)
    }

    /// Takes none of the access tokens it gave any more, as when they
    /// expire early or are revoked; their refresh tokens still go.
    pub fn refuse_access(&self) {
        self.shared.access.lock().unwrap().clear();
    }

    /// Takes none of its refresh tokens any more: a sign-in revoked.
    pub fn refuse_refresh(&self) {
        self.shared.refresh.lock().unwrap().clear();
    }
}

/// One connection to the [`OAuthServer`].
async fn oauth_connection(mut socket: tokio::net::TcpStream, shared: Arc<OAuthShared>) {
    let (head, body, raw) = read_text(&mut socket).await;
    let first = head.lines().next().unwrap_or_default().to_string();
    let method = first.split_whitespace().next().unwrap_or("").to_string();
    let path = path_of(&head);
    let authorization = (head.lines())
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim().to_string());
    shared.seen.lock().unwrap().push(OAuthRequest {
        method: method.clone(),
        path: path.clone(),
        authorization: authorization.clone(),
        body: raw.clone(),
    });
    let base = shared.base.clone();
    let json = |status: &str, value: Value, extra: &str| {
        let body = value.to_string();
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let route = path.split('?').next().unwrap_or("").to_string();
    let answer = match (method.as_str(), route.as_str()) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") => json(
            "200 OK",
            json!({ "resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/auth")], "scopes_supported": ["mcp:read", "mcp:write"] }),
            "",
        ),
        ("GET", "/.well-known/oauth-authorization-server/auth") => {
            let mut metadata = json!({
                "issuer": format!("{base}/auth"),
                "authorization_endpoint": format!("{base}/auth/authorize"),
                "token_endpoint": format!("{base}/auth/token"),
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none", "client_secret_basic"],
                "client_id_metadata_document_supported": shared.options.metadata_documents,
                "authorization_response_iss_parameter_supported": true,
            });
            if shared.options.registration {
                metadata["registration_endpoint"] = json!(format!("{base}/auth/register"));
            }
            json("200 OK", metadata, "")
        }
        ("POST", "/auth/register") if shared.options.registration => {
            let n = shared
                .next
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let redirects = body["redirect_uris"].clone();
            json(
                "201 Created",
                json!({ "client_id": format!("registered-{n}"), "redirect_uris": redirects, "token_endpoint_auth_method": "none" }),
                "",
            )
        }
        ("POST", "/auth/token") => oauth_token(&shared, &raw, authorization.as_deref()),
        ("POST", "/mcp") => oauth_mcp(&shared, &body, authorization.as_deref()),
        _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    };
    let _ = socket.write_all(answer.as_bytes()).await;
}

/// A request's head, its body as JSON (null if it isn't), and its body.
async fn read_text(socket: &mut tokio::net::TcpStream) -> (String, Value, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let length = (head.lines())
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_string())
            })
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        if buf.len() >= end + 4 + length {
            let raw = String::from_utf8_lossy(&buf[end + 4..end + 4 + length]).into_owned();
            let body = serde_json::from_str(&raw).unwrap_or(Value::Null);
            return (head, body, raw);
        }
    }
    (
        String::from_utf8_lossy(&buf).into_owned(),
        Value::Null,
        String::new(),
    )
}

/// The [`OAuthServer`]'s token endpoint: a code exchanged, PKCE checked,
/// or a refresh token rotated.
fn oauth_token(shared: &OAuthShared, raw: &str, authorization: Option<&str>) -> String {
    use base64::Engine;
    use sha2::Digest;
    let form: Vec<(String, String)> = reqwest::Url::parse(&format!("http://x/?{raw}"))
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default();
    let get = |key: &str| {
        (form.iter())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let refused = |error: &str| {
        let body = json!({ "error": error }).to_string();
        format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    if let Some((id, secret)) = &shared.options.client {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"));
        if authorization != Some(format!("Basic {basic}").as_str()) {
            return refused("invalid_client");
        }
    }
    if get("resource") != format!("{}/mcp", shared.base) {
        return refused("invalid_target");
    }
    let scope = match get("grant_type").as_str() {
        "authorization_code" => {
            let asked = shared.asked.lock().unwrap().take();
            let Some(asked) = asked else {
                return refused("invalid_grant");
            };
            let verified = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(get("code_verifier").as_bytes()));
            let ok = get("code") == asked.code
                && verified == asked.challenge
                && get("redirect_uri") == asked.redirect
                && get("client_id") == asked.client;
            if !ok {
                return refused("invalid_grant");
            }
            asked.scope
        }
        "refresh_token" => match shared.refresh.lock().unwrap().remove(&get("refresh_token")) {
            Some(scope) => scope,
            None => return refused("invalid_grant"),
        },
        _ => return refused("unsupported_grant_type"),
    };
    let n = shared
        .next
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (access, refresh) = (format!("at-{n}"), format!("rt-{n}"));
    shared
        .access
        .lock()
        .unwrap()
        .insert(access.clone(), scope.clone());
    shared
        .refresh
        .lock()
        .unwrap()
        .insert(refresh.clone(), scope.clone());
    let body = json!({ "access_token": access, "token_type": "Bearer", "expires_in": shared.options.expires_in, "refresh_token": refresh, "scope": scope }).to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The [`OAuthServer`]'s MCP server: only its tokens, its tool `whoami`
/// saying which; a call needing a scope the token hasn't, refused 403.
fn oauth_mcp(shared: &OAuthShared, body: &Value, authorization: Option<&str>) -> String {
    let token = authorization
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");
    let scope = shared.access.lock().unwrap().get(token).cloned();
    let Some(scope) = scope else {
        let challenge = format!(
            "WWW-Authenticate: Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"mcp:read\"\r\n",
            shared.base
        );
        return format!("HTTP/1.1 401 Unauthorized\r\n{challenge}Content-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body["method"].as_str().unwrap_or("");
    if method == "tools/call" {
        if let Some(needed) = &shared.options.call_needs {
            if !scope.split(' ').any(|s| s == needed) {
                let challenge = format!("WWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"mcp:read {needed}\"\r\n");
                return format!("HTTP/1.1 403 Forbidden\r\n{challenge}Content-Length: 0\r\nConnection: close\r\n\r\n");
            }
        }
    }
    let result = match method {
        "initialize" => {
            json!({ "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": "signed", "version": "1" } })
        }
        "tools/list" => {
            json!({ "tools": [{ "name": "whoami", "description": "Say who's calling.", "inputSchema": { "type": "object" } }] })
        }
        "tools/call" => {
            json!({ "content": [{ "type": "text", "text": format!("token {token}") }] })
        }
        _ => json!({}),
    };
    if id.is_null() {
        return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
    }
    let answer = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    )
}

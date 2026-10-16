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
/// runs each get their own replies, in their own order).
pub fn route_of(body: &Value, markers: &[&str]) -> Option<usize> {
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
    let mut scripts: Vec<Vec<(Vec<(Duration, String)>, bool)>> =
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

/// A request's path and JSON body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
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
                let body = serde_json::from_slice(&buf[start..start + length]).unwrap_or_default();
                return (path_of(&buf), body);
            }
        }
    }
    (path_of(&buf), Value::Null)
}

/// The path on a request's first line ("POST /v1/chat/completions HTTP/1.1").
fn path_of(request: &[u8]) -> String {
    let line = request.split(|b| *b == b'\n').next().unwrap_or_default();
    String::from_utf8_lossy(line)
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
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

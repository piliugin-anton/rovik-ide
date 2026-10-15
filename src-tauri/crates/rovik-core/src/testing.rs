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
                return serde_json::from_slice(&buf[start..start + length]).unwrap_or_default();
            }
        }
    }
    Value::Null
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

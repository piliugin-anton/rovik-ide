//! A debugging log of every chat request sent to a model provider and what
//! came back, verbatim, as JSON lines. Off unless `ROVIK_API_LOG` is set:
//!
//!   ROVIK_API_LOG=1                 → <temp dir>/rovik-api-log.jsonl
//!   ROVIK_API_LOG=/path/file.jsonl  → that file
//!
//! Two lines per attempt (a retry is a new attempt), sharing an `id`:
//!   {"dir":"request", "id", "at", "url", "streaming", "body"}
//!   {"dir":"response", "id", "at", "ms", "status", "outcome", "raw"}
//! `body` is the JSON sent as is: system prompt, history, tools, notes.
//! `tags`, when the request was made in a run, says for whom: the lead or a
//! subagent, its kind and chats (`TAGS`).
//! `raw` is the reply text as received: the SSE stream (`data: …` lines),
//! the whole JSON for a non-streaming reply, or an error body. The API key
//! is never written: it's only in a header (Authorization, or Anthropic's
//! x-api-key), and headers aren't logged. The file grows fast (every request repeats the history); delete
//! it when done.

use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Where to write, when enabled (read once, at the first request).
static TARGET: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    let value = std::env::var("ROVIK_API_LOG").ok()?;
    let value = value.trim();
    if value.is_empty() || value == "0" {
        return None;
    }
    let path = if value == "1" || value.eq_ignore_ascii_case("true") {
        std::env::temp_dir().join("rovik-api-log.jsonl")
    } else {
        PathBuf::from(value)
    };
    eprintln!(
        "api_log: writing requests and replies to {}",
        path.display()
    );
    Some(path)
});
tokio::task_local! {
    /// Who the requests made in this scope are for (agent_run sets it
    /// around a model turn: the lead, or a subagent of a kind), written with
    /// each request line.
    pub static TAGS: Value;
}

/// Tests: a log file of their own, whatever the environment says.
#[cfg(test)]
pub static TEST_TARGET: Mutex<Option<PathBuf>> = Mutex::new(None);

fn target() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(p) = TEST_TARGET
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Some(p);
    }
    TARGET.clone()
}

/// The request line: the tags of the scope it's sent in, if any.
fn request_line(id: u64, url: &str, body: &Value, streaming: bool) -> Value {
    let mut line = json!({
        "dir": "request",
        "id": id,
        "at": now(),
        "url": url,
        "streaming": streaming,
        "body": body,
    });
    if let Ok(tags) = TAGS.try_with(Clone::clone) {
        line["tags"] = tags;
    }
    line
}

/// One writer at a time, so lines from parallel requests don't interleave.
static WRITE: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    ms.to_string()
}

fn append(line: &Value) {
    let Some(path) = target() else { return };
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    // Best effort: a log that can't be written never breaks a request.
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    // Prompts, chat history and code: only the user may read them.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if let Ok(mut file) = options.open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// One attempt's log: the request is written when it starts, the response
/// when this is dropped, so every way out of the attempt (success, error,
/// timeout, Stop) is recorded.
pub struct Exchange {
    id: u64,
    started: Instant,
    pub status: Option<u16>,
    /// The reply text as received.
    pub raw: String,
    pub outcome: &'static str,
}

impl Exchange {
    /// None when logging is off (then nothing is collected either).
    pub fn start(url: &str, body: &Value, streaming: bool) -> Option<Exchange> {
        target()?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        append(&request_line(id, url, body, streaming));
        Some(Exchange {
            id,
            started: Instant::now(),
            status: None,
            raw: String::new(),
            outcome: "unfinished",
        })
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        append(&json!({
            "dir": "response",
            "id": self.id,
            "at": now(),
            "ms": self.started.elapsed().as_millis() as u64,
            "status": self.status,
            "outcome": self.outcome,
            "raw": self.raw,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_lines_carry_the_tags_of_their_scope() {
        let line = request_line(1, "http://x", &json!({ "a": 1 }), true);
        assert!(line.get("tags").is_none(), "no run: no tags");
        let tags = json!({ "role": "sub", "kind": "scout" });
        let line = TAGS
            .scope(tags.clone(), async {
                request_line(2, "http://x", &json!({}), false)
            })
            .await;
        assert_eq!(line["tags"], tags);
        assert_eq!(line["dir"], "request");
        assert_eq!(line["id"], 2);
    }
}

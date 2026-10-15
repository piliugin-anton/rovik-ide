//! Sending one chat completion request: timeouts, retries, cancellation and
//! typed errors. `agent::send_chat_message` builds the request body; this
//! module runs it, on `/chat/completions` or, for models that need it,
//! OpenAI's Responses API (`responses`).
//!
//! - Every failure becomes a `ChatError` with a kind, so the UI can explain
//!   it and offer the right action.
//! - Retries only happen while nothing visible has been emitted (no text, no
//!   tool call). After that, a failure stops the turn and keeps the partial
//!   reply (`partial: true`).
//! - Waits use exponential backoff with jitter, or the provider's own
//!   Retry-After / reset time when longer. A wait over `MAX_RETRY_WAIT`
//!   ends the request with a `rate_limited` error saying when to try again.
//! - Timeouts count time since the last *data* event: keep-alive comments
//!   (": OPENROUTER PROCESSING") keep a connection open without progress.
//! - The wait for the first data counts from when the last of the request
//!   went out (`Upload`), so a big upload that keeps moving isn't a stall.
//!   An attempt that got nothing back in time gives the next one twice as
//!   long: the provider may still be working through a long prompt.

use crate::agent::ChatStreamEvent;
use bytes::{Buf, Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub const MAX_ATTEMPTS: u32 = 4;
/// Waits before attempts 2, 3 and 4.
#[cfg(not(test))]
const BACKOFF_MS: [u64; 3] = [1_000, 3_000, 8_000];
#[cfg(test)]
const BACKOFF_MS: [u64; 3] = [10, 20, 40];
/// Longer waits aren't retried: the user is told when to try again.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
const DETAILS_MAX_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Until the first data event of an attempt, from when the last of the
    /// request went out. Doubled after an attempt that got nothing back.
    pub first_byte: Duration,
    /// Between data events once the reply has started.
    pub idle: Duration,
    /// Whole attempt.
    pub overall: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            first_byte: Duration::from_secs(60),
            idle: Duration::from_secs(45),
            overall: Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Finished without any text or tool call.
    EmptyResponse,
    /// No data within the first-byte or idle timeout.
    Stalled,
    /// The attempt exceeded the overall deadline.
    Timeout,
    RateLimited,
    ServerError,
    Network,
    Auth,
    BadRequest,
    NotFound,
    ContextLength,
    /// The output limit was hit before anything was written.
    Length,
    /// An error reported inside the response by the provider.
    Provider,
    Cancelled,
}

impl ErrorKind {
    fn retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::EmptyResponse
                | ErrorKind::Stalled
                | ErrorKind::Timeout
                | ErrorKind::RateLimited
                | ErrorKind::ServerError
                | ErrorKind::Network
                | ErrorKind::Provider
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatError {
    pub kind: ErrorKind,
    pub message: String,
    pub retryable: bool,
    /// How long until the provider accepts requests again, when known.
    pub retry_after_ms: Option<u64>,
    pub attempts: u32,
    pub status: Option<u16>,
    /// Raw provider response or transport error, for a "Details" view.
    pub details: Option<String>,
    /// Part of a reply had already arrived (kept by the UI).
    pub partial: bool,
}

impl ChatError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        ChatError {
            kind,
            message: message.into(),
            retryable: kind.retryable(),
            retry_after_ms: None,
            attempts: 0,
            status: None,
            details: None,
            partial: false,
        }
    }
    pub fn with_details(mut self, details: impl AsRef<str>) -> Self {
        let d: String = details.as_ref().chars().take(DETAILS_MAX_CHARS).collect();
        if !d.trim().is_empty() {
            self.details = Some(d);
        }
        self
    }
    fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
    fn with_retry_after(mut self, wait: Option<Duration>) -> Self {
        self.retry_after_ms = wait.map(|d| d.as_millis() as u64);
        self
    }
}

/*
 * ---------------------------------------------------------------------------
 * Cancellation registry
 * ---------------------------------------------------------------------------
 */

/// Running requests by id, so `cancel_chat_request` can stop one.
#[derive(Default)]
pub struct ChatRequests(Mutex<RequestMap>);

#[derive(Default)]
struct RequestMap {
    running: HashMap<String, watch::Sender<bool>>,
    /// Cancels that arrived before their request registered (the frontend
    /// can press Stop while the request is still being prepared).
    early: Vec<String>,
}

const EARLY_CANCELS_KEPT: usize = 32;

pub struct Registration<'a> {
    requests: &'a ChatRequests,
    id: Option<String>,
    pub cancel: watch::Receiver<bool>,
}

impl ChatRequests {
    pub fn register(&self, id: Option<String>) -> Registration<'_> {
        let (tx, rx) = watch::channel(false);
        if let (Some(id), Ok(mut map)) = (&id, self.0.lock()) {
            if let Some(pos) = map.early.iter().position(|e| e == id) {
                map.early.remove(pos);
                let _ = tx.send(true); // Stop was pressed before we got here
            }
            map.running.insert(id.clone(), tx);
        }
        Registration {
            requests: self,
            id,
            cancel: rx,
        }
    }

    /// Cancels a running request, or remembers the cancel for a request that
    /// hasn't registered yet.
    pub fn cancel(&self, id: &str) -> bool {
        let Ok(mut map) = self.0.lock() else {
            return false;
        };
        match map.running.get(id) {
            Some(tx) => {
                let _ = tx.send(true);
            }
            None => {
                map.early.push(id.to_string());
                let excess = map.early.len().saturating_sub(EARLY_CANCELS_KEPT);
                map.early.drain(..excess);
            }
        }
        true
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let (Some(id), Ok(mut map)) = (&self.id, self.requests.0.lock()) {
            map.running.remove(id);
        }
    }
}

/// Async so it runs off the main thread: Stop must work even if the main
/// thread is busy.
#[tauri::command]
pub async fn cancel_chat_request(
    requests: tauri::State<'_, ChatRequests>,
    request_id: String,
) -> Result<bool, String> {
    Ok(requests.cancel(&request_id))
}

/// Resolves when cancellation is requested (never, if the sender is gone).
pub async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * Classifying HTTP errors
 * ---------------------------------------------------------------------------
 */

/// "2m59.56s", "1h2m", "750ms", "20s", or a plain number of seconds.
fn parse_duration_text(text: &str) -> Option<Duration> {
    let t = text.trim();
    if let Ok(secs) = t.parse::<f64>() {
        return (secs >= 0.0).then(|| Duration::from_secs_f64(secs));
    }
    let mut total = 0.0f64;
    let mut number = String::new();
    let mut chars = t.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        let unit = match c {
            'h' => 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        total += value * unit;
        any = true;
    }
    (any && number.is_empty()).then(|| Duration::from_secs_f64(total))
}

/// A reset value that may be an epoch time (s or ms) or a duration.
fn parse_reset(value: &str, now_epoch: Duration) -> Option<Duration> {
    let v = value.trim();
    if let Ok(n) = v.parse::<f64>() {
        if n > 1e12 {
            return Some(Duration::from_millis(n as u64).saturating_sub(now_epoch));
        }
        if n > 1e9 {
            return Some(Duration::from_secs(n as u64).saturating_sub(now_epoch));
        }
    }
    parse_duration_text(v)
}

fn now_epoch() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// The longest wait any rate-limit header or error metadata asks for.
pub fn retry_after(headers: &HeaderMap, body: Option<&serde_json::Value>) -> Option<Duration> {
    let now = now_epoch();
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let mut waits: Vec<Duration> = Vec::new();

    if let Some(ms) = header("retry-after-ms").and_then(|v| v.trim().parse::<u64>().ok()) {
        waits.push(Duration::from_millis(ms));
    }
    if let Some(d) = header("retry-after").and_then(parse_duration_text) {
        waits.push(d);
    }
    // A reset header alone doesn't mean "blocked": only consulted when the
    // response says we are rate limited (the caller decides).
    for name in [
        "x-ratelimit-reset",
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
    ] {
        if let Some(d) = header(name).and_then(|v| parse_reset(v, now)) {
            waits.push(d);
        }
    }
    // OpenRouter puts the upstream headers into the error body.
    if let Some(meta) = body.and_then(|b| b.pointer("/error/metadata/headers")) {
        for key in [
            "X-RateLimit-Reset",
            "x-ratelimit-reset",
            "Retry-After",
            "retry-after",
        ] {
            if let Some(v) = meta.get(key) {
                let text = v
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string());
                if let Some(d) = parse_reset(&text, now) {
                    waits.push(d);
                }
            }
        }
    }
    waits.into_iter().max()
}

fn looks_like_context_overflow(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "context length",
        "context_length",
        "maximum context",
        "context window",
        "too many tokens",
        "prompt is too long",
        "reduce the length",
    ]
    .iter()
    .any(|needle| t.contains(needle))
}

/// The provider doesn't serve this model on the API the request used
/// (`{"type":"error","error":{"type":"ModelProtocolUnsupported","message":
/// "Model does not support this protocol."}}`): not worth retrying as is,
/// but the other route may work (`responses::wrong_route`).
pub fn looks_like_unsupported_protocol(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "modelprotocolunsupported",
        "protocol_unsupported",
        "not support this protocol",
        "protocol is not supported",
        "unsupported protocol",
    ]
    .iter()
    .any(|needle| t.contains(needle))
}

fn error_message_from(body: &serde_json::Value) -> Option<String> {
    body.pointer("/error/message")
        .or_else(|| body.get("message"))
        .or_else(|| body.get("error").filter(|e| e.is_string()))
        .and_then(|m| m.as_str())
        .map(str::to_string)
}

pub fn classify_status(status: u16, headers: &HeaderMap, body: &str) -> ChatError {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let provider_message = json.as_ref().and_then(error_message_from);
    let shown = provider_message
        .clone()
        .unwrap_or_else(|| format!("HTTP {status}"));

    let kind = match status {
        _ if looks_like_unsupported_protocol(body) => ErrorKind::BadRequest,
        429 => ErrorKind::RateLimited,
        401 | 403 => ErrorKind::Auth,
        404 => ErrorKind::NotFound,
        408 | 409 | 425 => ErrorKind::ServerError,
        400 | 413 | 422 if looks_like_context_overflow(body) => ErrorKind::ContextLength,
        500..=599 => ErrorKind::ServerError,
        _ => ErrorKind::BadRequest,
    };
    let message = match kind {
        ErrorKind::RateLimited => format!("Rate limited by the provider: {shown}"),
        ErrorKind::Auth => format!("The API key was rejected: {shown}"),
        ErrorKind::NotFound => format!("Model or endpoint not found: {shown}"),
        ErrorKind::ContextLength => format!("The conversation is too long for this model: {shown}"),
        ErrorKind::ServerError => format!("The provider had an error ({status}): {shown}"),
        _ => format!("The request was rejected ({status}): {shown}"),
    };
    let wait = if kind == ErrorKind::RateLimited || status == 503 {
        retry_after(headers, json.as_ref())
    } else {
        None
    };
    ChatError::new(kind, message)
        .with_status(status)
        .with_retry_after(wait)
        .with_details(body)
}

/// An `{"error": …}` object inside a 200 response or a stream chunk.
pub fn classify_body_error(value: &serde_json::Value) -> Option<ChatError> {
    let error = value.get("error")?;
    if error.is_null() {
        return None;
    }
    let code = error
        .get("code")
        .and_then(|c| {
            c.as_u64()
                .or_else(|| c.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|c| c as u16);
    let message = error_message_from(value).unwrap_or_else(|| error.to_string());
    // In a 200 response or a stream: the request, not the provider, is at fault.
    if looks_like_unsupported_protocol(&value.to_string()) {
        return Some(
            ChatError::new(
                ErrorKind::BadRequest,
                format!("The request was rejected: {message}"),
            )
            .with_details(value.to_string()),
        );
    }
    let mut e = match code {
        Some(c) => classify_status(c, &HeaderMap::new(), &value.to_string()),
        None => ChatError::new(
            ErrorKind::Provider,
            format!("The provider reported an error: {message}"),
        )
        .with_details(value.to_string()),
    };
    if code.is_some() && e.kind == ErrorKind::BadRequest {
        // In-stream errors with a 4xx code are usually upstream failures.
        e = ChatError::new(
            ErrorKind::Provider,
            format!("The provider reported an error: {message}"),
        )
        .with_details(value.to_string());
    }
    Some(e)
}

/*
 * ---------------------------------------------------------------------------
 * Parsing a response
 * ---------------------------------------------------------------------------
 */

/// Turns SSE data payloads into stream events. Tool calls are keyed by
/// `index`: the OpenAI format only sends a call's `id` in its first chunk.
#[derive(Default)]
pub struct ReplyParser {
    calls: Vec<(u64, PendingCall)>,
    /// A tool call was emitted (stays true after the calls are drained).
    emitted_call: bool,
    pub text_len: usize,
    finish_reason: Option<String>,
    /// Token counts, if the provider reported them (usually in the last chunk).
    usage: Option<serde_json::Value>,
    /// Characters of reasoning shown so far.
    reasoning_len: usize,
    /// `reasoning_details` entries, merged from the stream (reasoning_echo.rs).
    reasoning_details: Vec<serde_json::Value>,
    /// Responses API: the output items that are commentary (`responses`).
    commentary: Vec<u64>,
    /// Their text, in case the reply turns out to be nothing else.
    commentary_text: String,
    /// Responses API: where to note that this model labels its messages
    /// with a phase (`responses::note_phases`).
    phase_key: Option<String>,
    /// Reasoning written into the reply text as `<think>…</think>`.
    think: InlineThink,
}

/// Reasoning some models write at the start of the reply text instead of
/// a field of its own (MiniMax by default, DeepSeek R1 on some hosts,
/// Perplexity, local servers without a reasoning parser): shown as
/// reasoning, not as the answer.
#[derive(Default)]
enum InlineThink {
    /// The reply's start so far: may still turn out to open a tag.
    #[default]
    Start,
    Undecided(String),
    /// Inside the tag (`think` or `thinking`); `carry` may be the start of
    /// the closing tag.
    Inside {
        tag: &'static str,
        carry: String,
    },
    Done,
}

const THINK_TAGS: [&str; 2] = ["think", "thinking"];

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
    started: bool,
    /// Provider data to echo back (Gemini 3 sends its thought signature here).
    extra_content: Option<serde_json::Value>,
}

impl ReplyParser {
    pub fn visible(&self) -> bool {
        self.text_len > 0 || self.emitted_call || self.calls.iter().any(|(_, c)| c.started)
    }

    fn call(&mut self, index: u64) -> &mut PendingCall {
        if let Some(pos) = self.calls.iter().position(|(i, _)| *i == index) {
            return &mut self.calls[pos].1;
        }
        self.calls.push((index, PendingCall::default()));
        &mut self.calls.last_mut().expect("just pushed").1
    }

    /// One `data:` payload (already JSON-parsed).
    pub fn feed(
        &mut self,
        value: &serde_json::Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        if let Some(e) = classify_body_error(value) {
            return Err(e);
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage.clone());
        }
        let choice = value.pointer("/choices/0");
        if let Some(delta) = choice.and_then(|c| c.get("delta")) {
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    self.content(content, emit);
                }
            }
            // Reasoning: `reasoning_content` (DeepSeek, vLLM, many gateways)
            // or `reasoning` (OpenRouter). Shown, but not part of the reply.
            // A provider sending both fields puts `null` in the one it
            // doesn't use: look past it to the other.
            let reasoning = delta
                .get("reasoning_content")
                .filter(|v| !v.is_null())
                .or_else(|| delta.get("reasoning"))
                .and_then(|v| v.as_str());
            if let Some(reasoning) = reasoning.filter(|r| !r.is_empty()) {
                self.reasoning_len += reasoning.len();
                emit(ChatStreamEvent::Reasoning {
                    content: reasoning.to_string(),
                });
            }
            // The structured form, to send back (OpenRouter and gateways like it).
            for fragment in delta
                .get("reasoning_details")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                crate::reasoning_echo::merge(&mut self.reasoning_details, fragment);
            }
            for (pos, tc) in delta
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .enumerate()
            {
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Gemini omits `index`: tell calls apart by id instead, so
                // parallel calls streamed in separate chunks don't merge.
                let index = match tc.get("index").and_then(|i| i.as_u64()) {
                    Some(i) => i,
                    None if !id.is_empty() => self
                        .calls
                        .iter()
                        .find(|(_, c)| c.id == id)
                        .map(|(i, _)| *i)
                        .unwrap_or_else(|| {
                            self.calls.iter().map(|(i, _)| i + 1).max().unwrap_or(0)
                        }),
                    None => pos as u64,
                };
                let name = tc
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = tc
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut started_now = false;
                let call = self.call(index);
                if !id.is_empty() && call.id.is_empty() {
                    call.id = id;
                }
                if !name.is_empty() && call.name.is_empty() {
                    call.name = name;
                }
                call.arguments.push_str(&args);
                if call.extra_content.is_none() {
                    call.extra_content = tc.get("extra_content").filter(|v| !v.is_null()).cloned();
                }
                if !call.started && !call.name.is_empty() {
                    if call.id.is_empty() {
                        // Some servers never send ids; make a stable one.
                        call.id = format!("call_{index}_{}", now_epoch().as_millis());
                    }
                    call.started = true;
                    started_now = true;
                    emit(ChatStreamEvent::ToolCallStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    });
                    if !call.arguments.is_empty() {
                        emit(ChatStreamEvent::ToolCallArgumentsDelta {
                            id: call.id.clone(),
                            arguments_delta: call.arguments.clone(),
                        });
                    }
                } else if call.started && !args.is_empty() {
                    emit(ChatStreamEvent::ToolCallArgumentsDelta {
                        id: call.id.clone(),
                        arguments_delta: args,
                    });
                }
                self.emitted_call |= started_now;
            }
        }
        if let Some(reason) = choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
        {
            self.finish_reason = Some(reason.to_string());
        }
        Ok(())
    }

    fn text(&mut self, text: &str, emit: &mut (dyn FnMut(ChatStreamEvent) + Send)) {
        if !text.is_empty() {
            self.text_len += text.len();
            emit(ChatStreamEvent::Delta {
                content: text.to_string(),
            });
        }
    }

    fn reasoning_text(&mut self, text: &str, emit: &mut (dyn FnMut(ChatStreamEvent) + Send)) {
        if !text.is_empty() {
            self.reasoning_len += text.len();
            emit(ChatStreamEvent::Reasoning {
                content: text.to_string(),
            });
        }
    }

    /// Reply text, with a leading `<think>…</think>` taken out as reasoning.
    fn content(&mut self, piece: &str, emit: &mut (dyn FnMut(ChatStreamEvent) + Send)) {
        let mut piece = piece.to_string();
        loop {
            match std::mem::take(&mut self.think) {
                InlineThink::Done => {
                    self.think = InlineThink::Done;
                    return self.text(&piece, emit);
                }
                InlineThink::Start => self.think = InlineThink::Undecided(String::new()),
                InlineThink::Undecided(mut seen) => {
                    seen.push_str(&piece);
                    let start = seen.trim_start();
                    let opened = THINK_TAGS
                        .iter()
                        .find(|t| start.starts_with(&format!("<{t}>")));
                    if let Some(tag) = opened {
                        let rest = start[tag.len() + 2..].to_string();
                        emit(ChatStreamEvent::ReasoningInline {
                            tag: tag.to_string(),
                        });
                        self.think = InlineThink::Inside {
                            tag,
                            carry: String::new(),
                        };
                        piece = rest;
                        continue;
                    }
                    // Whitespace, or "<thi": wait for more.
                    let may_open = start.is_empty()
                        || THINK_TAGS
                            .iter()
                            .any(|t| format!("<{t}>").starts_with(start));
                    if may_open {
                        self.think = InlineThink::Undecided(seen);
                        return;
                    }
                    self.think = InlineThink::Done;
                    return self.text(&seen, emit);
                }
                InlineThink::Inside { tag, mut carry } => {
                    carry.push_str(&piece);
                    let close = format!("</{tag}>");
                    if let Some(at) = carry.find(&close) {
                        let thought = carry[..at].to_string();
                        let after = carry[at + close.len()..].to_string();
                        self.reasoning_text(&thought, emit);
                        self.think = InlineThink::Done;
                        return self.text(&after, emit);
                    }
                    // Keep what could be the start of the closing tag.
                    let mut keep = carry.len().saturating_sub(close.len() - 1);
                    while !carry.is_char_boundary(keep) {
                        keep -= 1;
                    }
                    let thought = carry[..keep].to_string();
                    self.reasoning_text(&thought, emit);
                    self.think = InlineThink::Inside {
                        tag,
                        carry: carry[keep..].to_string(),
                    };
                    return;
                }
            }
        }
    }

    /// End of the reply: completes tool calls and checks it said something.
    pub fn finish(
        &mut self,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        // Text held back while it might have opened or closed a tag.
        match std::mem::replace(&mut self.think, InlineThink::Done) {
            InlineThink::Undecided(seen) => self.text(&seen, emit),
            // Never closed: all of it was reasoning.
            InlineThink::Inside { carry, .. } => self.reasoning_text(&carry, emit),
            InlineThink::Start | InlineThink::Done => {}
        }
        // Decide before draining: the drained calls are what made it visible.
        let said_something = self.visible();
        for (_, call) in self.calls.drain(..) {
            if call.started {
                emit(ChatStreamEvent::ToolCallComplete {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                    extra_content: call.extra_content,
                });
            }
        }
        if self.finish_reason.as_deref() == Some("error") {
            return Err(ChatError::new(
                ErrorKind::Provider,
                "The provider ended the reply with an error.",
            ));
        }
        if !self.reasoning_details.is_empty() {
            emit(ChatStreamEvent::ReasoningDetails {
                details: std::mem::take(&mut self.reasoning_details),
            });
        }
        if !said_something {
            if self.finish_reason.as_deref() == Some("length") {
                return Err(ChatError::new(
                    ErrorKind::Length,
                    "The model reached its output limit before writing anything.",
                ));
            }
            return Err(ChatError::new(
                ErrorKind::EmptyResponse,
                "The model returned an empty response.",
            ));
        }
        emit(ChatStreamEvent::Finished {
            finish_reason: self.finish_reason.clone(),
            usage: self.usage.take(),
        });
        Ok(())
    }

    /// A complete non-streaming response body.
    pub fn feed_complete(
        &mut self,
        body: &serde_json::Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        if let Some(e) = classify_body_error(body) {
            return Err(e);
        }
        if let Some(usage) = body.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage.clone());
        }
        let message = body.pointer("/choices/0/message");
        let as_delta = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": message.and_then(|m| m.get("content")).cloned().unwrap_or(serde_json::Value::Null),
                    "reasoning_content": message
                        .and_then(|m| {
                            m.get("reasoning_content")
                                .filter(|v| !v.is_null())
                                .or_else(|| m.get("reasoning"))
                        })
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    "reasoning_details": message
                        .and_then(|m| m.get("reasoning_details"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    "tool_calls": message
                        .and_then(|m| m.get("tool_calls"))
                        .and_then(|t| t.as_array())
                        .map(|calls| calls.iter().enumerate().map(|(i, c)| {
                            let mut c = c.clone();
                            c["index"] = serde_json::json!(i);
                            c
                        }).collect::<Vec<_>>())
                        .unwrap_or_default(),
                },
                "finish_reason": body.pointer("/choices/0/finish_reason").cloned().unwrap_or(serde_json::Value::Null),
            }]
        });
        self.feed(&as_delta, emit)
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/*
 * ---------------------------------------------------------------------------
 * Running a request
 * ---------------------------------------------------------------------------
 */

pub struct RequestSpec<'a> {
    pub client: &'a Client,
    pub url: &'a str,
    pub api_key: &'a str,
    pub body: &'a serde_json::Value,
    pub streaming: bool,
    pub limits: Limits,
    /// Which API `url` and `body` are for: how the reply is read.
    pub api: Api,
    /// The chat this request is for: its session on opencode.ai (`opencode`).
    pub chat_id: Option<&'a str>,
}

/// Why an attempt ended unsuccessfully, and whether anything was shown.
struct AttemptFailure {
    error: ChatError,
    visible: bool,
    /// Nothing came back within the first-byte limit.
    silent: bool,
}

/// The size of the pieces the connection is handed the request body in.
const PIECE_BYTES: usize = 64 * 1024;

/// The request body, handed to the connection a piece at a time so the
/// attempt can tell a slow upload from a silent provider: the first-byte
/// limit counts from when the connection last took a piece. A big request
/// (attachments) on a slow link keeps moving; a connection that takes
/// nothing for that long has stopped.
struct Upload(Arc<Mutex<Sent>>);

#[derive(Clone, Copy)]
struct Sent {
    /// When the connection last took a piece (until then, when the request
    /// was ready to go).
    at: Instant,
    /// It has taken the last one.
    all: bool,
}

impl Upload {
    /// The body to send, and its length.
    fn start(body: &serde_json::Value) -> (Upload, reqwest::Body, u64) {
        let bytes = Bytes::from(body.to_string());
        let len = bytes.len();
        let sent = Arc::new(Mutex::new(Sent {
            at: Instant::now(),
            all: len == 0,
        }));
        let noted = sent.clone();
        // The connection asks for the next piece once it has room for it.
        let pieces = (0..len).step_by(PIECE_BYTES).map(move |start| {
            let end = (start + PIECE_BYTES).min(len);
            if let Ok(mut sent) = noted.lock() {
                *sent = Sent {
                    at: Instant::now(),
                    all: end == len,
                };
            }
            Ok::<_, std::io::Error>(bytes.slice(start..end))
        });
        let body = reqwest::Body::wrap_stream(futures_util::stream::iter(pieces));
        (Upload(sent), body, len as u64)
    }

    fn sent(&self) -> Sent {
        match self.0.lock() {
            Ok(sent) => *sent,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

async fn attempt(
    spec: &RequestSpec<'_>,
    first_byte: Duration,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<(), AttemptFailure> {
    let started = Instant::now();
    let deadline = started + spec.limits.overall;
    let fail = |error: ChatError, visible: bool| AttemptFailure {
        error,
        visible,
        silent: false,
    };
    let silent = || AttemptFailure {
        error: ChatError::new(
            ErrorKind::Stalled,
            format!("No response within {} s.", first_byte.as_secs()),
        ),
        visible: false,
        silent: true,
    };
    // Debugging only (ROVIK_API_LOG): the request now, the reply when this
    // attempt ends, whichever way it ends.
    let mut log = crate::api_log::Exchange::start(spec.url, spec.body, spec.streaming);
    let overall_error = || {
        ChatError::new(
            ErrorKind::Timeout,
            format!(
                "The reply took longer than {} minutes.",
                spec.limits.overall.as_secs() / 60
            ),
        )
    };

    let (upload, body, length) = Upload::start(spec.body);
    let request = spec
        .client
        .post(spec.url)
        .bearer_auth(spec.api_key)
        .headers(crate::opencode::headers(spec.url, spec.chat_id))
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, length)
        .body(body)
        .send();
    let mut request = std::pin::pin!(request);
    // Waiting for response headers. Non-streaming replies only arrive when
    // complete, so only the overall deadline applies to them.
    let response = loop {
        let wait_until = if spec.streaming {
            (upload.sent().at + first_byte).min(deadline)
        } else {
            deadline
        };
        tokio::select! {
            _ = cancelled(cancel) => return Err(fail(ChatError::new(ErrorKind::Cancelled, "Stopped."), false)),
            r = &mut request => match r {
                Err(e) => return Err(fail(
                    ChatError::new(ErrorKind::Network, "Could not reach the provider.").with_details(e.to_string()), false)),
                Ok(r) => break r,
            },
            _ = tokio::time::sleep_until(wait_until.into()) => {
                if Instant::now() >= deadline {
                    return Err(fail(overall_error(), false));
                }
                let sent = upload.sent();
                if Instant::now() < sent.at + first_byte {
                    continue; // the connection took more of the request since
                }
                if !sent.all {
                    return Err(fail(ChatError::new(ErrorKind::Network, format!(
                        "Sending the request made no progress for {} s.", first_byte.as_secs())), false));
                }
                return Err(silent());
            }
        }
    };

    let status = response.status();
    if let Some(log) = log.as_mut() {
        log.status = Some(status.as_u16());
    }
    if !status.is_success() {
        let headers = response.headers().clone();
        // The error body is small, but a misbehaving server could still
        // stall here: bound it and keep it cancellable.
        let body = tokio::select! {
            _ = cancelled(cancel) => return Err(fail(ChatError::new(ErrorKind::Cancelled, "Stopped."), false)),
            r = tokio::time::timeout(Duration::from_secs(10), response.text()) => {
                r.ok().and_then(|t| t.ok()).unwrap_or_default()
            }
        };
        if let Some(log) = log.as_mut() {
            log.raw = body.clone();
            log.outcome = "error status";
        }
        return Err(fail(
            classify_status(status.as_u16(), &headers, &body),
            false,
        ));
    }
    let headers = response.headers().clone();

    let mut parser = ReplyParser {
        phase_key: (spec.api == Api::Responses).then(|| {
            responses::phase_key(
                spec.url,
                spec.body
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or(""),
            )
        }),
        ..Default::default()
    };

    if !spec.streaming {
        let text = tokio::select! {
            _ = cancelled(cancel) => return Err(fail(ChatError::new(ErrorKind::Cancelled, "Stopped."), false)),
            r = tokio::time::timeout_at(deadline.into(), response.text()) => match r {
                Err(_) => return Err(fail(overall_error(), false)),
                Ok(Err(e)) => return Err(fail(ChatError::new(ErrorKind::Network, "The connection dropped.").with_details(e.to_string()), false)),
                Ok(Ok(t)) => t,
            },
        };
        if let Some(log) = log.as_mut() {
            log.raw = text.clone();
            log.outcome = "complete";
        }
        let body: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            fail(
                ChatError::new(
                    ErrorKind::Provider,
                    "The provider sent a response that isn't JSON.",
                )
                .with_details(&text),
                false,
            )
        })?;
        let fed = match spec.api {
            Api::ChatCompletions => parser.feed_complete(&body, emit),
            Api::Responses => parser.feed_complete_responses(&body, emit),
        };
        let mut e = fed.and_then(|_| parser.finish(emit));
        if let Err(err) = &mut e {
            if err.kind == ErrorKind::RateLimited && err.retry_after_ms.is_none() {
                *err = err
                    .clone()
                    .with_retry_after(retry_after(&headers, Some(&body)));
            }
        }
        return e.map_err(|error| fail(error, parser.visible()));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = BytesMut::new();
    let mut last_data: Option<Instant> = None;
    // The server closed the connection: this pass over the buffer is the last.
    let mut closed = false;

    loop {
        let wait_until = match last_data {
            None => upload.sent().at + first_byte,
            Some(t) => t + spec.limits.idle,
        }
        .min(deadline);

        let chunk = tokio::select! {
            _ = cancelled(cancel) => {
                // P6.2: what the provider already said the request used (a
                // request asked again counts it: agent_run/run.rs).
                if let Some(usage) = parser.usage.take() {
                    emit(ChatStreamEvent::Finished { finish_reason: None, usage: Some(usage) });
                }
                return Err(fail(ChatError::new(ErrorKind::Cancelled, "Stopped."), parser.visible()));
            }
            r = tokio::time::timeout_at(wait_until.into(), stream.next()) => r,
        };
        let chunk = match chunk {
            Err(_) if Instant::now() >= deadline => {
                return Err(fail(overall_error(), parser.visible()))
            }
            Err(_) if last_data.is_none() => return Err(silent()),
            Err(_) => {
                let message = format!(
                    "The reply stopped arriving for {} s.",
                    spec.limits.idle.as_secs()
                );
                return Err(fail(
                    ChatError::new(ErrorKind::Stalled, message),
                    parser.visible(),
                ));
            }
            Ok(None) => {
                // Connection closed without [DONE]. A last event the server
                // didn't end with a blank line (some close right after the
                // final chunk) is still in the buffer: read it below as if
                // it had been, then stop.
                if let Some(log) = log.as_mut() {
                    log.outcome = "closed";
                }
                closed = true;
                bytes::Bytes::new()
            }
            Ok(Some(Err(e))) => {
                return Err(fail(
                    ChatError::new(ErrorKind::Network, "The connection dropped.")
                        .with_details(e.to_string()),
                    parser.visible(),
                ));
            }
            Ok(Some(Ok(bytes))) => bytes,
        };
        buffer.extend_from_slice(&chunk);
        if closed && buffer.iter().any(|b| !b.is_ascii_whitespace()) {
            buffer.extend_from_slice(b"\n\n");
        }
        if let Some(log) = log.as_mut() {
            log.raw.push_str(&String::from_utf8_lossy(&chunk));
        }

        while let Some((pos, sep)) = find_sse_boundary(&buffer) {
            let event = buffer.split_to(pos);
            buffer.advance(sep);
            let event = String::from_utf8_lossy(&event);
            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                }; // ":" comments are keep-alives
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                last_data = Some(Instant::now());
                if data == "[DONE]" {
                    if let Some(log) = log.as_mut() {
                        log.outcome = "done";
                    }
                    return parser.finish(emit).map_err(|e| fail(e, parser.visible()));
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                let fed = match spec.api {
                    Api::ChatCompletions => parser.feed(&value, emit).map(|_| false),
                    // Ends with an event, not [DONE].
                    Api::Responses => parser.feed_responses(&value, emit),
                };
                match fed {
                    Ok(false) => {}
                    Ok(true) => {
                        if let Some(log) = log.as_mut() {
                            log.outcome = "done";
                        }
                        return parser.finish(emit).map_err(|e| fail(e, parser.visible()));
                    }
                    Err(mut error) => {
                        if error.kind == ErrorKind::RateLimited && error.retry_after_ms.is_none() {
                            error = error.with_retry_after(retry_after(&headers, Some(&value)));
                        }
                        return Err(fail(error, parser.visible()));
                    }
                }
            }
        }
        if closed {
            break;
        }
    }
    parser.finish(emit).map_err(|e| fail(e, parser.visible()))
}

pub fn backoff(attempt_number: u32) -> Duration {
    let base = BACKOFF_MS[(attempt_number as usize)
        .saturating_sub(1)
        .min(BACKOFF_MS.len() - 1)];
    // ±20% jitter so many clients don't retry in lockstep.
    let nanos = now_epoch().subsec_nanos() as u64;
    let jitter = 0.8 + (nanos % 400) as f64 / 1000.0;
    Duration::from_millis((base as f64 * jitter) as u64)
}

/// Runs the request with retries. Emits stream events as they arrive and a
/// `Retrying` event before each wait; returns the final error, if any.
pub async fn run(
    spec: &RequestSpec<'_>,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<(), ChatError> {
    let mut attempt_number = 1;
    let mut first_byte = spec.limits.first_byte;
    loop {
        let failure = match attempt(spec, first_byte, cancel, emit).await {
            Ok(()) => {
                emit(ChatStreamEvent::Done);
                return Ok(());
            }
            Err(f) => f,
        };
        // The provider may still be working through a long prompt (a cold
        // cache, a local server's prompt processing) and be further along
        // when asked again: give it longer.
        if failure.silent {
            first_byte = first_byte.saturating_mul(2).min(spec.limits.overall);
        }
        let mut error = failure.error;
        error.attempts = attempt_number;

        // Something was shown: keep it and stop (the user can continue).
        if failure.visible {
            error.partial = true;
            error.retryable = false;
            return Err(error);
        }
        if !error.retryable || attempt_number >= MAX_ATTEMPTS {
            return Err(error);
        }

        let suggested = error.retry_after_ms.map(Duration::from_millis);
        if let Some(wait) = suggested.filter(|w| *w > MAX_RETRY_WAIT) {
            error.retryable = false;
            error.message = format!("{} Try again in {}.", error.message, human_duration(wait));
            return Err(error);
        }
        let delay = backoff(attempt_number).max(suggested.unwrap_or_default());

        emit(ChatStreamEvent::Retrying {
            attempt: attempt_number + 1,
            max_attempts: MAX_ATTEMPTS,
            delay_ms: delay.as_millis() as u64,
            reason: error.clone(),
        });
        tokio::select! {
            _ = cancelled(cancel) => {
                let mut stopped = ChatError::new(ErrorKind::Cancelled, "Stopped.");
                stopped.attempts = attempt_number;
                return Err(stopped);
            }
            _ = tokio::time::sleep(delay) => {}
        }
        attempt_number += 1;
    }
}

pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs.div_ceil(60)),
        _ => {
            let (h, m) = (secs / 3600, (secs % 3600) / 60);
            if m == 0 {
                format!("{h} h")
            } else {
                format!("{h} h {m} min")
            }
        }
    }
}

pub mod responses;
pub use responses::Api;

#[cfg(test)]
mod tests;

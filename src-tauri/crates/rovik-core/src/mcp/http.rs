//! A URL's server (Streamable HTTP): each message Rovik sends is a POST,
//! answered with JSON or an event stream that ends with the answer
//! (docs/design/mcp-plan.md, Q03). It goes through the app's HTTP client,
//! so Settings → Network's proxies apply; on a daemon, a server on the
//! loopback of the user's computer is posted to through the app there
//! (mcp-plan-2.md, Q11).

use super::rpc::{self, Failure, Incoming};
use super::Relayed;
use crate::agent_run::StopSignal;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use rovik_protocol::mcp::RelayedPost;
use rovik_protocol::Secret;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The most an answer may be: a big tool result fits.
const MAX_BODY: usize = 64 << 20;
/// How much of an error's body its text keeps.
const ERROR_BODY_SHOWN: usize = 300;
/// The header of a legacy server's session.
const SESSION: &str = "mcp-session-id";
/// What a POST takes and what it accepts back.
const JSON: &str = "application/json";
const ACCEPTED: &str = "application/json, text/event-stream";
/// How long a notification's POST may take.
const NOTIFY_WITHIN: Duration = Duration::from_secs(10);

/// A server's address, its headers, and a legacy server's session.
pub(super) struct Http {
    url: String,
    /// The settings' headers, their secrets filled in: never shown.
    headers: HeaderMap,
    /// The session a legacy server gave (`Mcp-Session-Id`), once it did.
    session: Mutex<Option<String>>,
    next: AtomicU64,
    /// On a daemon, the relay its POSTs go through, to the computer whose
    /// loopback the URL is on.
    relayed: Option<Relayed>,
}

impl Http {
    /// The server at `url`, with these headers on each request, posted to
    /// through `relayed` when it's given. A URL that isn't http(s), or a
    /// header HTTP can't carry, is refused, naming the header but never its
    /// value.
    pub(super) fn new(
        url: &str,
        headers: &[(String, String)],
        relayed: Option<Relayed>,
    ) -> Result<Http, String> {
        let parsed =
            reqwest::Url::parse(url.trim()).map_err(|e| format!("The URL can't be used: {e}."))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("The URL must start with http:// or https://.".into());
        }
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let header = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| format!("The header name \"{name}\" can't be sent."))?;
            let mut value = HeaderValue::from_str(value.trim())
                .map_err(|_| format!("The value of the header {name} can't be sent."))?;
            value.set_sensitive(true);
            map.insert(header, value);
        }
        Ok(Http {
            url: parsed.to_string(),
            headers: map,
            session: Mutex::new(None),
            next: AtomicU64::new(1),
            relayed,
        })
    }

    fn session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Forgets the legacy session, to start another.
    pub(super) fn end_session(&self) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Whether a legacy server gave a session.
    pub(super) fn has_session(&self) -> bool {
        self.session().is_some()
    }

    /// A POST of `message` with the era's `headers`, its session's too.
    async fn post(
        &self,
        message: &Value,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, Failure> {
        if let Some((relay, computer)) = &self.relayed {
            let post = self.relayed_post(message, headers);
            return relay.post(computer, post).await.map_err(Failure::Gone);
        }
        let client = crate::net::client().await;
        let mut request = client
            .post(&self.url)
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, JSON)
            .header(ACCEPT, ACCEPTED);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(session) = self.session() {
            request = request.header(SESSION, session);
        }
        request
            .body(message.to_string())
            .send()
            .await
            .map_err(|e| Failure::Gone(crate::net::error_text(&e.without_url())))
    }

    /// `message` as the relay takes it: with the headers `post` sends.
    fn relayed_post(&self, message: &Value, headers: &[(String, String)]) -> RelayedPost {
        let value = |v: &HeaderValue| Secret(String::from_utf8_lossy(v.as_bytes()).into_owned());
        let mut all: Vec<(String, Secret)> = (self.headers.iter())
            .map(|(name, v)| (name.as_str().to_string(), value(v)))
            .collect();
        all.push((CONTENT_TYPE.as_str().into(), Secret(JSON.into())));
        all.push((ACCEPT.as_str().into(), Secret(ACCEPTED.into())));
        all.extend(headers.iter().map(|(n, v)| (n.clone(), Secret(v.clone()))));
        if let Some(session) = self.session() {
            all.push((SESSION.into(), Secret(session)));
        }
        RelayedPost {
            url: self.url.clone(),
            headers: all,
            body: message.to_string(),
        }
    }

    /// Sends the request with `headers` (the era's: its version, a modern
    /// request's method and names), and reads its answer, waiting up to
    /// `within`, or until `stop`. A legacy server that gives a session
    /// keeps it for the requests after. One that doesn't come in time is
    /// given up: a legacy server is told (`notifications/cancelled`), a
    /// modern one sees its stream closed, which is how it's told.
    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        headers: &[(String, String)],
        within: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<Value, Failure> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let message = rpc::request(id, method, params);
        let asked = async {
            let response = self.post(&message, headers).await?;
            self.answer_of(response, id, headers).await
        };
        match super::wait_for(asked, within, stop).await {
            Ok(outcome) => outcome,
            Err(failure) => {
                if self.has_session() {
                    let reason = match failure {
                        Failure::Stopped => "stopped by the user",
                        _ => "it took too long",
                    };
                    let params = serde_json::json!({ "requestId": id, "reason": reason });
                    self.notify("notifications/cancelled", params, headers)
                        .await;
                }
                Err(failure)
            }
        }
    }

    /// Sends a notification, whatever becomes of it.
    pub(super) async fn notify(&self, method: &str, params: Value, headers: &[(String, String)]) {
        let message = rpc::notification(method, params);
        let posted = tokio::time::timeout(NOTIFY_WITHIN, self.post(&message, headers)).await;
        if let Ok(Ok(response)) = posted {
            // Read to its end, so the connection is kept for the next.
            let _ = response.bytes().await;
        }
    }

    /// The answer to request `id` in `response`: its JSON, or the event
    /// with it in its stream, where a legacy server's own requests are
    /// answered as they come. An error status carries its JSON-RPC error
    /// when its body has one; a legacy session's 404 means the session is
    /// gone.
    async fn answer_of(
        &self,
        response: reqwest::Response,
        id: u64,
        headers: &[(String, String)],
    ) -> Result<Value, Failure> {
        let status = response.status();
        if let Some(session) = response
            .headers()
            .get(SESSION)
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.to_string());
        }
        let stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.trim_start().starts_with("text/event-stream"));
        if !status.is_success() {
            let body = read_capped(response, ERROR_BODY_SHOWN * 64).await;
            if status.as_u16() == 404 && self.has_session() {
                return Err(Failure::Http {
                    status: 404,
                    body: shown(&body),
                });
            }
            return Err(rpc::error_in(&body).unwrap_or_else(|| Failure::Http {
                status: status.as_u16(),
                body: shown(&body),
            }));
        }
        if !stream {
            let body = read_capped(response, MAX_BODY).await;
            let message: Value = serde_json::from_slice(&body).map_err(|_| {
                Failure::Gone(format!("the server's answer isn't JSON: {}", shown(&body)))
            })?;
            return match rpc::incoming(&message) {
                Incoming::Response { id: got, outcome } if got == id => outcome,
                _ => Err(Failure::Gone(
                    "the server's answer was for another request".into(),
                )),
            };
        }
        let mut chunks = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut searched = 0;
        while let Some(chunk) = chunks.next().await {
            let chunk =
                chunk.map_err(|e| Failure::Gone(crate::net::error_text(&e.without_url())))?;
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_BODY {
                return Err(Failure::Gone("the server's answer is too large".into()));
            }
            while let Some((at, length)) = crate::chat_request::find_sse_boundary(&buffer, searched)
            {
                let event: Vec<u8> = buffer.drain(..at).collect();
                buffer.drain(..length);
                searched = 0;
                let Some(message) = event_message(&event) else {
                    continue;
                };
                match rpc::incoming(&message) {
                    Incoming::Response { id: got, outcome } if got == id => return outcome,
                    Incoming::Request { id, method } => {
                        let answer = rpc::answer(&id, &method);
                        if let Ok(posted) = self.post(&answer, headers).await {
                            let _ = posted.bytes().await;
                        }
                    }
                    _ => {}
                }
            }
            searched = buffer.len();
        }
        // An answer the stream's end cut short of its blank line.
        if let Some(message) = event_message(&buffer) {
            if let Incoming::Response { id: got, outcome } = rpc::incoming(&message) {
                if got == id {
                    return outcome;
                }
            }
        }
        Err(Failure::Gone(
            "the server's stream ended before its answer".into(),
        ))
    }
}

/// The JSON message an event carries: its `data:` lines, joined.
fn event_message(event: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(event);
    let data: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(&data.join("\n")).ok()
}

/// The body, up to `max` bytes of it.
async fn read_capped(response: reqwest::Response, max: usize) -> Vec<u8> {
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(Ok(chunk)) = chunks.next().await {
        let room = max.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= max {
            break;
        }
    }
    body
}

/// The start of a body, for an error's text.
fn shown(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    let mut shown: String = text.chars().take(ERROR_BODY_SHOWN).collect();
    if shown.len() < text.len() {
        shown.push('…');
    }
    shown
}

/// A value as a `Mcp-Name` or `Mcp-Param-…` header carries it: as it is
/// when it's plain visible ASCII, else in the spec's Base64 form
/// (`=?base64?…?=`), which a value that looks like one takes too.
pub(super) fn header_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value.trim() == value
        && value.chars().all(|c| matches!(c, ' ' | '\t' | '!'..='~'))
        && !(value.starts_with("=?base64?") && value.ends_with("?="));
    if plain {
        return value.to_string();
    }
    use base64::Engine;
    format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's own examples (Streamable HTTP, "Value Encoding").
    #[test]
    fn header_values_are_plain_or_base64_as_the_spec_says() {
        assert_eq!(header_value("us-west1"), "us-west1");
        assert_eq!(
            header_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(header_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(header_value("line1\nline2"), "=?base64?bGluZTEKbGluZTI=?=");
        assert_eq!(
            header_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
    }

    #[test]
    fn an_event_s_data_lines_make_its_message() {
        assert_eq!(
            event_message(b"event: message\ndata: {\"a\":\ndata: 1}"),
            Some(serde_json::json!({ "a": 1 }))
        );
        assert_eq!(event_message(b": keepalive"), None);
        assert_eq!(
            event_message(b"data:{\"b\":2}"),
            Some(serde_json::json!({ "b": 2 }))
        );
    }

    #[test]
    fn a_url_must_be_http_and_headers_must_be_sendable() {
        assert!(Http::new("https://example.com/mcp", &[], None).is_ok());
        assert!(Http::new("ftp://example.com/mcp", &[], None).is_err());
        assert!(Http::new("not a url", &[], None).is_err());
        let bad = Http::new(
            "https://example.com/mcp",
            &[("Authorization".into(), "Bearer a\nb".into())],
            None,
        )
        .err()
        .unwrap();
        assert!(bad.contains("Authorization"), "{bad}");
        assert!(!bad.contains("Bearer"), "never its value: {bad}");
    }
}

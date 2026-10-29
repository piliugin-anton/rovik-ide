//! A URL's server (Streamable HTTP): each message Rovik sends is a POST,
//! answered with JSON or an event stream that ends with the answer
//! (docs/design/mcp-plan.md, Q03). It goes through the app's HTTP client,
//! so Settings → Network's proxies apply; on a daemon, a server on the
//! loopback of the user's computer is posted to through the app there
//! (mcp-plan-2.md, Q11). The old HTTP+SSE transport (sse.rs) sends its
//! requests the same way ([`Wire`]) and reads its stream's events alike
//! ([`Events`]).

use super::rpc::{self, Failure, Incoming};
use super::Relayed;
use crate::agent_run::StopSignal;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use rovik_protocol::mcp::RelayedPost;
use rovik_protocol::Secret;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The most an answer, or one event of a stream, may be: a big tool
/// result fits.
const MAX_BODY: usize = 64 << 20;
/// How much of an error's body its text keeps.
const ERROR_BODY_SHOWN: usize = 300;
/// The header of a legacy server's session.
const SESSION: &str = "mcp-session-id";
/// What a POST takes and what it accepts back.
pub(super) const JSON: &str = "application/json";
const ACCEPTED: &str = "application/json, text/event-stream";
/// What an event stream is.
const EVENT_STREAM: &str = "text/event-stream";
/// How long a notification's POST may take.
pub(super) const NOTIFY_WITHIN: Duration = Duration::from_secs(10);

/// The address of a URL's server, as its settings have it, checked: http
/// or https.
pub(super) fn checked_url(url: &str) -> Result<reqwest::Url, String> {
    let parsed =
        reqwest::Url::parse(url.trim()).map_err(|e| format!("The URL can't be used: {e}."))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("The URL must start with http:// or https://.".into());
    }
    Ok(parsed)
}

/// What a request's failure to be made says: the connection's error, its
/// URL left out.
fn unsent(e: reqwest::Error) -> Failure {
    Failure::Gone(crate::net::error_text(&e.without_url()))
}

/// How a URL server's requests go out: with the settings' headers, their
/// secrets filled in, through the app's HTTP client; or, on a daemon,
/// through the relay to the computer whose loopback the URL is on (Q11).
#[derive(Clone)]
pub(super) struct Wire {
    /// Never shown.
    headers: HeaderMap,
    relayed: Option<Relayed>,
}

impl Wire {
    /// Requests with these headers, through `relayed` when it's given. A
    /// header HTTP can't carry is refused, naming it but never its value.
    pub(super) fn new(
        headers: &[(String, String)],
        relayed: Option<Relayed>,
    ) -> Result<Wire, String> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let header = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| format!("The header name \"{name}\" can't be sent."))?;
            let mut value = HeaderValue::from_str(value.trim())
                .map_err(|_| format!("The value of the header {name} can't be sent."))?;
            value.set_sensitive(true);
            map.insert(header, value);
        }
        Ok(Wire {
            headers: map,
            relayed,
        })
    }

    /// POSTs `body` to `url`, with `extra` headers after the settings'.
    pub(super) async fn post(
        &self,
        url: &str,
        body: String,
        extra: &[(String, String)],
    ) -> Result<reqwest::Response, Failure> {
        if let Some((relay, computer)) = &self.relayed {
            let post = self.relayed_request(url, extra, body, false);
            return relay.post(computer, post).await.map_err(Failure::Gone);
        }
        let client = crate::net::client().await;
        let mut request = client.post(url).headers(self.headers.clone());
        for (name, value) in extra {
            request = request.header(name.as_str(), value.as_str());
        }
        request.body(body).send().await.map_err(unsent)
    }

    /// GETs the event stream at `url` (the HTTP+SSE transport, Q13).
    pub(super) async fn stream(&self, url: &str) -> Result<reqwest::Response, Failure> {
        let accept = [(ACCEPT.as_str().to_string(), EVENT_STREAM.to_string())];
        if let Some((relay, computer)) = &self.relayed {
            let get = self.relayed_request(url, &accept, String::new(), true);
            return relay.post(computer, get).await.map_err(Failure::Gone);
        }
        let client = crate::net::client().await;
        let request = client.get(url).headers(self.headers.clone());
        request
            .header(ACCEPT, EVENT_STREAM)
            .send()
            .await
            .map_err(unsent)
    }

    /// A request as the relay takes it: the settings' headers, then
    /// `extra`; a GET of an event stream when `stream` says so.
    fn relayed_request(
        &self,
        url: &str,
        extra: &[(String, String)],
        body: String,
        stream: bool,
    ) -> RelayedPost {
        let value = |v: &HeaderValue| Secret(String::from_utf8_lossy(v.as_bytes()).into_owned());
        let mut headers: Vec<(String, Secret)> = (self.headers.iter())
            .map(|(name, v)| (name.as_str().to_string(), value(v)))
            .collect();
        headers.extend(extra.iter().map(|(n, v)| (n.clone(), Secret(v.clone()))));
        RelayedPost {
            url: url.to_string(),
            headers,
            body,
            stream,
        }
    }
}

/// A server's address, how its requests go out, and a legacy server's
/// session.
pub(super) struct Http {
    url: String,
    wire: Wire,
    /// The session a legacy server gave (`Mcp-Session-Id`), once it did.
    session: Mutex<Option<String>>,
    next: AtomicU64,
}

impl Http {
    /// The server at `url`, its requests going out through `wire`. A URL
    /// that isn't http(s) is refused.
    pub(super) fn new(url: &str, wire: Wire) -> Result<Http, String> {
        Ok(Http {
            url: checked_url(url)?.to_string(),
            wire,
            session: Mutex::new(None),
            next: AtomicU64::new(1),
        })
    }

    /// Its address, and how its requests go out: for the old HTTP+SSE
    /// transport, which a server that refuses Streamable HTTP may speak
    /// (Q13).
    pub(super) fn parts(&self) -> (&str, &Wire) {
        (&self.url, &self.wire)
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
        let mut extra = vec![
            (CONTENT_TYPE.as_str().to_string(), JSON.to_string()),
            (ACCEPT.as_str().to_string(), ACCEPTED.to_string()),
        ];
        extra.extend(headers.iter().cloned());
        if let Some(session) = self.session() {
            extra.push((SESSION.to_string(), session));
        }
        self.wire.post(&self.url, message.to_string(), &extra).await
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
        let stream = is_event_stream(&response);
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
        let mut events = Events::of(response);
        while let Some(event) = events.next().await {
            let Some(message) = event_message(&event?) else {
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
        Err(Failure::Gone(
            "the server's stream ended before its answer".into(),
        ))
    }
}

/// Whether `response` is an event stream.
pub(super) fn is_event_stream(response: &reqwest::Response) -> bool {
    (response.headers().get(CONTENT_TYPE))
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.trim_start().starts_with(EVENT_STREAM))
}

/// An event stream's events as they come, each without the blank line
/// that ends it; the last one too, when the stream's end cuts it short of
/// that.
pub(super) struct Events {
    chunks: BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    buffer: Vec<u8>,
    /// How much of `buffer` holds no blank line.
    searched: usize,
    ended: bool,
}

impl Events {
    pub(super) fn of(response: reqwest::Response) -> Events {
        Events {
            chunks: response.bytes_stream().boxed(),
            buffer: Vec::new(),
            searched: 0,
            ended: false,
        }
    }

    /// The next event; none once the stream has ended. A connection that
    /// breaks, or an event past [`MAX_BODY`], ends it with that failure.
    pub(super) async fn next(&mut self) -> Option<Result<Vec<u8>, Failure>> {
        loop {
            let boundary = crate::chat_request::find_sse_boundary(&self.buffer, self.searched);
            if let Some((at, length)) = boundary {
                let event = self.buffer.drain(..at).collect();
                self.buffer.drain(..length);
                self.searched = 0;
                return Some(Ok(event));
            }
            self.searched = self.buffer.len();
            if self.ended {
                let rest = std::mem::take(&mut self.buffer);
                return (!rest.iter().all(u8::is_ascii_whitespace)).then_some(Ok(rest));
            }
            match self.chunks.next().await {
                Some(Ok(chunk)) => self.buffer.extend_from_slice(&chunk),
                Some(Err(e)) => return Some(Err(self.end(unsent(e)))),
                None => self.ended = true,
            }
            if self.buffer.len() > MAX_BODY {
                let failure = Failure::Gone("the server's answer is too large".into());
                return Some(Err(self.end(failure)));
            }
        }
    }

    /// Ends the stream with `failure`, what's left of it dropped.
    fn end(&mut self, failure: Failure) -> Failure {
        self.ended = true;
        self.buffer.clear();
        failure
    }
}

/// An event's field `name` (`data`, `event`): its lines' values, a line's
/// first space taken off, joined by line breaks. None without the field.
fn field(event: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let prefix = format!("{name}:");
    let lines: Vec<&str> = (text.lines())
        .filter_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|value| value.strip_prefix(' ').unwrap_or(value))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// An event's data.
pub(super) fn event_data(event: &[u8]) -> Option<String> {
    field(event, "data")
}

/// An event's name, as its `event:` line gives it.
pub(super) fn event_name(event: &[u8]) -> Option<String> {
    field(event, "event").map(|name| name.trim().to_string())
}

/// The JSON message an event carries: its data.
pub(super) fn event_message(event: &[u8]) -> Option<Value> {
    serde_json::from_str(&event_data(event)?).ok()
}

/// The body, up to `max` bytes of it.
pub(super) async fn read_capped(response: reqwest::Response, max: usize) -> Vec<u8> {
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
pub(super) fn shown(body: &[u8]) -> String {
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
        assert!(checked_url("https://example.com/mcp").is_ok());
        assert!(checked_url("ftp://example.com/mcp").is_err());
        assert!(checked_url("not a url").is_err());
        let bad = Wire::new(&[("Authorization".into(), "Bearer a\nb".into())], None)
            .err()
            .unwrap();
        assert!(bad.contains("Authorization"), "{bad}");
        assert!(!bad.contains("Bearer"), "never its value: {bad}");
    }

    /// An event's name and data, as an event stream's lines give them.
    #[test]
    fn an_event_s_name_and_data_are_its_lines() {
        let endpoint = b"event: endpoint\ndata: /messages?session=1";
        assert_eq!(event_name(endpoint).as_deref(), Some("endpoint"));
        assert_eq!(event_data(endpoint).as_deref(), Some("/messages?session=1"));
        assert_eq!(event_name(b"data: {}"), None);
        assert_eq!(event_data(b": keepalive"), None);
    }
}

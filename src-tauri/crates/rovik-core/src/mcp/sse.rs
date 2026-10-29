//! A URL's server on the old HTTP+SSE transport (2024-11-05; mcp-plan-2.md,
//! Q13): a GET opens an event stream whose first event, `endpoint`, names
//! where messages are POSTed, on the stream's own origin; the answers to
//! them, and the server's own requests, come on the stream. Its requests go
//! out as Streamable HTTP's do (http.rs's `Wire`): through the app's
//! proxies, and on a daemon through the relay to the computer whose
//! loopback the URL is on (Q11).

use super::http::{
    event_data, event_message, event_name, is_event_stream, read_capped, shown, Events, Wire, JSON,
    NOTIFY_WITHIN,
};
use super::rpc::{self, Failure, Incoming};
use crate::agent_run::StopSignal;
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::oneshot;

/// How much of an error's body is read.
const ERROR_BODY: usize = 16 << 10;

/// The requests waiting for their answers on the stream, by id.
type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Failure>>>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A server spoken to on its event stream.
pub(super) struct Sse {
    /// Where messages are POSTed, as its stream named it.
    endpoint: String,
    wire: Wire,
    next: AtomicU64,
    waiting: Waiting,
    /// Why its stream ended, once it has.
    ended: Arc<Mutex<Option<String>>>,
    /// What reads its stream, stopped with it.
    reader: tokio::task::JoinHandle<()>,
}

impl Sse {
    /// Opens the event stream at `url`, its requests going out through
    /// `wire`, and reads it from its `endpoint` event on: within `within`.
    /// One that isn't an event stream, or that names an endpoint on another
    /// origin, is refused.
    pub(super) async fn open(url: &str, wire: Wire, within: Duration) -> Result<Sse, Failure> {
        let base = reqwest::Url::parse(url)
            .map_err(|e| Failure::Gone(format!("The URL can't be used: {e}.")))?;
        let opened = async {
            let response = wire.stream(url).await?;
            let status = response.status();
            if !status.is_success() {
                let body = read_capped(response, ERROR_BODY).await;
                return Err(rpc::error_in(&body).unwrap_or_else(|| Failure::Http {
                    status: status.as_u16(),
                    body: shown(&body),
                }));
            }
            if !is_event_stream(&response) {
                return Err(Failure::Gone(
                    "the server's answer to the GET of its events isn't an event stream".into(),
                ));
            }
            let mut events = Events::of(response);
            while let Some(event) = events.next().await {
                let event = event?;
                if event_name(&event).as_deref() == Some("endpoint") {
                    let named = event_data(&event).unwrap_or_default();
                    return Ok((endpoint(&base, named.trim())?, events));
                }
            }
            Err(Failure::Gone(
                "the server's event stream ended before it named where to post".into(),
            ))
        };
        let (endpoint, events) = tokio::time::timeout(within, opened)
            .await
            .map_err(|_| Failure::TimedOut)??;
        let waiting = Waiting::default();
        let ended = Arc::new(Mutex::new(None));
        let reader = tokio::spawn(read(
            events,
            waiting.clone(),
            ended.clone(),
            wire.clone(),
            endpoint.clone(),
        ));
        Ok(Sse {
            endpoint,
            wire,
            next: AtomicU64::new(1),
            waiting,
            ended,
            reader,
        })
    }

    /// Why its stream ended, once it has: it can't be spoken to any more.
    pub(super) fn ended(&self) -> Option<String> {
        lock(&self.ended).clone()
    }

    /// Sends the request and waits for its answer on the stream, up to
    /// `within`, or until `stop`. One that doesn't come in time is given
    /// up, the server told (`notifications/cancelled`).
    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        within: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<Value, Failure> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answered, answer) = oneshot::channel();
        lock(&self.waiting).insert(id, answered);
        // A stream that ended before the request waited fails it now: no
        // answer can come.
        if let Some(why) = self.ended() {
            lock(&self.waiting).remove(&id);
            return Err(Failure::Gone(why));
        }
        let asked = async {
            post(
                &self.wire,
                &self.endpoint,
                &rpc::request(id, method, params),
            )
            .await?;
            answer.await.unwrap_or_else(|_| {
                let why = self.ended();
                Err(Failure::Gone(
                    why.unwrap_or_else(|| "the server's stream ended".into()),
                ))
            })
        };
        let outcome = super::wait_for(asked, within, stop).await;
        lock(&self.waiting).remove(&id);
        match outcome {
            Ok(outcome) => outcome,
            Err(failure) => {
                let reason = match failure {
                    Failure::Stopped => "stopped by the user",
                    _ => "it took too long",
                };
                let params = json!({ "requestId": id, "reason": reason });
                self.notify("notifications/cancelled", params).await;
                Err(failure)
            }
        }
    }

    /// Sends a notification, whatever becomes of it.
    pub(super) async fn notify(&self, method: &str, params: Value) {
        let message = rpc::notification(method, params);
        let sent = post(&self.wire, &self.endpoint, &message);
        if let Ok(Err(failure)) = tokio::time::timeout(NOTIFY_WITHIN, sent).await {
            log::info!(
                "an MCP server's notification {method} failed: {}",
                failure.text()
            );
        }
    }

    /// Stops reading its stream, which closes it.
    pub(super) fn stop(&self) {
        self.reader.abort();
    }
}

impl Drop for Sse {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Where a server's event stream says its messages go, as the app sees the
/// stream it relays for a host (Q11, Q13): fed the stream's bytes as they
/// come, it gives the endpoint its `endpoint` event names, once, resolved
/// against the stream's URL and on its origin. The app posts there for the
/// host while the stream lasts.
pub struct EndpointWatch {
    url: reqwest::Url,
    buffer: Vec<u8>,
    searched: usize,
    done: bool,
}

impl EndpointWatch {
    /// A watch on the event stream at `url`; none for a URL that isn't one.
    pub fn new(url: &str) -> Option<EndpointWatch> {
        Some(EndpointWatch {
            url: reqwest::Url::parse(url).ok()?,
            buffer: Vec::new(),
            searched: 0,
            done: false,
        })
    }

    /// The endpoint, once `bytes` complete its event. An endpoint on
    /// another origin is no endpoint; nothing is looked for past the first.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        if self.done {
            return None;
        }
        self.buffer.extend_from_slice(bytes);
        while let Some((at, length)) =
            crate::chat_request::find_sse_boundary(&self.buffer, self.searched)
        {
            let event: Vec<u8> = self.buffer.drain(..at).collect();
            self.buffer.drain(..length);
            self.searched = 0;
            if event_name(&event).as_deref() == Some("endpoint") {
                self.done = true;
                self.buffer = Vec::new();
                let named = event_data(&event).unwrap_or_default();
                return endpoint(&self.url, named.trim()).ok();
            }
        }
        self.searched = self.buffer.len();
        // An event this long is no endpoint's: none is looked for.
        if self.buffer.len() > ERROR_BODY {
            self.done = true;
            self.buffer = Vec::new();
        }
        None
    }
}

/// Where the stream at `base` says to post, `named` resolved against it:
/// on its own origin only, so a server can't have Rovik post elsewhere.
fn endpoint(base: &reqwest::Url, named: &str) -> Result<String, Failure> {
    let joined = base.join(named).map_err(|_| {
        Failure::Gone(format!(
            "the server named where to post as \"{named}\", which isn't a URL"
        ))
    })?;
    if joined.origin() != base.origin() {
        return Err(Failure::Gone(format!(
            "the server named {} to post to, not its own {}: Rovik doesn't post there",
            joined.origin().ascii_serialization(),
            base.origin().ascii_serialization()
        )));
    }
    Ok(joined.to_string())
}

/// POSTs `message` to `endpoint`: accepted, its answer to come on the
/// stream; or refused, as its status or JSON-RPC error says.
async fn post(wire: &Wire, endpoint: &str, message: &Value) -> Result<(), Failure> {
    let json = [(CONTENT_TYPE.as_str().to_string(), JSON.to_string())];
    let response = wire.post(endpoint, message.to_string(), &json).await?;
    let status = response.status();
    // Read to its end, so the connection is kept for the next.
    let body = read_capped(response, ERROR_BODY).await;
    if status.is_success() {
        return Ok(());
    }
    Err(rpc::error_in(&body).unwrap_or_else(|| Failure::Http {
        status: status.as_u16(),
        body: shown(&body),
    }))
}

/// Reads the stream: each answer goes to the request waiting for it, and
/// each of the server's own requests is answered (`rpc::answer`). Its end,
/// or a break, fails what still waits, saying why.
async fn read(
    mut events: Events,
    waiting: Waiting,
    ended: Arc<Mutex<Option<String>>>,
    wire: Wire,
    endpoint: String,
) {
    let why = loop {
        let event = match events.next().await {
            None => break "the server's event stream ended".to_string(),
            Some(Err(failure)) => {
                break format!("the server's event stream broke: {}", failure.text())
            }
            Some(Ok(event)) => event,
        };
        let Some(message) = event_message(&event) else {
            continue;
        };
        match rpc::incoming(&message) {
            Incoming::Response { id, outcome } => {
                if let Some(answered) = lock(&waiting).remove(&id) {
                    let _ = answered.send(outcome);
                }
            }
            Incoming::Request { id, method } => {
                // On a task of its own: the stream is read on meanwhile.
                let (wire, endpoint) = (wire.clone(), endpoint.clone());
                tokio::spawn(async move {
                    let answer = rpc::answer(&id, &method);
                    if let Err(failure) = post(&wire, &endpoint, &answer).await {
                        log::info!(
                            "an MCP server's {method} wasn't answered: {}",
                            failure.text()
                        );
                    }
                });
            }
            Incoming::Other => {}
        }
    };
    *lock(&ended) = Some(why.clone());
    for (_, answered) in lock(&waiting).drain() {
        let _ = answered.send(Err(Failure::Gone(why.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q13: the stream names where to post, on its own origin; a path is
    /// taken as the stream's, and another origin is refused.
    #[test]
    fn the_endpoint_is_on_the_stream_s_own_origin() {
        let base = reqwest::Url::parse("http://127.0.0.1:8931/sse").unwrap();
        assert_eq!(
            endpoint(&base, "/messages?sessionId=1").unwrap(),
            "http://127.0.0.1:8931/messages?sessionId=1"
        );
        assert_eq!(
            endpoint(&base, "http://127.0.0.1:8931/m").unwrap(),
            "http://127.0.0.1:8931/m"
        );
        for elsewhere in [
            "http://evil.example/m",
            "https://127.0.0.1:8931/m",
            "http://127.0.0.1:9/m",
        ] {
            let refused = endpoint(&base, elsewhere).unwrap_err().text();
            assert!(refused.contains("Rovik doesn't post there"), "{refused}");
        }
    }

    /// The app finds the endpoint in the stream it relays, however the
    /// chunks cut it, once; on another origin it finds none.
    #[test]
    fn the_endpoint_is_found_in_a_relayed_stream_however_it_s_cut() {
        let mut watch = EndpointWatch::new("http://localhost:8931/sse").unwrap();
        assert_eq!(watch.feed(b": hello\n\nevent: endp"), None);
        assert_eq!(watch.feed(b"oint\ndata: /messages?s=1\r"), None);
        assert_eq!(
            watch.feed(b"\n\r\n").as_deref(),
            Some("http://localhost:8931/messages?s=1")
        );
        assert_eq!(
            watch.feed(b"event: endpoint\ndata: /other\n\n"),
            None,
            "once"
        );
        let mut far = EndpointWatch::new("http://localhost:8931/sse").unwrap();
        assert_eq!(
            far.feed(b"event: endpoint\ndata: http://evil.example/m\n\n"),
            None
        );
        assert!(EndpointWatch::new("not a url").is_none());
    }
}

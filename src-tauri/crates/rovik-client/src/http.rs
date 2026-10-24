//! HTTP to the daemon, each request on a connection of its own (4.1, K41):
//! `POST /call` for a call or a reply too big for a frame, the bundle that
//! moves a chat at `/chats/import` (6.8), the attachments' files at
//! `/blobs/<sha256>` (6.11), the projects' packs at `/packs` (6.4), a byte
//! stream into the user's private network at `/forward` (D20), and
//! `/health`, which needs no token, for the connection test (H04).

use crate::client::{unreachable, Identity};
use crate::connector::{Connector, Stream};
use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes, Frame as BodyFrame, Incoming, SizeHint};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, UPGRADE};
use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use rovik_protocol::packs::{Fetch, Pushed, Refs, Update};
use rovik_protocol::sync::Imported;
use rovik_protocol::{
    Fail, FailCode, Frame, Response, FORWARD_UPGRADE, MAX_BLOB, MAX_BUNDLE, MAX_FRAME,
};
use serde::de::DeserializeOwned;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// How much of a file goes at a time, and between two progress reports.
const CHUNK: usize = 64 << 10;

/// The most a small JSON answer (refs, a push's or an import's result, a
/// refusal, `/health`) takes: what a frame does.
const ANSWER: usize = MAX_FRAME;

/// The most a `POST /call` answer takes: the most the protocol moves at
/// once, a chat's rows (the daemon's replies have no smaller bound).
const CALL_ANSWER: usize = MAX_BUNDLE;
const JSON: &str = "application/json";
const FILE: &str = "application/octet-stream";
const PACK: &str = "application/x-git-packed-objects";

/// The reply to `call`, the text of a `Call` frame.
pub async fn post_call(
    connector: &dyn Connector,
    identity: &Identity,
    call: String,
) -> Result<Response, Fail> {
    let body = Full::new(Bytes::from(call));
    let (status, body) = exchange(
        connector,
        identity,
        Method::POST,
        "/call",
        JSON,
        body,
        CALL_ANSWER,
    )
    .await?;
    if status != StatusCode::OK {
        return Err(refusal(status, &body));
    }
    match serde_json::from_slice(&body) {
        Ok(Frame::Reply { res, .. }) => res,
        _ => Err(Fail::new(
            FailCode::Internal,
            "The daemon's answer isn't a reply.",
        )),
    }
}

/// Sends the bundle that moves a chat, as JSON, for the move `move_id`,
/// telling `progress` the bytes sent so far.
pub async fn import_chat(
    connector: &dyn Connector,
    identity: &Identity,
    move_id: &str,
    bundle: Vec<u8>,
    progress: Box<dyn Fn(u64) + Send + Sync>,
) -> Result<Imported, Fail> {
    let body = Upload {
        bytes: Bytes::from(bundle),
        sent: 0,
        progress,
    };
    let path = format!("/chats/import?move_id={move_id}");
    let (status, body) =
        exchange(connector, identity, Method::POST, &path, JSON, body, ANSWER).await?;
    reply(status, &body)
}

/// Sends an attachment's file, telling `progress` the bytes sent so far.
pub async fn put_blob(
    connector: &dyn Connector,
    identity: &Identity,
    id: &str,
    bytes: Vec<u8>,
    progress: Box<dyn Fn(u64) + Send + Sync>,
) -> Result<(), Fail> {
    let body = Upload {
        bytes: Bytes::from(bytes),
        sent: 0,
        progress,
    };
    let path = format!("/blobs/{id}");
    let (status, body) =
        exchange(connector, identity, Method::PUT, &path, FILE, body, ANSWER).await?;
    if status == StatusCode::OK {
        Ok(())
    } else {
        Err(refusal(status, &body))
    }
}

/// An attachment's file, from the daemon; none when it has none.
pub async fn get_blob(
    connector: &dyn Connector,
    identity: &Identity,
    id: &str,
) -> Result<Option<Bytes>, Fail> {
    let path = format!("/blobs/{id}");
    let body = Empty::<Bytes>::new();
    let (status, body) = exchange(
        connector,
        identity,
        Method::GET,
        &path,
        FILE,
        body,
        MAX_BLOB,
    )
    .await?;
    match status {
        StatusCode::OK => Ok(Some(body)),
        StatusCode::NOT_FOUND => Ok(None),
        status => Err(refusal(status, &body)),
    }
}

/// The project's refs on the host: the tips a pack for it is built
/// against.
pub async fn pack_refs(
    connector: &dyn Connector,
    identity: &Identity,
    project: &str,
) -> Result<Refs, Fail> {
    let path = format!("/packs/{project}/refs");
    let body = Empty::<Bytes>::new();
    let (status, body) =
        exchange(connector, identity, Method::GET, &path, JSON, body, ANSWER).await?;
    reply(status, &body)
}

/// Sends a pack as `pieces` bring it, for `update` to move its ref,
/// telling `progress` the bytes sent so far.
pub async fn push_pack(
    connector: &dyn Connector,
    identity: &Identity,
    project: &str,
    update: &Update,
    pieces: mpsc::Receiver<Bytes>,
    progress: Box<dyn Fn(u64) + Send + Sync>,
) -> Result<Pushed, Fail> {
    let path = format!("/packs/{project}?{}", update.query());
    let body = Streamed {
        pieces,
        sent: 0,
        progress,
    };
    let (status, body) =
        exchange(connector, identity, Method::POST, &path, PACK, body, ANSWER).await?;
    reply(status, &body)
}

/// Asks the host for a fetch's pack: its body, to be read as it comes.
pub async fn fetch_pack(
    connector: &dyn Connector,
    identity: &Identity,
    project: &str,
    fetch: &Fetch,
) -> Result<Incoming, Fail> {
    let path = format!("/packs/{project}/fetch");
    let body = Full::new(Bytes::from(serde_json::to_vec(fetch).unwrap_or_default()));
    let response = send(connector, identity, Method::POST, &path, JSON, body).await?;
    if response.status() == StatusCode::OK {
        return Ok(response.into_body());
    }
    let status = response.status();
    let body = Limited::new(response.into_body(), ANSWER).collect().await;
    let body = body
        .map_err(|e| too_large(&*e, ANSWER).unwrap_or_else(|| unreachable(&identity.host)))?
        .to_bytes();
    Err(refusal(status, &body))
}

/// One request with this computer's token, on a connection of its own:
/// the response's status and body, of at most `limit` bytes. A daemon
/// that sends more is refused as it does, never held whole in memory.
async fn exchange<B>(
    connector: &dyn Connector,
    identity: &Identity,
    method: Method,
    path: &str,
    content_type: &str,
    body: B,
    limit: usize,
) -> Result<(StatusCode, Bytes), Fail>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let response = send(connector, identity, method, path, content_type, body).await?;
    let status = response.status();
    let (body, moved) = Watched::new(response.into_body());
    let body = while_moving(Limited::new(body, limit).collect(), &moved, STALL_WAIT)
        .await
        .ok_or_else(|| unreachable(&identity.host))?
        .map_err(|e| too_large(&*e, limit).unwrap_or_else(|| unreachable(&identity.host)))?
        .to_bytes();
    Ok((status, body))
}

/// The refusal for an answer over `limit` bytes, when that's why reading
/// it failed (`Limited`'s error); None for any other failure.
fn too_large(error: &(dyn std::error::Error + 'static), limit: usize) -> Option<Fail> {
    error.downcast_ref::<LengthLimitError>()?;
    Some(Fail::new(
        FailCode::TooLarge,
        format!(
            "The daemon's answer is over the {} MB it may be.",
            limit.div_ceil(1 << 20)
        ),
    ))
}

/// A body that notes when it last moved (a frame read or written), for
/// [`while_moving`].
struct Watched<B> {
    inner: Pin<Box<B>>,
    moved: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
}

/// When a [`Watched`] body last moved.
type Moved = std::sync::Arc<std::sync::Mutex<std::time::Instant>>;

impl<B> Watched<B> {
    fn new(inner: B) -> (Watched<B>, Moved) {
        let moved: Moved = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
        let body = Watched {
            inner: Box::pin(inner),
            moved: moved.clone(),
        };
        (body, moved)
    }
}

impl<B: Body> Body for Watched<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<BodyFrame<Self::Data>, Self::Error>>> {
        let polled = self.inner.as_mut().poll_frame(cx);
        if polled.is_ready() {
            *self.moved.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// `work`'s outcome, unless its body stops moving for `stall` (`moved`):
/// None then. A body on a slow link takes as long as it takes; one on a
/// link that died stops.
async fn while_moving<F: std::future::Future>(
    work: F,
    moved: &Moved,
    stall: std::time::Duration,
) -> Option<F::Output> {
    tokio::pin!(work);
    loop {
        let since = moved.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        let left = stall.checked_sub(since).filter(|l| !l.is_zero())?;
        if let Ok(done) = tokio::time::timeout(left, &mut work).await {
            return Some(done);
        }
    }
}

/// How long the daemon has to answer a request's headers once it's sent:
/// a link that half-dies (no keepalive on a Unix socket) would otherwise
/// hold the caller, and whatever waits on it, for good.
const RESPONSE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a body may go without moving, a request's being sent or a
/// reply's arriving, before the link counts as dead; and how long the
/// daemon has to answer once the request's body is all sent (it answers
/// only then). A first snapshot's pack on a slow uplink takes as long as
/// it takes, so long as it moves.
const STALL_WAIT: std::time::Duration = RESPONSE_WAIT;

/// One request with this computer's token, on a connection of its own:
/// the response, with its body still to come. A body of unknown length
/// goes chunked.
async fn send<B>(
    connector: &dyn Connector,
    identity: &Identity,
    method: Method,
    path: &str,
    content_type: &str,
    body: B,
) -> Result<hyper::Response<Incoming>, Fail>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let io = tokio::time::timeout(RESPONSE_WAIT, connector.connect())
        .await
        .map_err(|_| unreachable(&identity.host))?
        .map_err(|_| unreachable(&identity.host))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|_| unreachable(&identity.host))?;
    tokio::spawn(connection);
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, "rovik-agentd")
        .header(AUTHORIZATION, format!("Bearer {}", identity.token.0))
        .header(CONTENT_TYPE, content_type);
    if let Some(length) = body.size_hint().exact() {
        request = request.header(CONTENT_LENGTH, length);
    }
    // The daemon answers once it has the body: a request that carries one
    // (a blob, a pack, a chat's import) is answered in the body's time, on
    // a link that may be slow, so the wait is for the body to move, then
    // for the answer once it's all sent.
    let (body, moved) = Watched::new(body);
    let request = request
        .body(body)
        .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    while_moving(sender.send_request(request), &moved, STALL_WAIT)
        .await
        .ok_or_else(|| unreachable(&identity.host))?
        .map_err(|_| unreachable(&identity.host))
}

/// A byte stream to `port` on the loopback of the user's private network
/// on the host (D20), on a connection of its own: one connection of a port
/// this computer forwards for a page. Refused with why when nothing
/// listens there, or the host gives its commands no private network.
pub async fn forward(
    connector: &dyn Connector,
    identity: &Identity,
    port: u16,
) -> Result<Stream, Fail> {
    let lost = || unreachable(&identity.host);
    let io = tokio::time::timeout(RESPONSE_WAIT, connector.connect())
        .await
        .map_err(|_| lost())?
        .map_err(|_| lost())?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|_| lost())?;
    // Upgraded, the connection is the stream's: it ends with the stream.
    tokio::spawn(connection.with_upgrades());
    let request = hyper::Request::get(format!("/forward/{port}"))
        .header(HOST, "rovik-agentd")
        .header(AUTHORIZATION, format!("Bearer {}", identity.token.0))
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, FORWARD_UPGRADE)
        .body(Empty::<Bytes>::new())
        .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    let response = tokio::time::timeout(RESPONSE_WAIT, sender.send_request(request))
        .await
        .map_err(|_| lost())?
        .map_err(|_| lost())?;
    let status = response.status();
    if status != StatusCode::SWITCHING_PROTOCOLS {
        let body = Limited::new(response.into_body(), ANSWER).collect().await;
        let body = body.map(|b| b.to_bytes()).unwrap_or_default();
        return Err(refusal(status, &body));
    }
    let upgraded = hyper::upgrade::on(response).await.map_err(|_| lost())?;
    Ok(Box::new(TokioIo::new(upgraded)))
}

/// What `/health` answers.
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Health {
    pub ok: bool,
    pub protocol: u32,
    pub version: String,
}

/// How long `/health` may take, connecting included: a daemon that takes
/// the connection and never answers would otherwise hang a deploy or a
/// check for good.
const HEALTH_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// The daemon's `/health`: that it answers, and its version.
pub async fn health(connector: &dyn Connector) -> Result<Health, Fail> {
    match tokio::time::timeout(HEALTH_WAIT, health_now(connector)).await {
        Ok(answer) => answer,
        Err(_) => Err(Fail::new(
            FailCode::Unreachable,
            "The daemon didn't answer /health in time.",
        )),
    }
}

async fn health_now(connector: &dyn Connector) -> Result<Health, Fail> {
    let lost = |why: String| Fail::new(FailCode::Unreachable, why);
    let io = connector.connect().await.map_err(|e| lost(e.to_string()))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| lost(e.to_string()))?;
    tokio::spawn(connection);
    let request = hyper::Request::get("/health")
        .header(HOST, "rovik-agentd")
        .body(Empty::<Bytes>::new())
        .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| lost(e.to_string()))?;
    let status = response.status();
    let body = Limited::new(response.into_body(), ANSWER).collect().await;
    let body = body
        .map_err(|e| too_large(&*e, ANSWER).unwrap_or_else(|| lost(e.to_string())))?
        .to_bytes();
    reply(status, &body)
}

/// The JSON a request asked for, or the refusal its status says.
fn reply<T: DeserializeOwned>(status: StatusCode, body: &[u8]) -> Result<T, Fail> {
    if status != StatusCode::OK {
        return Err(refusal(status, body));
    }
    serde_json::from_slice(body).map_err(|_| {
        Fail::new(
            FailCode::Internal,
            "The daemon's answer isn't what was asked.",
        )
    })
}

/// A refusal says why, as a `Fail`.
fn refusal(status: StatusCode, body: &[u8]) -> Fail {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| Fail::new(FailCode::Internal, format!("The daemon answered {status}.")))
}

/// A pack sent as it's built, telling `progress` how far it got.
struct Streamed {
    pieces: mpsc::Receiver<Bytes>,
    sent: u64,
    progress: Box<dyn Fn(u64) + Send + Sync>,
}

impl Body for Streamed {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<BodyFrame<Bytes>, Infallible>>> {
        let Poll::Ready(piece) = self.pieces.poll_recv(cx) else {
            return Poll::Pending;
        };
        Poll::Ready(piece.map(|piece| {
            self.sent += piece.len() as u64;
            (self.progress)(self.sent);
            Ok(BodyFrame::data(piece))
        }))
    }
}

/// A file sent a chunk at a time, telling `progress` how far it got.
struct Upload {
    bytes: Bytes,
    sent: usize,
    progress: Box<dyn Fn(u64) + Send + Sync>,
}

impl Body for Upload {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<BodyFrame<Bytes>, Infallible>>> {
        if self.sent >= self.bytes.len() {
            return Poll::Ready(None);
        }
        let end = (self.sent + CHUNK).min(self.bytes.len());
        let chunk = self.bytes.slice(self.sent..end);
        self.sent = end;
        (self.progress)(end as u64);
        Poll::Ready(Some(Ok(BodyFrame::data(chunk))))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact((self.bytes.len() - self.sent) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A body that keeps moving is waited for however long it takes; one
    /// that stops is given up once it hasn't moved for the stall time.
    #[tokio::test]
    async fn a_body_is_waited_for_while_it_moves_and_not_once_it_stalls() {
        let stall = Duration::from_millis(200);
        let moved: Moved = std::sync::Arc::new(std::sync::Mutex::new(Instant::now()));
        let mover = moved.clone();
        // Moves every 50 ms for 800 ms: four times the stall time in all.
        let work = async move {
            for _ in 0..16 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                *mover.lock().unwrap() = Instant::now();
            }
            "sent"
        };
        assert_eq!(while_moving(work, &moved, stall).await, Some("sent"));

        let still: Moved = std::sync::Arc::new(std::sync::Mutex::new(Instant::now()));
        let started = Instant::now();
        let never = std::future::pending::<()>();
        assert_eq!(while_moving(never, &still, stall).await, None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// An answer over its limit is refused as too large, not read whole.
    #[tokio::test]
    async fn an_answer_over_its_limit_is_refused_as_too_large() {
        let body = Full::new(Bytes::from(vec![0u8; 2 << 20]));
        let read = Limited::new(body, 1 << 20).collect().await;
        let error = read.expect_err("over the limit");
        let fail = too_large(&*error, 1 << 20).expect("a TooLarge refusal");
        assert_eq!(fail.code, FailCode::TooLarge);
        assert!(fail.message.contains("1 MB"), "{}", fail.message);
        let other: Box<dyn std::error::Error + Send + Sync> = "reset".into();
        assert!(too_large(&*other, 1 << 20).is_none());
    }

    /// Reading a watched body moves it.
    #[tokio::test]
    async fn a_watched_body_notes_when_it_moves() {
        let (body, moved) = Watched::new(Full::new(Bytes::from_static(b"pack")));
        let before = *moved.lock().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"pack");
        assert!(*moved.lock().unwrap() > before);
    }
}

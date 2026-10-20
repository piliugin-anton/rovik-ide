//! HTTP to the daemon, each request on a connection of its own (4.1, K41):
//! `POST /call` for a call or a reply too big for a frame, the bundle that
//! moves a chat at `/chats/import` (6.8), the attachments' files at
//! `/blobs/<sha256>` (6.11), the projects' packs at `/packs` (6.4), and
//! `/health`, which needs no token, for the connection test (H04).

use crate::client::{unreachable, Identity};
use crate::connector::Connector;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Body, Bytes, Frame as BodyFrame, Incoming, SizeHint};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use rovik_protocol::packs::{Fetch, Pushed, Refs, Update};
use rovik_protocol::sync::Imported;
use rovik_protocol::{Fail, FailCode, Frame, Response};
use serde::de::DeserializeOwned;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// How much of a file goes at a time, and between two progress reports.
const CHUNK: usize = 64 << 10;
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
    let (status, body) = exchange(connector, identity, Method::POST, "/call", JSON, body).await?;
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
    let (status, body) = exchange(connector, identity, Method::POST, &path, JSON, body).await?;
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
    let (status, body) = exchange(connector, identity, Method::PUT, &path, FILE, body).await?;
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
    let (status, body) = exchange(connector, identity, Method::GET, &path, FILE, body).await?;
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
    let (status, body) = exchange(connector, identity, Method::GET, &path, JSON, body).await?;
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
    let (status, body) = exchange(connector, identity, Method::POST, &path, PACK, body).await?;
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
    let body = response.into_body().collect().await;
    let body = body.map_err(|_| unreachable(&identity.host))?.to_bytes();
    Err(refusal(status, &body))
}

/// One request with this computer's token, on a connection of its own:
/// the response's status and body.
async fn exchange<B>(
    connector: &dyn Connector,
    identity: &Identity,
    method: Method,
    path: &str,
    content_type: &str,
    body: B,
) -> Result<(StatusCode, Bytes), Fail>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let response = send(connector, identity, method, path, content_type, body).await?;
    let status = response.status();
    let body = tokio::time::timeout(BODY_WAIT, response.into_body().collect())
        .await
        .map_err(|_| unreachable(&identity.host))?
        .map_err(|_| unreachable(&identity.host))?
        .to_bytes();
    Ok((status, body))
}

/// How long the daemon has to answer a request's headers once it's sent:
/// a link that half-dies (no keepalive on a Unix socket) would otherwise
/// hold the caller, and whatever waits on it, for good.
const RESPONSE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a reply's body may take to arrive whole: they're bounded
/// (`MAX_CALL`, `MAX_BLOB`), on a link that may be slow. A request's body
/// too: the daemon answers once it has it all.
const BODY_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// A request body up to this is as good as none: answered in a header's
/// time, not a body's.
const SMALL_BODY: u64 = 64 * 1024;

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
    // a link that may be slow, not in a header's.
    let wait = match body.size_hint().upper() {
        Some(n) if n <= SMALL_BODY => RESPONSE_WAIT,
        _ => BODY_WAIT,
    };
    let request = request
        .body(body)
        .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    tokio::time::timeout(wait, sender.send_request(request))
        .await
        .map_err(|_| unreachable(&identity.host))?
        .map_err(|_| unreachable(&identity.host))
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
    let body = response.into_body().collect().await;
    let body = body.map_err(|e| lost(e.to_string()))?.to_bytes();
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

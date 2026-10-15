//! HTTP to the daemon, each request on a connection of its own (4.1, K41):
//! `POST /call` for a call or a reply too big for a frame, and the
//! attachments' files at `/blobs/<sha256>` (6.11).

use crate::client::{unreachable, Identity};
use crate::connector::Connector;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Body, Bytes, Frame as BodyFrame, SizeHint};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use rovik_protocol::{Fail, FailCode, Frame, Response};
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

/// How much of a file goes at a time, and between two progress reports.
const CHUNK: usize = 64 << 10;
const JSON: &str = "application/json";
const FILE: &str = "application/octet-stream";

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
    let lost = |_| unreachable(&identity.host);
    let io = connector.connect().await.map_err(lost)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|_| unreachable(&identity.host))?;
    tokio::spawn(connection);
    let length = body.size_hint().exact().unwrap_or(0);
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, "rovik-agentd")
        .header(AUTHORIZATION, format!("Bearer {}", identity.token.0))
        .header(CONTENT_TYPE, content_type)
        .header(CONTENT_LENGTH, length)
        .body(body)
        .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| unreachable(&identity.host))?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|_| unreachable(&identity.host))?
        .to_bytes();
    Ok((status, body))
}

/// A refusal says why, as a `Fail`.
fn refusal(status: StatusCode, body: &[u8]) -> Fail {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| Fail::new(FailCode::Internal, format!("The daemon answered {status}.")))
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

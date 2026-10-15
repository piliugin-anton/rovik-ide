//! `POST /call` (4.1): a call, or a call whose reply came back `TooLarge`,
//! on a connection of its own, with the `Call` as its body and the `Reply`
//! as its response.

use crate::client::{unreachable, Identity};
use crate::connector::Connector;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use rovik_protocol::{Fail, FailCode, Frame, Response};

/// The reply to `call`, the text of a `Call` frame.
pub async fn post_call(
    connector: &dyn Connector,
    identity: &Identity,
    call: String,
) -> Result<Response, Fail> {
    let lost = |_| unreachable(&identity.host);
    let io = connector.connect().await.map_err(lost)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|_| unreachable(&identity.host))?;
    tokio::spawn(connection);
    let request = hyper::Request::post("/call")
        .header(HOST, "rovik-agentd")
        .header(AUTHORIZATION, format!("Bearer {}", identity.token.0))
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(call)))
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
    if status != StatusCode::OK {
        // A refusal says why, as a `Fail`.
        return Err(serde_json::from_slice(&body).unwrap_or_else(|_| {
            Fail::new(FailCode::Internal, format!("The daemon answered {status}."))
        }));
    }
    match serde_json::from_slice(&body) {
        Ok(Frame::Reply { res, .. }) => res,
        _ => Err(Fail::new(
            FailCode::Internal,
            "The daemon's answer isn't a reply.",
        )),
    }
}

//! HTTP on the daemon's socket (4.4): `/health`, the WebSocket at `/rpc`,
//! and `/call` for a call too big for a frame. Only the daemon's own user
//! can connect (the socket is 0600 and each peer's uid is checked, D5), and
//! every request but `/health` names its computer with a token.

use crate::cli::VERSION;
use crate::daemon::Daemon;
use crate::session;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, SEC_WEBSOCKET_ACCEPT,
    SEC_WEBSOCKET_KEY, UPGRADE,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rovik_protocol::{Fail, FailCode, Frame, MAX_CALL, PROTOCOL};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/rpc", get(rpc))
        .route("/call", post(call))
        .with_state(daemon)
}

/// No token: a client sees whether the daemon is there, and which.
async fn health() -> Response {
    json(
        StatusCode::OK,
        &serde_json::json!({ "ok": true, "protocol": PROTOCOL, "version": VERSION }),
    )
}

/// A call too big for a frame: the `Call` in, its `Reply` out. A reply is
/// sent whole, however big: this is where a `TooLarge` one is asked again.
async fn call(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    request: Request,
) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<usize>().ok());
    let too_large = || {
        refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            Fail::new(
                FailCode::TooLarge,
                format!("A call takes {} MB at most.", MAX_CALL >> 20),
            ),
        )
    };
    if declared.is_some_and(|n| n > MAX_CALL) {
        return too_large();
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_CALL).await else {
        return too_large();
    };
    let Ok(Frame::Call { id, req }) = serde_json::from_slice::<Frame>(&body) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            Fail::new(FailCode::BadRequest, "The body isn't a call."),
        );
    };
    let res = daemon.answer(&computer, req).await;
    json(StatusCode::OK, &Frame::Reply { id, res })
}

/// The WebSocket of 4.1. tungstenite speaks it once the upgrade is done;
/// the session checks the `Hello` it starts with.
async fn rpc(
    State(daemon): State<Arc<Daemon>>,
    computer: Computer,
    mut request: Request,
) -> Response {
    let Some(key) = websocket_key(request.headers()) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            Fail::new(FailCode::BadRequest, "/rpc is a WebSocket."),
        );
    };
    let Some(upgrade) = request
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>()
    else {
        return refusal(
            StatusCode::BAD_REQUEST,
            Fail::new(FailCode::BadRequest, "The connection can't be upgraded."),
        );
    };
    tokio::spawn(async move {
        match upgrade.await {
            Ok(io) => session::run(daemon, computer.0, hyper_util::rt::TokioIo::new(io)).await,
            Err(e) => log::warn!("a WebSocket upgrade failed: {e}"),
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, "websocket")
        .header(SEC_WEBSOCKET_ACCEPT, derive_accept_key(key.as_bytes()))
        .body(Body::empty())
        .unwrap_or_default()
}

fn websocket_key(headers: &HeaderMap) -> Option<String> {
    let has = |name, value: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case(value))
    };
    if !(has(CONNECTION, "upgrade") && has(UPGRADE, "websocket")) {
        return None;
    }
    headers
        .get(SEC_WEBSOCKET_KEY)?
        .to_str()
        .ok()
        .map(String::from)
}

/// The computer a request comes from, named by its token: `Authorization:
/// Bearer <token>` (4.4).
pub struct Computer(pub String);

impl FromRequestParts<Arc<Daemon>> for Computer {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        daemon: &Arc<Daemon>,
    ) -> Result<Computer, Response> {
        parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok()?.strip_prefix("Bearer "))
            .and_then(|token| daemon.tokens.computer(token.trim()))
            .map(|c| Computer(c.to_string()))
            .ok_or_else(|| {
                refusal(
                    StatusCode::UNAUTHORIZED,
                    Fail::new(
                        FailCode::BadRequest,
                        "This computer's token isn't one this server knows.",
                    ),
                )
            })
    }
}

/// A request refused before it got to the daemon, with why.
fn refusal(status: StatusCode, fail: Fail) -> Response {
    json(status, &fail)
}

fn json(status: StatusCode, body: &impl serde::Serialize) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    (
        status,
        [(CONTENT_TYPE, "application/json")],
        Bytes::from(bytes),
    )
        .into_response()
}

/// The socket, bound with mode 0600. A socket file left from an earlier
/// daemon goes only once connecting to it fails: one that answers is a
/// daemon still running.
pub fn bind(path: &Path) -> Result<UnixListener, String> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(format!("A daemon already listens at {}.", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(listener)
}

/// Accepts only connections from the daemon's own user: other users of a
/// team server can't reach it even if the socket's mode were opened (D5).
pub struct OwnUser {
    pub listener: UnixListener,
    pub uid: u32,
}

impl axum::serve::Listener for OwnUser {
    type Io = UnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    async fn accept(&mut self) -> (UnixStream, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, addr)) => match stream.peer_cred() {
                    Ok(peer) if peer.uid() == self.uid => return (stream, addr),
                    Ok(peer) => log::warn!("refused a connection from uid {}", peer.uid()),
                    Err(e) => log::warn!("refused a connection of an unknown user: {e}"),
                },
                Err(e) => {
                    log::warn!("accepting a connection failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

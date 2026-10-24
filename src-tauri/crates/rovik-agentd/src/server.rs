//! HTTP on the daemon's socket (4.4): `/health`, the WebSocket at `/rpc`,
//! `/call` for a call too big for a frame, the bundle that moves a chat at
//! `/chats/import`, the attachments' files at `/blobs`, and the projects'
//! packs at `/packs`. Only the daemon's own user can connect (the socket
//! is 0600 and each peer's uid is checked, D5), and every request but
//! `/health` names its computer with a token.

use crate::cli::VERSION;
use crate::daemon::Daemon;
use crate::moves;
use crate::session;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, SEC_WEBSOCKET_ACCEPT,
    SEC_WEBSOCKET_KEY, UPGRADE,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use rovik_core::chat_db::attachments;
use rovik_core::rt;
use rovik_protocol::packs::{Fetch, Pushed, Update};
use rovik_protocol::sync::ChatBundle;
use rovik_protocol::{Fail, FailCode, Frame, MAX_BLOB, MAX_BUNDLE, MAX_CALL, PROTOCOL};
use std::convert::Infallible;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/rpc", get(rpc))
        .route("/call", post(call))
        .route("/chats/import", post(import_chat))
        .route("/blobs/{id}", get(blob).put(put_blob))
        .route("/packs/{project}/refs", get(pack_refs))
        .route("/packs/{project}", post(push_pack))
        .route("/packs/{project}/fetch", post(fetch_pack))
        .with_state(daemon)
}

/// How long a pack may go without a byte moving before it's given up (4.4):
/// a big one over a slow link takes minutes, but its bytes keep moving.
const IDLE: Duration = Duration::from_secs(60);
/// How long a body read whole (a call, a file) may take to arrive: bounded
/// in size, on a link that may be slow; a client that stalls holds no task
/// for good.
const BODY_WAIT: Duration = Duration::from_secs(10 * 60);
/// A pack's pieces on their way between the socket and the repository.
const PIECES: usize = 16;

/// No token: a client sees whether the daemon is there, and which.
async fn health() -> Response {
    json(
        StatusCode::OK,
        &serde_json::json!({ "ok": true, "protocol": PROTOCOL, "version": VERSION }),
    )
}

/// The size a request's body declares, when it does.
fn declared_length(request: &Request) -> Option<usize> {
    request
        .headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// A call too big for a frame: the `Call` in, its `Reply` out. A reply is
/// sent whole, however big: this is where a `TooLarge` one is asked again.
async fn call(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    request: Request,
) -> Response {
    let declared = declared_length(&request);
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
    let body = match whole_body(request.into_body(), MAX_CALL, too_large).await {
        Ok(body) => body,
        Err(response) => return response,
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

/// The bundle that moves a chat here (6.8), parsed as it comes: at most
/// `MAX_BUNDLE`, and refused whole when it doesn't all come. Its rows go in
/// in one transaction, once (moves.rs).
async fn import_chat(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    request: Request,
) -> Response {
    let move_id = request.uri().query().and_then(|query| {
        let mut pairs = query.split('&').filter_map(|pair| pair.split_once('='));
        pairs
            .find(|(k, _)| *k == "move_id")
            .map(|(_, v)| v.to_string())
    });
    let Some(move_id) = move_id else {
        return failed(Fail::new(
            FailCode::BadRequest,
            "An import names its move: ?move_id=…",
        ));
    };
    let too_large = || {
        failed(Fail::new(
            FailCode::TooLarge,
            format!("A chat moves with {} MB of rows at most.", MAX_BUNDLE >> 20),
        ))
    };
    let declared = declared_length(&request);
    if declared.is_some_and(|n| n > MAX_BUNDLE) {
        return too_large();
    }
    let (pieces, taken) = mpsc::channel::<Bytes>(PIECES);
    let parsing = rt::spawn_blocking(move || {
        let body = std::io::BufReader::with_capacity(1 << 16, Pieces::new(taken));
        serde_json::from_reader::<_, ChatBundle>(body)
    });
    let mut body = request.into_body().into_data_stream();
    let mut size = 0;
    let came = loop {
        match tokio::time::timeout(IDLE, body.next()).await {
            Ok(Some(Ok(piece))) => {
                size += piece.len();
                if size > MAX_BUNDLE {
                    break Came::TooMuch;
                }
                // The parser stopped: what it found wrong says why.
                if pieces.send(piece).await.is_err() {
                    break Came::Unread;
                }
            }
            Ok(None) => break Came::Whole,
            // Quiet for too long, or broken off.
            Ok(Some(Err(_))) | Err(_) => break Came::Cut,
        }
    };
    drop(pieces);
    let bundle = match (came, parsing.await) {
        (Came::TooMuch, _) => return too_large(),
        (Came::Whole, Ok(Ok(bundle))) => bundle,
        (Came::Whole | Came::Unread, Ok(Err(e))) => {
            return failed(Fail::new(
                FailCode::BadRequest,
                format!("The body isn't a chat's bundle: {e}"),
            ))
        }
        (_, Err(e)) => return failed(Fail::new(FailCode::Internal, e.to_string())),
        _ => {
            return failed(Fail::new(
                FailCode::BadRequest,
                "The chat's bundle didn't all come: send it again.",
            ))
        }
    };
    match moves::import(&daemon, &computer, &move_id, bundle).await {
        Ok(imported) => json(StatusCode::OK, &imported),
        Err(fail) => failed(fail),
    }
}

/// How much of an import's body came.
enum Came {
    Whole,
    /// Not all: the connection went quiet or broke.
    Cut,
    TooMuch,
    /// The bundle's parser stopped before the end.
    Unread,
}

/// A body's pieces as they come, read as one stream.
struct Pieces {
    taken: mpsc::Receiver<Bytes>,
    piece: Bytes,
}

impl Pieces {
    fn new(taken: mpsc::Receiver<Bytes>) -> Pieces {
        Pieces {
            taken,
            piece: Bytes::new(),
        }
    }
}

impl std::io::Read for Pieces {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.piece.is_empty() {
            match self.taken.blocking_recv() {
                Some(piece) => self.piece = piece,
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.piece.len());
        buf[..n].copy_from_slice(&self.piece.split_to(n));
        Ok(n)
    }
}

/// An attachment's file (6.11), such as a screenshot a run took here, for
/// the app to show.
async fn blob(State(daemon): State<Arc<Daemon>>, _: Computer, Path(id): Path<String>) -> Response {
    let data_dir = daemon.core.db.data_dir().to_path_buf();
    match rt::spawn_blocking(move || attachments::read(&data_dir, &id)).await {
        Ok(Some((mime, bytes))) => {
            (StatusCode::OK, [(CONTENT_TYPE, mime)], Bytes::from(bytes)).into_response()
        }
        _ => refusal(
            StatusCode::NOT_FOUND,
            Fail::new(FailCode::BadRequest, "The server has no such file."),
        ),
    }
}

/// A file of a message about to be sent (6.11): it must be what its name
/// says, its SHA-256, and it's stored as the chat input stores one, its
/// type read from its bytes.
async fn put_blob(
    State(daemon): State<Arc<Daemon>>,
    _: Computer,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let too_large = || {
        refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            Fail::new(
                FailCode::TooLarge,
                format!("A file takes {} MB at most.", MAX_BLOB >> 20),
            ),
        )
    };
    let declared = declared_length(&request);
    if declared.is_some_and(|n| n > MAX_BLOB) {
        return too_large();
    }
    if let Err(fail) = room_on_disk(&daemon).await {
        return failed(fail);
    }
    let body = match whole_body(request.into_body(), MAX_BLOB, too_large).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    if attachments::id_of(&body) != id {
        return refusal(
            StatusCode::BAD_REQUEST,
            Fail::new(FailCode::BadRequest, "The file doesn't match its name."),
        );
    }
    match attachments::store_attached(&daemon.core.db, body.to_vec()).await {
        Ok(stored) if stored.id == id => json(StatusCode::OK, &stored),
        Ok(_) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            Fail::new(
                FailCode::BadRequest,
                "The image isn't one the app prepared: it changed when stored.",
            ),
        ),
        Err(reason) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            Fail::new(FailCode::BadRequest, reason),
        ),
    }
}

/// The project's refs, which a client builds its pack against (6.4).
async fn pack_refs(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    Path(project): Path<String>,
) -> Response {
    // Another computer's project's refs (its chats') aren't this one's.
    if let Err(fail) = daemon.store.check_project_owner(&computer, &project) {
        return failed(fail);
    }
    let projects = daemon.projects.clone();
    match rt::spawn_blocking(move || projects.refs(&project)).await {
        Ok(Ok(refs)) => json(StatusCode::OK, &refs),
        Ok(Err(fail)) => failed(fail),
        Err(e) => failed(Fail::new(FailCode::Internal, e.to_string())),
    }
}

/// A pack that moves one of the project's refs (6.4). It's taken in as it
/// comes, and the ref moves only once it's all here and the commit is
/// complete. A pack cut short leaves no ref, and is sent again whole.
async fn push_pack(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    Path(project): Path<String>,
    request: Request,
) -> Response {
    let Some(update) = request.uri().query().and_then(Update::parse) else {
        return failed(Fail::new(
            FailCode::BadRequest,
            "A push names a ref under refs/rovik/, and the commits it moves from and to.",
        ));
    };
    // A project's refs move only for the computer that carried it here.
    if let Err(fail) = daemon.store.check_project_owner(&computer, &project) {
        return failed(fail);
    }
    // A chat's refs move only for the computer whose chat it is, and every
    // ref a client may push names its chat (`packs::pushable`).
    let Some(chat) = chat_of_ref(&update.name) else {
        return failed(Fail::new(
            FailCode::BadRequest,
            "A push moves a chat's snapshot ref.",
        ));
    };
    if let Err(fail) = daemon.store.check_owner(&computer, chat) {
        return failed(fail);
    }
    if let Err(fail) = room_on_disk(&daemon).await {
        return failed(fail);
    }
    let (pieces, mut taken) = mpsc::channel::<Vec<u8>>(PIECES);
    let projects = daemon.projects.clone();
    let receiving = rt::spawn_blocking(move || {
        let pieces = std::iter::from_fn(|| taken.blocking_recv());
        projects.receive(&project, &update, pieces)
    });
    let mut body = request.into_body().into_data_stream();
    while let Ok(Some(Ok(piece))) = tokio::time::timeout(IDLE, body.next()).await {
        if pieces.send(piece.to_vec()).await.is_err() {
            break;
        }
    }
    drop(pieces);
    match receiving.await {
        Ok(Ok(objects)) => json(StatusCode::OK, &Pushed { objects }),
        Ok(Err(fail)) => failed(fail),
        Err(e) => failed(Fail::new(FailCode::Internal, e.to_string())),
    }
}

/// Whether Rovik's folders are under the server's disk cap (5.5): a push or
/// an upload adds to them only then.
async fn room_on_disk(daemon: &Daemon) -> Result<(), Fail> {
    let caps = daemon.caps.clone();
    let room = rt::spawn_blocking(move || caps.room_on_disk()).await;
    let room = room.map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
    room.map_err(|why| Fail::new(FailCode::Busy, why))
}

/// The chat whose refs these are: `refs/rovik/chats/<chat>/...`.
fn chat_of_ref(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("refs/rovik/chats/")?;
    let (chat, _) = rest.split_once('/')?;
    (!chat.is_empty()).then_some(chat)
}

/// The pack of what a client wants from the project and lacks (6.4), sent
/// as it's built.
async fn fetch_pack(
    State(daemon): State<Arc<Daemon>>,
    Computer(computer): Computer,
    Path(project): Path<String>,
    body: Bytes,
) -> Response {
    // Nor its objects, the code of its chats.
    if let Err(fail) = daemon.store.check_project_owner(&computer, &project) {
        return failed(fail);
    }
    let Ok(fetch) = serde_json::from_slice::<Fetch>(&body) else {
        return failed(Fail::new(FailCode::BadRequest, "The body isn't a fetch."));
    };
    let projects = daemon.projects.clone();
    let (checked, ready) = oneshot::channel();
    let (pieces, built) = mpsc::channel::<Result<Bytes, Infallible>>(PIECES);
    rt::spawn_blocking(move || {
        let wanted = match projects.wanted(&project, &fetch) {
            Ok(wanted) => wanted,
            Err(fail) => {
                let _ = checked.send(Err(fail));
                return;
            }
        };
        let _ = checked.send(Ok(()));
        let mut piece = |piece: &[u8]| {
            pieces
                .blocking_send(Ok(Bytes::copy_from_slice(piece)))
                .is_ok()
        };
        if let Err(fail) = wanted.send(&mut piece) {
            log::warn!("a fetch of {project} stopped: {fail}");
        }
    });
    match ready.await {
        Ok(Ok(())) => {
            let pack = futures_util::stream::unfold(built, |mut built| async move {
                built.recv().await.map(|piece| (piece, built))
            });
            let kind = [(CONTENT_TYPE, "application/x-git-packed-objects")];
            (StatusCode::OK, kind, Body::from_stream(pack)).into_response()
        }
        Ok(Err(fail)) => failed(fail),
        Err(_) => failed(Fail::new(FailCode::Internal, "The pack couldn't be built.")),
    }
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

/// A request's body, whole: within `limit` bytes, and within BODY_WAIT.
/// Else the reply that says so: `too_large` past the limit (a link that
/// broke reads the same, the body short of its length), or, past the
/// time, one asking for the request again rather than calling it too big
/// for good.
async fn whole_body(
    body: axum::body::Body,
    limit: usize,
    too_large: impl Fn() -> Response,
) -> Result<Bytes, Response> {
    match tokio::time::timeout(BODY_WAIT, axum::body::to_bytes(body, limit)).await {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(_)) => Err(too_large()),
        Err(_) => Err(refusal(
            StatusCode::REQUEST_TIMEOUT,
            Fail::new(
                FailCode::Busy,
                "The request's body didn't arrive in time: send it again.",
            ),
        )),
    }
}

/// A request refused before it got to the daemon, with why.
fn refusal(status: StatusCode, fail: Fail) -> Response {
    json(status, &fail)
}

/// A refusal with the status its code calls for: a moved ref is a conflict,
/// another computer's chat is forbidden.
fn failed(fail: Fail) -> Response {
    let status = match fail.code {
        FailCode::BadRequest => StatusCode::BAD_REQUEST,
        FailCode::Busy => StatusCode::CONFLICT,
        FailCode::NotOwner => StatusCode::FORBIDDEN,
        FailCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    refusal(status, fail)
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
pub fn bind(path: &std::path::Path) -> Result<UnixListener, String> {
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

//! What the daemon tests share: the real binary started on a temporary
//! socket and data folder, in a process of its own so its statics never
//! meet the test's (plan section 9), and a small client for it: HTTP and
//! the WebSocket of 4.1 over any stream.

#![allow(dead_code)] // each test file uses part of it

use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request as HttpRequest, StatusCode};
use hyper_util::rt::TokioIo;
use rovik_core::agent_run::permissions::{ApprovalRule, Approver, Permissions};
use rovik_core::agent_run::{AgentConfig, ModelProfile, StartKind, StartRequest, UserMessage};
use rovik_core::testing::{temp_dir, TempDir};
use rovik_protocol::{Fail, Frame, Request, Response, Secret, PROTOCOL};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use tokio_tungstenite::WebSocketStream;

/// The computers config.toml names.
pub const COMPUTER: &str = "laptop";
pub const OTHER: &str = "desk";

pub const BINARY: &str = env!("CARGO_BIN_EXE_rovik-agentd");

pub struct Daemon {
    child: Child,
    /// What `serve` was given after its own arguments.
    args: Vec<String>,
    pub socket: PathBuf,
    /// The daemon's folder (`--data`).
    pub data: PathBuf,
    /// The tokens of `COMPUTER` and `OTHER`.
    pub token: String,
    pub other_token: String,
    /// Goes after the daemon.
    pub dir: TempDir,
}

/// A stored message, as the daemon's database has it.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub role: String,
    pub content: String,
    pub finalized: bool,
    /// Null when it has none.
    pub meta: Value,
    pub error: Option<String>,
}

impl Daemon {
    /// A daemon serving `COMPUTER`, with `args` after its own.
    pub async fn start(args: &[&str]) -> Daemon {
        let dir = temp_dir();
        let data = dir.0.join("rovik");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (token, other_token) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        std::fs::write(
            data.join("config.toml"),
            format!(
                "[tokens]\n{COMPUTER} = \"{}\"\n{OTHER} = \"{}\"\n",
                sha256_hex(&token),
                sha256_hex(&other_token)
            ),
        )
        .unwrap();
        let socket = dir.0.join("agentd.sock");
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let child = spawn(&dir, &data, &socket, &args);
        let mut daemon = Daemon {
            child,
            args,
            socket,
            data,
            token,
            other_token,
            dir,
        };
        daemon.wait_until_it_answers().await;
        daemon
    }

    /// `kill -9`: the daemon gets no chance to stop its runs.
    pub async fn kill(&mut self) {
        self.child.start_kill().unwrap();
        self.child.wait().await.unwrap();
    }

    /// The daemon again, on the same folder, socket and arguments.
    pub async fn restart(&mut self) {
        self.child = spawn(&self.dir, &self.data, &self.socket, &self.args);
        self.wait_until_it_answers().await;
    }

    async fn wait_until_it_answers(&mut self) {
        for _ in 0..300 {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("the daemon exited ({status}):\n{}", self.output());
            }
            if UnixStream::connect(&self.socket).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the daemon didn't answer:\n{}", self.output());
    }

    /// Its stderr and its log, for a failing test's message.
    pub fn output(&self) -> String {
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        format!(
            "stderr:\n{}\nlog:\n{}",
            read(self.dir.0.join("stderr")),
            self.log()
        )
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.data.join("logs/agentd.log")).unwrap_or_default()
    }

    /// The chat's stored messages, in order: none while the database is
    /// busy, so callers poll.
    pub fn rows(&self, dialog_id: &str) -> Vec<Row> {
        use rusqlite::{Connection, OpenFlags};
        let read = || -> rusqlite::Result<Vec<Row>> {
            let path = self.data.join("data/chats.db");
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut query = db.prepare(
                "SELECT id, role, content, finalized, meta, error FROM messages \
                 WHERE dialog_id = ?1 ORDER BY seq",
            )?;
            let rows = query.query_map([dialog_id], |r| {
                let meta: Option<String> = r.get(4)?;
                Ok(Row {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    finalized: r.get::<_, i64>(3)? != 0,
                    meta: meta
                        .and_then(|m| serde_json::from_str(&m).ok())
                        .unwrap_or_default(),
                    error: r.get(5)?,
                })
            })?;
            rows.collect()
        };
        read().unwrap_or_default()
    }

    /// state.json, as the daemon last wrote it.
    pub fn state(&self) -> Value {
        let text = std::fs::read_to_string(self.data.join("state.json")).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub async fn stream(&self) -> UnixStream {
        UnixStream::connect(&self.socket).await.unwrap()
    }

    /// An HTTP request on a connection of its own.
    pub async fn http(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, Bytes) {
        http(self.stream().await, method, path, token, body).await
    }

    /// `POST /call` with `COMPUTER`'s token: the reply, as a frame.
    pub async fn post_call(&self, id: u64, req: Request) -> Frame {
        let body = serde_json::to_vec(&Frame::Call { id, req }).unwrap();
        let (status, body) = self.http("POST", "/call", Some(&self.token), body).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    /// A WebSocket session of `COMPUTER`, past its Welcome.
    pub async fn session(&self) -> Session<UnixStream> {
        self.session_as(COMPUTER, &self.token).await
    }

    pub async fn session_as(&self, computer: &str, token: &str) -> Session<UnixStream> {
        Session::open_as(self.stream().await, computer, token)
            .await
            .unwrap_or_else(|e| panic!("no session: {e}\n{}", self.output()))
    }
}

/// `serve` on `data` and `socket`, its stderr added to the folder's.
fn spawn(dir: &TempDir, data: &Path, socket: &Path, args: &[String]) -> Child {
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.0.join("stderr"))
        .unwrap();
    Command::new(BINARY)
        .arg("serve")
        .arg("--data")
        .arg(data)
        .arg("--socket")
        .arg(socket)
        .arg("--no-virtual-files")
        .args(args)
        // A login shell that answers at once, and no real home.
        .env("SHELL", "/bin/sh")
        .env("HOME", &dir.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

/// Every file under `dir`.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else if path.is_file() {
            files.push(path);
        }
    }
    files
}

/// What `check` gives once it gives something, polled for up to 30 s.
pub async fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    for _ in 0..1200 {
        if let Some(found) = check() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never: {what}");
}

pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub async fn http<S>(
    io: S,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Bytes)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .unwrap();
    tokio::spawn(connection);
    let mut request = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("host", "rovik-agentd");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = sender
        .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        response.into_body().collect().await.unwrap().to_bytes(),
    )
}

/// A WebSocket upgrade at /rpc with this token.
pub async fn websocket<S>(
    io: S,
    token: &str,
) -> Result<WebSocketStream<S>, tokio_tungstenite::tungstenite::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let uri: Uri = "ws://rovik-agentd/rpc".parse().unwrap();
    let request =
        ClientRequestBuilder::new(uri).with_header("Authorization", format!("Bearer {token}"));
    tokio_tungstenite::client_async(request, io)
        .await
        .map(|(socket, _)| socket)
}

pub struct Session<S> {
    pub socket: WebSocketStream<S>,
    pub welcome: Frame,
    next_id: u64,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Hello as `COMPUTER` with `token`, and the daemon's Welcome.
    pub async fn open(io: S, token: &str) -> Result<Session<S>, String> {
        Session::open_as(io, COMPUTER, token).await
    }

    pub async fn open_as(io: S, computer: &str, token: &str) -> Result<Session<S>, String> {
        let mut socket = websocket(io, token).await.map_err(|e| e.to_string())?;
        let hello = Frame::Hello {
            protocol: PROTOCOL,
            client: "daemon tests".into(),
            device: computer.into(),
            token: Secret(token.into()),
        };
        send(&mut socket, &hello).await;
        match recv(&mut socket).await {
            Some(welcome @ Frame::Welcome { .. }) => Ok(Session {
                socket,
                welcome,
                next_id: 1,
            }),
            other => Err(format!("no Welcome: {other:?}")),
        }
    }

    pub async fn send(&mut self, frame: &Frame) {
        send(&mut self.socket, frame).await
    }

    pub async fn recv(&mut self) -> Option<Frame> {
        recv(&mut self.socket).await
    }

    /// The daemon's boot, from its Welcome.
    pub fn boot(&self) -> String {
        match &self.welcome {
            Frame::Welcome { boot, .. } => boot.clone(),
            _ => unreachable!(),
        }
    }

    /// Subscribes: how many events were replayed, and whether that was
    /// everything after `since`.
    pub async fn subscribe(&mut self, since: Option<(String, u64)>) -> (u64, bool) {
        self.send(&Frame::Subscribe { since }).await;
        loop {
            match self.recv().await.expect("the daemon closed the session") {
                Frame::Subscribed { replayed, complete } => return (replayed, complete),
                _ => continue,
            }
        }
    }

    /// The events that come (each its seq and `ev` as JSON), up to the
    /// first that `last` matches.
    pub async fn events_until(&mut self, last: impl Fn(&Value) -> bool) -> Vec<(u64, Value)> {
        let mut events = Vec::new();
        loop {
            match self.recv().await.expect("the daemon closed the session") {
                Frame::Event { seq, ev } => {
                    let ev = serde_json::to_value(ev).unwrap();
                    let done = last(&ev);
                    events.push((seq, ev));
                    if done {
                        return events;
                    }
                }
                _ => continue,
            }
        }
    }

    /// The next frame, if one comes within `wait`.
    pub async fn recv_within(&mut self, wait: Duration) -> Option<Frame> {
        tokio::time::timeout(wait, self.recv()).await.ok().flatten()
    }

    /// A call, and its reply; frames that come meanwhile are dropped.
    pub async fn call(&mut self, req: Request) -> Result<Response, Fail> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Frame::Call { id, req }).await;
        loop {
            match self.recv().await.expect("the daemon closed the session") {
                Frame::Reply { id: got, res } if got == id => return res,
                _ => continue,
            }
        }
    }
}

pub fn hello(token: &str) -> Frame {
    Frame::Hello {
        protocol: PROTOCOL,
        client: "daemon tests".into(),
        device: COMPUTER.into(),
        token: Secret(token.into()),
    }
}

pub async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    frame: &Frame,
) {
    let text = serde_json::to_string(frame).unwrap();
    socket.send(Message::Text(text.into())).await.unwrap();
}

/// The next frame, or none once the daemon has closed.
pub async fn recv<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Option<Frame> {
    let wait = Duration::from_secs(30);
    loop {
        match tokio::time::timeout(wait, socket.next()).await.ok()?? {
            Ok(Message::Text(text)) => return Some(serde_json::from_str(&text).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

/// The close frame's code and reason, once the daemon closes the session.
pub async fn closed_with<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Option<(u16, String)> {
    loop {
        match socket.next().await? {
            Ok(Message::Close(Some(frame))) => {
                return Some((frame.code.into(), frame.reason.to_string()))
            }
            Ok(Message::Close(None)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

/// A start of chat-1: `text` sent to the model at `url`, working in
/// `project`, with edits waiting for the user's approval.
pub fn start_request(url: &str, project: &std::path::Path, text: String) -> StartRequest {
    StartRequest {
        dialog_id: "chat-1".into(),
        start: StartKind::Send {
            message: UserMessage {
                id: "u1".into(),
                content: text,
                meta: None,
                attachments: Vec::new(),
            },
        },
        profile: ModelProfile {
            id: "p::m".into(),
            label: "Test/m".into(),
            model: "m".into(),
            base_url: url.into(),
            api_key: String::new(),
            stream: true,
            vision: false,
            pdf: false,
            // Room for the 3 MB message.
            context_tokens: Some(4_000_000),
            no_tools: false,
            first_byte_timeout_ms: None,
            api_route: None,
        },
        options: None,
        mode: Some("agent".into()),
        context_mode: Some("trimming".into()),
        root: Some(project.to_string_lossy().into_owned()),
        current_user_content: None,
        local_date: Some("2026-10-15".into()),
        config: Some(AgentConfig {
            permissions: Permissions {
                edit_approval: ApprovalRule {
                    required: true,
                    by: Approver::User,
                },
                verify_changes: false,
                ..Permissions::default()
            },
            ..AgentConfig::default()
        }),
        isolate: None,
        project_settings: None,
    }
}

/// Writes a file, and its folders.
pub fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Commits every file under `root` that no ignore rule names, in a
/// repository created the first time.
pub fn commit_all(root: &Path) -> git2::Oid {
    let repo =
        git2::Repository::open(root).unwrap_or_else(|_| git2::Repository::init(root).unwrap());
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let me = git2::Signature::now("Test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &me, &me, "work", &tree, &parents)
        .unwrap()
}

//! A message's images and PDFs to and from the daemon (P09, 6.11): they go
//! up before the message, once, and again when the daemon lost them; Stop
//! cancels a slow upload; a file only the daemon has comes down before its
//! row; and the daemon refuses what isn't a file it takes.

mod common;

use common::{start_request, until, Daemon, COMPUTER};
use rovik_client::UnixConnector;
use rovik_client::{CloudEvent, Connecting, Connector, HostState, Identity, Remote, Stream};
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::{StartKind, StartRequest, UserMessage};
use rovik_core::chat_db::attachments;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::testing::{model, temp_dir, Reply, TempDir};
use rovik_core::{Core, EventSink, Paths};
use rovik_protocol::{Request, Response, Secret};
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const HOST: &str = "server";
const CHAT: &str = "chat-1";

/// A 1×1 PNG.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A one-page PDF, padded to `size` bytes with a comment.
fn pdf_file(size: usize) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
        2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
        3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 3 3]>>endobj\n\
        trailer<</Root 1 0 R>>\n%"
        .to_vec();
    pdf.resize(size.saturating_sub(6).max(pdf.len()), b' ');
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
}

/// The app: its core, the events it sends the frontend and the upload
/// progress, and its remote chats.
struct App {
    core: Core,
    events: Arc<Mutex<Vec<Value>>>,
    uploads: Arc<Mutex<Vec<CloudEvent>>>,
    remote: Arc<Remote>,
    _dir: TempDir,
}

/// The frontend's view: each event, and for each `messagesAdded`, whether
/// the files its rows name were here by then.
struct Frontend {
    data: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
}

impl EventSink for Frontend {
    fn agent(&self, event: &AgentEvent) {
        let mut seen = serde_json::to_value(event).unwrap();
        if let AgentEvent::MessagesAdded { messages, .. } = event {
            let named = messages.iter().flat_map(|m| m.attachments.iter());
            let here = named
                .clone()
                .all(|id| attachments::file_for(&self.data, id).is_some());
            seen["filesHere"] = json!(named.count() > 0 && here);
        }
        self.events.lock().unwrap().push(seen);
    }
}

impl App {
    async fn new(daemon: &Daemon) -> App {
        App::through(daemon, UnixConnector(daemon.socket.clone())).await
    }

    /// An app of `COMPUTER`'s, reaching the daemon through `connector`,
    /// with `CHAT` there.
    async fn through(daemon: &Daemon, connector: impl Connector) -> App {
        let dir = temp_dir();
        let events = Arc::default();
        let frontend = Frontend {
            data: dir.0.join("data"),
            events: Arc::clone(&events),
        };
        let paths = Paths::new(dir.0.join("data"), dir.0.join("config"));
        let core = Core::open(paths, Arc::new(frontend)).unwrap();
        let dialog = NewDialog {
            id: CHAT.into(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: None,
        };
        core.db
            .write(move |c| store::create_dialog(c, &dialog))
            .await
            .unwrap();
        let remote = Remote::new(core.clone());
        let uploads: Arc<Mutex<Vec<CloudEvent>>> = Arc::default();
        let progress = Arc::clone(&uploads);
        remote.on_cloud(Arc::new(move |event| progress.lock().unwrap().push(event)));
        let identity = Identity {
            host: HOST.into(),
            computer: COMPUTER.into(),
            token: Secret(daemon.token.clone()),
            client: "attachment tests".into(),
        };
        remote.connect(HOST, connector, identity);
        remote.place(CHAT, HOST);
        until("the connection", || {
            (remote.states() == [(HOST.to_string(), HostState::Ready)]).then_some(())
        })
        .await;
        App {
            core,
            events,
            uploads,
            remote,
            _dir: dir,
        }
    }

    /// A file attached in the input, stored here as the app stores it.
    async fn attach(&self, bytes: Vec<u8>) -> String {
        attachments::store_attached(&self.core.db, bytes)
            .await
            .unwrap()
            .id
    }

    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    /// Bytes uploaded so far, by the progress the app was told.
    fn uploaded(&self) -> u64 {
        let uploads = self.uploads.lock().unwrap();
        let progress: Vec<(&str, u64)> = uploads
            .iter()
            .filter_map(|event| match event {
                CloudEvent::Uploading {
                    message_id, sent, ..
                } => Some((message_id.as_str(), *sent)),
                _ => None,
            })
            .collect();
        let last = |message: &str| {
            let sent = progress.iter().rev().find(|(id, _)| *id == message);
            sent.map(|(_, sent)| *sent)
        };
        let messages: std::collections::BTreeSet<&str> =
            progress.iter().map(|(id, _)| *id).collect();
        messages.into_iter().filter_map(last).sum()
    }

    async fn ended(&self, runs: usize) {
        until("the runs' ends", || {
            let ended = self
                .events()
                .iter()
                .filter(|e| e["type"] == "runEnded")
                .count();
            (ended >= runs).then_some(())
        })
        .await;
    }
}

/// A send of `CHAT` with these files, from a model that sees both kinds.
fn send(url: &str, project: &Path, id: &str, image: &str, pdf: &str) -> StartRequest {
    let mut start = start_request(url, project, "Look at these".into());
    start.profile.vision = true;
    start.profile.pdf = true;
    start.start = StartKind::Send {
        message: UserMessage {
            id: id.into(),
            content: "Look at these".into(),
            meta: Some(json!({ "pdfs": [{ "id": pdf, "name": "notes.pdf", "pages": 1 }] })),
            attachments: vec![image.into(), pdf.into()],
        },
    };
    start
}

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

/// The daemon's copy of an attachment, if it has one.
fn on_daemon(daemon: &Daemon, id: &str) -> Option<PathBuf> {
    attachments::file_for(&daemon.data.join("data"), id)
}

#[tokio::test(flavor = "multi_thread")]
async fn files_go_up_once_and_again_when_the_daemon_lost_them() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let app = App::new(&daemon).await;
    let image = app.attach(PNG.to_vec()).await;
    let pdf_bytes = pdf_file(4 << 10);
    let pdf = app.attach(pdf_bytes.clone()).await;
    // Pasted into the input: nothing goes to the daemon before the message.
    assert!(on_daemon(&daemon, &image).is_none() && on_daemon(&daemon, &pdf).is_none());
    let (url, seen) = model(vec![Reply::Text("I see them.")]).await;

    app.remote
        .start(send(&url, &project, "u1", &image, &pdf))
        .await
        .unwrap();
    app.ended(1).await;
    assert!(on_daemon(&daemon, &image).is_some() && on_daemon(&daemon, &pdf).is_some());
    let image_size = std::fs::metadata(on_daemon(&daemon, &image).unwrap())
        .unwrap()
        .len();
    assert_eq!(app.uploaded(), image_size + pdf_bytes.len() as u64);
    // The model got them as it would from here.
    let request = seen.lock().unwrap()[0].clone();
    let parts = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .clone();
    let kinds: Vec<&str> = parts
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["type"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"image_url") && kinds.contains(&"file"),
        "{parts}"
    );

    // The same files again: nothing goes up.
    app.uploads.lock().unwrap().clear();
    app.remote
        .start(send(&url, &project, "u2", &image, &pdf))
        .await
        .unwrap();
    app.ended(2).await;
    assert_eq!(app.uploaded(), 0);

    // The daemon lost the image's file, its row left behind (K47).
    std::fs::remove_file(on_daemon(&daemon, &image).unwrap()).unwrap();
    // A PDF went up for a send whose message never came, and the daemon's
    // cleanup removed it once its grace period was over.
    let later_bytes = pdf_file(8 << 10);
    let later = app.attach(later_bytes.clone()).await;
    let path = format!("/blobs/{later}");
    let (status, _) = daemon
        .http("PUT", &path, Some(&daemon.token), later_bytes.clone())
        .await;
    assert_eq!(status.as_u16(), 200);
    let db = rusqlite::Connection::open(daemon.data.join("data/chats.db")).unwrap();
    db.execute(
        "UPDATE attachments SET created_at = 0 WHERE id = ?1",
        [&later],
    )
    .unwrap();
    attachments::collect_garbage(&db, &daemon.data.join("data")).unwrap();
    assert!(on_daemon(&daemon, &later).is_none(), "cleaned up");
    // Both go up again.
    app.uploads.lock().unwrap().clear();
    app.remote
        .start(send(&url, &project, "u3", &image, &later))
        .await
        .unwrap();
    app.ended(3).await;
    assert_eq!(app.uploaded(), image_size + later_bytes.len() as u64);
    assert!(on_daemon(&daemon, &image).is_some() && on_daemon(&daemon, &later).is_some());
}

/// A stream that writes slowly: 16 KB, then 20 ms of nothing.
struct Slow<S> {
    inner: S,
    rest: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Slow<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Slow<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if let Some(rest) = &mut self.rest {
            if rest.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        let n = buf.len().min(16 << 10);
        let written = Pin::new(&mut self.inner).poll_write(cx, &buf[..n]);
        if written.is_ready() {
            self.rest = Some(Box::pin(tokio::time::sleep(Duration::from_millis(20))));
        }
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct SlowConnector(UnixConnector);

impl Connector for SlowConnector {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            let inner = self.0.connect().await?;
            Ok(Box::new(Slow { inner, rest: None }) as Stream)
        })
    }
}

/// K48: Stop during a slow upload cancels the send: the daemon starts no
/// run and stores no message, and the send fails, so the input gets the
/// message back.
#[tokio::test(flavor = "multi_thread")]
async fn stop_during_a_slow_upload_starts_no_run() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let app = App::through(&daemon, SlowConnector(UnixConnector(daemon.socket.clone()))).await;
    let image = app.attach(PNG.to_vec()).await;
    let pdf = app.attach(pdf_file(2 << 20)).await;
    let (url, seen) = model(vec![Reply::Text("Too late.")]).await;

    let remote = Arc::clone(&app.remote);
    let request = send(&url, &project, "u1", &image, &pdf);
    let sending = tokio::spawn(async move { remote.start(request).await });
    until("the upload under way", || {
        (app.uploaded() > 0).then_some(())
    })
    .await;
    let stopped = app
        .remote
        .route(
            CHAT,
            Request::AgentStop {
                dialog_id: CHAT.into(),
            },
        )
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
    let sent = sending.await.unwrap();
    assert!(sent.is_err(), "{sent:?}");

    assert!(daemon.rows(CHAT).is_empty(), "no message there");
    let status = app.remote.status().await.unwrap();
    assert!(status.runs.is_empty());
    assert!(seen.lock().unwrap().is_empty(), "no request to the model");
}

/// 6.1: a file only the daemon has, such as a screenshot a run took there,
/// is here before the frontend hears of the row that names it. Another
/// app of the same computer sends one, so no browser is needed.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_only_the_daemon_has_comes_before_its_row() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let sender = App::new(&daemon).await;
    let watcher = App::new(&daemon).await;
    let image = sender.attach(PNG.to_vec()).await;
    let pdf = sender.attach(pdf_file(4 << 10)).await;
    let (url, _) = model(vec![Reply::Text("I see them.")]).await;
    sender
        .remote
        .start(send(&url, &project, "u1", &image, &pdf))
        .await
        .unwrap();

    let added = until("the row with the files", || {
        watcher
            .events()
            .into_iter()
            .find(|e| e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u1")
    })
    .await;
    assert_eq!(added["filesHere"], true, "{added}");
    let data = watcher.core.db.data_dir().to_path_buf();
    assert!(attachments::file_for(&data, &image).is_some());
    assert!(attachments::file_for(&data, &pdf).is_some());
}

/// 6.11: what isn't the file its name says, or isn't a file the daemon
/// takes, is refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_refuses_what_isnt_a_file_it_takes() {
    let daemon = Daemon::start(&[]).await;
    let put = |bytes: Vec<u8>, name: String| {
        let daemon = &daemon;
        async move {
            let path = format!("/blobs/{name}");
            let (status, body) = daemon.http("PUT", &path, Some(&daemon.token), bytes).await;
            let reason: Value = serde_json::from_slice(&body).unwrap_or_default();
            (
                status.as_u16(),
                reason["message"].as_str().unwrap_or("").to_string(),
            )
        }
    };
    let png = PNG.to_vec();
    let (status, reason) = put(png.clone(), "0".repeat(64)).await;
    assert_eq!(
        (status, reason.as_str()),
        (400, "The file doesn't match its name.")
    );

    let word = b"PK\x03\x04\x14\x00\x06\x00word/document.xml".to_vec();
    let (status, reason) = put(word.clone(), attachments::id_of(&word)).await;
    assert_eq!(status, 422);
    assert!(
        reason.starts_with("Only PNG, JPEG, WebP and GIF images and PDFs"),
        "{reason}"
    );

    let big = pdf_file(21 << 20);
    let (status, reason) = put(big.clone(), attachments::id_of(&big)).await;
    assert_eq!(
        (status, reason.as_str()),
        (413, "A file takes 20 MB at most.")
    );

    // What the app sends, an image it prepared, is taken and served back.
    let app = rovik_core::testing::TestCore::new();
    let stored = attachments::store_attached(&app.core.db, png)
        .await
        .unwrap();
    let prepared =
        std::fs::read(attachments::file_for(app.core.db.data_dir(), &stored.id).unwrap()).unwrap();
    let (status, _) = put(prepared.clone(), stored.id.clone()).await;
    assert_eq!(status, 200);
    let path = format!("/blobs/{}", stored.id);
    let (status, body) = daemon
        .http("GET", &path, Some(&daemon.token), Vec::new())
        .await;
    assert_eq!((status.as_u16(), body.to_vec()), (200, prepared));
}

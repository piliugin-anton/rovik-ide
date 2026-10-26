//! The mirror of a remote chat against the real daemon (M05, 6.3): pulled
//! after the chat's events, on each connection and when a run ends, it
//! ends as the daemon has the chat, its files included; the frontend hears
//! of every row once it's here; "Re-sync this chat" replaces it, keeping
//! the input's draft; and a connection's first pulls end with `CaughtUp`.

mod common;

use common::{start_request, until, Daemon, COMPUTER};
use rovik_client::{CloudEvent, HostState, Identity, Remote, UnixConnector};
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::StartKind;
use rovik_core::chat_db::store::{self, MessageRow, NewDialog, NewMessage};
use rovik_core::chat_db::{attachments, mirror};
use rovik_core::testing::{model, temp_dir, Reply, TempDir};
use rovik_core::{Core, EventSink, Paths};
use rovik_protocol::Secret;
use rusqlite::types::Value as Column;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// An app of `COMPUTER`'s with `CHAT` on the daemon: its core, what its
/// frontend heard, and its remote chats while it's online.
struct App {
    core: Core,
    events: Arc<Mutex<Vec<Value>>>,
    /// What the frontend heard on `cloud-event` since the app last connected.
    cloud: Arc<Mutex<Vec<CloudEvent>>>,
    remote: Option<Arc<Remote>>,
    database: PathBuf,
    _dir: TempDir,
}

/// The frontend's view: each event, and for each `messagesAdded`, whether
/// its rows were in the database by then, and the files they name here.
struct Frontend {
    database: PathBuf,
    data: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
}

impl EventSink for Frontend {
    fn agent(&self, event: &AgentEvent) {
        let mut seen = serde_json::to_value(event).unwrap();
        if let AgentEvent::MessagesAdded { messages, .. } = event {
            let db = read_only(&self.database);
            let has = |id: &str| {
                db.query_row("SELECT 1 FROM messages WHERE id = ?1", [id], |_| Ok(()))
                    .is_ok()
            };
            seen["rowsHere"] = json!(messages.iter().all(|m| has(&m.id)));
            let named: Vec<&String> = messages.iter().flat_map(|m| &m.attachments).collect();
            let here = |id: &String| attachments::file_for(&self.data, id).is_some();
            seen["filesHere"] = json!(!named.is_empty() && named.into_iter().all(here));
        }
        self.events.lock().unwrap().push(seen);
    }
}

impl App {
    /// The app, its chat on the daemon, connected.
    async fn new(daemon: &Daemon) -> App {
        let dir = temp_dir();
        let events = Arc::default();
        let (data, database) = (dir.0.join("data"), dir.0.join("data/chats.db"));
        let frontend = Frontend {
            database: database.clone(),
            data: data.clone(),
            events: Arc::clone(&events),
        };
        let paths = Paths::new(data, dir.0.join("config"));
        let core = Core::open(paths, Arc::new(frontend)).unwrap();
        let dialog = NewDialog {
            id: CHAT.into(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: None,
            folders: None,
        };
        let made = core.db.write(move |c| {
            store::create_dialog(c, &dialog)?;
            mirror::place(c, CHAT, HOST)
        });
        made.await.unwrap();
        let mut app = App {
            core,
            events,
            cloud: Arc::default(),
            remote: None,
            database,
            _dir: dir,
        };
        app.connect(daemon).await;
        app
    }

    fn remote(&self) -> &Arc<Remote> {
        self.remote.as_ref().expect("the app is online")
    }

    /// Connects as the app does when it starts: a new `Remote`, which knows
    /// from the database which chats are on the host.
    async fn connect(&mut self, daemon: &Daemon) {
        let remote = Remote::new(self.core.clone());
        remote.load().await.unwrap();
        let heard: Arc<Mutex<Vec<CloudEvent>>> = Arc::default();
        let cloud = Arc::clone(&heard);
        remote.on_cloud(Arc::new(move |event| cloud.lock().unwrap().push(event)));
        self.cloud = heard;
        let identity = Identity {
            host: HOST.into(),
            computer: COMPUTER.into(),
            token: Secret(daemon.token.clone()),
            client: "mirror tests".into(),
        };
        remote.connect(HOST, UnixConnector(daemon.socket.clone()), identity);
        for _ in 0..400 {
            if remote.states() == [(HOST.to_string(), HostState::Ready)] {
                self.remote = Some(remote);
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never connected\n{}", daemon.output());
    }

    /// The app goes offline.
    async fn disconnect(&mut self) {
        self.remote = None;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    async fn rows(&self) -> Vec<MessageRow> {
        let read = self
            .core
            .db
            .read(|c| store::get_messages(c, CHAT, None, 500));
        read.await.unwrap().messages
    }

    /// An image attached in the chat input, here.
    async fn attach(&self, bytes: &[u8]) -> String {
        let stored = attachments::store_attached(&self.core.db, bytes.to_vec());
        stored.await.unwrap().id
    }

    /// Waits until the mirror has the chat's messages as the daemon has
    /// them, every column, body and attachment.
    async fn settled(&self, daemon: &Daemon) {
        let theirs = || messages_in(&daemon.data.join("data/chats.db"));
        for _ in 0..800 {
            let ours = messages_in(&self.database);
            if !ours.is_empty() && ours == theirs() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "the mirror never caught up:\nhere {:?}\nthere {:?}",
            messages_in(&self.database),
            theirs()
        );
    }

    /// Waits for the frontend to hear an event `matches`.
    async fn heard(&self, matches: impl Fn(&Value) -> bool) -> Value {
        until("an event for the frontend", || {
            self.events().into_iter().find(|e| matches(e))
        })
        .await
    }
}

fn read_only(database: &Path) -> Connection {
    Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// The chat's messages as a database has them.
fn messages_in(database: &Path) -> Vec<Vec<Column>> {
    common::messages_in(database, CHAT)
}

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

/// Polls the app's rows until `done` takes them.
async fn rows_until(app: &App, what: &str, done: impl Fn(&[MessageRow]) -> bool) {
    for _ in 0..800 {
        if done(&app.rows().await) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}: {:?}", app.rows().await);
}

/// The roles and texts of the rows.
fn texts(rows: &[MessageRow]) -> Vec<(String, String)> {
    rows.iter()
        .map(|r| (r.role.clone(), r.content.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_streamed_reply_ends_in_the_mirror_as_on_the_daemon() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let app = App::new(&daemon).await;
    let piece = "A long answer, streamed. ";
    let (url, _) = model(vec![Reply::Trickle(vec![piece; 16], 300)]).await;
    let start = start_request(&url, &project, "Tell me".into());
    app.remote().start(start).await.unwrap();

    // While it streams, its checkpoints reach the mirror.
    rows_until(&app, "a checkpoint of the reply", |rows| {
        let reply = rows.iter().find(|r| r.role == "assistant");
        reply.is_some_and(|r| !r.finalized && !r.content.is_empty())
    })
    .await;
    app.heard(|e| e["type"] == "runEnded").await;
    app.settled(&daemon).await;
    let rows = app.rows().await;
    assert_eq!(rows.last().unwrap().content, piece.repeat(16));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_app_offline_during_a_run_catches_up_when_it_reconnects() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let mut app = App::new(&daemon).await;
    let later = Reply::Slow(1500, Box::new(Reply::Text("Done while you were away.")));
    let (url, _) = model(vec![later]).await;
    let start = start_request(&url, &project, "Do it".into());
    app.remote().start(start).await.unwrap();
    app.disconnect().await;
    until("the run's end there", || {
        let rows = daemon.rows(CHAT);
        rows.iter()
            .any(|r| r.role == "assistant" && r.finalized)
            .then_some(())
    })
    .await;

    app.connect(&daemon).await;
    app.settled(&daemon).await;
    let expected = [
        ("user".to_string(), "Do it".to_string()),
        ("assistant".into(), "Done while you were away.".into()),
    ];
    assert_eq!(texts(&app.rows().await), expected);
}

/// K23: `CaughtUp` comes once the mirror has pulled what the daemon stored
/// while the app was away: a message sent before is judged only then.
#[tokio::test(flavor = "multi_thread")]
async fn caught_up_comes_once_the_mirror_has_what_was_stored_meanwhile() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let mut app = App::new(&daemon).await;
    let later = Reply::Slow(1500, Box::new(Reply::Text("Done while you were away.")));
    let (url, _) = model(vec![later]).await;
    let start = start_request(&url, &project, "Do it".into());
    app.remote().start(start).await.unwrap();
    app.disconnect().await;
    until("the run's end there", || {
        let rows = daemon.rows(CHAT);
        rows.iter()
            .any(|r| r.role == "assistant" && r.finalized)
            .then_some(())
    })
    .await;

    app.connect(&daemon).await;
    until("CaughtUp", || {
        let heard = app.cloud.lock().unwrap();
        let caught_up = |e: &CloudEvent| matches!(e, CloudEvent::CaughtUp { host } if host == HOST);
        heard.iter().any(caught_up).then_some(())
    })
    .await;
    assert!(app.remote().caught_up(HOST), "a window that missed it asks");
    let expected = [
        ("user".to_string(), "Do it".to_string()),
        ("assistant".into(), "Done while you were away.".into()),
    ];
    assert_eq!(texts(&app.rows().await), expected, "pulled before it");
}

/// K22: rows that came without their events reach the frontend once
/// they're in the database, so a chat it opened before them ends with
/// every one. Another app of the same computer runs the chat; this one
/// connects after.
#[tokio::test(flavor = "multi_thread")]
async fn every_stored_message_reaches_a_chat_opened_before_its_pull() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let sender = App::new(&daemon).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "list_dir",
            args: json!({}),
        },
        Reply::Text("Listed."),
    ])
    .await;
    let start = start_request(&url, &project, "List it".into());
    sender.remote().start(start).await.unwrap();
    sender.heard(|e| e["type"] == "runEnded").await;
    sender.settled(&daemon).await;

    let watcher = App::new(&daemon).await;
    watcher.settled(&daemon).await;
    let mut stored: Vec<String> = daemon.rows(CHAT).into_iter().map(|r| r.id).collect();
    stored.sort();
    assert_eq!(stored.len(), 4, "{stored:?}");
    let announced = || -> Vec<Value> {
        let events = watcher.events().into_iter();
        events.filter(|e| e["type"] == "messagesAdded").collect()
    };
    let ids = |announced: &[Value]| -> Vec<String> {
        let mut ids: Vec<String> = announced
            .iter()
            .flat_map(|e| e["messages"].as_array().unwrap())
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        // A pull brings rows in the order they changed, not the chat's.
        ids.sort();
        ids
    };
    // The frontend hears of rows just after they're written.
    let announced = until("every stored row announced", || {
        let announced = announced();
        (ids(&announced) == stored).then_some(announced)
    })
    .await;
    assert!(
        announced.iter().all(|e| e["rowsHere"] == true),
        "{announced:?}"
    );
}

/// A remote chat read with its host away shows all that was stored before
/// the disconnection, a long tool output's whole body included.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_chat_read_offline_shows_what_was_stored_before() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    std::fs::write(project.join("notes.txt"), "a line of notes\n".repeat(600)).unwrap();
    let mut app = App::new(&daemon).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "notes.txt" }),
        },
        Reply::Text("Read it."),
    ])
    .await;
    let start = start_request(&url, &project, "Read the notes".into());
    app.remote().start(start).await.unwrap();
    app.heard(|e| e["type"] == "runEnded").await;
    app.settled(&daemon).await;
    let there = messages_in(&daemon.data.join("data/chats.db"));

    app.disconnect().await;
    daemon.kill().await;
    assert_eq!(messages_in(&app.database), there);
    let rows = app.rows().await;
    let result = rows.iter().find(|r| r.role == "tool").unwrap();
    assert!(result.body_external, "a long output's body is apart");
    let id = result.id.clone();
    let body = app.core.db.read(move |c| store::get_message_body(c, &id));
    let body = body.await.unwrap();
    assert!(body.len() > store::EXTERNAL_BODY_BYTES, "{body}");
    assert_eq!(body.matches("a line of notes").count(), 600);
}

/// 4.1: a message too big for a frame, with ten 1 MB text files in its
/// text, reaches the mirror whole: its event is a `Resync`, and its page
/// comes through `POST /call`.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_with_ten_megabytes_of_text_files_reaches_the_mirror_whole() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let app = App::new(&daemon).await;
    let files: String = (0..10)
        .map(|n| {
            format!(
                "<file name=\"f{n}.txt\">\n{}\n</file>\n",
                "x".repeat(1 << 20)
            )
        })
        .collect();
    let (url, _) = model(vec![Reply::Text("Got them.")]).await;
    let start = start_request(&url, &project, files.clone());
    app.remote().start(start).await.unwrap();

    app.heard(|e| e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u1")
        .await;
    app.heard(|e| e["type"] == "runEnded").await;
    app.settled(&daemon).await;
    let rows = app.rows().await;
    assert_eq!(rows[0].content.len(), files.len());
    assert!(rows[0].content == files, "the message, whole");
}

/// 6.3: a file only the daemon has (a screenshot a run took there, here
/// one another app sent) comes with the pull, before the row that names
/// it, so the chat shows it.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_only_the_daemon_has_comes_with_the_pull() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let sender = App::new(&daemon).await;
    let image = sender.attach(&PNG).await;
    let (url, _) = model(vec![Reply::Text("I see it.")]).await;
    let mut start = start_request(&url, &project, "Look".into());
    if let StartKind::Send { message } = &mut start.start {
        message.attachments = vec![image.clone()];
    }
    sender.remote().start(start).await.unwrap();
    sender.heard(|e| e["type"] == "runEnded").await;

    let watcher = App::new(&daemon).await;
    watcher.settled(&daemon).await;
    let added = watcher
        .heard(|e| e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u1")
        .await;
    assert_eq!(added["filesHere"], true, "{added}");
    let data = watcher.core.db.data_dir().to_path_buf();
    assert!(attachments::file_for(&data, &image).is_some());
}

/// K44: a file the daemon has lost gets a row without a file here, and the
/// pull goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_the_daemon_lost_doesnt_stop_the_pull() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let sender = App::new(&daemon).await;
    let image = sender.attach(&PNG).await;
    let (url, _) = model(vec![Reply::Text("I see it.")]).await;
    let mut start = start_request(&url, &project, "Look".into());
    if let StartKind::Send { message } = &mut start.start {
        message.attachments = vec![image.clone()];
    }
    sender.remote().start(start).await.unwrap();
    sender.heard(|e| e["type"] == "runEnded").await;
    let there = attachments::file_for(&daemon.data.join("data"), &image).unwrap();
    std::fs::remove_file(there).unwrap();

    let watcher = App::new(&daemon).await;
    watcher.settled(&daemon).await;
    let data = watcher.core.db.data_dir().to_path_buf();
    assert!(attachments::file_for(&data, &image).is_none());
    let bytes: i64 = read_only(&watcher.database)
        .query_row(
            "SELECT bytes FROM attachments WHERE id = ?1",
            [&image],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bytes, 0, "a row without a file");
}

/// "Re-sync this chat" puts the daemon's rows in place of the mirror's,
/// and keeps the chat's row, with the input's draft and its image (K45).
#[tokio::test(flavor = "multi_thread")]
async fn a_resync_keeps_the_inputs_draft_and_its_images() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let app = App::new(&daemon).await;
    let (url, _) = model(vec![Reply::Text("Hello.")]).await;
    let start = start_request(&url, &project, "Hi".into());
    app.remote().start(start).await.unwrap();
    app.heard(|e| e["type"] == "runEnded").await;
    app.settled(&daemon).await;

    let image = app.attach(&PNG).await;
    let draft = json!([
        { "type": "text", "text": "Next, look at " },
        { "type": "image", "image": { "id": image, "name": "shot.png" } }
    ])
    .to_string();
    let (stray, kept) = (
        NewMessage {
            id: "stray".into(),
            role: "user".into(),
            content: "Never on the server".into(),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
            error: None,
            failed: false,
            attachments: Vec::new(),
            created_at: 1,
            finalized: true,
        },
        draft.clone(),
    );
    let written = app.core.db.write(move |c| {
        store::append_messages(c, CHAT, &[stray])?;
        store::save_composer_draft(c, CHAT, &kept)
    });
    written.await.unwrap();
    assert_ne!(messages_in(&app.database).len(), 2);

    app.remote().resync_chat(CHAT).await.unwrap();
    let there = messages_in(&daemon.data.join("data/chats.db"));
    assert_eq!(messages_in(&app.database), there);
    let read = app.core.db.read(|c| store::get_composer_draft(c, CHAT));
    assert_eq!(read.await.unwrap(), Some(draft));
    let data = app.core.db.data_dir().to_path_buf();
    let collected = app
        .core
        .db
        .write(move |c| attachments::collect_garbage(c, &data));
    collected.await.unwrap();
    let data = app.core.db.data_dir().to_path_buf();
    assert!(
        attachments::file_for(&data, &image).is_some(),
        "the draft's image"
    );
}

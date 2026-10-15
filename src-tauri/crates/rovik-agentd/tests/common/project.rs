//! A project carried to the real daemon (T03): its folder as the desktop
//! has it, its shadow, a client of the daemon, and the calls that give a
//! chat its worktree there and start runs in it.

use super::{commit_all, start_request, write, Daemon, COMPUTER};
use git2::Repository;
use rovik_client::shadow::Shadow;
use rovik_client::{Client, HostState, Identity, Incoming, UnixConnector};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind, StartRequest, UserMessage};
use rovik_core::testing::Reply;
use rovik_protocol::agent::AgentStatus;
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::work::{Project, Synced};
use rovik_protocol::{Fail, Request, Secret};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const PROJECT: &str = "9e2f4a61-3b7c-4d58-a1e0-6c5b8d2f7a93";
pub const CHAT: &str = "chat-1";

/// A project as the desktop has it, its shadow, and a client of the daemon.
pub struct Setup {
    pub daemon: Daemon,
    pub project: PathBuf,
    pub shadow: PathBuf,
    pub client: Client,
}

impl Setup {
    pub async fn new() -> Setup {
        let daemon = Daemon::start(&[]).await;
        let project = daemon.dir.0.join("project");
        write(&project.join("src/main.rs"), "fn main() {}\n");
        // What the desktop wrote when it first opened the folder.
        write(&project.join(".rovik/.gitignore"), "project.json\n");
        write(
            &project.join(".rovik/project.json"),
            "{ \"id\": \"desktop\" }\n",
        );
        commit_all(&project);
        let client = client_as(&daemon, COMPUTER, &daemon.token).await;
        Setup {
            shadow: daemon.dir.0.join("shadow.git"),
            daemon,
            project,
            client,
        }
    }

    /// The chat's snapshot, pushed to the daemon.
    pub async fn pushed(&self, chat: &str) -> String {
        let shadow = Shadow::open(&self.shadow, &self.project).unwrap();
        let snapshot = shadow.snapshot(chat).unwrap().commit;
        let name = snapshot_ref(chat);
        (self.client)
            .push(PROJECT, &self.shadow, &name, snapshot, |_| {})
            .await
            .unwrap();
        snapshot.to_string()
    }

    pub async fn ensure(&self) {
        let ensure = Request::ProjectEnsure {
            project: Project {
                id: PROJECT.into(),
                name: "app".into(),
                subpath: String::new(),
            },
            settings: Some(json!({ "checks": ["cargo test"] })),
            excludes: vec!["*.mine".into()],
        };
        self.client.request::<()>(ensure).await.unwrap();
    }

    pub async fn sync(&self, chat: &str, snapshot: &str) -> Result<Synced, Fail> {
        sync_as(&self.client, chat, snapshot).await
    }

    /// The chat's landing base on the daemon.
    pub fn base(&self) -> String {
        let state = self.daemon.state();
        state["chats"][CHAT]["base"].as_str().unwrap().to_string()
    }

    /// The project's repository on the daemon.
    pub fn there(&self) -> Repository {
        let path = self.daemon.data.join(format!("projects/{PROJECT}.git"));
        Repository::open_bare(path).unwrap()
    }

    /// The daemon's database, as it is now.
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.daemon.data.join("data/chats.db")).unwrap()
    }
}

pub async fn client_as(daemon: &Daemon, computer: &str, token: &str) -> Client {
    let identity = Identity {
        host: "server".into(),
        computer: computer.into(),
        token: Secret(token.to_string()),
        client: "work tests".into(),
    };
    let client = Client::connect(UnixConnector(daemon.socket.clone()), identity, {
        Arc::new(|_| {})
    });
    for _ in 0..400 {
        if client.state() == HostState::Ready {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never connected\n{}", daemon.output());
}

pub async fn sync_as(client: &Client, chat: &str, snapshot: &str) -> Result<Synced, Fail> {
    let sync = Request::WorkSync {
        dialog_id: chat.into(),
        project: PROJECT.into(),
        snapshot: snapshot.into(),
        excludes: vec!["*.mine".into()],
        changes: None,
    };
    client.request(sync).await
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A chat with its worktree, ready to run: its root there.
pub async fn ready_to_run(setup: &Setup) -> PathBuf {
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;
    PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root)
}

/// A start of `CHAT` with these settings, naming the desktop's folder: the
/// daemon runs it in the chat's worktree.
pub fn start(setup: &Setup, url: &str, permissions: Permissions) -> StartRequest {
    let mut start = start_request(url, &setup.project, "Do it".into());
    start.config = Some(AgentConfig {
        permissions,
        ..AgentConfig::default()
    });
    start
}

/// A later message of `CHAT`, with its own `id`.
pub fn message(setup: &Setup, url: &str, id: &str) -> StartRequest {
    let mut next = start(setup, url, unattended());
    next.start = StartKind::Send {
        message: UserMessage {
            id: id.into(),
            content: "More".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    next
}

/// The model writes a file.
pub fn writes(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

/// Any command, no approvals, no checks.
pub fn unattended() -> Permissions {
    Permissions {
        allow_any_command: true,
        verify_changes: false,
        tool_call_checkpoint: None,
        ..Permissions::default()
    }
}

pub async fn status(client: &Client) -> AgentStatus {
    client.request(Request::AgentStatus).await.unwrap()
}

/// Waits for the chat's run to end.
pub async fn ended(client: &Client) {
    for _ in 0..1200 {
        if status(client)
            .await
            .runs
            .iter()
            .all(|r| r.dialog_id != CHAT)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the run never ended");
}

/// The tool results the chat's run stored.
pub fn tool_results(setup: &Setup) -> Vec<String> {
    let rows = setup.daemon.rows(CHAT).into_iter();
    rows.filter(|r| r.role == "tool")
        .map(|r| r.content)
        .collect()
}

/// A client of `COMPUTER` that keeps what it hears.
pub async fn listening(daemon: &Daemon) -> (Client, mpsc::UnboundedReceiver<Incoming>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let identity = Identity {
        host: "server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "listening".into(),
    };
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    let client = Client::connect(UnixConnector(daemon.socket.clone()), identity, sink);
    for _ in 0..400 {
        if client.state() == HostState::Ready {
            return (client, rx);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never connected\n{}", daemon.output());
}

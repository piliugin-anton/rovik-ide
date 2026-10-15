//! Projects and worktrees on the real daemon (T03, 6.4): a chat's worktree
//! holds its snapshot and the project's ID file, its root is one of its
//! project's folders, it ignores what the client ignores, and deleting the
//! chat, or a start-up after it was deleted, takes all of it away.

mod common;

use common::{commit_all, write, Daemon, COMPUTER, OTHER};
use git2::{BranchType, Repository, StatusOptions};
use rovik_client::shadow::Shadow;
use rovik_client::{Client, HostState, Identity, UnixConnector};
use rovik_core::chat_db::{projects, store};
use rovik_core::project_profile;
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::work::{Project, Synced};
use rovik_protocol::{Fail, FailCode, Request, Secret};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const PROJECT: &str = "9e2f4a61-3b7c-4d58-a1e0-6c5b8d2f7a93";
const CHAT: &str = "chat-1";

/// A project as the desktop has it, its shadow, and a client of the daemon.
struct Setup {
    daemon: Daemon,
    project: PathBuf,
    shadow: PathBuf,
    client: Client,
}

impl Setup {
    async fn new() -> Setup {
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
    async fn pushed(&self, chat: &str) -> String {
        let shadow = Shadow::open(&self.shadow, &self.project).unwrap();
        let snapshot = shadow.snapshot(chat).unwrap().commit;
        let name = snapshot_ref(chat);
        (self.client)
            .push(PROJECT, &self.shadow, &name, snapshot, |_| {})
            .await
            .unwrap();
        snapshot.to_string()
    }

    async fn ensure(&self) {
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

    async fn sync(&self, chat: &str, snapshot: &str) -> Result<Synced, Fail> {
        sync_as(&self.client, chat, snapshot).await
    }

    /// The project's repository on the daemon.
    fn there(&self) -> Repository {
        let path = self.daemon.data.join(format!("projects/{PROJECT}.git"));
        Repository::open_bare(path).unwrap()
    }

    /// The daemon's database, as it is now.
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.daemon.data.join("data/chats.db")).unwrap()
    }
}

async fn client_as(daemon: &Daemon, computer: &str, token: &str) -> Client {
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

async fn sync_as(client: &Client, chat: &str, snapshot: &str) -> Result<Synced, Fail> {
    let sync = Request::WorkSync {
        dialog_id: chat.into(),
        project: PROJECT.into(),
        snapshot: snapshot.into(),
        excludes: vec!["*.mine".into()],
    };
    client.request(sync).await
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[tokio::test]
async fn a_chat_gets_a_worktree_at_its_snapshot() {
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;

    let synced = setup.sync(CHAT, &snapshot).await.unwrap();

    let root = PathBuf::from(&synced.root);
    let expected = setup.daemon.data.join("work").join(CHAT);
    assert_eq!(
        root.canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
    assert_eq!(read(&root.join("src/main.rs")), "fn main() {}\n");
    let id: Value = serde_json::from_str(&read(&root.join(".rovik/project.json"))).unwrap();
    assert_eq!(id["id"], PROJECT);
    // The checkout is the snapshot, on the chat's branch: the agent's
    // `git status` starts clean, the ID file ignored.
    let worktree = Repository::open(&root).unwrap();
    assert_eq!(
        worktree.head().unwrap().shorthand().unwrap(),
        "rovik/chat-1"
    );
    let mut untracked = StatusOptions::new();
    untracked.include_untracked(true);
    assert!(worktree.statuses(Some(&mut untracked)).unwrap().is_empty());
    // A file only the client's global excludes name, made on the server,
    // stays out.
    write(&root.join("notes.mine"), "made on the server\n");
    write(&root.join("new.rs"), "fn new() {}\n");
    assert!(worktree.is_path_ignored("notes.mine").unwrap());
    assert!(!worktree.is_path_ignored("new.rs").unwrap());
    // The chat's root is one of its project's folders, with its settings.
    let db = setup.db();
    assert!(projects::chat_may_use(&db, Some(PROJECT), &root).unwrap());
    assert_eq!(
        store::dialog_project(&db, CHAT).unwrap().as_deref(),
        Some(PROJECT)
    );
    let config = setup.daemon.data.join("config");
    let settings = project_profile::load_project_settings(&config, &synced.root);
    assert_eq!(settings, json!({ "checks": ["cargo test"] }));

    // Another sync keeps the worktree as it is: syncs merge with T07.
    assert_eq!(setup.sync(CHAT, &snapshot).await.unwrap(), synced);
    assert!(root.join("new.rs").exists());
}

#[tokio::test]
async fn deleting_the_chat_takes_its_worktree_branch_and_refs() {
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);

    let delete = Request::ChatDelete {
        dialog_id: CHAT.into(),
    };
    setup.client.request::<()>(delete).await.unwrap();

    assert!(!root.exists());
    let there = setup.there();
    assert!(there
        .find_branch("rovik/chat-1", BranchType::Local)
        .is_err());
    assert!(there.find_reference(&snapshot_ref(CHAT)).is_err());
    assert!(there.find_worktree(CHAT).is_err());
    assert!(!projects::chat_may_use(&setup.db(), Some(PROJECT), &root).unwrap());
    assert!(setup.daemon.state()["chats"].get(CHAT).is_none());
}

#[tokio::test]
async fn start_up_collects_the_worktrees_of_chats_deleted_meanwhile() {
    let mut setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);
    setup.daemon.kill().await;
    // The chat's rows went while the daemon wasn't there, and a folder no
    // chat has was left behind.
    let deleted = setup
        .db()
        .execute("DELETE FROM dialogs WHERE id = ?1", [CHAT]);
    assert_eq!(deleted.unwrap(), 1);
    let stray = setup.daemon.data.join("work/stray");
    write(&stray.join("file.txt"), "left\n");

    setup.daemon.restart().await;

    assert!(!root.exists());
    assert!(!stray.exists());
    let there = setup.there();
    assert!(there
        .find_branch("rovik/chat-1", BranchType::Local)
        .is_err());
    assert!(there.find_reference(&snapshot_ref(CHAT)).is_err());
}

#[tokio::test]
async fn a_sync_needs_its_project_its_snapshot_a_plain_id_and_its_computer() {
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    let early = setup.sync(CHAT, &snapshot).await.unwrap_err();
    assert_eq!(early.code, FailCode::BadRequest, "{early:?}");

    setup.ensure().await;
    let stranger = "0123456789abcdef0123456789abcdef01234567";
    let unknown = setup.sync(CHAT, stranger).await.unwrap_err();
    assert_eq!(unknown.code, FailCode::BadRequest, "{unknown:?}");
    assert!(!setup.daemon.data.join("work").join(CHAT).exists());

    let escape = setup.sync("../../escape", &snapshot).await.unwrap_err();
    assert_eq!(escape.code, FailCode::BadRequest, "{escape:?}");

    setup.sync(CHAT, &snapshot).await.unwrap();
    let other = client_as(&setup.daemon, OTHER, &setup.daemon.other_token).await;
    let refused = sync_as(&other, CHAT, &snapshot).await.unwrap_err();
    assert_eq!(refused.code, FailCode::NotOwner);
}

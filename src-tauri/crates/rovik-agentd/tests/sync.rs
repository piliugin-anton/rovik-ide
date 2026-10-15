//! Syncing before each message (T07, 6.7, D14): a local edit to a file the
//! chat didn't touch is in its worktree before its next turn. With "chat
//! resolves conflicts" on, a conflicting text edit gets the local kind of
//! markers there, the chat is told, and its result waits until they're
//! gone, by itself and with a manual Land. With it off the sync waits, the
//! chat goes on with its older copy, and the app says why. A binary
//! conflict waits whatever the setting.

mod common;

use common::app::{landed_as, App};
use common::project::{
    message, read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{until, write};
use git2::{Repository, Status, StatusOptions};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::{chats, import};
use rovik_core::testing::{model, Reply};
use rovik_protocol::work::SyncState;
use serde_json::Value;
use std::path::Path;

/// The chat's run changed the worktree and ended: its result waits here,
/// the project not being open.
async fn result_waits() {
    until("the result waiting for its project", || {
        chats::pending(CHAT).filter(|p| p.reason == import::READY)
    })
    .await;
}

/// What the agent's `git status` says in the worktree.
fn status(root: &Path) -> Vec<(String, Status)> {
    let worktree = Repository::open(root).unwrap();
    let mut untracked = StatusOptions::new();
    untracked.include_untracked(true);
    let statuses = worktree.statuses(Some(&mut untracked)).unwrap();
    let entries = statuses.iter();
    entries
        .map(|entry| (entry.path().unwrap().to_string(), entry.status()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_edit_is_in_the_worktree_before_the_next_turn() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("src/lib.rs"), "pub fn lib() {}\n");
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
        Reply::Text("Seen."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;
    write(
        &setup.project.join("src/lib.rs"),
        "pub fn lib() { local() }\n",
    );

    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    assert_eq!(read(&root.join("src/lib.rs")), "pub fn lib() { local() }\n");
    // The chat's change stays its own.
    assert_eq!(status(&root), [("src/new.rs".to_string(), Status::WT_NEW)]);
    assert_eq!(
        app.remote.sync_state(CHAT).unwrap().state,
        SyncState::Synced
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_gets_markers_and_the_result_waits_for_them() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, asked) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        Reply::Text("Done."),
        Reply::Text("I'll look at the markers later."),
        writes("w3", "src/main.rs", "fn main() { local(); remote() }\n"),
        Reply::Text("Settled."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );

    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    let marked = "<<<<<<< project (now)\nfn main() { local() }\n||||||| before\nfn main() {}\n\
                  =======\nfn main() { remote() }\n>>>>>>> this chat\n";
    assert_eq!(read(&root.join("src/main.rs")), marked);
    let sync = app.remote.sync_state(CHAT).unwrap();
    assert_eq!(sync.state, SyncState::Marked);
    assert_eq!(sync.files, ["src/main.rs"]);
    // The chat was told, with the message.
    until("the chat told of its markers", || {
        let note = "changes made in the project meanwhile, in: `src/main.rs`";
        let requests = asked.lock().unwrap();
        requests
            .iter()
            .map(Value::to_string)
            .find(|r| r.contains(note))
    })
    .await;
    // Its result waits for the markers, by itself and with a manual Land.
    until("the marked result", || {
        Some(chats::markers_left(CHAT)).filter(|left| *left == ["src/main.rs"])
    })
    .await;
    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));
    let waits = until("the result waiting for its markers", || {
        chats::pending(CHAT).filter(|p| p.reason.contains("conflict markers"))
    })
    .await;
    assert!(waits.reason.contains("src/main.rs"), "{}", waits.reason);
    assert!(app.remote.land(CHAT, false).await.is_err());
    assert_eq!(
        read(&setup.project.join("src/main.rs")),
        "fn main() { local() }\n"
    );

    // Settled by the chat, it lands.
    app.remote.start(message(&setup, &url, "u3")).await.unwrap();

    let both = "fn main() { local(); remote() }\n";
    landed_as(&setup, "src/main.rs", both).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_chat_resolving_the_sync_waits() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        Reply::Text("Done."),
        Reply::Text("Still here."),
    ])
    .await;
    // The setting comes with each start.
    let keeps = Permissions {
        chat_resolves_conflicts: false,
        ..unattended()
    };
    app.remote.start(start(&setup, &url, keeps)).await.unwrap();
    result_waits().await;
    let base = setup.base();
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );

    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    assert_eq!(read(&root.join("src/main.rs")), "fn main() { remote() }\n");
    assert_eq!(setup.base(), base, "nothing applied");
    let sync = app.remote.sync_state(CHAT).unwrap();
    assert_eq!(sync.state, SyncState::Conflicts);
    assert_eq!(sync.files, ["src/main.rs"]);
    let said = app
        .test
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e["type"] == "notice" && e["title"] == "Sync waits" && e["dialogId"] == CHAT);
    assert!(said, "the app says why");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_conflict_waits_whatever_the_setting() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("logo.bin"), "\0logo\n");
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "logo.bin", "\0chat\n"),
        Reply::Text("Done."),
        Reply::Text("Still here."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;
    write(&setup.project.join("logo.bin"), "\0local\n");

    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    assert_eq!(read(&root.join("logo.bin")), "\0chat\n");
    let sync = app.remote.sync_state(CHAT).unwrap();
    assert_eq!(sync.state, SyncState::Unmergeable);
    assert_eq!(sync.files, ["logo.bin"]);
}

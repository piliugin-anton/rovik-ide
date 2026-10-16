//! Syncing before each message (T07, 6.7, D14): a local edit to a file the
//! chat didn't touch is in its worktree before its next turn. With "chat
//! resolves conflicts" on, a conflicting text edit gets the local kind of
//! markers there, the chat is told, its run is reminded of them as it
//! ends, and its result waits until they're gone, by itself and with a
//! manual Land. With it off the sync waits, the chat goes on with its
//! older copy, and the bar says why: the user lets it wait (Keep the older
//! copy) or hands the conflicts to the chat, which gets the markers then.
//! A binary conflict waits whatever the setting. Sync now brings the
//! project as it is here between messages (U05).

mod common;

use common::app::{landed_as, App};
use common::project::{
    ended, message, read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{until, write};
use git2::{Repository, Status, StatusOptions};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::{chats, import, markers};
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
        // Reminded of them twice as its run ends, it leaves them.
        Reply::Text("I'll look at the markers later."),
        Reply::Text("Later."),
        Reply::Text("Later still."),
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
async fn a_run_reminded_of_its_markers_settles_them() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    ready_to_run(&setup).await;
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
    let first = import::imported(CHAT).unwrap().commit;
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );

    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    // Reminded as its run ends, the chat settles them in the same run.
    until("the chat reminded of its markers", || {
        let reminder = "Conflict markers are still in `src/main.rs`";
        let requests = asked.lock().unwrap();
        requests
            .iter()
            .map(Value::to_string)
            .find(|r| r.contains(reminder))
    })
    .await;
    until("its settled result here", || {
        let newer = import::imported(CHAT).filter(|i| i.commit != first);
        newer.filter(|_| chats::markers_left(CHAT).is_empty())
    })
    .await;
    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));
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
    let first = start(&setup, &url, keeps.clone());
    app.remote.start(first).await.unwrap();
    result_waits().await;
    let base = setup.base();
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );

    let mut next = message(&setup, &url, "u2");
    next.config = start(&setup, &url, keeps).config;
    app.remote.start(next).await.unwrap();

    assert_eq!(read(&root.join("src/main.rs")), "fn main() { remote() }\n");
    assert_eq!(setup.base(), base, "nothing applied");
    let sync = app.remote.sync_state(CHAT).unwrap();
    assert_eq!(sync.state, SyncState::Conflicts);
    assert_eq!(sync.files, ["src/main.rs"]);
    let bar = app.remote.overlay_status(CHAT).await.unwrap();
    assert_eq!(bar.sync.as_ref(), Some(&sync), "the bar says why");

    // Keep the older copy: the bar stops asking.
    app.remote.keep_older_copy(CHAT);
    let bar = app.remote.overlay_status(CHAT).await.unwrap();
    assert_eq!(bar.sync, None);
    assert_eq!(read(&root.join("src/main.rs")), "fn main() { remote() }\n");
    // Sync now waits the same, and the bar asks again. It comes once the
    // run has ended, as the app's command sees to: a sync under a run
    // that writes the worktree is refused.
    ended(&setup.client).await;
    let again = app.remote.sync_now(CHAT).await.unwrap().unwrap();
    assert_eq!(again.state, SyncState::Conflicts);
    let bar = app.remote.overlay_status(CHAT).await.unwrap();
    assert_eq!(bar.sync, Some(again));

    // Hand to the chat: the markers go in, the sync applies, and the
    // message that asks the chat to settle them tells it of them.
    let handed = app.remote.sync_to_chat(CHAT).await.unwrap();
    assert_eq!(handed.sync.state, SyncState::Marked);
    assert_eq!(handed.sync.files, ["src/main.rs"]);
    assert!(
        handed.request.contains("`src/main.rs`"),
        "{}",
        handed.request
    );
    let marked = read(&root.join("src/main.rs"));
    let sides = ["fn main() { local() }", "fn main() { remote() }"];
    assert!(sides.iter().all(|side| marked.contains(side)), "{marked}");
    assert!(markers::left_in(marked.as_bytes()), "{marked}");
    assert_ne!(setup.base(), base, "applied");
    let state = setup.daemon.state();
    let chat = &state["chats"][CHAT];
    assert_eq!(chat["marked"], serde_json::json!(["src/main.rs"]));
    assert!(
        chat.get("tellMarked").is_none(),
        "not again at the next start: {chat}"
    );
    assert_eq!(app.remote.sync_now(CHAT).await.unwrap(), None, "it has it");
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_now_brings_the_project_between_messages() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;
    write(&setup.project.join("src/other.rs"), "fn other() {}\n");

    let sync = app.remote.sync_now(CHAT).await.unwrap().unwrap();

    assert_eq!(sync.state, SyncState::Synced);
    assert_eq!(read(&root.join("src/other.rs")), "fn other() {}\n");
    assert_eq!(read(&root.join("src/main.rs")), "fn main() { remote() }\n");
    assert_eq!(app.remote.sync_now(CHAT).await.unwrap(), None, "it has it");
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

//! Remote chats' results, back on the desktop (T06, 6.5): the app imports
//! each result as the chat's overlay store and lands it by itself in the
//! open project, a conflict waits for the user, who resolves it or lands
//! anyway, the daemon hears of every landing so an undone one stays
//! undone, a result that ended while the app was away comes in when it
//! connects, and text comes back with the project's own line endings.

mod common;

use common::app::App;
use common::project::{ended, read, ready_to_run, start, unattended, Setup, CHAT};
use common::{until, write};
use git2::{Oid, Repository, StatusOptions};
use rovik_core::agent_run::{StartKind, StartRequest, UserMessage};
use rovik_core::overlay::chats;
use rovik_core::testing::{model, Reply};
use rovik_protocol::work::ChatResult;
use rovik_protocol::Request;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

/// The model writes a file.
fn writes(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

/// The chat's next message, after the first.
fn more(setup: &Setup, url: &str) -> StartRequest {
    let mut next = start(setup, url, unattended());
    next.start = StartKind::Send {
        message: UserMessage {
            id: "u2".into(),
            content: "More".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    next
}

/// The chat's landing base on the daemon.
fn base(setup: &Setup) -> String {
    let state = setup.daemon.state();
    state["chats"][CHAT]["base"].as_str().unwrap().to_string()
}

/// Waits for a file to land in the desktop's project: there, and the
/// landing over, its import gone.
async fn landed(setup: &Setup, path: &str) -> String {
    let file = setup.project.join(path);
    until(&format!("{path} landed in the project"), || {
        (file.exists() && !chats::exists(CHAT)).then(|| read(&file))
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_change_lands_here_and_an_undo_stays_undone() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let snapshot = base(&setup);
    let (url, _) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
        writes("w2", "src/other.rs", "fn other() {}\n"),
        Reply::Text("Done."),
    ])
    .await;

    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();

    assert_eq!(landed(&setup, "src/new.rs").await, "fn new() {}\n");
    assert!(!chats::exists(CHAT), "landed: the import is gone");
    // The daemon heard of it: its landing base has the file now.
    let moved = until("the landing base moved", || {
        Some(base(&setup)).filter(|b| *b != snapshot)
    })
    .await;
    let there = setup.there();
    let commit = there.find_commit(Oid::from_str(&moved).unwrap()).unwrap();
    assert_eq!(commit.parent_id(0).unwrap().to_string(), snapshot);
    assert!(commit
        .tree()
        .unwrap()
        .get_path(Path::new("src/new.rs"))
        .is_ok());
    // The agent's git status no longer lists what landed.
    let worktree = Repository::open(&root).unwrap();
    let mut untracked = StatusOptions::new();
    untracked.include_untracked(true);
    assert!(worktree.statuses(Some(&mut untracked)).unwrap().is_empty());

    let undo = chats::last_landing(CHAT).unwrap();
    let agents = &app.test.core.agents;
    chats::overlay_undo(agents, CHAT.into(), undo.id)
        .await
        .unwrap();
    assert!(!setup.project.join("src/new.rs").exists());
    app.remote.start(more(&setup, &url)).await.unwrap();

    assert_eq!(landed(&setup, "src/other.rs").await, "fn other() {}\n");
    assert!(
        !setup.project.join("src/new.rs").exists(),
        "the next result didn't bring the undone change back"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_waits_for_resolve_and_land_anyway() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("src/lib.rs"), "pub fn lib() {}\n");
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    // Changed here since the snapshot went, on the lines the chat changes.
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );
    write(
        &setup.project.join("src/lib.rs"),
        "pub fn lib() { local() }\n",
    );
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        writes("w2", "src/lib.rs", "pub fn lib() { remote() }\n"),
        Reply::Text("Done."),
        writes("w3", "src/later.rs", "fn later() {}\n"),
        Reply::Text("Done."),
    ])
    .await;

    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();

    let pending = until("the conflicts in the bar", || {
        chats::pending(CHAT).filter(|p| !p.conflicts.is_empty())
    })
    .await;
    let paths: Vec<&str> = pending.conflicts.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs", "src/main.rs"]);
    assert_eq!(
        read(&setup.project.join("src/main.rs")),
        "fn main() { local() }\n"
    );
    let snapshot = base(&setup);
    // The chat goes on meanwhile: its newer result waits while the
    // conflicts are settled.
    app.remote.start(more(&setup, &url)).await.unwrap();
    until("the newer result held back", || {
        let events = app.test.events.lock().unwrap();
        let held = |e: &&Value| e["title"] == "A newer result of this chat is ready";
        events.iter().find(held).cloned()
    })
    .await;
    assert_eq!(chats::pending(CHAT).unwrap().conflicts.len(), 2);

    // Resolve takes both sides of one file, Land anyway the chat's side of
    // the other.
    chats::conflict_file(CHAT, "src/main.rs").unwrap();
    let both = b"fn main() { local(); remote() }\n".to_vec();
    let left = chats::resolve_conflict(CHAT, "src/main.rs", Some(both), None).unwrap();
    assert_eq!(left.len(), 1);
    let landing = app.remote.land(CHAT, true).await.unwrap();

    assert!(landing.is_some());
    let project = |path: &str| read(&setup.project.join(path));
    assert_eq!(project("src/main.rs"), "fn main() { local(); remote() }\n");
    assert_eq!(project("src/lib.rs"), "pub fn lib() { remote() }\n");
    assert_ne!(base(&setup), snapshot, "the daemon heard of the landing");
    // The newer result comes in now, on the new base: only its own change.
    assert_eq!(landed(&setup, "src/later.rs").await, "fn later() {}\n");
    assert_eq!(project("src/main.rs"), "fn main() { local(); remote() }\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_that_ended_while_away_comes_in_on_connecting() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/away.rs", "fn away() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    // The run starts and ends while the app isn't connected.
    let started = Request::AgentStart(start(&setup, &url, unattended()));
    setup
        .client
        .request::<Option<String>>(started)
        .await
        .unwrap();
    ended(&setup.client).await;
    for _ in 0..1200 {
        let results: Vec<ChatResult> = setup.client.request(Request::WorkResults).await.unwrap();
        if !results.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    app.connect(&setup).await;

    assert_eq!(landed(&setup, "src/away.rs").await, "fn away() {}\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_autocrlf_text_comes_back_with_crlf() {
    let mut setup = Setup::new().await;
    let project = Repository::open(&setup.project).unwrap();
    project
        .config()
        .unwrap()
        .set_bool("core.autocrlf", true)
        .unwrap();
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/crlf.rs", "one\ntwo\n"),
        Reply::Text("Done."),
    ])
    .await;

    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();

    assert_eq!(landed(&setup, "src/crlf.rs").await, "one\r\ntwo\r\n");
}

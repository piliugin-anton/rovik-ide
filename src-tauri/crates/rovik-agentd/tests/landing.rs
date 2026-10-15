//! Remote chats' results, back on the desktop (T06, 6.5): the app imports
//! each result as the chat's overlay store and lands it by itself in the
//! open project, a conflict waits for the user, who resolves it or lands
//! anyway, the daemon hears of every landing so an undone one stays
//! undone, a result that ended while the app was away comes in when it
//! connects, and text comes back with the project's own line endings.
//! Results are numbered: an older one told again is passed over, and the
//! same changes after another run land by that run's outcome. A result
//! waits while its project's folder is open as another project's, until
//! the user says which project it is.

mod common;

use common::app::{landed, landed_as, App, HOST};
use common::project::{
    ended, message, read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{until, write};
use git2::{Oid, Repository, StatusOptions};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::{chats, import};
use rovik_core::testing::{model, Reply};
use rovik_protocol::work::ChatResult;
use rovik_protocol::Request;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_change_lands_here_and_an_undo_stays_undone() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let snapshot = setup.base();
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
        Some(setup.base()).filter(|b| *b != snapshot)
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
    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    // The next message's sync took the undo there, as the user's change.
    assert!(!root.join("src/new.rs").exists());
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
    app.remote.set_open_project(None);
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        writes("w2", "src/lib.rs", "pub fn lib() { remote() }\n"),
        Reply::Text("Done."),
        writes("w3", "src/later.rs", "fn later() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    // The chat doesn't resolve conflicts itself: the next message's sync
    // waits, and its changes stay as they are there.
    let keeps = Permissions {
        chat_resolves_conflicts: false,
        ..unattended()
    };
    app.remote.start(start(&setup, &url, keeps)).await.unwrap();
    until("the result waiting for its project", || {
        chats::pending(CHAT).filter(|p| p.reason == import::READY)
    })
    .await;
    // Changed here meanwhile, on the lines the chat changed.
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );
    write(
        &setup.project.join("src/lib.rs"),
        "pub fn lib() { local() }\n",
    );

    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));

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
    let snapshot = setup.base();
    // The chat goes on meanwhile: its newer result waits while the
    // conflicts are settled.
    app.remote.start(message(&setup, &url, "u2")).await.unwrap();
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
    assert_ne!(setup.base(), snapshot, "the daemon heard of the landing");
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

/// The chat's newest result on the daemon.
async fn newest_result(setup: &Setup) -> ChatResult {
    let results: Vec<ChatResult> = setup.client.request(Request::WorkResults).await.unwrap();
    results.into_iter().find(|r| r.dialog_id == CHAT).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_result_told_again_is_passed_over() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { one() }\n"),
        Reply::Text("Done."),
        writes("w2", "src/main.rs", "fn main() { two() }\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    landed_as(&setup, "src/main.rs", "fn main() { one() }\n").await;
    let older = newest_result(&setup).await;
    app.remote.start(message(&setup, &url, "u2")).await.unwrap();
    landed_as(&setup, "src/main.rs", "fn main() { two() }\n").await;
    let newer = newest_result(&setup).await;
    assert!(older.seq < newer.seq, "{} then {}", older.seq, newer.seq);

    // The first one's `ResultReady` again, as a replay after a connection
    // can bring it behind the newer result. Its commit is at no ref there
    // any more: fetching it would fail, and say the changes didn't come.
    app.remote.result_ready(HOST, older);
    // Behind it in the queue: once Land is through, so is the result.
    let _ = app.remote.land(CHAT, false).await;

    let events = app.test.events.lock().unwrap().clone();
    let failed = |e: &Value| e["title"] == "This chat's changes didn't come back";
    assert!(!events.iter().any(failed), "passed over, not fetched");
    assert!(!chats::exists(CHAT), "nothing came of it");
    assert_eq!(
        read(&setup.project.join("src/main.rs")),
        "fn main() { two() }\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_kept_by_a_stop_land_after_a_run_that_changed_nothing() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    let (url, asked) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Hang(None),
        Reply::Text("Nothing more to do."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    // Stopped while the model doesn't answer, after the chat's write.
    until("the model asked after the write", || {
        (asked.lock().unwrap().len() == 2).then_some(())
    })
    .await;
    assert!(root.join("src/new.rs").exists());
    let stop = Request::AgentStop {
        dialog_id: CHAT.into(),
    };
    setup.client.request::<bool>(stop).await.unwrap();
    until("the stopped run's changes waiting", || {
        chats::pending(CHAT).filter(|p| p.reason.contains("stopped"))
    })
    .await;

    // The same changes come back from a run that ended well.
    app.remote.start(message(&setup, &url, "u2")).await.unwrap();

    assert_eq!(landed(&setup, "src/new.rs").await, "fn new() {}\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_lands_once_its_folder_is_said_to_be_its_project() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    // The folder open as another project's, as a copy is until the user
    // answers what it is.
    let other = ("original".to_string(), setup.project.clone());
    app.remote.set_open_project(Some(other));
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    until("the result waiting for its project", || {
        chats::pending(CHAT).filter(|p| p.reason == import::READY)
    })
    .await;

    app.remote.folder_answered(&setup.project, PROJECT);

    assert_eq!(landed(&setup, "src/new.rs").await, "fn new() {}\n");
}

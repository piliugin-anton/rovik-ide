//! Remote chats' results, back on the desktop (T06, 6.5): the app imports
//! each result as the chat's overlay store and lands it by itself in the
//! open project, a conflict waits for the user, who resolves it or lands
//! anyway, a newer result waits meanwhile unless the user takes it in
//! their place, the daemon hears of every landing so an undone one stays
//! undone, a result that ended while the app was away comes in when it
//! connects, and text comes back with the project's own line endings.
//! Results are numbered: an older one told again is passed over, and the
//! same changes after another run land by that run's outcome. A result
//! waits while its project's folder is open as another project's, until
//! the user says which project it is. A file the chat moved lands with the
//! user's edit to it where it was. Which result came in last, and a landing
//! the daemon wasn't told of, outlive a restart of the app (M01).

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
use serde_json::{json, Value};
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

/// A result's conflicts with changes made here meanwhile wait in the open
/// project, and the chat's newer result, which went on with its older copy
/// of their files, is held back while they're settled. Returns the app,
/// and the chat's landing base on the daemon.
async fn conflicts_and_a_newer_result(setup: &mut Setup) -> (App, String) {
    write(&setup.project.join("src/lib.rs"), "pub fn lib() {}\n");
    let app = App::new(setup).await;
    app.connect(setup).await;
    ready_to_run(setup).await;
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
    app.remote.start(start(setup, &url, keeps)).await.unwrap();
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
    // conflicts are settled, and the bar says so.
    app.remote.start(message(setup, &url, "u2")).await.unwrap();
    app.bar("the newer result held back", |s| s.newer_result)
        .await;
    assert_eq!(chats::pending(CHAT).unwrap().conflicts.len(), 2);
    (app, snapshot)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_waits_for_resolve_and_land_anyway() {
    let mut setup = Setup::new().await;
    let (app, snapshot) = conflicts_and_a_newer_result(&mut setup).await;

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
async fn the_newer_result_taken_comes_in_place_of_the_conflicts() {
    let mut setup = Setup::new().await;
    let (app, snapshot) = conflicts_and_a_newer_result(&mut setup).await;
    let older = import::imported(CHAT).unwrap().commit;
    // One of the two files resolved so far.
    chats::conflict_file(CHAT, "src/main.rs").unwrap();
    let both = b"fn main() { local(); remote() }\n".to_vec();
    let left = chats::resolve_conflict(CHAT, "src/main.rs", Some(both), None).unwrap();
    assert_eq!(left.len(), 1);

    app.remote.take_newer(CHAT).await.unwrap();

    // The newer result is the chat's store now: its own change with the
    // older ones, and both conflicts again, what was resolved gone.
    let imported = import::imported(CHAT).unwrap();
    assert_ne!(imported.commit, older);
    assert!(
        imported.changed.iter().any(|p| p == "src/later.rs"),
        "{imported:?}"
    );
    let pending = chats::pending(CHAT).unwrap();
    assert_eq!(pending.conflicts.len(), 2, "{pending:?}");
    let bar = app.remote.overlay_status(CHAT).await.unwrap();
    assert!(!bar.newer_result);
    let project = |path: &str| read(&setup.project.join(path));
    assert_eq!(project("src/main.rs"), "fn main() { local() }\n");
    assert!(
        !setup.project.join("src/later.rs").exists(),
        "nothing landed"
    );
    assert_eq!(setup.base(), snapshot);
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

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_file_lands_with_the_users_edit_where_it_was() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("src/old.rs"), "1\n2\n3\n4\n5\n6\n");
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "m1",
            name: "bash",
            args: json!({ "command": "mv src/old.rs src/new.rs" }),
        },
        writes("w2", "src/new.rs", "ONE\n2\n3\n4\n5\n6\n"),
        Reply::Text("Moved."),
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
    // Edited here meanwhile, where it was, on another line.
    write(&setup.project.join("src/old.rs"), "1\n2\n3\n4\n5\nSIX\n");

    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));

    landed_as(&setup, "src/new.rs", "ONE\n2\n3\n4\n5\nSIX\n").await;
    assert!(!setup.project.join("src/old.rs").exists());
}

/// The notices the app gave about the chat's changes coming back or landing.
fn landing_notices(app: &App) -> usize {
    let titles = [
        "Landed",
        "This chat's changes didn't land",
        "This chat's changes didn't come back",
    ];
    let events = app.test.events.lock().unwrap();
    events
        .iter()
        .filter(|e| e["type"] == "notice" && titles.iter().any(|t| e["title"] == *t))
        .count()
}

/// M01: the result imported last is kept with its number (`mirror_state`),
/// so after a restart the app doesn't import it again, as it would told of
/// it once more on connecting: a landing the user undid stays undone.
#[tokio::test(flavor = "multi_thread")]
async fn after_a_restart_the_result_imported_last_isnt_imported_again() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let snapshot = setup.base();
    let (url, _) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    assert_eq!(landed(&setup, "src/new.rs").await, "fn new() {}\n");
    until("the daemon told of the landing", || {
        Some(setup.base()).filter(|b| *b != snapshot)
    })
    .await;
    let undo = chats::last_landing(CHAT).unwrap();
    let agents = &app.test.core.agents;
    chats::overlay_undo(agents, CHAT.into(), undo.id)
        .await
        .unwrap();
    assert!(!setup.project.join("src/new.rs").exists());
    let newest = newest_result(&setup).await;
    let told = landing_notices(&app);

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    // Told of it again, as `WorkResults` on connecting tells it, or a
    // `ResultReady` replayed.
    app.remote.result_ready(HOST, newest);
    let _ = app.remote.land(CHAT, false).await;

    assert!(
        !setup.project.join("src/new.rs").exists(),
        "the undone landing didn't come back"
    );
    assert_eq!(
        landing_notices(&app),
        told,
        "nothing came in or landed again"
    );
    assert!(!chats::exists(CHAT), "no store was made for it");
}

/// M01: a landing the daemon couldn't be told of is kept (`mirror_state`),
/// and told after the next connection, though the app restarted meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn a_landing_the_daemon_wasnt_told_of_is_told_after_a_restart() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    app.remote.set_open_project(None);
    let snapshot = setup.base();
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
    // The daemon goes away, and the user lands the result.
    setup.daemon.kill().await;
    let landing = app.remote.land(CHAT, false).await.unwrap();
    assert!(landing.is_some());
    assert_eq!(read(&setup.project.join("src/new.rs")), "fn new() {}\n");
    assert_eq!(setup.base(), snapshot, "the daemon wasn't told");

    let app = app.restart(&setup).await;
    setup.daemon.restart().await;
    app.connect(&setup).await;

    let moved = until("the daemon told of the landing", || {
        Some(setup.base()).filter(|b| *b != snapshot)
    })
    .await;
    let there = setup.there();
    let commit = there.find_commit(Oid::from_str(&moved).unwrap()).unwrap();
    assert!(commit
        .tree()
        .unwrap()
        .get_path(Path::new("src/new.rs"))
        .is_ok());
}

//! The isolation bar's remote half (T10, 6.1), through the app's routes for
//! a remote chat: what the chat changed in its worktree, until it lands; a
//! changed file's versions; Discard; handing conflicts to the chat, whose
//! resolution then lands; a plan read from the worktree; the buttons only
//! a local chat has, refused; the app told to look again when a result
//! comes in and when it lands; a file the chat moved, listed as moved; and
//! a merge or markers taken back out there, with Undo the merge and Unmark.

mod common;

use common::app::{landed_as, App};
use common::project::{
    ended, message, read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{until, write};
use rovik_client::{CloudEvent, Remote};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::chats::{self, ChangedFile};
use rovik_core::overlay::import;
use rovik_core::testing::{model, Reply};
use rovik_protocol::packs::kept_ref;
use rovik_protocol::Request;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The setup with its app connected, the project closed so results wait,
/// and the chat's worktree ready: its root there.
async fn waiting() -> (Setup, App, PathBuf) {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    let root = ready_to_run(&setup).await;
    (setup, app, root)
}

/// The chat's run changed its worktree, and its result waits here.
async fn result_waits() {
    until("the result waiting for its project", || {
        chats::pending(CHAT).filter(|p| p.reason == import::READY)
    })
    .await;
}

async fn files(remote: &Remote) -> Vec<(String, &'static str)> {
    let status = remote.overlay_status(CHAT).await.unwrap();
    let files = status.files.into_iter();
    files
        .map(|ChangedFile { path, change, .. }| (path, change))
        .collect()
}

/// Polls `check` until it holds, for up to 30 s.
async fn eventually<F: Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    for _ in 0..1200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never: {what}");
}

fn open(setup: &Setup, app: &App) {
    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bar_lists_what_the_chat_changed_until_it_lands() {
    let (setup, app, _) = waiting().await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { changed() }\n"),
        writes("w2", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;

    let changed = [
        ("src/main.rs".to_string(), "changed"),
        ("src/new.rs".to_string(), "added"),
    ];
    assert_eq!(files(&app.remote).await, changed);
    let status = app.remote.overlay_status(CHAT).await.unwrap();
    assert_eq!(status.pending.unwrap().reason, import::READY);
    assert!(status.sync.is_some(), "the message's sync");
    let versions = app.remote.overlay_file(CHAT, "src/main.rs").await.unwrap();
    assert_eq!(versions.project.as_deref(), Some("fn main() {}\n"));
    assert_eq!(versions.chat.as_deref(), Some("fn main() { changed() }\n"));

    open(&setup, &app);

    landed_as(&setup, "src/main.rs", "fn main() { changed() }\n").await;
    eventually("the bar empty", || async {
        files(&app.remote).await.is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discard_puts_the_worktree_back_at_its_landing_base() {
    let (setup, app, root) = waiting().await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { changed() }\n"),
        writes("w2", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;

    app.remote.check_idle(CHAT).await.unwrap();
    app.remote.discard(CHAT).await.unwrap();

    assert_eq!(read(&root.join("src/main.rs")), "fn main() {}\n");
    assert!(!root.join("src/new.rs").exists());
    assert!(root.join(".rovik/project.json").exists(), "ignored: kept");
    assert!(!chats::exists(CHAT), "the import here went too");
    assert!(files(&app.remote).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_handed_to_the_chat_land_once_it_settles_them() {
    let (setup, app, root) = waiting().await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        Reply::Text("Done."),
        writes("w2", "src/main.rs", "fn main() { local(); remote() }\n"),
        Reply::Text("Settled."),
    ])
    .await;
    // The chat takes conflicts only when they're handed to it: a sync
    // that conflicts waits.
    let handed_only = Permissions {
        chat_resolves_conflicts: false,
        ..unattended()
    };
    app.remote
        .start(start(&setup, &url, handed_only))
        .await
        .unwrap();
    result_waits().await;
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );
    open(&setup, &app);
    until("the conflict in the bar", || {
        chats::pending(CHAT).filter(|p| !p.conflicts.is_empty())
    })
    .await;

    let handed = app.remote.conflicts_to_chat(CHAT).await.unwrap();

    assert_eq!(handed.files, ["src/main.rs"]);
    assert!(
        handed.request.contains("conflict markers"),
        "{}",
        handed.request
    );
    let marked = "<<<<<<< project (now)\nfn main() { local() }\n||||||| before\nfn main() {}\n\
                  =======\nfn main() { remote() }\n>>>>>>> this chat\n";
    assert_eq!(read(&root.join("src/main.rs")), marked);
    assert!(!chats::exists(CHAT), "the chat settles them now");
    // The message that hands them over; the chat settles them, and its
    // resolution lands on the project's version.
    let mut handing = message(&setup, &url, "u2");
    if let rovik_core::agent_run::StartKind::Send { message } = &mut handing.start {
        message.content = handed.request.clone();
    }
    app.remote.start(handing).await.unwrap();

    let both = "fn main() { local(); remote() }\n";
    landed_as(&setup, "src/main.rs", both).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_reads_from_the_chats_worktree() {
    let (setup, app, root) = waiting().await;
    let plan = "# Plan\n\n1. Do it.\n";
    let (url, _) = model(vec![
        writes("w1", ".rovik/plans/plan.md", plan),
        Reply::Text("Planned."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;

    let named = root.join(".rovik/plans/plan.md");
    let read = app.remote.read_plan(CHAT, &named.to_string_lossy()).await;

    assert_eq!(read.unwrap(), plan);
    let elsewhere = app.remote.read_plan(CHAT, "/etc/passwd").await;
    assert!(elsewhere.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_waits_for_the_run_or_is_local_only_is_refused() {
    let (setup, app, _) = waiting().await;
    let (url, _) = model(vec![Reply::Hang(None)]).await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    eventually("the run going there", || async {
        app.remote.check_idle(CHAT).await.is_err()
    })
    .await;

    let refused = app.remote.check_idle(CHAT).await.unwrap_err();
    assert!(refused.contains("still going"), "{refused}");
    let local_only = app.remote.local_only(CHAT).unwrap_err();
    assert_eq!(local_only, rovik_client::LOCAL_ONLY);
    assert!(app.remote.local_only("a-local-chat").is_ok());
    let stop = Request::AgentStop {
        dialog_id: CHAT.into(),
    };
    setup.client.request::<bool>(stop).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bar_looks_again_when_a_result_comes_in_and_lands() {
    let (setup, app, _) = waiting().await;
    // Each time the app is told, whether the result waited, and whether it
    // had landed.
    let heard = Arc::new(Mutex::new(Vec::new()));
    let (sink, file) = (heard.clone(), setup.project.join("src/new.rs"));
    app.remote.on_cloud(Arc::new(move |event| {
        if matches!(&event, CloudEvent::Changed { dialog_id } if dialog_id == CHAT) {
            let waits = chats::pending(CHAT).is_some_and(|p| p.reason == import::READY);
            sink.lock().unwrap().push((waits, file.exists()));
        }
    }));
    let (url, _) = model(vec![
        writes("w1", "src/new.rs", "fn new() {}\n"),
        Reply::Text("Done."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;

    let told = |which: fn(&(bool, bool)) -> bool| {
        let heard = heard.clone();
        move || {
            let told = heard.lock().unwrap().iter().any(which);
            async move { told }
        }
    };
    eventually("told once it came in", told(|(waits, _)| *waits)).await;
    open(&setup, &app);
    landed_as(&setup, "src/new.rs", "fn new() {}\n").await;
    eventually("told once it landed", told(|(_, landed)| *landed)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bar_lists_a_moved_file_as_moved() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("src/old.rs"), "fn old() {}\n");
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "m1",
            name: "bash",
            args: serde_json::json!({ "command": "mv src/old.rs src/new.rs" }),
        },
        Reply::Text("Moved."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;

    let status = app.remote.overlay_status(CHAT).await.unwrap();
    let listed: Vec<(&str, &str, Option<&str>)> = status
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.change, f.from.as_deref()))
        .collect();
    let moved = [
        ("src/old.rs", "deleted", None),
        ("src/new.rs", "moved", Some("src/old.rs")),
    ];
    assert_eq!(listed, moved);
}

#[tokio::test(flavor = "multi_thread")]
async fn unmark_takes_markers_handed_over_back_out() {
    let (setup, app, root) = waiting().await;
    let (url, _) = model(vec![
        writes("w1", "src/main.rs", "fn main() { remote() }\n"),
        Reply::Text("Done."),
    ])
    .await;
    let handed_only = Permissions {
        chat_resolves_conflicts: false,
        ..unattended()
    };
    app.remote
        .start(start(&setup, &url, handed_only))
        .await
        .unwrap();
    result_waits().await;
    write(
        &setup.project.join("src/main.rs"),
        "fn main() { local() }\n",
    );
    open(&setup, &app);
    let conflicts = || chats::pending(CHAT).filter(|p| !p.conflicts.is_empty());
    until("the conflict in the bar", conflicts).await;
    let handed = app.remote.conflicts_to_chat(CHAT).await.unwrap();

    // The chat can't take them: they come back out.
    let undone = app.remote.undo_merge(CHAT, Some(handed.files)).await;

    assert_eq!(undone.unwrap().restored, ["src/main.rs"]);
    assert_eq!(read(&root.join("src/main.rs")), "fn main() { remote() }\n");
    // Its next result brings the conflict back here.
    let pending = until("the conflict back in the bar", conflicts).await;
    assert_eq!(pending.conflicts[0].path, "src/main.rs");
}

/// A merge on the server: `a.txt` changed by the chat on one line, then
/// here on another, which the next message's sync merges in; that run
/// over, and its result committed. The chat's worktree's root.
async fn merged_there(setup: &mut Setup) -> (App, PathBuf) {
    write(&setup.project.join("a.txt"), "1\n2\n3\n4\n5\n");
    let app = App::new(setup).await;
    app.remote.set_open_project(None);
    app.connect(setup).await;
    let root = ready_to_run(setup).await;
    let (url, _) = model(vec![
        writes("w1", "a.txt", "ONE\n2\n3\n4\n5\n"),
        Reply::Text("Done."),
        Reply::Text("Seen."),
    ])
    .await;
    app.remote
        .start(start(setup, &url, unattended()))
        .await
        .unwrap();
    result_waits().await;
    write(&setup.project.join("a.txt"), "1\n2\n3\n4\nFIVE\n");
    app.remote.start(message(setup, &url, "u2")).await.unwrap();
    assert_eq!(read(&root.join("a.txt")), "ONE\n2\n3\n4\nFIVE\n");
    ended(&setup.client).await;
    until("the merged run's result", || {
        let results = setup.daemon.state()["chats"][CHAT]["result"]["seq"].as_u64();
        results.filter(|seq| *seq >= 2)
    })
    .await;
    (app, root)
}

/// What `git gc` does to the daemon's project repository at its most
/// thorough: objects no ref reaches are gone.
fn gc(setup: &Setup) {
    let repo = setup.there();
    let done = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["gc", "--prune=now", "--quiet"])
        .status()
        .expect("git");
    assert!(done.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_the_merge_puts_the_chats_own_version_back_after_a_gc() {
    let mut setup = Setup::new().await;
    let (app, root) = merged_there(&mut setup).await;
    let status = app.remote.overlay_status(CHAT).await.unwrap();
    assert_eq!(status.merged_in, ["a.txt"]);
    let held = setup.there().find_reference(&kept_ref(CHAT)).is_ok();
    assert!(held, "the kept versions have a ref");
    gc(&setup);

    let undone = app.remote.undo_merge(CHAT, None).await.unwrap();

    assert_eq!(undone.restored, ["a.txt"]);
    assert_eq!(read(&root.join("a.txt")), "ONE\n2\n3\n4\n5\n");
    let status = app.remote.overlay_status(CHAT).await.unwrap();
    assert!(status.merged_in.is_empty());
    assert!(setup.there().find_reference(&kept_ref(CHAT)).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_whose_earlier_versions_are_gone_is_left_as_it_is() {
    let mut setup = Setup::new().await;
    let (app, root) = merged_there(&mut setup).await;
    // Its kept versions lost, as nothing reached them before the ref did.
    let there = setup.there();
    there
        .find_reference(&kept_ref(CHAT))
        .unwrap()
        .delete()
        .unwrap();
    gc(&setup);

    let undone = app.remote.undo_merge(CHAT, None).await.unwrap();

    assert!(undone.restored.is_empty());
    assert_eq!(undone.changed_since, ["a.txt"]);
    assert_eq!(read(&root.join("a.txt")), "ONE\n2\n3\n4\nFIVE\n");
    let status = app.remote.overlay_status(CHAT).await.unwrap();
    assert!(status.merged_in.is_empty(), "forgotten");
}

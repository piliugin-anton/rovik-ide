//! The isolation bar's remote half (T10, 6.1), through the app's routes for
//! a remote chat: what the chat changed in its worktree, until it lands; a
//! changed file's versions; Discard; handing conflicts to the chat, whose
//! resolution then lands; a plan read from the worktree; and the buttons
//! only a local chat has, refused.

mod common;

use common::app::{landed_as, App};
use common::project::{
    message, read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{until, write};
use rovik_client::Remote;
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::chats::{self, ChangedFile};
use rovik_core::overlay::import;
use rovik_core::testing::{model, Reply};
use rovik_protocol::Request;
use std::future::Future;
use std::path::PathBuf;
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

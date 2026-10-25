//! Chats moved to the real daemon (M04, M07, 6.8): the bundle a computer
//! sends, imported once per move, with its worktree, owned by that
//! computer; `ChatOwner`; `ChatRelease`, after which the chat is only read;
//! and `ChatForget`, after which its rows are gone, with its files, and its
//! worktree goes with the cleanup (R01).

mod common;

use common::project::{client_as, ended, message, writes, Setup, CHAT, PROJECT};
use common::{until, OTHER};
use rovik_client::{Client, HostState};
use rovik_core::chat_db::store::{self, ChatModel, NewDialog, NewMessage};
use rovik_core::chat_db::{attachments, sync};
use rovik_core::testing::{beating, beats, model, Reply, TestCore};
use rovik_protocol::events::BackgroundTask;
use rovik_protocol::host::Storage;
use rovik_protocol::packs::{result_ref, snapshot_ref};
use rovik_protocol::sync::{ChangeSet, ChangesPage, ChatBundle, Imported, Ownership};
use rovik_protocol::work::Project;
use rovik_protocol::{Fail, FailCode, Request, Response};
use serde_json::json;
use std::path::PathBuf;

fn row(id: &str, role: &str, content: &str) -> NewMessage {
    NewMessage {
        id: id.into(),
        role: role.into(),
        content: content.into(),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: Vec::new(),
        created_at: 2,
        finalized: true,
    }
}

/// The chat as the desktop has it, titled `title` with `messages`: its
/// rows since 0, which a move's bundle carries.
async fn rows(title: &str, messages: Vec<NewMessage>) -> ChangeSet {
    let desktop = TestCore::new();
    let title = title.to_string();
    let written = desktop.core.db.write(move |c| {
        let dialog = NewDialog {
            id: CHAT.into(),
            title,
            created_at: 1,
            updated_at: 1,
            project_id: None,
        };
        store::create_dialog(c, &dialog)?;
        for file in messages.iter().flat_map(|m| &m.attachments) {
            attachments::add_unavailable(c, file)?;
        }
        store::append_messages(c, CHAT, &messages).map(drop)
    });
    written.await.unwrap();
    let read = desktop
        .core
        .db
        .read(|c| sync::changes_since(c, CHAT, 0, None, usize::MAX));
    read.await.unwrap().changes
}

/// The bundle that moves the chat with `rows`, its snapshot pushed first.
async fn bundle(setup: &Setup, rows: ChangeSet) -> ChatBundle {
    ChatBundle {
        dialog_id: CHAT.into(),
        rows,
        project: Project {
            id: PROJECT.into(),
            name: "app".into(),
            subpath: String::new(),
        },
        settings: Some(json!({ "checks": ["cargo test"] })),
        excludes: vec!["*.mine".into()],
        snapshot: setup.pushed(CHAT).await,
        changes: None,
        pending: None,
    }
}

async fn import(client: &Client, move_id: &str, bundle: &ChatBundle) -> Result<Imported, Fail> {
    client.import_chat(move_id, bundle, |_| {}).await
}

async fn owner(client: &Client) -> Option<Ownership> {
    let asked = Request::ChatOwner {
        dialog_id: CHAT.into(),
    };
    client.request(asked).await.unwrap()
}

async fn release(client: &Client, move_id: Option<&str>) -> Result<bool, Fail> {
    let release = Request::ChatRelease {
        dialog_id: CHAT.into(),
        move_id: move_id.map(String::from),
    };
    client.request(release).await
}

fn ids(setup: &Setup) -> Vec<String> {
    setup.daemon.rows(CHAT).into_iter().map(|r| r.id).collect()
}

/// The chat's row on the daemon: its title, project and location.
fn chat_row(setup: &Setup) -> (String, Option<String>, Option<String>) {
    let db = setup.db();
    let row = db.query_row(
        "SELECT title, project_id, location FROM dialogs WHERE id = ?1",
        [CHAT],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    row.unwrap()
}

fn worktree(setup: &Setup) -> PathBuf {
    let state = setup.daemon.state();
    PathBuf::from(state["chats"][CHAT]["worktree"].as_str().unwrap())
}

/// Starts a run of the chat with a later message, and waits for its end.
async fn run(setup: &Setup, replies: Vec<Reply>) {
    let (url, _) = model(replies).await;
    let start = Request::AgentStart(Box::new(message(setup, &url, "later")));
    let started: Option<String> = setup.client.request(start).await.unwrap();
    assert!(started.is_some());
    ended(&setup.client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_move_imported_twice_makes_one_chat() {
    let setup = Setup::new().await;
    let rows = rows(
        "Moving",
        vec![
            row("u1", "user", "Fix it"),
            row("a1", "assistant", "Fixed."),
        ],
    )
    .await;
    let bundle = bundle(&setup, rows).await;
    let first = import(&setup.client, "move-1", &bundle).await.unwrap();
    assert!(!first.already);
    let again = import(&setup.client, "move-1", &bundle).await.unwrap();
    assert_eq!(
        again,
        Imported {
            rev: first.rev,
            already: true,
        }
    );

    assert_eq!(ids(&setup), ["u1", "a1"]);
    let chats: i64 = setup
        .db()
        .query_row("SELECT COUNT(*) FROM dialogs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(chats, 1);
    // Owned here, in the project, by the computer that moved it.
    let (title, project, location) = chat_row(&setup);
    assert_eq!(title, "Moving");
    assert_eq!(project.as_deref(), Some(PROJECT));
    assert_eq!(location, None);
    let owned = Ownership {
        move_id: Some("move-1".into()),
        released: false,
        rev: first.rev,
    };
    assert_eq!(owner(&setup.client).await, Some(owned));
    assert_eq!(setup.daemon.state()["chats"][CHAT]["computer"], "laptop");
    // Its worktree was made with the import, and a run goes on there.
    assert!(worktree(&setup).join("src/main.rs").is_file());
    run(&setup, vec![Reply::Text("Carried on.")]).await;
    let rows = setup.daemon.rows(CHAT);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(rows[3].content, "Carried on.");
}

#[tokio::test(flavor = "multi_thread")]
async fn another_computers_chat_isnt_taken_by_an_import() {
    let setup = Setup::new().await;
    let mine = bundle(&setup, rows("Mine", vec![row("u1", "user", "Mine")]).await).await;
    import(&setup.client, "move-1", &mine).await.unwrap();

    let desk = client_as(&setup.daemon, OTHER, &setup.daemon.other_token).await;
    let theirs = ChatBundle {
        rows: rows("Theirs", vec![row("d1", "user", "The desk's")]).await,
        ..mine.clone()
    };
    let fail = import(&desk, "move-2", &theirs).await.unwrap_err();
    assert_eq!(fail.code, FailCode::NotOwner, "{fail}");
    // Nor does the desk learn of it.
    let asked = Request::ChatOwner {
        dialog_id: CHAT.into(),
    };
    assert_eq!(desk.call(asked).await.unwrap_err().code, FailCode::NotOwner);
    assert_eq!(ids(&setup), ["u1"]);
    assert_eq!(chat_row(&setup).0, "Mine");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_released_chat_is_only_read() {
    let setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    let imported = import(&setup.client, "move-1", &bundle).await.unwrap();

    // Not while its run goes on.
    let (url, _) = model(vec![Reply::Hang(None)]).await;
    let start = Request::AgentStart(Box::new(message(&setup, &url, "later")));
    setup.client.call(start).await.unwrap();
    let busy = release(&setup.client, Some("move-1")).await.unwrap_err();
    assert_eq!(busy.code, FailCode::Busy, "{busy}");
    let stop = Request::AgentStop {
        dialog_id: CHAT.into(),
    };
    setup.client.call(stop).await.unwrap();
    ended(&setup.client).await;

    // Another move's release gives nothing back; this move's does, once.
    assert_eq!(release(&setup.client, Some("move-0")).await, Ok(false));
    assert_eq!(release(&setup.client, Some("move-1")).await, Ok(true));
    assert_eq!(release(&setup.client, None).await, Ok(true));
    let owned = Ownership {
        move_id: Some("move-1".into()),
        released: true,
        rev: imported.rev,
    };
    assert_eq!(owner(&setup.client).await, Some(owned));
    assert_eq!(chat_row(&setup).2.as_deref(), Some("released"));
    assert!(setup.daemon.state()["chats"][CHAT]["released"].is_i64());

    // What would change it is refused.
    let (url, _) = model(vec![Reply::Text("No.")]).await;
    let refused = [
        Request::ChatRename {
            dialog_id: CHAT.into(),
            title: "Changed".into(),
        },
        Request::AgentStart(Box::new(message(&setup, &url, "after"))),
        Request::WorkSync {
            dialog_id: CHAT.into(),
            project: PROJECT.into(),
            snapshot: bundle.snapshot.clone(),
            excludes: Vec::new(),
            changes: None,
            mark: false,
        },
        Request::ChatDelete {
            dialog_id: CHAT.into(),
        },
    ];
    for request in refused {
        let shown = format!("{request:?}");
        let fail = setup.client.call(request).await.unwrap_err();
        assert_eq!(fail.code, FailCode::NotOwner, "{shown}: {fail}");
        assert!(fail.message.contains("went back"), "{shown}: {fail}");
    }
    assert_eq!(chat_row(&setup).0, "Moved");
    // Its changes are still read: the computer's last pull.
    let pull = Request::ChatChangesSince {
        dialog_id: CHAT.into(),
        rev: 0,
        cursor: None,
    };
    let page: ChangesPage = setup.client.request(pull).await.unwrap();
    assert!(page.next.is_none());
    let pulled: Vec<&str> = page
        .changes
        .messages
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(pulled[0], "u1");
    assert_eq!(page.changes.dialogs[0].title, "Moved");
}

async fn forget(client: &Client, chat: &str) -> Result<bool, Fail> {
    let forget = Request::ChatForget {
        dialog_id: chat.into(),
    };
    client.request(forget).await
}

/// 6.8: the rows of a chat its computer took back go when that computer
/// has them all, and not before it let the chat go. Its worktree stays for
/// the cleanup (R01), and the daemon still knows whose it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_is_forgotten_only_once_its_computer_took_it_back() {
    let setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();

    let early = forget(&setup.client, CHAT).await.unwrap_err();
    assert_eq!(early.code, FailCode::BadRequest, "{early}");
    assert!(early.message.contains("release it first"), "{early}");
    let desk = client_as(&setup.daemon, OTHER, &setup.daemon.other_token).await;
    let theirs = forget(&desk, CHAT).await.unwrap_err();
    assert_eq!(theirs.code, FailCode::NotOwner, "{theirs}");
    assert_eq!(ids(&setup), ["u1"]);

    assert_eq!(release(&setup.client, None).await, Ok(true));
    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));
    assert!(ids(&setup).is_empty());
    let left: i64 = setup
        .db()
        .query_row("SELECT COUNT(*) FROM dialogs WHERE id = ?1", [CHAT], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(left, 0, "the chat's row went too");
    assert!(worktree(&setup).is_dir(), "the cleanup takes the worktree");
    assert!(owner(&setup.client).await.is_some_and(|o| o.released));
    // Told again, or of a chat it never had, it's done all the same.
    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));
    assert_eq!(forget(&setup.client, "chat-never").await, Ok(false));
}

/// A one-page PDF.
fn pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
      2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
      3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 3 3]>>endobj\n\
      trailer<</Root 1 0 R>>\n%%EOF"
        .to_vec()
}

/// K43, R01: the files only a forgotten chat's rows named go with them,
/// past their grace period, without waiting for the daily cleanup.
#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_chats_files_go_with_its_rows() {
    let setup = Setup::new().await;
    let file = pdf();
    let id = attachments::id_of(&file);
    setup.client.put_blob(&id, file, |_| {}).await.unwrap();
    let mut asks = row("u1", "user", "Read it");
    asks.attachments = vec![id.clone()];
    let bundle = bundle(&setup, rows("With a file", vec![asks]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    let data = setup.daemon.data.join("data");
    assert!(attachments::file_for(&data, &id).is_some());
    assert_eq!(release(&setup.client, None).await, Ok(true));
    // It went up two hours ago, with its message.
    let aged = setup.db().execute(
        "UPDATE attachments SET created_at = created_at - ?1",
        [2 * 60 * 60 * 1000],
    );
    assert_eq!(aged.unwrap(), 1);

    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));
    assert!(ids(&setup).is_empty());
    assert!(
        attachments::file_for(&data, &id).is_none(),
        "its PDF went too"
    );
}

/// R01: a chat taken back and forgotten leaves its worktree for a week,
/// a restart or two included, then the cleanup takes it with its refs;
/// "Clean up now" doesn't wait. One taken back but not forgotten keeps it:
/// its computer may still finish taking it back (K51).
#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_chats_worktree_goes_a_week_on_or_when_asked() {
    let mut setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    let worktree = worktree(&setup);
    assert_eq!(release(&setup.client, None).await, Ok(true));
    let cleaned = setup.client.request::<Storage>(Request::CleanUp).await;
    cleaned.unwrap();
    assert!(worktree.is_dir(), "not forgotten, it stays");

    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));
    setup.daemon.kill().await;
    setup.daemon.restart().await;
    assert!(worktree.is_dir(), "forgotten, it stays for its week");
    // Released eight days ago: the next start takes it.
    setup.daemon.kill().await;
    let mut state = setup.daemon.state();
    let released = state["chats"][CHAT]["released"].as_i64().unwrap();
    state["chats"][CHAT]["released"] = json!(released - 8 * 24 * 60 * 60 * 1000);
    std::fs::write(setup.daemon.data.join("state.json"), state.to_string()).unwrap();
    setup.daemon.restart().await;
    assert!(!worktree.exists());
    assert!(setup.there().find_reference(&snapshot_ref(CHAT)).is_err());
    until("the client is back", || {
        (setup.client.state() == HostState::Ready).then_some(())
    })
    .await;
    assert!(owner(&setup.client).await.is_none(), "nor whose it was");
}

/// R01: a chat taken back but never forgotten (its computer gone for good)
/// keeps its worktree for its week, then the start's cleanup takes it,
/// while its rows stay for that computer's last pull (6.8).
#[tokio::test(flavor = "multi_thread")]
async fn a_released_chats_worktree_goes_after_its_week_even_unforgotten() {
    let mut setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    let worktree = worktree(&setup);
    assert_eq!(release(&setup.client, None).await, Ok(true));
    setup.daemon.kill().await;
    let mut state = setup.daemon.state();
    let released = state["chats"][CHAT]["released"].as_i64().unwrap();
    state["chats"][CHAT]["released"] = json!(released - 8 * 24 * 60 * 60 * 1000);
    std::fs::write(setup.daemon.data.join("state.json"), state.to_string()).unwrap();
    setup.daemon.restart().await;
    assert!(!worktree.exists(), "its week is over");
    assert!(setup.there().find_reference(&snapshot_ref(CHAT)).is_err());
    assert_eq!(ids(&setup), ["u1"], "its rows stay for the last pull");
    until("the client is back", || {
        (setup.client.state() == HostState::Ready).then_some(())
    })
    .await;
    assert!(
        owner(&setup.client).await.is_some_and(|o| o.released),
        "still released, for the pull"
    );
    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));
    assert!(ids(&setup).is_empty());
    // Its worktree gone and its rows forgotten, state.json lets it go too.
    let cleaned = setup.client.request::<Storage>(Request::CleanUp).await;
    cleaned.unwrap();
    assert!(
        setup.daemon.state()["chats"].get(CHAT).is_none(),
        "{}",
        setup.daemon.state()
    );
}

/// 6.8, 5.4: a chat its computer took back is never started here again,
/// whatever state.json says of its run: its computer runs it now.
#[tokio::test(flavor = "multi_thread")]
async fn a_released_chat_isnt_restarted_here() {
    let mut setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    run(&setup, vec![Reply::Text("Done.")]).await;
    assert_eq!(release(&setup.client, None).await, Ok(true));
    setup.daemon.kill().await;
    let mut state = setup.daemon.state();
    state["chats"][CHAT]["running"] = json!(true);
    std::fs::write(setup.daemon.data.join("state.json"), state.to_string()).unwrap();
    setup.daemon.restart().await;
    let chat = &setup.daemon.state()["chats"][CHAT];
    assert_eq!(chat["running"], false, "{chat}");
    assert!(
        setup
            .daemon
            .log()
            .contains("went back to its computer, so not restarted here"),
        "{}",
        setup.daemon.log()
    );
    assert_eq!(ids(&setup).len(), 3, "no row from a run here");
}

/// R01: "Clean up now" takes a forgotten chat's worktree at once.
#[tokio::test(flavor = "multi_thread")]
async fn clean_up_now_takes_a_forgotten_chats_worktree_at_once() {
    let setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    let worktree = worktree(&setup);
    assert_eq!(release(&setup.client, None).await, Ok(true));
    assert_eq!(forget(&setup.client, CHAT).await, Ok(true));

    let after: Storage = setup.client.request(Request::CleanUp).await.unwrap();
    assert!(!worktree.exists());
    assert!(after.projects > 0, "{after:?}");
}

/// K36: a chat its computer took back can move here again, and what its
/// earlier stay left goes first.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_taken_back_can_move_here_again() {
    let setup = Setup::new().await;
    let first = bundle(
        &setup,
        rows("First stay", vec![row("u1", "user", "One")]).await,
    )
    .await;
    import(&setup.client, "move-1", &first).await.unwrap();
    run(
        &setup,
        vec![
            writes("w1", "there.txt", "first stay\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    assert!(worktree(&setup).join("there.txt").is_file());
    common::until("the first stay's result", || {
        setup
            .there()
            .find_reference(&result_ref(CHAT))
            .ok()
            .map(drop)
    })
    .await;
    assert_eq!(release(&setup.client, None).await, Ok(true));

    let rows = rows(
        "Second stay",
        vec![
            row("u1", "user", "One"),
            row("a1", "assistant", "Back home"),
            row("u3", "user", "Two"),
        ],
    )
    .await;
    let second = bundle(&setup, rows).await;
    let imported = import(&setup.client, "move-2", &second).await.unwrap();
    assert!(!imported.already);
    let owned = Ownership {
        move_id: Some("move-2".into()),
        released: false,
        rev: imported.rev,
    };
    assert_eq!(owner(&setup.client).await, Some(owned));

    // Its rows are the second stay's alone, owned here again.
    assert_eq!(ids(&setup), ["u1", "a1", "u3"]);
    assert_eq!(
        chat_row(&setup),
        ("Second stay".to_string(), Some(PROJECT.to_string()), None)
    );
    // The first stay's worktree, result and state went.
    assert!(!worktree(&setup).join("there.txt").exists());
    assert!(worktree(&setup).join("src/main.rs").is_file());
    assert!(setup.there().find_reference(&result_ref(CHAT)).is_err());
    let state = setup.daemon.state();
    let chat = &state["chats"][CHAT];
    assert!(
        chat.get("released").is_none() && chat.get("result").is_none(),
        "{chat}"
    );
    // And it takes changes again.
    let model = ChatModel {
        provider_id: "p".into(),
        model_id: "m".into(),
        effort: None,
    };
    let set = Request::ChatSetModel {
        dialog_id: CHAT.into(),
        model,
    };
    assert_eq!(setup.client.call(set).await, Ok(Response::Done));
}

/// K44: a file the client couldn't send doesn't stop the import, and the
/// model is told it's no longer shown.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_the_server_lacks_doesnt_stop_the_import() {
    let setup = Setup::new().await;
    let image = "c".repeat(64);
    let with_image = NewMessage {
        attachments: vec![image.clone()],
        ..row("u1", "user", "Look at this")
    };
    let rows = rows("Screens", vec![with_image, row("a1", "assistant", "Seen.")]).await;
    let bundle = bundle(&setup, rows).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();

    // A row without a file stands for it, which BlobsMissing names.
    let db = setup.db();
    let bytes: i64 = db
        .query_row(
            "SELECT bytes FROM attachments WHERE id = ?1",
            [&image],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bytes, 0);
    let missing = Request::BlobsMissing {
        ids: vec![image.clone()],
    };
    let missing: Vec<String> = setup.client.request(missing).await.unwrap();
    assert_eq!(missing, [image]);

    let (url, seen) = model(vec![Reply::Text("Noted.")]).await;
    let mut start = message(&setup, &url, "later");
    start.profile.vision = true;
    setup
        .client
        .call(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();
    ended(&setup.client).await;
    let sent = seen.lock().unwrap()[0].to_string();
    assert!(sent.contains("no longer shown"), "{sent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_of_fifty_megabytes_imports() {
    let setup = Setup::new().await;
    let long = "x".repeat(1 << 20);
    let messages = (0..50)
        .map(|n| {
            let role = if n % 2 == 0 { "user" } else { "assistant" };
            row(&format!("m{n}"), role, &long)
        })
        .collect();
    let bundle = bundle(&setup, rows("Long", messages).await).await;
    let size = serde_json::to_vec(&bundle).unwrap().len();
    assert!(size > 50 << 20, "{size}");

    import(&setup.client, "move-1", &bundle).await.unwrap();
    let db = setup.db();
    let (count, bytes): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), SUM(LENGTH(COALESCE(b.content, m.content)))
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
             WHERE m.dialog_id = ?1",
            [CHAT],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, bytes), (50, 50 << 20));
}

/// What the chat's agent left running here (a dev server, bash's
/// `background`) stops as the chat goes back to its computer, which runs
/// it from then on: with its release, not only once its rows are
/// forgotten, which can wait for the computer's next connection.
#[tokio::test(flavor = "multi_thread")]
async fn what_a_chat_left_running_here_stops_as_it_goes_back() {
    let setup = Setup::new().await;
    let bundle = bundle(&setup, rows("Moved", vec![row("u1", "user", "Go")]).await).await;
    import(&setup.client, "move-1", &bundle).await.unwrap();
    let serve = Reply::Call {
        id: "b1",
        name: "bash",
        args: json!({ "command": beating("listening"), "background": true }),
    };
    run(&setup, vec![serve, Reply::Text("It runs.")]).await;
    let worktree = worktree(&setup);
    assert!(beats(&worktree), "it runs on after the run");
    let running = || async {
        let asked = Request::BackgroundTasks {
            dialog_id: CHAT.into(),
        };
        let tasks: Vec<BackgroundTask> = setup.client.request(asked).await.unwrap();
        tasks
    };
    assert_eq!(running().await.len(), 1);

    assert_eq!(release(&setup.client, Some("move-1")).await, Ok(true));
    assert!(!beats(&worktree), "stopped with the release");
    assert!(running().await.is_empty());
}

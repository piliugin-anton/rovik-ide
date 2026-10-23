//! Moving a chat to the real daemon and back (M06, M07, 6.8): a run going
//! pauses for the move and goes on where the chat went, nothing starts in
//! the chat while it moves, its files go with it, its changes come back to
//! wait here, and a move the app left unfinished is finished or undone once
//! the host answers, or cancelled while it can't. And a chat there (M08,
//! 6.1): its writes go to the daemon, and so does its deletion, at once or
//! when the daemon is back.

mod common;

use common::app::{landed, App, HOST};
use common::project::{
    ended, message, start, tool_results, unattended, writes, Setup, CHAT, PROJECT,
};
use common::{messages_in, until};
use rovik_client::shadow::Shadow;
use rovik_client::CloudEvent;
use rovik_core::agent_run::permissions::{ApprovalRule, Approver, Permissions};
use rovik_core::agent_run::{ApprovalDecision, StartKind, UserMessage};
use rovik_core::chat_db::location::{self, Location};
use rovik_core::chat_db::moves::{self, CloudMove};
use rovik_core::chat_db::store::{self, ChatModel, NewMessage};
use rovik_core::chat_db::{attachments, mirror, sync};
use rovik_core::overlay::chats;
use rovik_core::testing::{model, Reply};
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::sync::{ChatBundle, Imported, Ownership};
use rovik_protocol::work::Project;
use rovik_protocol::{FailCode, Request, Response};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A 1×1 PNG.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A one-page PDF.
fn pdf_file() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
      2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
      3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 3 3]>>endobj\n\
      trailer<</Root 1 0 R>>\n%%EOF"
        .to_vec()
}

/// Each step a move reported, and its message.
type Steps = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// The steps of moves the app reports on `cloud-event`, a move back's as
/// "back <step>".
fn moves_of(app: &App) -> Steps {
    let steps: Steps = Arc::default();
    let seen = Arc::clone(&steps);
    app.remote.on_cloud(Arc::new(move |event| match event {
        CloudEvent::Moving { step, message, .. } => seen.lock().unwrap().push((step, message)),
        CloudEvent::MovingBack { step, message, .. } => {
            seen.lock().unwrap().push((format!("back {step}"), message))
        }
        _ => {}
    }));
    steps
}

fn steps(moves: &Mutex<Vec<(String, Option<String>)>>) -> Vec<String> {
    moves
        .lock()
        .unwrap()
        .iter()
        .map(|(s, _)| s.clone())
        .collect()
}

/// A command that waits for the user's approval.
fn asks() -> Permissions {
    Permissions {
        command_approval: ApprovalRule {
            required: true,
            by: Approver::User,
        },
        ..unattended()
    }
}

fn ls(id: &'static str) -> Reply {
    Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": "ls" }),
    }
}

/// Waits until the app was asked to approve tool call `id`.
async fn asked(app: &App, id: &str) {
    until(&format!("the approval of {id} asked"), || {
        let asked = events(app)
            .into_iter()
            .any(|e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == id);
        asked.then_some(())
    })
    .await;
}

fn approve(id: &str) -> Request {
    Request::AgentResolveApproval {
        dialog_id: CHAT.into(),
        tool_call_id: id.into(),
        decision: ApprovalDecision::Allow,
    }
}

/// The chat moved to the daemon from the app, with nothing running.
async fn moved_there(app: &App) {
    app.remote.move_chat(CHAT, HOST, None).await.unwrap();
    assert_eq!(where_is(app).await, Location::Host(HOST.into()));
}

/// A run on the daemon that wrote `notes.txt`, waiting for the approval
/// of its next command (`c2`), and a model that goes on with `then` once
/// the chat is back.
async fn writing_there(
    app: &App,
    setup: &Setup,
    then: Vec<Reply>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let mut replies = vec![writes("c1", "notes.txt", "from the server\n"), ls("c2")];
    replies.extend(then);
    let (url, seen) = model(replies).await;
    app.remote.start(start(setup, &url, asks())).await.unwrap();
    asked(app, "c2").await;
    (url, seen)
}

fn events(app: &App) -> Vec<Value> {
    app.test.events.lock().unwrap().clone()
}

async fn where_is(app: &App) -> Location {
    let read = app.test.core.db.read(|c| location::of_chat(c, CHAT));
    read.await.unwrap()
}

async fn its_move(app: &App) -> Option<CloudMove> {
    let read = app.test.core.db.read(|c| moves::of_chat(c, CHAT));
    read.await.unwrap()
}

/// Waits until the app has the chat where `wanted` says.
async fn until_at(app: &App, wanted: Location) {
    for _ in 0..800 {
        if where_is(app).await == wanted && its_move(app).await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the chat stayed at {:?}, its move {:?}",
        where_is(app).await,
        its_move(app).await
    );
}

/// Waits until the app's mirror has the chat as the daemon has it.
async fn mirrored(app: &App, setup: &Setup) {
    let here = app.test.core.paths.data_dir.join("chats.db");
    let there = setup.daemon.data.join("data/chats.db");
    for _ in 0..800 {
        let ours = messages_in(&here, CHAT);
        if !ours.is_empty() && ours == messages_in(&there, CHAT) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the mirror never caught up:\nhere {:?}\nthere {:?}",
        messages_in(&here, CHAT),
        messages_in(&there, CHAT)
    );
}

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
        created_at: 1,
        finalized: true,
    }
}

/// A move done by hand up to its import, as `Remote::move_chat` does it,
/// the app then closing: the chat locked at `step`, and with `import`, its
/// bundle imported on the daemon. The import's answer, if it went.
async fn cut_short(app: &App, setup: &Setup, step: &'static str, import: bool) -> Option<Imported> {
    let written = app.test.core.db.write(|c| {
        store::append_messages(
            c,
            CHAT,
            &[row("u1", "user", "Hi"), row("a1", "assistant", "Hello.")],
        )?;
        moves::lock(c, CHAT, HOST, "move-1")?;
        moves::step(c, CHAT, step)
    });
    written.await.unwrap();
    if !import {
        return None;
    }
    let shadow = Shadow::open(&setup.shadow, &setup.project).unwrap();
    let folded = shadow.fold(CHAT, &[]).unwrap();
    let name = snapshot_ref(CHAT);
    let pushed = setup
        .client
        .push(PROJECT, &setup.shadow, &name, folded.start, |_| {});
    pushed.await.unwrap();
    let read = app
        .test
        .core
        .db
        .read(|c| sync::changes_since(c, CHAT, 0, None, usize::MAX));
    let bundle = ChatBundle {
        dialog_id: CHAT.into(),
        rows: read.await.unwrap().changes,
        project: Project {
            id: PROJECT.into(),
            name: "app".into(),
            subpath: String::new(),
        },
        settings: None,
        excludes: Vec::new(),
        snapshot: folded.base.to_string(),
        changes: Some(folded.start.to_string()),
        pending: None,
    };
    let imported = setup.client.import_chat("move-1", &bundle, |_| {}).await;
    Some(imported.unwrap())
}

async fn owner_there(setup: &Setup) -> Option<Ownership> {
    let asked = Request::ChatOwner {
        dialog_id: CHAT.into(),
    };
    setup.client.request(asked).await.unwrap()
}

/// 6.8: a run going pauses for the move once its tool call has its result,
/// what it waits on can still be answered, nothing else starts meanwhile
/// (K25), and it goes on on the daemon, whose rows the mirror then has.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_going_moves_with_the_chat_and_goes_on_there() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let steps = moves_of(&app);
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": "ls" }),
        },
        Reply::Text("Carried on there."),
    ])
    .await;
    let asks = Permissions {
        command_approval: ApprovalRule {
            required: true,
            by: Approver::User,
        },
        ..unattended()
    };
    app.remote.start(start(&setup, &url, asks)).await.unwrap();
    until("the approval asked here", || {
        events(&app)
            .into_iter()
            .find(|e| e["type"] == "approvalRequested")
    })
    .await;

    let (remote, template) = (app.remote.clone(), start(&setup, &url, unattended()));
    let moving = tokio::spawn(async move { remote.move_chat(CHAT, HOST, Some(template)).await });
    until("the move waiting for the run", || {
        let steps = steps.lock().unwrap();
        steps
            .iter()
            .any(|(step, _)| step == "pausing")
            .then_some(())
    })
    .await;
    assert_eq!(where_is(&app).await, Location::Moving(HOST.into()));
    // Nothing starts in the chat while it moves.
    let next = app.remote.start(message(&setup, &url, "u2")).await;
    let fail = next.unwrap_err();
    assert!(fail.message.contains("moving to server"), "{fail}");
    let mut again = start(&setup, &url, unattended());
    again.start = StartKind::ContinueRun;
    assert!(app.remote.start(again).await.is_err());
    // What the run waits on is still answered.
    let approve = Request::AgentResolveApproval {
        dialog_id: CHAT.into(),
        tool_call_id: "c1".into(),
        decision: ApprovalDecision::Allow,
    };
    let answered = app.remote.route(CHAT, approve).await;
    assert_eq!(answered, Ok(Response::Bool(true)));
    moving.await.unwrap().unwrap();

    assert_eq!(where_is(&app).await, Location::Host(HOST.into()));
    assert_eq!(app.remote.host_of(CHAT).as_deref(), Some(HOST));
    assert!(its_move(&app).await.is_none());
    let reported: Vec<String> = steps
        .lock()
        .unwrap()
        .iter()
        .map(|(s, _)| s.clone())
        .collect();
    assert_eq!(
        reported,
        ["pausing", "pushing", "uploading", "importing", "moved"]
    );
    // The run went on there, and its events reached the app.
    until("the reply from there", || {
        let rows = setup.daemon.rows(CHAT);
        let done = rows
            .last()
            .is_some_and(|r| r.content == "Carried on there.");
        done.then_some(())
    })
    .await;
    ended(&setup.client).await;
    mirrored(&app, &setup).await;
    let rows = setup.daemon.rows(CHAT);
    let pause = rows.iter().find(|r| r.meta["paused"]["kind"] == "move");
    let pause = pause.unwrap_or_else(|| panic!("no pause for the move: {rows:?}"));
    assert_eq!(pause.meta["paused"]["host"], HOST);
    let heard = events(&app).into_iter().any(|e| {
        e["type"] == "messagesAdded" && e["messages"][0]["content"] == "Carried on there."
            || e["type"] == "messagePatched" && e["patch"]["content"] == "Carried on there."
    });
    assert!(heard, "the reply's events reached the app");
    assert_eq!(seen.lock().unwrap().len(), 2, "one request here, one there");
}

/// Killed between the import and the flip: the next start finishes the
/// flip, since the host has the chat from that move.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_cut_short_after_its_import_is_finished_at_the_next_start() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    let imported = cut_short(&app, &setup, moves::IMPORTING, true)
        .await
        .unwrap();

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    until_at(&app, Location::Host(HOST.into())).await;
    let read = app.test.core.db.read(|c| mirror::state(c, CHAT));
    assert_eq!(read.await.unwrap().unwrap().rev, imported.rev);
    assert_eq!(app.remote.host_of(CHAT).as_deref(), Some(HOST));
}

/// Killed before the import: the host doesn't know the chat, so the move
/// is undone.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_cut_short_before_its_import_rolls_back() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    cut_short(&app, &setup, moves::PUSHED, false).await;

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    until_at(&app, Location::Here).await;
    assert!(app.remote.host_of(CHAT).is_none());
    assert_eq!(owner_there(&setup).await, None);
}

/// "Cancel move" while the host can't be asked: the chat is local at once,
/// and the release it queued reaches the host when it's back.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_move_with_the_host_away_queues_its_release() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    cut_short(&app, &setup, moves::IMPORTING, true).await;
    let app = app.restart(&setup).await;

    app.remote.cancel_move(CHAT).await.unwrap();
    assert_eq!(where_is(&app).await, Location::Here);
    let queued = its_move(&app).await.unwrap();
    assert_eq!(queued.step, moves::CANCELLED);
    let owned = owner_there(&setup).await.unwrap();
    assert!(!owned.released, "the host doesn't know yet");

    app.connect(&setup).await;
    for _ in 0..400 {
        if its_move(&app).await.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(its_move(&app).await.is_none(), "the queued release went");
    let owned = owner_there(&setup).await.unwrap();
    assert_eq!(owned.move_id.as_deref(), Some("move-1"));
    assert!(owned.released);
    assert!(setup.daemon.rows(CHAT).is_empty(), "and its rows there");
    assert_eq!(where_is(&app).await, Location::Here);
}

/// 6.11: the files a chat's history names go with it, and requests there
/// send them to the model as here.
#[tokio::test(flavor = "multi_thread")]
async fn a_chats_image_and_pdf_reach_the_model_after_its_move() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let stored = |bytes: Vec<u8>| attachments::store_attached(&app.test.core.db, bytes);
    let image = stored(PNG.to_vec()).await.unwrap().id;
    let pdf = stored(pdf_file()).await.unwrap().id;
    let (url, seen) = model(vec![Reply::Text("I see them.")]).await;
    let mut first = start(&setup, &url, unattended());
    first.profile.vision = true;
    first.profile.pdf = true;
    first.start = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "Look at these".into(),
            meta: Some(json!({ "pdfs": [{ "id": pdf, "name": "notes.pdf", "pages": 1 }] })),
            attachments: vec![image.clone(), pdf.clone()],
        },
    };
    app.remote.start(first.clone()).await.unwrap();
    until("the reply here", || {
        let found = events(&app).into_iter().find(|e| e["type"] == "runEnded");
        found.map(drop)
    })
    .await;

    app.remote.move_chat(CHAT, HOST, None).await.unwrap();
    let there = setup.daemon.data.join("data");
    assert!(attachments::file_for(&there, &image).is_some());
    assert!(attachments::file_for(&there, &pdf).is_some());

    let mut next = first;
    next.start = StartKind::Send {
        message: UserMessage {
            id: "u2".into(),
            content: "And now?".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    app.remote.start(next).await.unwrap();
    ended(&setup.client).await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let sent = requests[1].to_string();
    assert!(sent.contains("data:image/png;base64,"), "the image");
    assert!(sent.contains("data:application/pdf;base64,"), "the PDF");
}

/// 6.8's move back: the run pauses there once its command has its result,
/// nothing starts in the chat meanwhile, the daemon refuses its writes
/// once it let the chat go and forgets its rows after the last pull, and
/// the chat's changes wait here while its run goes on, in a view of its
/// own that has them, and land when it ends.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_moved_back_waits_with_its_changes_and_its_run_goes_on_here() {
    // Without FUSE the chat's view can't be mounted, so its changes land
    // first instead of waiting (the test below without isolation).
    if !rovik_core::vfs::mount_works() {
        return;
    }
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let moves = moves_of(&app);
    moved_there(&app).await;
    let here = vec![ls("c3"), Reply::Text("Done here.")];
    let (url, seen) = writing_there(&app, &setup, here).await;

    let (remote, template) = (app.remote.clone(), start(&setup, &url, asks()));
    let moving = tokio::spawn(async move { remote.move_back(CHAT, Some(template)).await });
    until("the move back waiting for the run", || {
        steps(&moves)
            .contains(&"back pausing".to_string())
            .then_some(())
    })
    .await;
    let next = app.remote.start(message(&setup, &url, "u2")).await;
    let fail = next.unwrap_err();
    assert!(fail.message.contains("moving back"), "{fail}");
    let rename = Request::ChatRename {
        dialog_id: CHAT.into(),
        title: "Renamed".into(),
    };
    assert!(app.remote.route(CHAT, rename.clone()).await.is_err());
    // What the run waits on is still answered there.
    let answered = app.remote.route(CHAT, approve("c2")).await;
    assert_eq!(answered, Ok(Response::Bool(true)));
    moving.await.unwrap().unwrap();

    assert_eq!(where_is(&app).await, Location::Here);
    assert!(app.remote.host_of(CHAT).is_none());
    assert!(its_move(&app).await.is_none(), "the daemon forgot the chat");
    let moved: Vec<String> = steps(&moves)
        .into_iter()
        .filter(|s| s.starts_with("back "))
        .collect();
    assert_eq!(
        moved,
        [
            "back pausing",
            "back fetching",
            "back releasing",
            "back pulling",
            "back moved"
        ]
    );
    // The daemon let the chat go: its writes are refused, and its rows
    // went once the app had them all.
    let refused = setup.client.request::<()>(rename).await.unwrap_err();
    assert_eq!(refused.code, FailCode::NotOwner, "{refused}");
    assert!(setup.daemon.rows(CHAT).is_empty());
    let here = app.test.core.paths.data_dir.join("chats.db");
    let rows = messages_in(&here, CHAT);
    assert!(
        format!("{rows:?}").contains(r#"\"paused\":{\"kind\":\"move\""#),
        "the pause for the move came back: {rows:?}"
    );

    // Its changes wait here as a local isolated chat's do.
    let pending = chats::pending(CHAT).expect("its changes wait");
    assert_eq!(pending.reason, "Not landed: the run didn't finish.");
    let changed: Vec<String> = chats::changes(CHAT)
        .iter()
        .map(|c| c.path().to_string())
        .collect();
    assert_eq!(changed, ["notes.txt"]);
    assert!(!setup.project.join("notes.txt").exists());
    // Its run goes on here, in a view that has them.
    asked(&app, "c3").await;
    let listed = app.remote.route(CHAT, approve("c3")).await;
    assert_eq!(listed, Ok(Response::Bool(true)));
    assert_eq!(landed(&setup, "notes.txt").await, "from the server\n");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 4, "two requests there, two here");
    assert!(
        requests[3].to_string().contains("notes.txt"),
        "the command here listed the chat's file: {}",
        requests[3]
    );
}

/// Where the chat's runs here couldn't see its changes, they land before
/// its run goes on, in the project.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_moved_back_without_isolation_lands_its_changes_first() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let moves = moves_of(&app);
    moved_there(&app).await;
    let (url, seen) = writing_there(&app, &setup, vec![Reply::Text("Done here.")]).await;

    let mut template = start(&setup, &url, asks());
    template.isolate = Some(false);
    let remote = app.remote.clone();
    let moving = tokio::spawn(async move { remote.move_back(CHAT, Some(template)).await });
    until("the move back waiting for the run", || {
        steps(&moves)
            .contains(&"back pausing".to_string())
            .then_some(())
    })
    .await;
    app.remote.route(CHAT, approve("c2")).await.unwrap();
    moving.await.unwrap().unwrap();

    assert!(steps(&moves).contains(&"back landing".to_string()));
    assert!(!chats::exists(CHAT), "nothing waits");
    let file = setup.project.join("notes.txt");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "from the server\n");
    until("the run going on here", || {
        (seen.lock().unwrap().len() == 3).then_some(())
    })
    .await;
}

/// The app killed after the daemon let the chat go: the daemon keeps its
/// rows, and the next start finishes the move back with them, then has
/// the daemon forget them (K51).
#[tokio::test(flavor = "multi_thread")]
async fn a_move_back_cut_short_after_its_release_finishes_at_the_next_start() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    // A turn there the app doesn't hear of.
    let app = app.restart(&setup).await;
    let (url, _) = model(vec![Reply::Text("Hello from there.")]).await;
    let turn = Request::AgentStart(Box::new(start(&setup, &url, unattended())));
    setup.client.call(turn).await.unwrap();
    ended(&setup.client).await;
    // Cut short after the release.
    let locked = app.test.core.db.write(|c| {
        moves::lock_back(c, CHAT, HOST, "back-1")?;
        moves::step(c, CHAT, moves::RELEASED)
    });
    locked.await.unwrap();
    let release = Request::ChatRelease {
        dialog_id: CHAT.into(),
        move_id: None,
    };
    assert!(setup.client.request::<bool>(release).await.unwrap());
    let there = setup.daemon.data.join("data/chats.db");
    let theirs = messages_in(&there, CHAT);
    assert_eq!(theirs.len(), 2, "the daemon keeps the chat's rows");

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    until_at(&app, Location::Here).await;
    let here = app.test.core.paths.data_dir.join("chats.db");
    assert_eq!(messages_in(&here, CHAT), theirs);
    assert!(app.remote.host_of(CHAT).is_none());
    assert!(messages_in(&there, CHAT).is_empty(), "then it forgot them");
}

/// The release asked, its answer lost: the host let the chat go, so the
/// move back isn't dropped, and the next start finishes it, as after a
/// release it heard. "Cancel move" can't drop it meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_back_whose_release_answer_was_lost_finishes_at_the_next_start() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let app = app.restart(&setup).await;
    let locked = app.test.core.db.write(|c| {
        moves::lock_back(c, CHAT, HOST, "back-1")?;
        moves::step(c, CHAT, moves::RELEASING)
    });
    locked.await.unwrap();
    let release = Request::ChatRelease {
        dialog_id: CHAT.into(),
        move_id: None,
    };
    assert!(setup.client.request::<bool>(release).await.unwrap());
    let refused = app.remote.cancel_move(CHAT).await.unwrap_err();
    assert!(refused.message.contains("can't be cancelled"), "{refused}");
    assert!(its_move(&app).await.is_some(), "kept");

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    until_at(&app, Location::Here).await;
    assert!(app.remote.host_of(CHAT).is_none());
}

/// The release asked, never taken: the move back is dropped once the host
/// says so, and the chat stays there.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_back_whose_release_never_reached_the_host_leaves_the_chat_there() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let app = app.restart(&setup).await;
    let locked = app.test.core.db.write(|c| {
        moves::lock_back(c, CHAT, HOST, "back-1")?;
        moves::step(c, CHAT, moves::RELEASING)
    });
    locked.await.unwrap();

    app.connect(&setup).await;
    until_at(&app, Location::Host(HOST.into())).await;
    assert_eq!(app.remote.host_of(CHAT).as_deref(), Some(HOST));
    assert!(!owner_there(&setup).await.unwrap().released);
}

/// Cut short before the daemon let the chat go: the move back is dropped,
/// and the chat stays there.
#[tokio::test(flavor = "multi_thread")]
async fn a_move_back_cut_short_before_its_release_leaves_the_chat_there() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let app = app.restart(&setup).await;
    let locked = app.test.core.db.write(|c| {
        moves::lock_back(c, CHAT, HOST, "back-1")?;
        moves::step(c, CHAT, moves::PAUSED)
    });
    locked.await.unwrap();

    app.connect(&setup).await;
    until_at(&app, Location::Host(HOST.into())).await;
    assert_eq!(app.remote.host_of(CHAT).as_deref(), Some(HOST));
    let owned = owner_there(&setup).await.unwrap();
    assert!(!owned.released);
}

/// The chat's own row as a database has it: its title and model.
fn chat_in(database: &std::path::Path) -> Option<(String, Option<String>, Option<String>)> {
    let db = rusqlite::Connection::open(database).ok()?;
    let row = db.query_row(
        "SELECT title, model_provider, model_id FROM dialogs WHERE id = ?1",
        [CHAT],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    row.ok()
}

/// 6.1: a rename, a model and a checklist's dismissal go to the daemon, and
/// the mirror has the chat as the daemon has it after them.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_chats_writes_go_to_its_host_and_its_mirror_follows() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let (url, _) = model(vec![Reply::Text("Hello.")]).await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    ended(&setup.client).await;
    mirrored(&app, &setup).await;

    let writes = [
        Request::ChatRename {
            dialog_id: CHAT.into(),
            title: "Renamed".into(),
        },
        Request::ChatSetModel {
            dialog_id: CHAT.into(),
            model: ChatModel {
                provider_id: "p".into(),
                model_id: "m2".into(),
                effort: None,
            },
        },
        Request::ChatDismissChecklist {
            dialog_id: CHAT.into(),
            message_id: "u1".into(),
        },
    ];
    for write in writes {
        let shown = format!("{write:?}");
        let answer = app.remote.route(CHAT, write).await;
        assert!(answer.is_ok(), "{shown}: {answer:?}");
    }
    let there = setup.daemon.data.join("data/chats.db");
    let theirs = chat_in(&there).unwrap();
    assert_eq!(
        theirs,
        ("Renamed".into(), Some("p".into()), Some("m2".into()))
    );
    let rows = setup.daemon.rows(CHAT);
    let u1 = rows.iter().find(|r| r.id == "u1").unwrap();
    assert_eq!(u1.meta["checklistClosed"], true);

    let here = app.test.core.paths.data_dir.join("chats.db");
    until("the mirror's chat as the daemon's", || {
        let same = chat_in(&here) == Some(theirs.clone())
            && messages_in(&here, CHAT) == messages_in(&there, CHAT);
        same.then_some(())
    })
    .await;
}

/// 6.1: a chat deleted here goes on the daemon at once: its rows, its
/// worktree, and what the daemon kept of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_chat_deleted_here_goes_on_its_host() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let tree = setup.daemon.state()["chats"][CHAT]["worktree"]
        .as_str()
        .map(std::path::PathBuf::from)
        .unwrap();

    app.remote.delete(CHAT).await.unwrap();
    let here = app.test.core.paths.data_dir.join("chats.db");
    assert_eq!(chat_in(&here), None);
    assert!(app.remote.host_of(CHAT).is_none());
    assert!(its_move(&app).await.is_none(), "nothing left to tell");
    assert_eq!(owner_there(&setup).await, None);
    assert!(!tree.exists());
}

/// 6.1: a chat deleted while the daemon is away goes here at once, and on
/// the daemon when it's back, from its tombstone.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_deleted_while_its_host_is_away_goes_there_when_it_is_back() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    moved_there(&app).await;
    let app = app.restart(&setup).await;

    app.remote.delete(CHAT).await.unwrap();
    let here = app.test.core.paths.data_dir.join("chats.db");
    assert_eq!(chat_in(&here), None);
    assert!(app.remote.host_of(CHAT).is_none());
    let tombstone = its_move(&app).await.unwrap();
    assert_eq!(tombstone.step, moves::DELETED);
    assert!(
        owner_there(&setup).await.is_some(),
        "the daemon has it still"
    );

    let app = app.restart(&setup).await;
    app.connect(&setup).await;
    for _ in 0..400 {
        if its_move(&app).await.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(its_move(&app).await.is_none(), "the tombstone was told");
    assert_eq!(owner_there(&setup).await, None);
    assert!(setup.daemon.rows(CHAT).is_empty());
}

/*
 * A move's run, picked up where the chat ends: a run cut for the move (it
 * waited only for the user) resumes, and one that paused for it goes on,
 * whether the chat moved or stayed.
 */

/// The chat as a crash left it: the user's message, and a reply whose
/// `ls` (`c1`) has no result yet.
async fn cut_by_a_crash(app: &App) {
    let assistant = NewMessage {
        tool_calls: Some(vec![store::ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: json!({ "command": "ls" }).to_string(),
            extra_content: None,
        }]),
        ..row("a1", "assistant", "")
    };
    let rows = vec![row("u1", "user", "List files"), assistant];
    let written = app.test.core.db.write(move |c| store::append_messages(c, CHAT, &rows));
    written.await.unwrap();
}

/// A run resumed after a crash, waiting only for the user to approve the
/// call the crash cut (`c1`): a move cuts it.
async fn resumed_waiting(app: &App, setup: &Setup, replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Value>>>) {
    cut_by_a_crash(app).await;
    let (url, seen) = model(replies).await;
    let mut resume = start(setup, &url, asks());
    resume.start = StartKind::Resume;
    app.remote.start(resume).await.unwrap();
    asked(app, "c1").await;
    (url, seen)
}

/// The steps the app reports, and "Cancel move" asked as soon as the move
/// reports `at`, as a person would click it.
fn cancelling_at(app: &App, at: &'static str) -> Steps {
    let steps: Steps = Arc::default();
    let seen = Arc::clone(&steps);
    let remote = Arc::downgrade(&app.remote);
    app.remote.on_cloud(Arc::new(move |event| {
        let CloudEvent::Moving { step, message, .. } = event else {
            return;
        };
        if step == at {
            if let Some(remote) = remote.upgrade() {
                tokio::spawn(async move { remote.cancel_move(CHAT).await.unwrap() });
            }
        }
        seen.lock().unwrap().push((step, message));
    }));
    steps
}

/// A move that fails once it pushes the chat's files: a file where the
/// project's shadow, which the push comes from, would be.
fn shadow_broken(setup: &Setup) {
    let _ = std::fs::remove_dir_all(&setup.shadow);
    std::fs::create_dir_all(setup.shadow.parent().unwrap()).unwrap();
    std::fs::write(&setup.shadow, "not a repository").unwrap();
}

/// Waits until the app was asked to approve `c1` again: an interrupted
/// call, resumed, is always confirmed by the user. Then approves it,
/// wherever the chat is.
async fn approved_again(app: &App) {
    until("c1's approval asked again", || {
        let asked = events(app)
            .into_iter()
            .filter(|e| e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "c1")
            .count();
        (asked >= 2).then_some(())
    })
    .await;
    assert_eq!(app.remote.route(CHAT, approve("c1")).await, Ok(Response::Bool(true)));
}

/// The chat's rows here: role and content.
async fn rows_here(app: &App) -> Vec<(String, String)> {
    let read = app.test.core.db.read(|c| store::get_messages(c, CHAT, None, 200));
    let page = read.await.unwrap();
    page.messages
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect()
}

/// Waits until the chat's last row here says `content` and its run ended.
async fn replied_here(app: &App, content: &str) {
    for _ in 0..1200 {
        let rows = rows_here(app).await;
        let last = rows.last().map(|(_, c)| c.as_str());
        if last == Some(content) && !app.test.core.agents.is_running(CHAT) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never replied {content:?} here: {:?}", rows_here(app).await);
}

/// 6.8: the cut run resumes where the chat went: its call, asked about
/// again, runs there once approved, and so does the request after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_cut_for_a_move_resumes_on_the_host() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let reported = moves_of(&app);
    let (url, seen) = resumed_waiting(&app, &setup, vec![Reply::Text("Listed there.")]).await;

    let template = start(&setup, &url, unattended());
    app.remote.move_chat(CHAT, HOST, Some(template)).await.unwrap();
    assert_eq!(where_is(&app).await, Location::Host(HOST.into()));
    assert!(!app.test.core.agents.is_running(CHAT), "cut here");
    approved_again(&app).await;
    until("the reply there", || {
        let rows = setup.daemon.rows(CHAT);
        rows.last()
            .is_some_and(|r| r.content == "Listed there.")
            .then_some(())
    })
    .await;
    ended(&setup.client).await;
    assert_eq!(tool_results(&setup).len(), 1, "c1 ran there");
    assert_eq!(seen.lock().unwrap().len(), 1, "no request here");
    assert_eq!(
        steps(&reported),
        ["pausing", "pushing", "uploading", "importing", "moved"]
    );
}

/// The cut run's move cancelled: the chat stays, and its run resumes
/// here, as it would have there. Nothing else resumes it: the app resumed
/// it once already.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_cut_for_a_cancelled_move_resumes_here() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let reported = cancelling_at(&app, "pushing");
    let (url, seen) = resumed_waiting(&app, &setup, vec![Reply::Text("Listed here.")]).await;

    let template = start(&setup, &url, unattended());
    let moved = app.remote.move_chat(CHAT, HOST, Some(template)).await;
    let fail = moved.unwrap_err();
    assert!(fail.message.contains("cancelled"), "{fail}");
    assert_eq!(where_is(&app).await, Location::Here);
    assert!(steps(&reported).contains(&"cancelled".to_string()), "{:?}", steps(&reported));

    approved_again(&app).await;
    replied_here(&app, "Listed here.").await;
    let rows = rows_here(&app).await;
    assert_eq!(
        rows.iter().filter(|(role, _)| role == "tool").count(),
        1,
        "c1 ran here: {rows:?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(setup.daemon.rows(CHAT).is_empty(), "nothing there");
}

/// The cut run's move failed: the chat stays, and its run resumes here.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_cut_for_a_failed_move_resumes_here() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let reported = moves_of(&app);
    let (url, seen) = resumed_waiting(&app, &setup, vec![Reply::Text("Listed here.")]).await;
    shadow_broken(&setup);

    let template = start(&setup, &url, unattended());
    let moved = app.remote.move_chat(CHAT, HOST, Some(template)).await;
    assert!(moved.is_err());
    assert_eq!(where_is(&app).await, Location::Here);
    assert_eq!(steps(&reported).last().map(String::as_str), Some("failed"));

    approved_again(&app).await;
    replied_here(&app, "Listed here.").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(setup.daemon.rows(CHAT).is_empty(), "nothing there");
}

/// A run that paused for the move at its safe point, the move then
/// failing: it goes on here, rather than stop at a pause that says the
/// chat moved.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_paused_for_a_failed_move_goes_on_here() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let reported = moves_of(&app);
    let (url, seen) = model(vec![ls("c1"), Reply::Text("Went on here.")]).await;
    app.remote.start(start(&setup, &url, asks())).await.unwrap();
    asked(&app, "c1").await;
    shadow_broken(&setup);

    let (remote, template) = (app.remote.clone(), start(&setup, &url, unattended()));
    let moving = tokio::spawn(async move { remote.move_chat(CHAT, HOST, Some(template)).await });
    until("the move waiting for the run", || {
        steps(&reported).contains(&"pausing".to_string()).then_some(())
    })
    .await;
    // Its call answered, the run reaches its safe point: it pauses.
    assert_eq!(app.remote.route(CHAT, approve("c1")).await, Ok(Response::Bool(true)));
    assert!(moving.await.unwrap().is_err());
    assert_eq!(where_is(&app).await, Location::Here);

    replied_here(&app, "Went on here.").await;
    assert_eq!(seen.lock().unwrap().len(), 2, "both requests here");
    assert!(setup.daemon.rows(CHAT).is_empty(), "nothing there");
}

/// "Cancel move" while the run makes its way to its safe point: the pause
/// is taken back, and the run, still at work, goes on by itself. Nothing
/// else starts in the chat meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_moves_pause_is_taken_back_goes_on_alone() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let reported = moves_of(&app);
    let (url, seen) = model(vec![ls("c1"), Reply::Text("Done here.")]).await;
    app.remote.start(start(&setup, &url, asks())).await.unwrap();
    asked(&app, "c1").await;

    let (remote, template) = (app.remote.clone(), start(&setup, &url, unattended()));
    let moving = tokio::spawn(async move { remote.move_chat(CHAT, HOST, Some(template)).await });
    until("the move waiting for the run", || {
        steps(&reported).contains(&"pausing".to_string()).then_some(())
    })
    .await;
    app.remote.cancel_move(CHAT).await.unwrap();
    let fail = moving.await.unwrap().unwrap_err();
    assert!(fail.message.contains("cancelled"), "{fail}");
    assert_eq!(where_is(&app).await, Location::Here);
    assert!(app.test.core.agents.is_running(CHAT), "the run goes on");
    let started = |app: &App| {
        events(app)
            .into_iter()
            .filter(|e| e["type"] == "runStarted")
            .count()
    };
    let before = started(&app);

    assert_eq!(app.remote.route(CHAT, approve("c1")).await, Ok(Response::Bool(true)));
    replied_here(&app, "Done here.").await;
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(started(&app), before, "no other run started");
    let rows = rows_here(&app).await;
    assert!(
        !rows.iter().any(|(_, c)| c.contains("Moved")),
        "no pause for a move here: {rows:?}"
    );
}

/// A cut run's move cancelled with no start to pick it up with (the app
/// gave none): nothing starts, and its call still waits for a `Resume`.
#[tokio::test(flavor = "multi_thread")]
async fn a_cut_run_with_no_start_waits_after_a_cancelled_move() {
    let mut setup = Setup::new().await;
    let app = App::local(&mut setup).await;
    app.connect(&setup).await;
    let _steps = cancelling_at(&app, "pushing");
    let (_, seen) = resumed_waiting(&app, &setup, Vec::new()).await;

    assert!(app.remote.move_chat(CHAT, HOST, None).await.is_err());
    assert_eq!(where_is(&app).await, Location::Here);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!app.test.core.agents.is_running(CHAT));
    let waits = rovik_core::agent_run::waits_for_results(&app.test.core, CHAT).await;
    assert!(waits.unwrap(), "c1 still has no result");
    let roles: Vec<String> = rows_here(&app).await.into_iter().map(|(r, _)| r).collect();
    assert_eq!(roles, ["user", "assistant"]);
    assert!(seen.lock().unwrap().is_empty(), "no request");
}

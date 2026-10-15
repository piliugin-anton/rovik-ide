//! The app's remote chats against the real daemon (P08): a chat on the
//! daemon runs with its events and its mirror here, a `Resync` brings back
//! the long message's row and the big file's approval, and the project's
//! settings reach the run there.

mod common;

use common::{start_request, until, Daemon, COMPUTER};
use rovik_client::{HostState, Identity, Remote, UnixConnector};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::AgentConfig;
use rovik_core::chat_db::store::{self, MessageRow, NewDialog};
use rovik_core::testing::{model, Reply, TestCore};
use rovik_protocol::{Request, Response, Secret};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const HOST: &str = "server";
const CHAT: &str = "chat-1";

/// The app: a core of the test's own, with `CHAT` on the daemon.
async fn app(daemon: &Daemon) -> (TestCore, Arc<Remote>) {
    let app = TestCore::new();
    let dialog = NewDialog {
        id: CHAT.into(),
        title: "Chat".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
    };
    app.core
        .db
        .write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    let remote = Remote::new(app.core.clone());
    let identity = Identity {
        host: HOST.into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "remote tests".into(),
    };
    remote.connect(HOST, UnixConnector(daemon.socket.clone()), identity);
    remote.place(CHAT, HOST);
    for _ in 0..400 {
        if remote.states() == [(HOST.to_string(), HostState::Ready)] {
            return (app, remote);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never connected\n{}", daemon.output());
}

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

async fn rows(app: &TestCore) -> Vec<MessageRow> {
    app.core
        .db
        .read(|c| store::get_messages(c, CHAT, None, 100))
        .await
        .unwrap()
        .messages
}

/// The first event the app sent the frontend that `matches`, once it has.
async fn sent(app: &TestCore, matches: impl Fn(&Value) -> bool) -> Value {
    until("an event for the frontend", || {
        app.events
            .lock()
            .unwrap()
            .iter()
            .find(|e| matches(e))
            .cloned()
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_chat_runs_with_its_events_and_mirror_here() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (app, remote) = app(&daemon).await;
    let (url, _) = model(vec![Reply::Text("Hello there.")]).await;
    let start = start_request(&url, &project, "Hi".into());
    let started = remote.start(start).await;
    assert!(
        matches!(started, Ok(Response::RunId(Some(_)))),
        "{started:?}"
    );
    sent(&app, |e| e["type"] == "runEnded").await;

    let rows = rows(&app).await;
    let texts: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.role.as_str(), r.content.as_str()))
        .collect();
    assert_eq!(texts, [("user", "Hi"), ("assistant", "Hello there.")]);
    assert!(rows[1].finalized, "the mirror followed the reply's patches");
    let status = remote.status().await.unwrap();
    assert!(status.runs.is_empty());
}

/// 4.1: the message and the approval are each too big for a frame, so the
/// daemon sends `Resync` for them, and the app gets them another way.
#[tokio::test(flavor = "multi_thread")]
async fn a_resync_brings_the_long_message_and_the_big_approval() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    std::fs::write(project.join("big.txt"), "old\n").unwrap();
    let big_file: String = (0..)
        .map(|n| format!("line {n}\n"))
        .scan(0, |size, line| {
            *size += line.len();
            (*size <= 2 << 20).then_some(line)
        })
        .collect();
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "big.txt", "content": big_file }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let (app, remote) = app(&daemon).await;
    let long = "y".repeat(2 << 20);
    let start = start_request(&url, &project, long.clone());
    remote.start(start).await.unwrap();

    let added = sent(&app, |e| {
        e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u1"
    })
    .await;
    assert_eq!(added["messages"][0]["content"], long.as_str());
    let approval = sent(&app, |e| e["type"] == "approvalRequested").await;
    let request = &approval["request"];
    assert_eq!(request["dialogId"], CHAT);
    assert_eq!(request["remote"], true);
    assert_eq!(request["edit"]["proposed"], big_file.as_str());
    let rows = rows(&app).await;
    assert_eq!(rows[0].id, "u1");
    assert_eq!(rows[0].content, long, "in the mirror");

    let stopped = remote
        .route(
            CHAT,
            Request::AgentStop {
                dialog_id: CHAT.into(),
            },
        )
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

/// K03: the checks the user set for the project are the ones the run on
/// the daemon runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_projects_settings_reach_the_run_there() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (app, remote) = app(&daemon).await;
    let root = project.to_string_lossy().into_owned();
    let settings = json!({ "checks": [{ "kind": "test", "command": "touch checked" }] });
    rovik_core::project_profile::save_project_settings(&app.core.paths.config_dir, root, settings)
        .unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "changed\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut start = start_request(&url, &project, "Change it".into());
    start.config = Some(AgentConfig {
        permissions: Permissions {
            allow_any_command: true,
            verify_changes: true,
            ..Permissions::default()
        },
        ..AgentConfig::default()
    });
    remote.start(start).await.unwrap();
    sent(&app, |e| e["type"] == "runEnded").await;
    assert!(project.join("checked").exists(), "{}", daemon.output());
}

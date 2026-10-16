//! The app's remote chats against the real daemon (P08): a chat on the
//! daemon runs with its events and its mirror here, a `Resync` brings back
//! the long message's row and the big file's approval, the project's
//! settings reach the run there, and a long run there is sent the request
//! as the app sent it, with its map, every step ending with the latest.

mod common;

use common::{start_request, until, write, Daemon, COMPUTER};
use rovik_client::{HostState, Identity, Remote, UnixConnector};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind};
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

/// The chat's rows here once the mirror has `n` of them, all final: it
/// pulls what the events didn't bring (6.3).
async fn settled(app: &TestCore, n: usize) -> Vec<MessageRow> {
    for _ in 0..400 {
        let rows = rows(app).await;
        if rows.len() >= n && rows.iter().all(|r| r.finalized) {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the mirror never caught up: {:?}", rows(app).await);
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

    let rows = settled(&app, 2).await;
    let texts: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.role.as_str(), r.content.as_str()))
        .collect();
    assert_eq!(texts, [("user", "Hi"), ("assistant", "Hello there.")]);
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

/// The text of a message the model was sent.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A long run there, of a request typed with a file open and lines
/// referenced with "@": what the app sends (the editor context and the
/// reference's instructions before what was typed, `meta.sentContent`) goes
/// out from the project's root (K08), with the repository map before it,
/// the same way on every step. Every later request ends with its latest
/// result, and the IDE's reminder quotes the request as typed.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_run_there_ends_each_request_with_its_latest_step() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let files = [
        ("src/lib.rs", "mod parse;\npub use parse::parse_line;\n"),
        (
            "src/parse.rs",
            "pub fn parse_line(s: &str) -> Vec<String> {\n    s.split(',').map(|x| x.to_string()).collect()\n}\n",
        ),
    ];
    for (path, text) in files {
        write(&project.join(path), text);
    }
    let reads = ["r0", "r1", "r2", "r3", "r4", "r5", "r6"];
    let mut replies: Vec<Reply> = reads
        .into_iter()
        .zip(files.iter().cycle())
        .map(|(id, (path, _))| Reply::Call {
            id,
            name: "read",
            args: json!({ "path": path }),
        })
        .collect();
    replies.push(Reply::Text("It's the split."));
    let (url, seen) = model(replies).await;
    let (app, remote) = app(&daemon).await;
    let typed = "parse.rs:1-3 keeps the spaces around each field; trim them.";
    let sent_from = |root: &str| {
        format!(
            "[Active editor context — file {root}src/parse.rs (rust); cursor at line 2, col 5. \
             Call read if you need the actual code.]\n\n\
             The user explicitly referenced these project files:\n\
             - src/parse.rs (lines 1-3, shown in the message as parse.rs:1-3)\n\n\
             Treat these as explicit file references from the user. Use the available file \
             tools to inspect them when relevant.\n\n{typed}"
        )
    };
    let here = sent_from(&format!("{}/", project.display()));
    let mut start = start_request(&url, &project, typed.into());
    if let StartKind::Send { message } = &mut start.start {
        message.meta = Some(json!({ "mode": "agent", "sentContent": here }));
    }
    start.current_user_content = Some(here);
    remote.start(start).await.unwrap();
    sent(&app, |e| e["type"] == "runEnded").await;

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 8, "seven reads, the reply");
    let there = sent_from("");
    for (i, r) in requests.iter().enumerate() {
        let messages = r["messages"].as_array().unwrap();
        let request = text_of(&messages[1]);
        assert!(
            request.starts_with("## Repository map\n"),
            "request {i}: {request}"
        );
        assert!(
            request.ends_with(&format!("\n\n{there}")),
            "request {i}: {request}"
        );
        assert_eq!(messages[1], requests[0]["messages"][1], "request {i}");
        assert!(
            messages[2..]
                .iter()
                .all(|m| !text_of(m).contains("## Repository map")),
            "request {i}: a map away from the request"
        );
        if i > 0 {
            assert_eq!(messages.last().unwrap()["role"], "tool", "request {i}");
        }
    }
    // Not the editor context or the reference's instructions, which read
    // as being told to go and read those lines again.
    let reminders: Vec<String> = requests
        .iter()
        .filter_map(|r| {
            let last = text_of(r["messages"].as_array()?.last()?);
            Some(last[last.find("[Reminder from the IDE")?..].to_string())
        })
        .collect();
    assert_eq!(reminders.len(), 2, "after six results");
    for note in &reminders {
        assert!(note.contains(&format!("> {typed}")), "{note}");
        assert!(!note.contains("Active editor context"), "{note}");
        assert!(!note.contains("explicitly referenced"), "{note}");
    }
}

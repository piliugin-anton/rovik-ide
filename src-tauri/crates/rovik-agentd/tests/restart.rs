//! Runs a `kill -9` cut, picked up when the daemon starts again (P06, 5.4):
//! a reply cut mid-stream is continued, or taken again when nothing of it
//! came, a tool call cut mid-way asks the user first, a run cut in its
//! end-of-run checks ends `interrupted`, a run an upgrade drained
//! continues, and a chat cut twice within the hour waits for the user.

mod common;

use common::{files_under, start_request, until, Daemon, Row};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartRequest};
use rovik_core::testing::{model, Reply};
use rovik_protocol::events::ApprovalRequest;
use rovik_protocol::{Frame, Request, Response};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const CHAT: &str = "chat-1";
/// Leaves a line in `runs` each time it runs, then takes a while.
const SLOW_COMMAND: &str = "echo ran >> runs && sleep 5";

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

/// A start of `CHAT` with these permissions.
fn start(url: &str, project: &Path, permissions: Permissions) -> StartRequest {
    let mut start = start_request(url, project, "Do it".into());
    start.config = Some(AgentConfig {
        permissions,
        ..AgentConfig::default()
    });
    start
}

/// A run that asks nobody and runs any command.
fn unattended() -> Permissions {
    Permissions {
        allow_any_command: true,
        verify_changes: false,
        tool_call_checkpoint: None,
        ..Permissions::default()
    }
}

fn slow_command() -> Reply {
    Reply::Call {
        id: "c1",
        name: "bash",
        args: json!({ "command": SLOW_COMMAND }),
    }
}

async fn started(daemon: &Daemon, start: StartRequest) {
    let reply = daemon.post_call(1, Request::AgentStart(start)).await;
    assert!(
        matches!(
            reply,
            Frame::Reply {
                res: Ok(Response::RunId(Some(_))),
                ..
            }
        ),
        "{reply:?}"
    );
}

/// The events this boot published, up to the first `runEnded`: the
/// daemon started the runs before any client could connect.
async fn events_of_this_boot(daemon: &Daemon) -> Vec<Value> {
    let mut session = daemon.session().await;
    let boot = session.boot();
    let (_, complete) = session.subscribe(Some((boot, 0))).await;
    assert!(complete, "the whole boot is in the ring");
    let events = session.events_until(|e| e["type"] == "runEnded").await;
    events.into_iter().map(|(_, ev)| ev).collect()
}

/// The approval the chat's run waits for.
async fn waiting_approval(daemon: &Daemon) -> ApprovalRequest {
    for _ in 0..1200 {
        if let Frame::Reply {
            res: Ok(Response::AgentStatus(status)),
            ..
        } = daemon.post_call(2, Request::AgentStatus).await
        {
            if let Some(approval) = status.approvals.into_iter().find(|a| a.dialog_id == CHAT) {
                return approval;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no approval waited\n{}", daemon.output());
}

fn chat_state(daemon: &Daemon) -> Value {
    daemon.state()["chats"][CHAT].clone()
}

fn restarts(daemon: &Daemon) -> usize {
    chat_state(daemon)["restarts"]
        .as_array()
        .map_or(0, |r| r.len())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_cut_mid_stream_is_continued() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (url, seen) = model(vec![
        Reply::Hang(Some("The first half")),
        Reply::Text(" and the second."),
    ])
    .await;
    let key = "sk-restarted-4c2e91";
    let mut start = start_request(&url, &project, "Explain it".into());
    start.profile.api_key = key.into();
    started(&daemon, start).await;
    // Killed once the reply's checkpoint has stored what came.
    until("the reply's checkpoint", || {
        let rows = daemon.rows(CHAT);
        let cut = |r: &Row| r.role == "assistant" && !r.finalized && r.content == "The first half";
        rows.iter().any(cut).then_some(())
    })
    .await;
    daemon.kill().await;
    daemon.restart().await;

    let events = events_of_this_boot(&daemon).await;
    assert_eq!(events.last().unwrap()["outcome"], "done", "{events:?}");
    let rows = daemon.rows(CHAT);
    let replies: Vec<&Row> = rows.iter().filter(|r| r.role == "assistant").collect();
    assert_eq!(replies.len(), 2, "{rows:?}");
    // What came is kept, marked cut by the restart; the rest follows it.
    assert!(replies[0].finalized);
    assert_eq!(replies[0].content, "The first half");
    assert_eq!(replies[0].meta["interrupted"]["kind"], "restarted");
    assert_eq!(replies[1].content, " and the second.");
    // The model was asked to go on from what it had written.
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let history = requests[1]["messages"].as_array().unwrap();
    assert!(
        history
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"] == "The first half"),
        "{history:?}"
    );
    let chat = chat_state(&daemon);
    assert_eq!(chat["running"], false);
    assert_eq!(chat["outcome"], "done");
    assert_eq!(restarts(&daemon), 1);
    // Plan rule 9: the restart took the key from keys.json, and left it
    // nowhere else.
    for file in files_under(&daemon.data) {
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).into_owned();
        assert_eq!(text.contains(key), file.ends_with("keys.json"), "{file:?}");
    }
}

/// Cut before anything came: the empty reply is marked failed, and the
/// turn is taken again.
#[tokio::test(flavor = "multi_thread")]
async fn a_reply_cut_before_it_began_is_retried() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (url, seen) = model(vec![Reply::Hang(None), Reply::Text("Hello.")]).await;
    started(&daemon, start_request(&url, &project, "Hi".into())).await;
    until("the request", || {
        (!seen.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    daemon.kill().await;
    daemon.restart().await;

    let events = events_of_this_boot(&daemon).await;
    assert_eq!(events.last().unwrap()["outcome"], "done", "{events:?}");
    let rows = daemon.rows(CHAT);
    let replies: Vec<&Row> = rows.iter().filter(|r| r.role == "assistant").collect();
    assert_eq!(replies.len(), 2, "{rows:?}");
    assert_eq!(replies[0].meta["failure"]["kind"], "restarted");
    assert_eq!(
        replies[0].error.as_deref(),
        Some("The server restarted while the reply was being written.")
    );
    assert_eq!(replies[1].content, "Hello.");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_cut_mid_way_asks_before_it_runs_again() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (url, _) = model(vec![slow_command(), Reply::Text("Done.")]).await;
    started(&daemon, start(&url, &project, unattended())).await;
    until("the command ran", || {
        project.join("runs").exists().then_some(())
    })
    .await;
    daemon.kill().await;
    daemon.restart().await;

    // Resume asks the user, whatever the settings say.
    let approval = waiting_approval(&daemon).await;
    assert_eq!(approval.tool_call_id, "c1");
    assert_eq!(approval.command.as_deref(), Some(SLOW_COMMAND));
    let note = approval.note.unwrap_or_default();
    assert!(note.starts_with("Interrupted"), "{note}");
    let runs = std::fs::read_to_string(project.join("runs")).unwrap();
    assert_eq!(
        runs.lines().count(),
        1,
        "not run again before the user says so"
    );
    assert_eq!(chat_state(&daemon)["running"], true);
    assert_eq!(restarts(&daemon), 1);
    let mut session = daemon.session().await;
    let stopped = session
        .call(Request::AgentStop {
            dialog_id: CHAT.into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_cut_in_its_end_of_run_checks_ends_interrupted() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    // The project's check, as its settings name it: slow enough to cut.
    let config = daemon.data.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let mut settings = serde_json::Map::new();
    settings.insert(
        project.to_string_lossy().into_owned(),
        json!({ "checks": [{ "kind": "test", "command": "touch checking && sleep 5" }] }),
    );
    std::fs::write(
        config.join("project-settings.json"),
        Value::Object(settings).to_string(),
    )
    .unwrap();
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "changed\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        verify_changes: true,
        ..unattended()
    };
    started(&daemon, start(&url, &project, permissions)).await;
    until("the checks ran", || {
        project.join("checking").exists().then_some(())
    })
    .await;
    let run_id = chat_state(&daemon)["runId"].clone();
    daemon.kill().await;
    daemon.restart().await;

    let events = events_of_this_boot(&daemon).await;
    let ended = events.last().unwrap();
    assert_eq!(ended["outcome"], "interrupted");
    assert_eq!(ended["runId"], run_id);
    assert!(
        !events.iter().any(|e| e["type"] == "runStarted"),
        "{events:?}"
    );
    let chat = chat_state(&daemon);
    assert_eq!(chat["running"], false);
    assert_eq!(chat["outcome"], "interrupted");
    assert_eq!(restarts(&daemon), 0);
    assert_eq!(seen.lock().unwrap().len(), 2, "no request after the cut");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_an_upgrade_drained_continues() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (url, seen) = model(vec![
        Reply::Call {
            id: "l1",
            name: "list_dir",
            args: json!({ "path": "." }),
        },
        Reply::Text("All done."),
    ])
    .await;
    let permissions = Permissions {
        tool_call_checkpoint: Some(1),
        ..unattended()
    };
    started(&daemon, start(&url, &project, permissions)).await;
    // It pauses after its call, as an upgrade's drain pauses it (R02).
    until("the pause", || {
        (chat_state(&daemon)["outcome"] == "paused-checkpoint").then_some(())
    })
    .await;
    daemon.kill().await;
    drained(&daemon);
    daemon.restart().await;

    let events = events_of_this_boot(&daemon).await;
    assert_eq!(events.last().unwrap()["outcome"], "done", "{events:?}");
    let rows = daemon.rows(CHAT);
    assert_eq!(rows.last().unwrap().content, "All done.", "{rows:?}");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(restarts(&daemon), 1);
}

/// What a drain leaves, until R02 makes one: the run's pause of kind
/// `drain`, and the chat still running.
fn drained(daemon: &Daemon) {
    let db = rusqlite::Connection::open(daemon.data.join("data/chats.db")).unwrap();
    let changed = db
        .execute(
            "UPDATE messages SET meta = json_set(meta, '$.paused.kind', 'drain') \
             WHERE dialog_id = ?1 AND json_extract(meta, '$.paused') IS NOT NULL",
            [CHAT],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let mut state = daemon.state();
    state["chats"][CHAT]["running"] = json!(true);
    std::fs::write(daemon.data.join("state.json"), state.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_cut_twice_within_the_hour_isnt_restarted_again() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (url, _) = model(vec![slow_command(), Reply::Text("Done.")]).await;
    started(&daemon, start(&url, &project, unattended())).await;
    until("the command ran", || {
        project.join("runs").exists().then_some(())
    })
    .await;
    for restarted in 1..=2 {
        daemon.kill().await;
        daemon.restart().await;
        waiting_approval(&daemon).await;
        assert_eq!(restarts(&daemon), restarted);
    }
    daemon.kill().await;
    daemon.restart().await;

    let events = events_of_this_boot(&daemon).await;
    let notice = events
        .iter()
        .find(|e| e["type"] == "notice")
        .unwrap_or_else(|| panic!("no notice: {events:?}"));
    assert_eq!(notice["dialogId"], CHAT);
    assert_eq!(notice["title"], "Stopped after the server restarted twice");
    assert_eq!(events.last().unwrap()["outcome"], "interrupted");
    let chat = chat_state(&daemon);
    assert_eq!(chat["running"], false);
    assert_eq!(chat["outcome"], "interrupted");
    assert_eq!(restarts(&daemon), 2);
    match daemon.post_call(3, Request::AgentStatus).await {
        Frame::Reply {
            res: Ok(Response::AgentStatus(status)),
            ..
        } => assert!(status.runs.is_empty() && status.approvals.is_empty()),
        other => panic!("{other:?}"),
    }
}

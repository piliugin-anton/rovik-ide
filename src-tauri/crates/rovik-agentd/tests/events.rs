//! The daemon's events, end to end (P04): who gets them, what goes in place
//! of one too big for a frame, and a client coming back.

mod common;

use common::{start_request, Daemon, OTHER};
use rovik_core::testing::{model, Reply};
use rovik_protocol::{Frame, Request, Response, MAX_FRAME};
use serde_json::{json, Value};
use std::time::Duration;

fn ended(ev: &Value) -> bool {
    ev["type"] == "runEnded"
}

#[tokio::test(flavor = "multi_thread")]
async fn a_computers_clients_get_its_events_in_order_and_another_none() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (url, _) = model(vec![Reply::Text("Hello there.")]).await;
    let mut clients = Vec::new();
    for _ in 0..3 {
        let mut client = daemon.session().await;
        assert_eq!(client.subscribe(None).await, (0, false));
        clients.push(client);
    }
    let mut desk = daemon.session_as(OTHER, &daemon.other_token).await;
    desk.subscribe(None).await;

    // Started from elsewhere: a session waiting for a reply drops the rest.
    let start = start_request(&url, &project, "Hi".into());
    let started = daemon
        .post_call(1, Request::AgentStart(Box::new(start)))
        .await;
    assert!(
        matches!(
            started,
            Frame::Reply {
                res: Ok(Response::RunId(Some(_))),
                ..
            }
        ),
        "{started:?}"
    );
    let mut seen = Vec::new();
    for client in &mut clients {
        seen.push(client.events_until(ended).await);
    }
    assert!(
        seen.iter().all(|events| events == &seen[0]),
        "the same events"
    );
    let kinds: Vec<&str> = seen[0]
        .iter()
        .map(|(_, ev)| ev["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(kinds.first(), Some(&"messagesAdded"), "{kinds:?}");
    assert!(kinds.contains(&"runStarted"), "{kinds:?}");
    let seqs: Vec<u64> = seen[0].iter().map(|(seq, _)| *seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order: {seqs:?}");
    // The other computer got nothing of this chat.
    let frame = desk.recv_within(Duration::from_millis(500)).await;
    assert!(frame.is_none(), "{frame:?}");
}

/// A row or an approval too big for a frame comes as `Resync` (4.1).
#[tokio::test(flavor = "multi_thread")]
async fn a_2_mb_row_and_a_2_mb_approval_each_send_resync() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
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
    let mut client = daemon.session().await;
    client.subscribe(None).await;
    let start = start_request(&url, &project, "y".repeat(2 << 20));
    daemon
        .post_call(1, Request::AgentStart(Box::new(start)))
        .await;

    // Up to the approval's place: a run that waits sends nothing after it.
    let mut frames = Vec::new();
    while let Some(frame) = client.recv_within(Duration::from_secs(20)).await {
        let resyncs = frames
            .iter()
            .filter(|f: &&Frame| {
                matches!(
                    f,
                    Frame::Event {
                        ev: rovik_protocol::Event::Resync { .. },
                        ..
                    }
                )
            })
            .count();
        frames.push(frame);
        if resyncs >= 2 && status_waits(&daemon).await {
            break;
        }
    }
    for frame in &frames {
        let text = serde_json::to_string(frame).unwrap();
        assert!(text.len() <= MAX_FRAME, "a frame of {} bytes", text.len());
    }
    let events: Vec<Value> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Event { ev, .. } => Some(serde_json::to_value(ev).unwrap()),
            _ => None,
        })
        .collect();
    let resyncs: Vec<&Value> = events.iter().filter(|e| e["t"] == "resync").collect();
    assert!(resyncs.len() >= 2, "{events:?}");
    assert!(resyncs.iter().all(|r| r["dialogId"] == "chat-1"));
    let row = events
        .iter()
        .any(|e| e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u1");
    assert!(!row, "the 2 MB row went as Resync");
    assert!(!events.iter().any(|e| e["type"] == "approvalRequested"));

    let mut session = daemon.session().await;
    let stopped = session
        .call(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

/// Whether the run waits for an approval, by the whole status (`/call`).
async fn status_waits(daemon: &Daemon) -> bool {
    match daemon.post_call(9, Request::AgentStatus).await {
        Frame::Reply {
            res: Ok(Response::AgentStatus(status)),
            ..
        } => !status.approvals.is_empty(),
        _ => false,
    }
}

/// A client that comes back with its `since` gets what it missed; one
/// from another boot, or with none, catches up from the database.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_back_with_its_since_gets_the_gap() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (url, _) = model(vec![Reply::Text("One."), Reply::Text("Two.")]).await;
    let mut client = daemon.session().await;
    client.subscribe(None).await;
    let start = start_request(&url, &project, "First".into());
    daemon
        .post_call(1, Request::AgentStart(Box::new(start.clone())))
        .await;
    let first = client.events_until(ended).await;
    let since = (client.boot(), first.last().unwrap().0);
    drop(client);

    // A second run while it's away.
    let mut away = daemon.session().await;
    let mut second = start;
    second.start = rovik_core::agent_run::StartKind::Send {
        message: rovik_core::agent_run::UserMessage {
            id: "u2".into(),
            content: "Second".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    away.call(Request::AgentStart(Box::new(second)))
        .await
        .unwrap();
    for _ in 0..400 {
        match away.call(Request::AgentStatus).await {
            Ok(Response::AgentStatus(s)) if s.runs.is_empty() => break,
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }

    let mut back = daemon.session().await;
    let (replayed, complete) = back.subscribe(Some(since.clone())).await;
    assert!(complete);
    let missed = back.events_until(ended).await;
    assert_eq!(missed.len() as u64, replayed);
    assert!(missed.iter().all(|(seq, _)| *seq > since.1));
    assert!(missed
        .iter()
        .any(|(_, e)| e["type"] == "messagesAdded" && e["messages"][0]["id"] == "u2"));

    let mut stale = daemon.session().await;
    assert_eq!(
        stale
            .subscribe(Some(("another-boot".into(), since.1)))
            .await,
        (0, false)
    );
}

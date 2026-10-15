//! The app's client against the real daemon (P07): its calls and events, a
//! daemon started again under it, and a reply too big for a frame while
//! events keep coming.

mod common;

use common::{start_request, until, Daemon, COMPUTER};
use rovik_client::{Client, HostState, Identity, Incoming, UnixConnector};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind, UserMessage};
use rovik_core::testing::{model, Reply};
use rovik_protocol::agent::AgentStatus;
use rovik_protocol::events::AgentEvent;
use rovik_protocol::host::HostInfo;
use rovik_protocol::{Event, FailCode, Request, Response, Secret, MAX_FRAME, PROTOCOL};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// What a client hears.
struct Heard(mpsc::UnboundedReceiver<Incoming>);

impl Heard {
    /// What comes, up to the first that `last` matches.
    async fn until(&mut self, last: impl Fn(&Incoming) -> bool) -> Vec<Incoming> {
        let mut heard = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(30), self.0.recv()).await;
            let incoming = next
                .ok()
                .flatten()
                .unwrap_or_else(|| panic!("nothing more came after {heard:?}"));
            let done = last(&incoming);
            heard.push(incoming);
            if done {
                return heard;
            }
        }
    }
}

/// `COMPUTER`'s client of the daemon, and what it hears.
fn client(daemon: &Daemon) -> (Client, Heard) {
    let (tx, rx) = mpsc::unbounded_channel();
    let identity = Identity {
        host: "test-server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "client tests".into(),
    };
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    let client = Client::connect(UnixConnector(daemon.socket.clone()), identity, sink);
    (client, Heard(rx))
}

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

fn subscribed(incoming: &Incoming) -> bool {
    matches!(incoming, Incoming::Subscribed { .. })
}

fn agent_event(incoming: &Incoming) -> Option<&AgentEvent> {
    match incoming {
        Incoming::Event {
            event: Event::Agent(event),
            ..
        } => Some(event),
        _ => None,
    }
}

fn run_ended(dialog: &'static str) -> impl Fn(&Incoming) -> bool {
    move |incoming| matches!(agent_event(incoming), Some(AgentEvent::RunEnded { dialog_id, .. }) if dialog_id == dialog)
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_and_events_go_through_the_real_daemon() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (client, mut heard) = client(&daemon);
    let first = heard.until(subscribed).await;
    assert_eq!(
        first,
        [
            Incoming::State(HostState::Ready),
            Incoming::Subscribed { complete: false }
        ],
        "a first connection catches up"
    );
    let info: HostInfo = client.request(Request::HostInfo).await.unwrap();
    assert_eq!(info.protocol, PROTOCOL);
    assert_eq!(client.daemon().unwrap().protocol, PROTOCOL);

    let (url, _) = model(vec![Reply::Text("Hello there.")]).await;
    let start = start_request(&url, &project, "Hi".into());
    let run_id: Option<String> = client.request(Request::AgentStart(start)).await.unwrap();
    let events = heard.until(run_ended("chat-1")).await;
    let started = events.iter().filter_map(agent_event).any(
        |e| matches!(e, AgentEvent::RunStarted { run_id: id, .. } if Some(id) == run_id.as_ref()),
    );
    assert!(started, "{events:?}");
    let seqs: Vec<u64> = events
        .iter()
        .filter_map(|i| match i {
            Incoming::Event { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order: {seqs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_started_again_under_the_client_is_reconnected_and_caught_up() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (client, mut heard) = client(&daemon);
    heard.until(subscribed).await;
    // A run whose command is going when the daemon goes.
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": "echo ran >> runs && sleep 5" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut start = start_request(&url, &project, "Do it".into());
    start.config = Some(AgentConfig {
        permissions: Permissions {
            allow_any_command: true,
            verify_changes: false,
            ..Permissions::default()
        },
        ..AgentConfig::default()
    });
    client
        .request::<Option<String>>(Request::AgentStart(start))
        .await
        .unwrap();
    until("the command ran", || {
        project.join("runs").exists().then_some(())
    })
    .await;

    daemon.kill().await;
    heard
        .until(|i| *i == Incoming::State(HostState::Offline))
        .await;
    let offline = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(offline.code, FailCode::Unreachable);
    assert!(offline.message.starts_with("test-server isn't reachable"));

    daemon.restart().await;
    // Back by itself, it catches up: what it had heard is another boot's.
    let back = heard.until(subscribed).await;
    assert!(
        back.contains(&Incoming::State(HostState::Ready)),
        "{back:?}"
    );
    assert_eq!(back.last(), Some(&Incoming::Subscribed { complete: false }));
    // Then what the daemon did before anyone could connect: the run it
    // restarted asks about the call it cut (5.4).
    heard
        .until(|i| matches!(agent_event(i), Some(AgentEvent::ApprovalRequested { request }) if request.tool_call_id == "c1"))
        .await;
    let status: AgentStatus = client.request(Request::AgentStatus).await.unwrap();
    assert_eq!(status.approvals.len(), 1);
    let stopped: bool = client
        .request(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await
        .unwrap();
    assert!(stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_too_big_for_a_frame_comes_through_post_call_while_events_keep_coming() {
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
    let (client, mut heard) = client(&daemon);
    heard.until(subscribed).await;
    let start = start_request(&url, &project, "Rewrite it".into());
    client
        .request::<Option<String>>(Request::AgentStart(start))
        .await
        .unwrap();
    // The edit waits for the user, with an approval too big for a frame.
    heard
        .until(|i| {
            matches!(
                i,
                Incoming::Event {
                    event: Event::Resync { .. },
                    ..
                }
            )
        })
        .await;

    // Another chat, whose events come while the big read is answered.
    let (slow, _) = model(vec![Reply::Slow(300, Box::new(Reply::Text("Slowly.")))]).await;
    let mut other = start_request(&slow, &project, "And this".into());
    other.dialog_id = "chat-2".into();
    other.start = StartKind::Send {
        message: UserMessage {
            id: "u2".into(),
            content: "And this".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    client
        .request::<Option<String>>(Request::AgentStart(other))
        .await
        .unwrap();
    let status: AgentStatus = client.request(Request::AgentStatus).await.unwrap();
    let approval = status.approvals.iter().find(|a| a.dialog_id == "chat-1");
    let edit = approval.and_then(|a| a.edit.as_ref()).expect("the edit");
    assert!(edit.proposed.len() > MAX_FRAME);
    // The WebSocket carried on meanwhile, without a new connection.
    let after = heard.until(run_ended("chat-2")).await;
    assert!(
        !after.iter().any(|i| matches!(i, Incoming::State(_))),
        "{after:?}"
    );
    let stopped = client
        .call(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

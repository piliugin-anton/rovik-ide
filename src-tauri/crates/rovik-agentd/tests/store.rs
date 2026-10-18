//! What the daemon keeps (P05), against the real binary: each computer's
//! configuration for its own chats, which no other computer reaches or sees
//! (M01), and keys in keys.json alone.

mod common;

use common::{files_under, start_request, until, Daemon, Session, OTHER};
use rovik_core::agent_run::permissions::{ApprovalRule, Approver, Permissions};
use rovik_core::agent_run::{AgentConfig, AgentStatus, ApprovalDecision};
use rovik_core::chat_db::store::{ChatModel, MessagePatch};
use rovik_core::testing::{model, Reply};
use rovik_protocol::{FailCode, Request, Response};
use serde_json::json;
use std::time::Duration;
use tokio::net::UnixStream;

fn writes(path: &'static str) -> Vec<Reply> {
    vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": path, "content": "written\n" }),
        },
        Reply::Text("Done."),
    ]
}

fn approvals(required: bool) -> AgentConfig {
    AgentConfig {
        permissions: Permissions {
            edit_approval: ApprovalRule {
                required,
                by: Approver::User,
            },
            verify_changes: false,
            ..Permissions::default()
        },
        ..AgentConfig::default()
    }
}

/// D16: two computers, two approval rules, each for its own chats' runs.
#[tokio::test(flavor = "multi_thread")]
async fn each_computers_configuration_applies_to_its_own_chats() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut laptop = daemon.session().await;
    let mut desk = daemon.session_as(OTHER, &daemon.other_token).await;
    let configured = laptop.call(Request::AgentConfigure(approvals(true))).await;
    assert_eq!(configured, Ok(Response::Done));
    let configured = desk.call(Request::AgentConfigure(approvals(false))).await;
    assert_eq!(configured, Ok(Response::Done));

    // Starts that carry no configuration of their own.
    let (laptop_model, _) = model(writes("laptop.txt")).await;
    let (desk_model, _) = model(writes("desk.txt")).await;
    let mut start = start_request(&laptop_model, &project, "Write it".into());
    start.config = None;
    start.dialog_id = "laptop-chat".into();
    laptop
        .call(Request::AgentStart(Box::new(start.clone())))
        .await
        .unwrap();
    start.profile.base_url = desk_model;
    start.dialog_id = "desk-chat".into();
    start.start = rovik_core::agent_run::StartKind::Send {
        message: rovik_core::agent_run::UserMessage {
            id: "u2".into(),
            content: "Write it".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    desk.call(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();

    // The desk's write needs no approval: its run ends.
    let mut desk_status = None;
    for _ in 0..400 {
        let s = status_of(&mut desk).await;
        if s.runs.is_empty() {
            desk_status = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let desk_status =
        desk_status.unwrap_or_else(|| panic!("the desk's run went on\n{}", daemon.output()));
    assert_eq!(
        std::fs::read_to_string(project.join("desk.txt"))
            .ok()
            .as_deref(),
        Some("written\n")
    );
    // The laptop's waits for the user's approval, which only the laptop
    // sees (M01).
    let mut laptop_status = AgentStatus::default();
    for _ in 0..400 {
        laptop_status = status_of(&mut laptop).await;
        if !laptop_status.approvals.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let waiting: Vec<&str> = laptop_status
        .approvals
        .iter()
        .map(|a| a.dialog_id.as_str())
        .collect();
    assert_eq!(waiting, ["laptop-chat"]);
    let runs: Vec<&str> = laptop_status
        .runs
        .iter()
        .map(|r| r.dialog_id.as_str())
        .collect();
    assert_eq!(runs, ["laptop-chat"]);
    assert!(desk_status.approvals.is_empty(), "{desk_status:?}");
    assert!(status_of(&mut desk).await.approvals.is_empty());
    assert!(!project.join("laptop.txt").exists());

    // Nor can the desk reach the laptop's chat, to change or to read it.
    let laptop_chat = || "laptop-chat".to_string();
    let theirs = [
        Request::AgentStop {
            dialog_id: laptop_chat(),
        },
        Request::AgentResolveApproval {
            dialog_id: laptop_chat(),
            tool_call_id: "w1".into(),
            decision: ApprovalDecision::Allow,
        },
        Request::ChatRename {
            dialog_id: laptop_chat(),
            title: "Mine now".into(),
        },
        Request::ChatSetModel {
            dialog_id: laptop_chat(),
            model: ChatModel {
                provider_id: "p".into(),
                model_id: "m".into(),
                effort: None,
            },
        },
        Request::ChatUpdateMessage {
            message_id: "u1".into(),
            patch: MessagePatch {
                content: Some("Changed".into()),
                ..Default::default()
            },
        },
        Request::ClmListRevisions {
            dialog_id: laptop_chat(),
        },
        Request::ChatChangesSince {
            dialog_id: laptop_chat(),
            rev: 0,
            cursor: None,
        },
        Request::ChatDelete {
            dialog_id: laptop_chat(),
        },
    ];
    for request in theirs {
        let shown = format!("{request:?}");
        match desk.call(request).await {
            Err(fail) => assert_eq!(fail.code, FailCode::NotOwner, "{shown}: {fail}"),
            Ok(reply) => panic!("{shown} wasn't refused: {reply:?}"),
        }
    }
    assert!(!project.join("laptop.txt").exists(), "nothing was approved");
    let renamed = laptop
        .call(Request::ChatRename {
            dialog_id: laptop_chat(),
            title: "Still the laptop's".into(),
        })
        .await;
    assert_eq!(renamed, Ok(Response::Done));
    // The laptop pulls its chat's rows (6.3).
    let pulled = laptop
        .call(Request::ChatChangesSince {
            dialog_id: laptop_chat(),
            rev: 0,
            cursor: None,
        })
        .await;
    let Ok(Response::ChatChanges(page)) = pulled else {
        panic!("no changes: {pulled:?}");
    };
    assert_eq!(page.changes.dialogs[0].title, "Still the laptop's");
    assert!(page.changes.messages.iter().any(|m| m.id == "u1"));
    assert!(page.next.is_none());
    let stopped = laptop
        .call(Request::AgentStop {
            dialog_id: laptop_chat(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

async fn status_of(session: &mut Session<UnixStream>) -> AgentStatus {
    match session.call(Request::AgentStatus).await {
        Ok(Response::AgentStatus(status)) => status,
        other => panic!("no status: {other:?}"),
    }
}

/// Plan rule 9: once a start has brought them, the keys are in keys.json
/// and nowhere else the daemon writes: not state.json, the database, the
/// agent log, the daemon's log or its temporary files.
#[tokio::test(flavor = "multi_thread")]
async fn keys_are_kept_in_keys_json_alone() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (url, _) = model(vec![Reply::Text("Hello.")]).await;
    let (key, reviewer_key) = ("sk-chat-6f1a2b", "sk-reviewer-93cd77");
    let mut start = start_request(&url, &project, "Hi".into());
    start.profile.api_key = key.into();
    let mut reviewer = start.profile.clone();
    reviewer.id = "reviewer::m".into();
    reviewer.api_key = reviewer_key.into();
    start.config.as_mut().unwrap().approval_profiles = vec![reviewer];
    let mut session = daemon.session().await;
    session
        .call(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();
    for _ in 0..400 {
        match session.call(Request::AgentStatus).await {
            Ok(Response::AgentStatus(s)) if s.runs.is_empty() => break,
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    // The run's end reaches state.json just after: no file is still being
    // written while they're read.
    until("the run's end in state.json", || {
        (daemon.state()["chats"]["chat-1"]["running"] == false).then_some(())
    })
    .await;
    let keys = std::fs::read_to_string(daemon.data.join("keys.json")).unwrap();
    assert!(keys.contains(key) && keys.contains(reviewer_key), "{keys}");
    let files = files_under(&daemon.data);
    assert!(files.iter().any(|f| f.ends_with("state.json")));
    assert!(files.iter().any(|f| f.ends_with("agentd.log")));
    for file in files.iter().filter(|f| !f.ends_with("keys.json")) {
        let text = String::from_utf8_lossy(&std::fs::read(file).unwrap()).into_owned();
        for secret in [key, reviewer_key] {
            assert!(!text.contains(secret), "{secret} in {file:?}");
        }
    }
}

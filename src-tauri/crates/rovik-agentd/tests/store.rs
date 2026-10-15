//! What the daemon keeps (P05), against the real binary: each computer's
//! configuration for its own chats, and keys in keys.json alone.

mod common;

use common::{files_under, start_request, until, Daemon, OTHER};
use rovik_core::agent_run::permissions::{ApprovalRule, Approver, Permissions};
use rovik_core::agent_run::AgentConfig;
use rovik_core::testing::{model, Reply};
use rovik_protocol::{Request, Response};
use serde_json::json;
use std::time::Duration;

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
        .call(Request::AgentStart(start.clone()))
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
    desk.call(Request::AgentStart(start)).await.unwrap();

    // The desk's write needs no approval: its run ends.
    let mut status = None;
    for _ in 0..400 {
        if let Ok(Response::AgentStatus(s)) = desk.call(Request::AgentStatus).await {
            let desk_runs = s.runs.iter().any(|r| r.dialog_id == "desk-chat");
            if !desk_runs && !s.approvals.is_empty() {
                status = Some(s);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = status.unwrap_or_else(|| panic!("no approval waited\n{}", daemon.output()));
    assert_eq!(
        std::fs::read_to_string(project.join("desk.txt"))
            .ok()
            .as_deref(),
        Some("written\n")
    );
    // The laptop's waits for the user's approval.
    let waiting: Vec<&str> = status
        .approvals
        .iter()
        .map(|a| a.dialog_id.as_str())
        .collect();
    assert_eq!(waiting, ["laptop-chat"]);
    assert!(!project.join("laptop.txt").exists());
    let stopped = laptop
        .call(Request::AgentStop {
            dialog_id: "laptop-chat".into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
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
    session.call(Request::AgentStart(start)).await.unwrap();
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

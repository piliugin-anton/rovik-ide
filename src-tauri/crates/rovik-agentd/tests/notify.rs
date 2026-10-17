//! The notice of a task's end (docs/design/autopilot-plan.md D18, H06):
//! the daemon POSTs `{ chat, task, stage, outcome, summary, branch, pr }`
//! to the URL the chat's computer named, once per end and per failed
//! stage, tries again after an error, and goes on with the task whatever
//! becomes of the call.

mod common;

use common::project::CHAT;
use common::task::*;
use common::{listening, until, Daemon, Session};
use rovik_core::testing::Reply;
use rovik_protocol::autopilot::{AutonomyConfig, Delivery};
use rovik_protocol::{Request, Response};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::{TcpListener, UnixStream};

/// This computer has the server keep its results, and call `url` when a
/// task ends or is blocked.
async fn notifying(session: &mut Session<UnixStream>, url: &str) {
    let config = AutonomyConfig {
        delivery: Delivery::KeepOnHost,
        notify_url: Some(url.into()),
    };
    let set = session.call(Request::AutonomyConfigure(config)).await;
    assert_eq!(set, Ok(Response::Done));
}

/// The task started, with the model's replies `first` and `review` as
/// `task_model` takes them.
async fn started(daemon: &Daemon, first: Reply) {
    let project = project(daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = task_model(first, review, vec![nothing_to_test()]).await;
    let reply = daemon
        .post_call(1, Request::AgentStart(task_start(&url, &project)))
        .await;
    assert!(format!("{reply:?}").contains("RunId(Some("), "{reply:?}");
}

/// H06: a task that ends tells the URL once, the body as specified: the
/// chat, the task, its last stage and outcome, the delivery's line, and
/// no branch on a server that keeps its results. The URL's token is
/// nowhere in the daemon's log.
#[tokio::test(flavor = "multi_thread")]
async fn a_tasks_end_is_told_once() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start(&["--same-machine"]).await;
    let (url, got) = listening(vec![200]).await;
    let mut session = daemon.session().await;
    notifying(&mut session, &format!("{url}/hook?token=s3cret")).await;
    started(&daemon, look()).await;
    assert_eq!(until_delivered(&daemon).await, delivered_whole());
    until("the notice", || {
        (!got.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let got = got.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "one call per end: {got:?}");
    let (path, body) = &got[0];
    assert_eq!(path, "/hook?token=s3cret");
    assert_eq!(
        body,
        &json!({
            "chat": CHAT,
            "task": "u1",
            "stage": "deliver",
            "outcome": "verified",
            "summary": "Delivered: the result waits on the server for its computer",
            "branch": null,
            "pr": null,
        })
    );
    assert!(!daemon.log().contains("s3cret"));
}

/// H06: a stage that fails is told, then the task's end: a plan's run
/// that saved no plan blocks its task.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_stage_is_told_and_then_the_end() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let (url, got) = listening(vec![200]).await;
    let mut session = daemon.session().await;
    notifying(&mut session, &url).await;
    started(&daemon, Reply::Text("I won't plan this.")).await;
    until("two notices", || {
        (got.lock().unwrap().len() >= 2).then_some(())
    })
    .await;
    let got = got.lock().unwrap().clone();
    let told: Vec<(&str, &str)> = got
        .iter()
        .map(|(_, b)| (b["stage"].as_str().unwrap(), b["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        told,
        [("plan", "plan-failed"), ("deliver", "plan-failed")],
        "{got:?}"
    );
    assert!(!got[0].1["summary"].as_str().unwrap().is_empty());
}

/// H06: a call the URL answers with an error is tried again, a second,
/// then two later; an unreachable URL changes nothing for the task, and
/// the log says, by the URL's host alone, that the notice didn't go.
#[tokio::test(flavor = "multi_thread")]
async fn a_notice_is_tried_again_and_never_stops_the_task() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start(&["--same-machine"]).await;
    let (url, got) = listening(vec![500, 503, 200]).await;
    let mut session = daemon.session().await;
    notifying(&mut session, &url).await;
    started(&daemon, look()).await;
    until_delivered(&daemon).await;
    for _ in 0..200 {
        if got.lock().unwrap().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let calls = got.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "two errors, then it went: {calls:?}");
    assert!(calls.iter().all(|(_, b)| b["stage"] == "deliver"));

    // Nothing listens there: the task goes on all the same.
    let daemon = Daemon::start(&["--same-machine"]).await;
    let mut session = daemon.session().await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone = format!("http://{}/hook?token=s3cret", closed.local_addr().unwrap());
    drop(closed);
    notifying(&mut session, &gone).await;
    started(&daemon, look()).await;
    assert_eq!(until_delivered(&daemon).await, delivered_whole());
    let mut logged = String::new();
    for _ in 0..300 {
        logged = daemon.log();
        if logged.contains("didn't go") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let line = logged
        .lines()
        .find(|l| l.contains("didn't go"))
        .unwrap_or_else(|| panic!("never logged: {logged}"));
    assert!(line.contains("/…"), "the URL by its host: {line}");
    assert!(!logged.contains("s3cret"), "{logged}");
}

//! Autopilot's tasks on a server (docs/design/autopilot-plan.md, A03): a
//! task a `kill -9` cuts in the middle of a stage goes on when the daemon
//! starts again, with nobody connected, to its delivery; its stage records
//! are rows of the chat like any other.

mod common;

use common::project::CHAT;
use common::task::*;
use common::{until, Daemon, Row};
use rovik_core::agent_run::AgentConfig;
use rovik_core::testing::Reply;
use rovik_protocol::{Frame, Request, Response};
use serde_json::Value;
use std::sync::Mutex;

/// A03: the daemon is killed while the task's plan streams; started again,
/// it picks the plan's run up where it was cut, and the task goes on by
/// itself: the plan's review (A07: on for Autopilot, though the computer's
/// subagents are off), the implementation, its checks, the delivery.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_cut_by_a_kill_goes_on_when_the_daemon_starts_again() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = task_model(
        Reply::Hang(Some("Looking at the project")),
        review,
        vec![nothing_to_test()],
    )
    .await;
    let start = task_start(&url, &project);
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
    // Killed once the plan's reply has a checkpoint stored.
    until("the plan's reply's checkpoint", || {
        let rows = daemon.rows(CHAT);
        let cut =
            |r: &Row| r.role == "assistant" && !r.finalized && r.content.starts_with("Looking");
        rows.iter().any(cut).then_some(())
    })
    .await;
    assert_eq!(
        stages(&daemon.rows(CHAT)),
        [("plan".into(), "running".into())]
    );
    daemon.kill().await;
    daemon.restart().await;

    let found = until_delivered(&daemon).await;
    assert_eq!(found, delivered_whole(), "{}", daemon.output());
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
    // The plan's run went on from what came before the kill.
    let rows = daemon.rows(CHAT);
    let plan_reply: Vec<&Value> = rows
        .iter()
        .filter(|r| r.role == "assistant" && r.content.starts_with("Looking"))
        .map(|r| &r.meta)
        .collect();
    assert_eq!(plan_reply.len(), 1, "{rows:?}");
}

/// H03: a task started by a client that then goes away runs to its
/// delivery on the server alone; when the client is back, its records are
/// there for it to pull, as the daemon has them.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_whose_client_goes_away_runs_to_its_delivery() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = task_model(look(), review, vec![nothing_to_test()]).await;
    let mut session = daemon.session().await;
    let started = session
        .call(Request::AgentStart(task_start(&url, &project)))
        .await;
    assert!(
        matches!(started, Ok(Response::RunId(Some(_)))),
        "{started:?}"
    );
    // The client goes: nobody is connected from here on.
    drop(session);

    let found = until_delivered(&daemon).await;
    assert_eq!(found, delivered_whole(), "{}", daemon.output());
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
    // Back: the client pulls the chat, its stage records with it.
    let mut session = daemon.session().await;
    let pulled = session
        .call(Request::ChatChangesSince {
            dialog_id: CHAT.into(),
            rev: 0,
            cursor: None,
        })
        .await;
    let Ok(Response::ChatChanges(page)) = pulled else {
        panic!("no changes: {pulled:?}");
    };
    let mut messages = page.changes.messages;
    messages.sort_by_key(|m| m.seq);
    let pulled: Vec<(String, String)> = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| {
            let meta: Value = serde_json::from_str(m.meta.as_deref()?).ok()?;
            let stage = meta.get("stage")?;
            Some((
                stage["name"].as_str()?.to_string(),
                stage["status"].as_str()?.to_string(),
            ))
        })
        .collect();
    assert_eq!(pulled, delivered_whole());
}

/// H03: `kill -9` once the task's plan is written, while its review is
/// (inside the plan's run, S02): started again, the daemon goes on with
/// the task on its own, the review asked again, the plan not made again.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_killed_while_its_plan_is_reviewed_goes_on() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    // The first review never comes: the kill cuts it.
    let review = vec![Reply::Hang(None), Reply::Text("## Findings\n\nNone.")];
    let (url, seen) = task_model(look(), review, vec![nothing_to_test()]).await;
    let reply = daemon
        .post_call(1, Request::AgentStart(task_start(&url, &project)))
        .await;
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
    let reviews = |seen: &Mutex<Vec<Value>>| asked(seen, PLAN_REVIEW);
    until("the plan's review asked for", || {
        (reviews(&seen) > 0).then_some(())
    })
    .await;
    assert_eq!(
        stages(&daemon.rows(CHAT)),
        [("plan".into(), "running".into())]
    );
    daemon.kill().await;
    daemon.restart().await;

    let found = until_delivered(&daemon).await;
    assert_eq!(found, delivered_whole(), "{}", daemon.output());
    assert_eq!(reviews(&seen), 2, "the review again, once");
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
}

/// H03: `kill -9` between two stages: the plan's stage and its review
/// done, the Tests stage's record written, its run not told yet. A kill
/// lands there only by chance (the driver writes records, then starts
/// the run), so it's made: the daemon is killed as the Tests stage's
/// writer is asked, and its state.json is set to say no run of the chat
/// went, as it would have said a moment before. Started again, the
/// daemon finds the task between two stages, with no run to restart, and
/// goes on with it on its own (`restart_interrupted`'s second pass).
#[tokio::test(flavor = "multi_thread")]
async fn a_task_killed_between_two_stages_goes_on() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let tests = vec![Reply::Hang(None), nothing_to_test()];
    let (url, seen) = task_model(look(), review, tests).await;
    let reply = daemon
        .post_call(1, Request::AgentStart(task_start(&url, &project)))
        .await;
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
    until("the Tests stage's writer asked", || {
        (asked(&seen, TESTS_FIRST) > 0).then_some(())
    })
    .await;
    daemon.kill().await;
    let file = daemon.data.join("state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    state["chats"][CHAT]["running"] = Value::Bool(false);
    std::fs::write(&file, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let before = stages(&daemon.rows(CHAT));
    assert_eq!(
        before.last(),
        Some(&("tests".into(), "running".into())),
        "{before:?}"
    );
    daemon.restart().await;

    let found = until_delivered(&daemon).await;
    assert!(
        daemon.log().contains("its task goes on"),
        "resumed between stages: {}",
        daemon.output()
    );
    // The Tests stage's record goes on where it was, the rest as ever.
    assert_eq!(found, delivered_whole(), "{}", daemon.output());
    assert_eq!(asked(&seen, TESTS_FIRST), 2, "the Tests stage again, once");
    assert_eq!(
        std::fs::read_to_string(project.join("ok.txt")).unwrap(),
        "ok"
    );
}

/// H07: the provider rate limits the task's plan through every attempt of
/// its requests; nobody is there to press Retry, so the daemon runs the
/// stage again once the limit resets, and the task completes, its plan's
/// stage the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_rate_limited_task_goes_on_once_the_limit_resets() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let limited = (0..4).map(|_| Reply::RateLimited(0)).collect();
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, seen) = task_model_after(limited, look(), review, vec![nothing_to_test()]).await;
    let reply = daemon
        .post_call(1, Request::AgentStart(task_start(&url, &project)))
        .await;
    assert!(format!("{reply:?}").contains("RunId(Some("), "{reply:?}");
    let found = until_delivered(&daemon).await;
    assert_eq!(found, delivered_whole(), "{}", daemon.output());
    let plan_requests = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|b| !b.to_string().contains(PLAN_REVIEW) && !b.to_string().contains(TESTS_FIRST))
        .count();
    assert!(plan_requests > 4, "the four limited, then the rest");
}

/// H02: what this computer has the server keep for its runs that go on
/// without it (where results go, whom to tell) is kept per computer and
/// read back after a restart: a later change of the state keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn the_autonomy_frame_is_kept_and_read_back_after_a_restart() {
    use common::COMPUTER;
    use rovik_protocol::autopilot::{AutonomyConfig, Delivery};
    use rovik_protocol::features;
    let mut daemon = Daemon::start(&[]).await;
    let mut session = daemon.session().await;
    let Frame::Welcome {
        features: served, ..
    } = &session.welcome
    else {
        unreachable!()
    };
    assert!(served.iter().any(|f| f == features::AUTONOMY), "{served:?}");
    let config = AutonomyConfig {
        delivery: Delivery::PushBranch {
            deploy_key: "~/.ssh/rovik_deploy".into(),
            open_pr: true,
        },
        notify_url: Some("https://ntfy.sh/rovik-topic".into()),
    };
    let sent = session
        .call(Request::AutonomyConfigure(config.clone()))
        .await;
    assert_eq!(sent, Ok(Response::Done));
    let kept = |daemon: &Daemon| daemon.state()["autonomy"][COMPUTER].clone();
    assert_eq!(kept(&daemon), serde_json::to_value(&config).unwrap());
    drop(session);
    daemon.stop().await;
    daemon.restart().await;
    // Another change of the state, written from what the daemon read back.
    let mut session = daemon.session().await;
    let configured = session
        .call(Request::AgentConfigure(AgentConfig::default()))
        .await;
    assert_eq!(configured, Ok(Response::Done));
    assert_eq!(
        kept(&daemon),
        serde_json::to_value(&config).unwrap(),
        "read back"
    );
    let forgot = session
        .call(Request::AutonomyConfigure(AutonomyConfig::default()))
        .await;
    assert_eq!(forgot, Ok(Response::Done));
    assert_eq!(kept(&daemon), Value::Null, "the default forgets it");
}

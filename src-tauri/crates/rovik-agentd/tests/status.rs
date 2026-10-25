//! `rovik-agentd status` (docs/design/autopilot-plan.md H08): this user's
//! chats on the server, each with its latest Autopilot task, read from the
//! daemon's folder while it runs; and the same over the protocol, of the
//! asking computer's chats (`TaskStatus`).

mod common;

use common::project::CHAT;
use common::task::*;
use common::{until, Daemon, BINARY, COMPUTER};
use rovik_core::testing::Reply;
use rovik_protocol::autopilot::ChatTask;
use rovik_protocol::{Request, Response};
use serde_json::Value;

/// The command's output, and that it succeeded.
fn status(daemon: &Daemon, json: bool) -> String {
    let mut command = std::process::Command::new(BINARY);
    command.arg("status").arg("--data").arg(&daemon.data);
    if json {
        command.arg("--json");
    }
    let out = command.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// H08: a task that ran to its delivery, as the command reads it from the
/// daemon's folder while the daemon runs, and as its computer asks for it.
#[tokio::test(flavor = "multi_thread")]
async fn status_lists_each_chat_with_its_task() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start(&["--same-machine"]).await;
    let empty: Value = serde_json::from_str(&status(&daemon, true)).unwrap();
    assert_eq!(empty, serde_json::json!({ "chats": [] }));
    assert_eq!(status(&daemon, false).trim(), "No chats here.");

    let project = project(&daemon);
    let review = vec![Reply::Text("## Findings\n\nNone.")];
    let (url, _) = task_model(look(), review, vec![nothing_to_test()]).await;
    let reply = daemon
        .post_call(1, Request::AgentStart(Box::new(task_start(&url, &project))))
        .await;
    assert!(format!("{reply:?}").contains("RunId(Some("), "{reply:?}");
    assert_eq!(until_delivered(&daemon).await, delivered_whole());

    // Delivering is the work of the task's last run, which ends just after
    // it: the daemon notes that end in state.json then, after the stage's
    // row says it's done. Nothing runs once that's noted.
    let (report, chats) = until("the delivering run's end", || {
        let report: Value = serde_json::from_str(&status(&daemon, true)).unwrap();
        let chats: Vec<ChatTask> = serde_json::from_value(report["chats"].clone()).unwrap();
        chats
            .iter()
            .all(|chat| chat.running.is_none())
            .then_some((report, chats))
    })
    .await;
    assert_eq!(chats.len(), 1, "{report}");
    let chat = &chats[0];
    assert_eq!(chat.chat, CHAT);
    assert_eq!(chat.computer, COMPUTER);
    let task = chat.task.as_ref().expect("its task");
    assert_eq!(task.id, "u1");
    assert_eq!(
        (task.stage.as_str(), task.status.as_str()),
        ("deliver", "done")
    );
    assert_eq!(task.outcome.as_deref(), Some("verified"));
    assert_eq!(
        task.summary.as_deref(),
        Some("Delivered: the result waits on the server for its computer")
    );
    assert!(task.ms > 0, "its runs' time: {task:?}");
    let line = status(&daemon, false);
    assert!(
        line.contains(&format!(
            "({CHAT}, from {COMPUTER}): deliver done, verified; "
        )),
        "{line}"
    );

    // Its computer asks the daemon: the same.
    let mut session = daemon.session().await;
    let asked = session.call(Request::TaskStatus).await;
    let Ok(Response::Tasks(tasks)) = asked else {
        panic!("no tasks: {asked:?}");
    };
    assert_eq!(&tasks, &chats);
    // Another computer's chats aren't its.
    let mut desk = daemon.session_as(common::OTHER, &daemon.other_token).await;
    assert_eq!(
        desk.call(Request::TaskStatus).await,
        Ok(Response::Tasks(Vec::new()))
    );
}

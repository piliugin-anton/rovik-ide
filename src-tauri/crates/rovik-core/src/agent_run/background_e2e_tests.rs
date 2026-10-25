//! Commands the agent leaves running (bash's `background`: a dev server),
//! against the scripted model: each outlives its call and its run until the
//! agent stops it, the user does in the chat's row of background tasks, or
//! the chat is deleted; the chat's windows are told what runs as that
//! changes.

use super::*;
use crate::testing::{beating, beats};
use rovik_protocol::events::BackgroundTask;

/// The project's server: it prints `listening`, then runs until it's
/// stopped, its life seen from outside its sandbox (`beats`).
fn server() -> String {
    beating("listening")
}

fn any_command() -> Permissions {
    Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    }
}

/// The id a command's result says it runs on as: bash's `background`.
fn left_running(result: &str) -> String {
    let status = result.lines().nth(1).unwrap_or_default();
    status
        .strip_prefix("running in the background as ")
        .and_then(|rest| rest.split(',').next())
        .unwrap_or_else(|| panic!("not left running: {result}"))
        .to_string()
}

/// A run in which the agent starts the server and leaves it running; the
/// id its start gave it.
async fn server_started(h: &Harness) -> String {
    let (url, seen) = model(vec![
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": server(), "background": true }),
        },
        Reply::Text("It runs."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Start the server"), any_command()))
        .await;
    assert_eq!(h.run_end().await, "done");
    // The lead was offered it: bash's `background`, and the tool for what
    // runs so.
    let tools = seen.lock().unwrap()[0]["tools"].clone();
    let named = |name: &str| {
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .cloned()
    };
    let bash = named("bash").unwrap();
    assert!(bash
        .pointer("/function/parameters/properties/background")
        .is_some());
    assert!(named("background").is_some());
    let started = result_of(&h.messages().await, "b1").content.clone();
    assert!(started.contains("\nlistening\n"), "{started}");
    left_running(&started)
}

/// The tasks the chat's windows are told of, once told `ids`.
async fn told(h: &Harness, ids: &[&str]) {
    let ids: Vec<Value> = ids.iter().map(|id| json!(id)).collect();
    h.wait_for("backgroundTasks", |e| {
        e["type"] == "backgroundTasks"
            && e["dialogId"] == h.dialog_id.as_str()
            && e["tasks"]
                .as_array()
                .is_some_and(|t| t.iter().map(|t| &t["id"]).eq(ids.iter()))
    })
    .await;
}

/// What the chat's row asks for as it opens: what the chat runs now.
async fn background_tasks(h: &Harness) -> Vec<BackgroundTask> {
    let request = Request::BackgroundTasks {
        dialog_id: h.dialog_id.clone(),
    };
    through(&h.core, request).await.unwrap()
}

/// The user's Stop in the chat's row: whether it was running.
async fn background_stop(h: &Harness, id: &str) -> bool {
    let request = Request::BackgroundStop {
        dialog_id: h.dialog_id.clone(),
        id: id.into(),
    };
    through(&h.core, request).await.unwrap()
}

/// The server outlives the call that started it and the run, the chat's
/// windows told it runs, until the agent, in a later turn, reads it by the
/// id its start gave and stops it with everything it started.
#[tokio::test]
async fn a_server_the_agent_leaves_running_outlives_its_run_until_the_agent_stops_it() {
    let (h, server) = (harness(&[]).await, server());
    let id = server_started(&h).await;
    told(&h, &[&id]).await;
    assert!(beats(&h.project), "it runs on after the run");
    let listed = background_tasks(&h).await;
    assert_eq!(listed.len(), 1);
    assert_eq!((&listed[0].id, &listed[0].command), (&id, &server));

    h.events.lock().unwrap().clear();
    let (url, _) = model(vec![
        Reply::Call {
            id: "l1",
            name: "background",
            args: json!({ "action": "list" }),
        },
        Reply::Call {
            id: "o1",
            name: "background",
            args: json!({ "action": "output", "id": id }),
        },
        Reply::Call {
            id: "s1",
            name: "background",
            args: json!({ "action": "stop", "id": id }),
        },
        Reply::Text("Stopped."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Stop the server"), any_command()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let listed = &result_of(&messages, "l1").content;
    assert!(
        listed.starts_with(&format!("{id}: `{server}`, running for ")),
        "{listed}"
    );
    let read = &result_of(&messages, "o1").content;
    assert!(
        read.starts_with(&format!("$ {server}\nrunning in the background as {id}, ")),
        "{read}"
    );
    assert!(read.contains("\n(no new output)\n"), "read once: {read}");
    let stopped = &result_of(&messages, "s1").content;
    assert!(
        stopped.starts_with(&format!("$ {server}\nstopped (killed), ")),
        "{stopped}"
    );
    assert!(!beats(&h.project), "what it started is stopped too");
    told(&h, &[]).await;
    assert!(background_tasks(&h).await.is_empty());
    assert!(!background_stop(&h, &id).await, "no longer there");
}

/// The user's Stop ends the server for good, the chat's windows told; the
/// agent hears it was stopped when it next looks.
#[tokio::test]
async fn the_user_stops_a_server_the_agent_left_running_and_the_agent_hears_it() {
    let (h, server) = (harness(&[]).await, server());
    let id = server_started(&h).await;
    told(&h, &[&id]).await;
    let started_at = background_tasks(&h).await[0].started_at;
    let now = crate::chat_db::store::now_ms();
    assert!((now - 60_000..=now).contains(&started_at), "{started_at}");

    assert!(background_stop(&h, &id).await);
    assert!(!beats(&h.project), "what it started is stopped too");
    told(&h, &[]).await;
    assert!(background_tasks(&h).await.is_empty());

    h.events.lock().unwrap().clear();
    let (url, _) = model(vec![
        Reply::Call {
            id: "l1",
            name: "background",
            args: json!({ "action": "list" }),
        },
        Reply::Call {
            id: "o1",
            name: "background",
            args: json!({ "action": "output", "id": id }),
        },
        Reply::Call {
            id: "l2",
            name: "background",
            args: json!({ "action": "list" }),
        },
        Reply::Text("It was stopped."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Is it up?"), any_command()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let listed = &result_of(&messages, "l1").content;
    assert!(
        listed.starts_with(&format!("{id}: `{server}`, stopped after ")),
        "{listed}"
    );
    let read = &result_of(&messages, "o1").content;
    assert!(
        read.starts_with(&format!("$ {server}\nstopped (killed), ")),
        "{read}"
    );
    // Told once, then forgotten.
    let listed = &result_of(&messages, "l2").content;
    assert_eq!(listed, "No command runs in the background.");
}

/// A deleted chat's server goes with it.
#[tokio::test]
async fn deleting_a_chat_stops_what_its_agent_left_running() {
    let h = harness(&[]).await;
    server_started(&h).await;
    assert!(beats(&h.project));
    crate::chat_db::api::chat_delete_dialog(&h.core, h.dialog_id.clone())
        .await
        .unwrap();
    assert!(!beats(&h.project));
    assert!(crate::command_tool::background::list(&h.dialog_id).is_empty());
}

/// In an isolated chat, a server the agent left running works in the
/// chat's view, which goes once the chat's changes land: they wait while it
/// runs, so that it goes on showing them, and say why. The user's Land
/// takes them, and stops it. Needs FUSE (skipped without it).
#[tokio::test]
async fn an_isolated_chats_changes_wait_to_land_while_its_server_runs_in_its_view() {
    if !crate::vfs::mount_works() {
        return;
    }
    // The server's heartbeat is a by-product, which doesn't land.
    let mut h = harness(&[(".gitignore", "beat\n"), ("a.txt", "one\n")]).await;
    h.isolate().await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "old_text": "one", "content": "two" }),
        },
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": server(), "background": true }),
        },
        Reply::Text("It runs."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Change it, and run it"), any_command());
    request.isolate = Some(true);
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let id = left_running(&result_of(&h.messages().await, "b1").content);
    assert_eq!(h.file("a.txt").as_deref(), Some("one\n"), "not landed");
    let pending = crate::overlay::chats::pending(&h.dialog_id).expect("why it waits");
    assert!(
        pending
            .reason
            .contains(&format!("`{}` runs in the background", server())),
        "{pending:?}"
    );
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    assert!(beats(&view), "it runs on in the chat's view");

    crate::overlay::chats::land_now(&h.dialog_id, false).unwrap();
    assert_eq!(h.file("a.txt").as_deref(), Some("two\n"));
    assert!(crate::command_tool::background::list(&h.dialog_id).is_empty());
    told(&h, &[]).await;
    assert!(!background_stop(&h, &id).await, "stopped with the landing");
}

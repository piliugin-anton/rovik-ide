//! Autopilot's tasks end to end against the scripted provider
//! (docs/design/autopilot-plan.md, section 7): the mode, its stages and
//! what each leaves in the chat.

use super::*;

/// A message sent in Autopilot mode: a task.
fn autopilot_send(text: &str) -> StartKind {
    StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: text.into(),
            meta: Some(json!({ "mode": "autopilot" })),
            attachments: Vec::new(),
        },
    }
}

/// The tools a request offered, by name.
fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The agent log's lines of `event`, once there is one: the log is written
/// off the run's thread, a moment after the run ends.
async fn log_lines(h: &Harness, event: &str) -> Vec<Value> {
    let path = h.core.paths.log_dir.join("agent-log.jsonl");
    for _ in 0..200 {
        let lines: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|l| l["event"] == event)
            .collect();
        if !lines.is_empty() {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no {event} line in {}", path.display());
}

/// A01: the task's first run is a Plan-mode run, marked as Autopilot's,
/// whatever the selector said when it started.
#[tokio::test]
async fn an_autopilot_message_starts_a_plan_mode_run() {
    let h = harness(&[("src/a.rs", "fn main() {}\n")]).await;
    let (url, seen) = model(vec![Reply::Text("Looked around.")]).await;
    let mut request = h.request(&url, autopilot_send("Add a flag"), quiet_permissions());
    request.mode = Some("agent".into()); // the message's mode wins
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");

    let requests = seen.lock().unwrap().clone();
    let tools = tool_names(&requests[0]);
    assert!(tools.iter().any(|t| t == "submit_plan"), "{tools:?}");
    assert!(
        !tools.iter().any(|t| t == "write" || t == "bash"),
        "{tools:?}"
    );
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("## Plan mode"), "{system}");
    let runs = log_lines(&h, "run").await;
    assert_eq!(runs.last().unwrap()["autopilot"], true, "{runs:?}");
}

/// A02: a stage record is a row of the chat, shown as it's added and as it
/// ends; read back from the database it's the same, also by a core opened
/// afresh on the same data (an app that reopens), and also when hundreds of
/// messages have come since.
#[tokio::test]
async fn stage_records_are_rows_that_read_back_after_a_reload() {
    use super::autopilot::{
        add_stage, end_stage, load, records_of, Effort, Stage, StageRecord, Status,
    };
    let h = harness(&[]).await;
    let core = h.core.clone();
    let plan = add_stage(
        &core,
        &h.dialog_id,
        &StageRecord::running("task-1", Stage::Plan, 1, Effort::Balanced),
    )
    .await
    .unwrap();
    let ended = end_stage(&core, &h.dialog_id, &plan, |r| {
        r.status = Status::Done;
        r.summary = Some("Saved the plan".into());
        r.reference = Some("m-plan".into());
    })
    .await
    .unwrap();
    assert_eq!(ended.status, Status::Done);
    let skipped = StageRecord::ended(
        "task-1",
        Stage::Prepare,
        1,
        Effort::Balanced,
        Status::Skipped,
    )
    .with_summary("Not available yet");
    add_stage(&core, &h.dialog_id, &skipped).await.unwrap();

    // Shown as they came: added, then the plan's end.
    let added: Vec<Value> = h
        .events_of("messagesAdded")
        .iter()
        .flat_map(|e| e["messages"].as_array().cloned().unwrap_or_default())
        .collect();
    assert_eq!(added.len(), 2);
    assert_eq!(added[0]["meta"]["stage"]["status"], "running");
    assert_eq!(added[0]["role"], "assistant");
    assert_eq!(added[0]["content"], "");
    let patched = h.events_of("messagePatched");
    assert_eq!(patched[0]["messageId"], plan.as_str());
    assert_eq!(patched[0]["patch"]["meta"]["stage"]["status"], "done");

    // Hundreds of messages later, still found.
    let (id, many) = (
        h.dialog_id.clone(),
        (0..600)
            .map(|i| run::new_message(if i % 2 == 0 { "user" } else { "assistant" }, "x"))
            .collect::<Vec<_>>(),
    );
    h.core
        .db
        .write(move |c| store::append_messages(c, &id, &many))
        .await
        .unwrap();
    let all = load(&core, &h.dialog_id).await.unwrap();
    let records: Vec<StageRecord> = records_of(&all, "task-1")
        .into_iter()
        .map(|r| r.record.clone())
        .collect();
    assert_eq!(records, vec![ended.clone(), skipped.clone()]);
    assert_eq!(all[0].id, plan);

    // A core opened afresh on the same data reads the same.
    let reopened = Core::open(
        crate::core::Paths::new(h._dir.0.join("data"), h._dir.0.join("config")),
        Arc::new(crate::testing::Recorder(Arc::new(Mutex::new(Vec::new())))),
    )
    .unwrap();
    let again = load(&reopened, &h.dialog_id).await.unwrap();
    assert_eq!(again, all);
    assert!(records_of(&again, "another task").is_empty());
}

/// A01: an Agent-mode run's log line says nothing of Autopilot.
#[tokio::test]
async fn an_agent_mode_run_is_not_autopilots() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Hello"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let runs = log_lines(&h, "run").await;
    assert!(runs.last().unwrap().get("autopilot").is_none(), "{runs:?}");
}

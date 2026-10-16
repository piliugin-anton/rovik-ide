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

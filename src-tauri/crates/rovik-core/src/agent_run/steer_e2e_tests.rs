//! Messages the user sends a run while it works (steering-plan.md), against
//! the scripted model: where each is read, what the next request sends,
//! and what goes back to the input when the run ends without reading it.
//! The numbers are the plan's tests (section 6).

use super::*;
use rovik_protocol::agent::SteerReply;

/// A message as the message box sends one to a run: what was typed, with
/// the editor context the frontend puts before it.
fn typed(text: &str) -> UserMessage {
    UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: text.into(),
        meta: Some(json!({
            "sentContent": format!("[Active editor context — file src/lib.rs]\n\n{text}")
        })),
        attachments: Vec::new(),
    }
}

const FRAME_START: &str = "[The user sent this message while you were working.";

const DENIED_WITH_MESSAGE: &str = "Error: not run: the user denied an earlier call in this reply and sent a message. Read it first.";

/// Commands wait for the user's approval: a run stops there, mid-reply,
/// for as long as a test needs. No checklist gate, no checks.
fn asks_for_commands() -> Permissions {
    Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        checklist: ChecklistMode::Off,
        ..quiet_permissions()
    }
}

/// Checks that take a moment: time to send a message while they run.
const SLOW_TESTS: &str = r#"{ "name": "t", "scripts": { "test": "node -e \"setTimeout(() => process.exit(0), 1500)\"" } }"#;

fn has_node() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

/// The checks on, the checklist gate off.
fn checks_on() -> Permissions {
    Permissions {
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    }
}

fn is_steer(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .is_some_and(|meta| meta.get("steer").is_some())
}

/// The text of a message sent to the model.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn sent(request: &Value) -> Vec<Value> {
    request["messages"].as_array().unwrap().clone()
}

/// The request's first user message, as a request sends it.
fn first_user(request: &Value) -> Value {
    sent(request)
        .into_iter()
        .find(|m| m["role"] == "user")
        .unwrap()
}

impl Harness {
    async fn steer(&self, text: &str) -> (String, SteerReply) {
        let message = typed(text);
        let id = message.id.clone();
        let request = Request::AgentSteer {
            dialog_id: self.dialog_id.clone(),
            message,
        };
        let reply = through(&self.core, request).await.unwrap();
        (id, reply)
    }

    async fn unsteer(&self, message_id: &str) -> bool {
        let request = Request::AgentUnsteer {
            dialog_id: self.dialog_id.clone(),
            message_id: message_id.into(),
        };
        through(&self.core, request).await.unwrap()
    }

    async fn approval_for(&self, call: &str) {
        self.wait_for("approvalRequested", |e| {
            e["type"] == "approvalRequested" && e["request"]["toolCallId"] == call
        })
        .await;
    }

    async fn decide(&self, call: &str, decision: ApprovalDecision) {
        let core = self.core.clone();
        assert!(agent_resolve_approval(&core, self.dialog_id.clone(), call.into(), decision).await);
    }

    /// The agent log's `steer` lines of the chat (D18).
    async fn steer_lines(&self) -> Vec<Value> {
        log::flushed().await;
        let path = self.core.paths.log_dir.join("agent-log.jsonl");
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|l| l["event"] == "steer" && l["dialogId"] == self.dialog_id.as_str())
            .collect()
    }

    /// Waits for the checks' record to say they're running.
    async fn checks_running(&self) {
        self.wait_for_within("checks running", Duration::from_secs(30), |e| {
            let meta = match e["type"].as_str() {
                Some("messagesAdded") => e["messages"]
                    .as_array()
                    .and_then(|m| m.iter().find_map(|m| m["meta"].get("verification")))
                    .cloned(),
                Some("messagePatched") => e["patch"]["meta"].get("verification").cloned(),
                _ => None,
            };
            meta.is_some_and(|v| v["status"] == "running")
        })
        .await;
    }
}

/// The ends where a message waiting is read and the run goes on (3.3).
#[test]
fn which_ends_go_on_with_a_message() {
    for end in [
        "done",
        "verified",
        "unverified",
        "verify-unverified",
        "verify-unchecked",
        "verify-flagged",
        "verify-unresolved",
        "verify-gave-up",
        "unfinished-checklist",
    ] {
        assert!(run::steer_goes_on(end), "{end}");
    }
    for end in [
        "stopped",
        "verify-stopped",
        "error",
        "repetition",
        "verify-needs-approval",
        "budget",
        "review-open",
        "unmet",
        "paused-move",
        "paused-checkpoint",
    ] {
        assert!(!run::steer_goes_on(end), "{end}");
    }
}

/// 1, 2: sent while a reply's calls run (an approval waits), then Allow:
/// the reply's other calls run, and the message is read after the last
/// result, before the next request, which sends it framed. The request's
/// own message is sent as before.
#[tokio::test]
async fn a_message_sent_while_the_calls_run_is_read_before_the_next_request() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("c1", "bash", json!({ "command": list("") })),
            ("c2", "read", json!({ "path": "src/lib.rs" })),
        ]),
        Reply::Text("Done, with the existing client."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Rename alpha"), asks_for_commands()))
        .await;
    let started = h
        .wait_for("runStarted", |e| e["type"] == "runStarted")
        .await;
    assert_eq!(started["steerable"], true);
    h.approval_for("c1").await;
    let (id, reply) = h.steer("use the existing client").await;
    assert_eq!(reply, SteerReply::Queued);
    // A page loaded now shows it waiting.
    let status = agent_status(&h.core).await;
    assert_eq!(status.steering.len(), 1);
    assert_eq!(status.steering[0].message.id, id);
    assert!(status.runs[0].steerable);
    h.decide("c1", ApprovalDecision::Allow).await;
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    assert_eq!(
        roles(&messages),
        ["user", "assistant", "tool", "tool", "user", "assistant"]
    );
    let read = &messages[4];
    assert_eq!(read.id, id, "stored with its id");
    assert_eq!(read.content, "use the existing client");
    assert!(is_steer(read));
    assert!(read.meta.as_ref().unwrap().get("mode").is_none());
    let added = h.events_of("messagesAdded");
    assert!(added.iter().any(|e| e["messages"][0]["id"] == id.as_str()));

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let last = sent(&requests[1]).last().unwrap().clone();
    assert_eq!(last["role"], "user");
    let text = text_of(&last);
    assert!(text.starts_with(FRAME_START), "{text}");
    assert!(
        text.ends_with("[Active editor context — file src/lib.rs]\n\nuse the existing client"),
        "{text}"
    );
    // The request's message is sent the same way on both steps: the
    // provider's cache keeps it, the repository map with it.
    assert_eq!(first_user(&requests[0]), first_user(&requests[1]));
    assert!(text_of(&first_user(&requests[1])).ends_with("Rename alpha"));

    let lines = h.steer_lines().await;
    let phases: Vec<&str> = lines.iter().filter_map(|l| l["phase"].as_str()).collect();
    assert_eq!(phases, ["queued", "delivered"], "{lines:#?}");
    assert_eq!(lines[0]["doing"], "approval");
    assert_eq!(lines[1]["at"], "step");
    assert_eq!(lines[1]["callsMeanwhile"], 2);
    assert!(h.steer("too late").await.1 == SteerReply::Ended);
}

/// 3: the user's Deny with a message waiting: the reply's later calls
/// aren't run, and the message is read at once.
#[tokio::test]
async fn the_users_deny_with_a_message_waiting_skips_the_rest_of_the_reply() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("c1", "bash", json!({ "command": list("") })),
            ("c2", "read", json!({ "path": "src/lib.rs" })),
        ]),
        Reply::Text("Reading the code instead."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Rename alpha"), asks_for_commands()))
        .await;
    h.approval_for("c1").await;
    let (id, _) = h.steer("no commands: read the code").await;
    h.decide("c1", ApprovalDecision::Deny).await;
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    assert_eq!(
        roles(&messages),
        ["user", "assistant", "tool", "tool", "user", "assistant"]
    );
    assert!(messages[2].content.starts_with("Error: the user denied"));
    assert_eq!(messages[3].tool_call_id.as_deref(), Some("c2"));
    assert_eq!(messages[3].content, DENIED_WITH_MESSAGE);
    assert_eq!(messages[4].id, id);
    let requests = seen.lock().unwrap().clone();
    assert!(text_of(sent(&requests[1]).last().unwrap()).ends_with("no commands: read the code"));
    let lines = h.steer_lines().await;
    assert_eq!(lines.last().unwrap()["at"], "deny", "{lines:#?}");
}

/// 3: a call refused by the user's settings isn't their Deny: the reply's
/// other calls run, and the message is read at the next step.
#[tokio::test]
async fn a_denial_by_the_settings_keeps_the_next_step_rule() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "bash", json!({ "command": list("") })),
            ("c2", "bash", json!({ "command": "sudo ls" })),
            ("c3", "read", json!({ "path": "src/lib.rs" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        denied_commands: vec!["sudo".into()],
        ..asks_for_commands()
    };
    h.start(h.request(&url, Harness::send("Look"), permissions))
        .await;
    h.approval_for("c1").await;
    h.steer("the tests are in tests/unit").await;
    h.decide("c1", ApprovalDecision::Allow).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let result = |id: &str| {
        messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content
            .clone()
    };
    assert_ne!(result("c2"), DENIED_WITH_MESSAGE);
    assert!(result("c3").contains("alpha"), "{}", result("c3"));
    assert_eq!(
        roles(&messages),
        [
            "user",
            "assistant",
            "tool",
            "tool",
            "tool",
            "user",
            "assistant"
        ]
    );
}

/// 4: sent while a scout runs: the lead reads it once the `task` call has
/// its result. The scout isn't told.
#[tokio::test]
async fn a_message_sent_while_subagents_run_waits_for_their_reports() {
    const REPORT: &str = "## Answer\nIt is in a.txt.\n\n## Evidence\n- `a.txt:1`: the line\n\n## Open questions\nNone\n\n## Web sources\nNone";
    let h = harness(&[("a.txt", "MARK-1 lives here\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            "MARK-1",
            vec![Reply::Slow(800, Box::new(Reply::Text(REPORT)))],
        )],
        vec![
            Reply::Call {
                id: "t1",
                name: "task",
                args: json!({ "kind": "scout", "goal": "Where is MARK-1 defined?" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut request = h.request(&url, Harness::send("Find it"), asks_for_commands());
    request.config.as_mut().unwrap().subagents = serde_json::from_value(
        json!({ "enabled": true, "kinds": { "scout": { "enabled": true } } }),
    )
    .unwrap();
    h.start(request).await;
    h.wait_for("subagentStarted", |e| e["type"] == "subagentStarted")
        .await;
    let (id, reply) = h.steer("it's in a.txt").await;
    assert_eq!(reply, SteerReply::Queued);
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(
        roles(&messages),
        ["user", "assistant", "tool", "user", "assistant"]
    );
    assert_eq!(messages[3].id, id);
    // The scout's requests never had it.
    let requests = seen.lock().unwrap().clone();
    let scout: Vec<&Value> = requests
        .iter()
        .filter(|b| route_of(b, &["MARK-1"]).is_some())
        .collect();
    assert!(!scout.is_empty());
    assert!(scout
        .iter()
        .all(|b| !b.to_string().contains("it's in a.txt")));
    let lines = h.steer_lines().await;
    assert_eq!(lines[0]["doing"], "subagents", "{lines:#?}");
}

/// 5: waiting when the model says it's done: read instead of the checks,
/// which run once, at the end of the run that read it.
#[tokio::test]
async fn a_message_waiting_at_done_is_read_before_the_checks() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "a.txt", "content": "a\n" }),
        },
        Reply::Slow(800, Box::new(Reply::Text("Done."))),
        Reply::Text("Kept it short, as asked."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write a"), checks_on()))
        .await;
    requests_seen(&seen, 2).await;
    let (id, _) = h.steer("keep it short").await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    let steer = messages.iter().position(|m| m.id == id).expect("read");
    let records: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.meta
                .as_ref()
                .is_some_and(|x| x.get("verification").is_some())
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(records.len(), 1, "the checks ran once");
    assert!(records[0] > steer, "after the message was read");
    assert_eq!(seen.lock().unwrap().len(), 3);
    let lines = h.steer_lines().await;
    assert_eq!(lines.last().unwrap()["at"], "done", "{lines:#?}");
}

/// 6: sent while the checks run: they finish, and the message is read with
/// their record; the run goes on. It changed nothing since: no new round.
#[tokio::test]
async fn a_message_sent_while_the_checks_run_is_read_with_their_results() {
    let h = harness(&[("package.json", SLOW_TESTS)]).await;
    if !has_node() {
        return;
    }
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x\n" }),
        },
        Reply::Text("Done."),
        Reply::Text("Noted for next time."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write x"), checks_on()))
        .await;
    h.checks_running().await;
    let (id, _) = h.steer("x is fine as it is").await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let messages = h.messages().await;
    let record = messages
        .iter()
        .position(|m| {
            m.meta
                .as_ref()
                .is_some_and(|x| x["verification"]["status"] == "passed")
        })
        .expect("the checks' record");
    let steer = messages.iter().position(|m| m.id == id).expect("read");
    assert!(steer > record, "read with their results");
    assert_eq!(messages.last().unwrap().content, "Noted for next time.");
    assert_eq!(seen.lock().unwrap().len(), 3);
    let lines = h.steer_lines().await;
    assert_eq!(lines[0]["doing"], "checks", "{lines:#?}");
    assert_eq!(lines.last().unwrap()["at"], "checks", "{lines:#?}");
}

/// 7, 17: sent, then Stop: handed back before the run's end, never stored;
/// the calls Stop cut are answered as stopped, not as the user's Deny.
#[tokio::test]
async fn stop_hands_a_waiting_message_back() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("c1", "bash", json!({ "command": list("") })),
            ("c2", "read", json!({ "path": "src/lib.rs" })),
        ]),
        Reply::Text("never"),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), asks_for_commands()))
        .await;
    h.approval_for("c1").await;
    let (id, _) = h.steer("actually, stop").await;
    let core = h.core.clone();
    agent_stop(&core, h.dialog_id.clone()).await;
    assert_eq!(h.run_end().await, "stopped");
    let events = h.events.lock().unwrap().clone();
    let returned = events
        .iter()
        .position(|e| e["type"] == "steerReturned")
        .expect("handed back");
    let ended = events.iter().position(|e| e["type"] == "runEnded").unwrap();
    assert!(returned < ended, "told before the run's end");
    assert_eq!(events[returned]["messageIds"], json!([id]));
    assert_eq!(events[returned]["outcome"], "stopped");
    let messages = h.messages().await;
    assert!(!messages.iter().any(|m| m.id == id || is_steer(m)));
    for m in messages.iter().filter(|m| m.role == "tool") {
        assert_ne!(m.content, DENIED_WITH_MESSAGE);
        assert!(m.content.contains("stopped"), "{}", m.content);
    }
    // Stopped, the chat takes none: the next is for a new run.
    assert_eq!(h.steer("hello again").await.1, SteerReply::Ended);
    let lines = h.steer_lines().await;
    assert_eq!(lines.last().unwrap()["phase"], "returned", "{lines:#?}");
    assert_eq!(lines.last().unwrap()["why"], "stopped");
}

/// 17: Stop while the checks run, a message waiting: handed back.
#[tokio::test]
async fn stop_during_the_checks_hands_the_message_back() {
    let h = harness(&[("package.json", SLOW_TESTS)]).await;
    if !has_node() {
        return;
    }
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Write x"), checks_on()))
        .await;
    h.checks_running().await;
    let (id, _) = h.steer("one more thing").await;
    let core = h.core.clone();
    agent_stop(&core, h.dialog_id.clone()).await;
    let end = h.run_end_within(Duration::from_secs(60)).await;
    assert!(!run::steer_goes_on(&end), "{end}");
    let returned = h.events_of("steerReturned");
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0]["messageIds"], json!([id]));
    assert!(!h.messages().await.iter().any(|m| m.id == id));
}

/// 8: a checkpoint due where a message is read doesn't happen: the next
/// one counts from there.
#[tokio::test]
async fn a_message_read_at_a_checkpoint_goes_on_past_it() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "src/lib.rs" }),
    };
    let (url, _) = model(vec![
        read("r1"),
        Reply::Call {
            id: "c2",
            name: "bash",
            args: json!({ "command": list("") }),
        },
        read("r3"),
        read("r4"),
        read("r5"),
        Reply::Text("never"),
    ])
    .await;
    let permissions = Permissions {
        tool_call_checkpoint: Some(2),
        ..asks_for_commands()
    };
    h.start(h.request(&url, Harness::send("Look"), permissions))
        .await;
    h.approval_for("c2").await;
    h.steer("go on").await;
    h.decide("c2", ApprovalDecision::Allow).await;
    assert_eq!(h.run_end().await, "paused-checkpoint");
    let messages = h.messages().await;
    let paused = messages.last().unwrap().meta.clone().unwrap();
    assert_eq!(paused["paused"]["toolCalls"], 4, "{paused}");
    let lines = h.steer_lines().await;
    assert_eq!(lines.last().unwrap()["at"], "pause", "{lines:#?}");
}

/// 9: Plan mode takes no messages while it works.
#[tokio::test]
async fn plan_mode_refuses_messages() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Slow(600, Box::new(Reply::Text("A plan.")))]).await;
    let mut request = h.request(&url, Harness::send("Plan it"), asks_for_commands());
    request.mode = Some("plan".into());
    h.start(request).await;
    let started = h
        .wait_for("runStarted", |e| e["type"] == "runStarted")
        .await;
    assert!(started.get("steerable").is_none(), "{started}");
    let (_, reply) = h.steer("and this").await;
    assert!(
        matches!(&reply, SteerReply::Refused { reason } if reason.contains("Plan mode")),
        "{reply:?}"
    );
    let status = agent_status(&h.core).await;
    assert!(!status.runs[0].steerable);
    h.run_end().await;
    assert!(!h.messages().await.iter().any(is_steer));
}

/// 10: taken back before it's read: true, and it's never stored; once
/// read, false.
#[tokio::test]
async fn a_message_taken_back_is_never_read() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": list("") }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), asks_for_commands()))
        .await;
    h.approval_for("c1").await;
    let (a, _) = h.steer("first thought").await;
    let (b, _) = h.steer("second thought").await;
    assert!(h.unsteer(&a).await);
    assert!(!h.unsteer(&a).await, "once");
    h.decide("c1", ApprovalDecision::Allow).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(!messages.iter().any(|m| m.id == a));
    assert!(messages.iter().any(|m| m.id == b));
    assert!(!h.unsteer(&b).await, "read already");
    let lines = h.steer_lines().await;
    let withdrawn: Vec<&Value> = lines.iter().filter(|l| l["phase"] == "withdrawn").collect();
    assert_eq!(withdrawn.len(), 1);
    assert_eq!(withdrawn[0]["messageId"], a.as_str());
}

/// 11: messages sent as the run ends, close together: each is read (the
/// run goes on with it) or answered `Ended`, never lost.
#[tokio::test]
async fn a_message_sent_as_the_run_ends_is_read_or_ended() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Hi"), asks_for_commands()))
        .await;
    // Each message read keeps the run going; the gaps grow until one is
    // longer than a turn, and the run ends between two messages.
    let mut queued = Vec::new();
    let mut ended = false;
    for i in 0..120u64 {
        match h.steer(&format!("note {i}")).await {
            (id, SteerReply::Queued) => queued.push(id),
            (_, SteerReply::Ended) => {
                ended = true;
                break;
            }
            (_, SteerReply::Refused { .. }) => {}
        }
        tokio::time::sleep(Duration::from_millis(i)).await;
    }
    assert!(ended, "the run ended meanwhile");
    assert!(!queued.is_empty());
    h.run_end().await;
    let messages = h.messages().await;
    for id in &queued {
        assert!(messages.iter().any(|m| &m.id == id), "{id} lost");
    }
    assert!(h.events_of("steerReturned").is_empty());
    assert_eq!(h.events_of("runEnded").len(), 1);
}

/// 12: a move with a message waiting: it's stored before the pause, and
/// the `ContinueRun` where the chat went sends it.
#[tokio::test]
async fn a_move_takes_a_waiting_message_along() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": list("") }),
        },
        Reply::Text("Done, as asked."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), asks_for_commands()))
        .await;
    h.approval_for("c1").await;
    let (id, _) = h.steer("and keep it short").await;
    let pausing = h.pause_for_move("server").await;
    h.decide("c1", ApprovalDecision::Allow).await;
    assert!(within("the pause", pausing).await.unwrap());
    let messages = h.messages().await;
    let steer = messages.iter().position(|m| m.id == id).expect("stored");
    assert!(messages[steer + 1..].iter().any(|m| m
        .meta
        .as_ref()
        .is_some_and(|x| x["paused"]["kind"] == "move")));
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::ContinueRun, asks_for_commands()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    let last = requests.last().unwrap();
    assert!(sent(last)
        .iter()
        .any(|m| text_of(m).ends_with("and keep it short")));
}

/// 13: a long run's focus note quotes what the user added while it worked.
#[tokio::test]
async fn a_long_runs_focus_note_quotes_the_message() {
    let h = harness(&[("src/lib.rs", "pub fn alpha() {}\n")]).await;
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": "src/lib.rs" }),
    };
    let (url, seen) = model(vec![
        read("r1"),
        Reply::Call {
            id: "c2",
            name: "bash",
            args: json!({ "command": list("") }),
        },
        read("r3"),
        read("r4"),
        read("r5"),
        read("r6"),
        read("r7"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Rename alpha"), asks_for_commands()))
        .await;
    h.approval_for("c2").await;
    h.steer("use the existing client").await;
    h.decide("c2", ApprovalDecision::Allow).await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap().clone();
    let last = text_of(sent(requests.last().unwrap()).last().unwrap());
    assert!(
        last.contains("Added by the user while you worked (quoted, oldest first):\n- > use the existing client"),
        "{last}"
    );
}

/// 16: "Run checks", a message sent while they run: read at their end, and
/// the run goes on into the loop.
#[tokio::test]
async fn a_message_sent_while_run_checks_runs_is_read_at_its_end() {
    let h = harness(&[("package.json", SLOW_TESTS)]).await;
    if !has_node() {
        return;
    }
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "x.txt", "content": "x\n" }),
        },
        Reply::Text("Done."),
        Reply::Text("Noted."),
    ])
    .await;
    let waits = Permissions {
        checklist: ChecklistMode::Off,
        ..needs_user_approval()
    };
    h.start(h.request(&url, Harness::send("Write x"), waits.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let record = h.messages().await.last().unwrap().id.clone();
    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::RunChecks { record_id: record }, waits))
        .await;
    h.checks_running().await;
    let (id, _) = h.steer("x is fine").await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let messages = h.messages().await;
    let steer = messages.iter().position(|m| m.id == id).expect("read");
    assert_eq!(messages.last().unwrap().content, "Noted.");
    assert!(steer < messages.len() - 1);
    assert_eq!(seen.lock().unwrap().len(), 3);
}

/// 18: sent while the change review runs: read with its findings, before
/// the checks start.
#[tokio::test]
async fn a_message_sent_during_the_review_is_read_before_the_checks() {
    const REVIEW: &str = "# Review this change";
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Slow(
                800,
                Box::new(Reply::Text("## Findings\n\nNone.")),
            )],
        )],
        vec![
            Reply::Call {
                id: "w1",
                name: "write",
                args: json!({ "path": "a.txt", "content": "new\n" }),
            },
            Reply::Text("Done."),
            Reply::Text("Noted."),
        ],
    )
    .await;
    let mut request = h.request(&url, Harness::send("Change a"), checks_on());
    request.config.as_mut().unwrap().subagents = serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "reviewer": { "enabled": true } },
        "reviewMinLines": 1,
    }))
    .unwrap();
    h.start(request).await;
    for _ in 0..400 {
        let reviewing = seen
            .lock()
            .unwrap()
            .iter()
            .any(|b| route_of(b, &[REVIEW]).is_some());
        if reviewing {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (id, _) = h.steer("a.txt is generated, don't hand-edit it").await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    let steer = messages.iter().position(|m| m.id == id).expect("read");
    let review = messages
        .iter()
        .position(|m| m.meta.as_ref().is_some_and(|x| x.get("review").is_some()))
        .expect("the review's record");
    let checks = messages
        .iter()
        .position(|m| {
            m.meta
                .as_ref()
                .is_some_and(|x| x.get("verification").is_some())
        })
        .expect("the checks' record");
    assert!(
        review < steer && steer < checks,
        "{review} {steer} {checks}"
    );
    let lines = h.steer_lines().await;
    assert_eq!(lines.last().unwrap()["at"], "review", "{lines:#?}");
}

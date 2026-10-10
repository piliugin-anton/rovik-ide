//! Whole runs with subagents (subagent.rs, subagent_run.rs) against the
//! scripted provider: the lead and each scout get their own script, routed
//! by a marker in their first message (`model_routed`).

use super::*;

const REPORT: &str = "## Answer\nIt is in a.txt.\n\n## Evidence\n- `a.txt:1`: the line\n\n## Open questions\nNone\n\n## Web sources\nNone";

fn scouts_on(extra: Value) -> subagent::SubagentConfig {
    let mut scout = json!({ "enabled": true });
    if let (Some(s), Some(e)) = (scout.as_object_mut(), extra.as_object()) {
        s.extend(e.clone());
    }
    serde_json::from_value(json!({ "enabled": true, "kinds": { "scout": scout } })).unwrap()
}

fn task(id: &'static str, goal: &str) -> Reply {
    Reply::Call {
        id,
        name: "task",
        args: json!({ "kind": "scout", "goal": goal }),
    }
}

fn write_call(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

fn task_call(id: &'static str, goal: &str) -> (&'static str, &'static str, Value) {
    (id, "task", json!({ "kind": "scout", "goal": goal }))
}

impl Harness {
    fn with_subagents(
        &self,
        url: &str,
        text: &str,
        permissions: Permissions,
        subagents: subagent::SubagentConfig,
    ) -> StartRequest {
        let mut request = self.request(url, Harness::send(text), permissions);
        request.config.as_mut().unwrap().subagents = subagents;
        request
    }

    async fn child_of(&self, call_id: &str) -> store::ChildDialog {
        let (id, call) = (self.dialog_id.clone(), call_id.to_string());
        let db = self.app.state::<DbState>().inner().clone();
        db.read(move |c| store::child_for_call(c, &id, &call))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no subagent chat for {call_id}"))
    }

    async fn messages_of(&self, dialog_id: &str) -> Vec<MessageRow> {
        let id = dialog_id.to_string();
        let db = self.app.state::<DbState>().inner().clone();
        db.read(move |c| store::get_messages(c, &id, None, 500))
            .await
            .unwrap()
            .messages
    }

    /// The lead's result for a `task` call.
    async fn task_result(&self, call_id: &str) -> MessageRow {
        self.messages()
            .await
            .into_iter()
            .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call_id))
            .unwrap_or_else(|| panic!("no result for {call_id}"))
    }
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn system_prompt(body: &Value) -> String {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The requests whose first message after the system prompt has `marker`.
fn requests_with(seen: &Arc<Mutex<Vec<Value>>>, marker: &str) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|b| route_of(b, &[marker]).is_some())
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_scout_explores_and_reports_to_the_lead() {
    let h = harness(&[("a.txt", "MARK-1 lives here\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            "MARK-1",
            vec![
                Reply::Call {
                    id: "s1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "Where is MARK-1 defined?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");

    // The lead got the report, framed as data, and only the report.
    let lead = h.messages().await;
    assert_eq!(roles(&lead), ["user", "assistant", "tool", "assistant"]);
    let result = h.task_result("t1").await;
    assert!(result.content.starts_with("Report from a scout subagent."));
    assert!(result
        .content
        .contains("Status: done (1 tool call, model Test/m)"));
    assert!(result.content.ends_with(REPORT));
    let meta = &result.meta.as_ref().unwrap()["subagent"];
    assert_eq!(meta["status"], "done");
    assert_eq!(meta["kind"], "scout");
    assert_eq!(meta["toolCalls"], 1);

    // Its own chat: the brief, its work, its report; hidden from the list.
    let child = h.child_of("t1").await;
    assert_eq!(meta["childDialogId"], child.id.as_str());
    assert_eq!(child.kind, "scout");
    let work = h.messages_of(&child.id).await;
    assert_eq!(roles(&work), ["user", "assistant", "tool", "assistant"]);
    assert!(work[0].content.contains("Where is MARK-1 defined?"));
    assert!(work[2].content.contains("MARK-1 lives here"));
    let db = h.app.state::<DbState>().inner().clone();
    let listed = db
        .read(|c| store::list_dialogs(c, None, None, 50))
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1, "only the lead's chat is listed");

    // The scout was offered its kind's tools only, with its own prompt; the
    // lead got `task` and the section about it.
    let scout = requests_with(&seen, "MARK-1");
    assert_eq!(scout.len(), 2);
    let tools = tool_names(&scout[0]);
    for t in [
        "read",
        "search_code",
        "outline",
        "callers",
        "neighbours",
        "web_search",
    ] {
        assert!(tools.contains(&t.to_string()), "{t} in {tools:?}");
    }
    for t in ["write", "bash", "todo", "task", "ask_user", "submit_plan"] {
        assert!(!tools.contains(&t.to_string()), "{t} offered to a scout");
    }
    assert!(system_prompt(&scout[0]).starts_with("You are a scout"));
    let first = seen.lock().unwrap()[0].clone();
    assert!(tool_names(&first).contains(&"task".to_string()));
    assert!(system_prompt(&first).contains("## Subagents"));

    // The chat heard about it.
    let started = h.events_of("subagentStarted");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["dialogId"], h.dialog_id.as_str());
    assert_eq!(started[0]["toolCallId"], "t1");
    assert_eq!(started[0]["childDialogId"], child.id.as_str());
    let ended = h.events_of("subagentEnded");
    assert_eq!(ended[0]["status"], "done");
    assert!(!h.events_of("subagentProgress").is_empty());
    // No run of its own in the list of runs, and nothing left registered.
    let app = h.app.handle().clone();
    assert!(!app.state::<AgentState>().is_running(&h.dialog_id));
}

#[tokio::test]
async fn with_subagents_off_nothing_changes() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, seen) = model(vec![task("t1", "Where?"), Reply::Text("Done.")]).await;
    // Kinds on, master switch off.
    let mut config = scouts_on(json!({}));
    config.enabled = false;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let first = seen.lock().unwrap()[0].clone();
    assert!(!tool_names(&first).contains(&"task".to_string()));
    assert!(!system_prompt(&first).contains("## Subagents"));
    let result = h.task_result("t1").await;
    assert!(
        result
            .content
            .starts_with("Error: subagents are turned off"),
        "{}",
        result.content
    );
    assert!(h.events_of("subagentStarted").is_empty());
}

#[tokio::test]
async fn scouts_run_side_by_side_three_at_a_time() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let slow = |text: &'static str| vec![Reply::Slow(500, Box::new(Reply::Text(text)))];
    let (url, _) = model_routed(
        vec![
            ("AREA-A", slow("## Answer\nfrom A")),
            ("AREA-B", slow("## Answer\nfrom B")),
            ("AREA-C", slow("## Answer\nfrom C")),
            ("AREA-D", slow("## Answer\nfrom D")),
        ],
        vec![
            Reply::Calls(vec![
                task_call("t1", "Look at AREA-A"),
                task_call("t2", "Look at AREA-B"),
                task_call("t3", "Look at AREA-C"),
                task_call("t4", "Look at AREA-D"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Survey", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");

    // Results in the calls' order, each its own.
    let lead = h.messages().await;
    let results: Vec<&MessageRow> = lead.iter().filter(|m| m.role == "tool").collect();
    let ids: Vec<&str> = results
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    assert_eq!(ids, ["t1", "t2", "t3", "t4"]);
    for (m, area) in results.iter().zip(["A", "B", "C", "D"]) {
        assert!(
            m.content.ends_with(&format!("from {area}")),
            "{}",
            m.content
        );
    }
    assert_eq!(
        parallel::batches_of(&h.dialog_id),
        [vec!["t1", "t2", "t3", "t4"]]
    );

    // The first three asked their models at once; the fourth waited for a
    // free place.
    let mut asked = Vec::new();
    for call in ["t1", "t2", "t3", "t4"] {
        let child = h.child_of(call).await;
        let work = h.messages_of(&child.id).await;
        asked.push(work[1].created_at);
    }
    let first = asked[0].min(asked[1]).min(asked[2]);
    let last = asked[0].max(asked[1]).max(asked[2]);
    assert!(last - first < 300, "three at once: {asked:?}");
    assert!(asked[3] - first >= 400, "the fourth waited: {asked:?}");
}

#[tokio::test]
async fn a_scout_cant_write_or_run_commands() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-W",
            vec![
                Reply::Calls(vec![
                    ("w1", "write", json!({ "path": "x.txt", "content": "no\n" })),
                    ("b1", "bash", json!({ "command": "ls" })),
                ]),
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "MARK-W: look around"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(h.file("x.txt"), None);
    let child = h.child_of("t1").await;
    let work = h.messages_of(&child.id).await;
    let refusals: Vec<&str> = work
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.as_str())
        .collect();
    assert!(refusals[0].starts_with("Error: write isn't available to a scout subagent."));
    assert!(refusals[1].starts_with("Error: bash isn't available to a scout subagent."));
}

#[tokio::test]
async fn a_scout_out_of_budget_reports_what_it_found() {
    let h = harness(&[("a.txt", "x\n"), ("b.txt", "y\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-B",
            vec![
                Reply::Call {
                    id: "r1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Call {
                    id: "r2",
                    name: "read",
                    args: json!({ "path": "b.txt" }),
                },
                // Asked for its report: no tools.
                Reply::Text("## Answer\nPartly: a.txt and b.txt.\n\n## Open questions\nThe rest."),
            ],
        )],
        vec![task("t1", "MARK-B: read everything"), Reply::Text("Done.")],
    )
    .await;
    let config = scouts_on(json!({ "maxToolCalls": 2 }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: partial"),
        "{}",
        result.content
    );
    assert!(result.content.contains("ran out of budget"));
    assert!(result.content.contains("Partly: a.txt and b.txt."));
    let child = h.child_of("t1").await;
    let work = h.messages_of(&child.id).await;
    let note = work
        .iter()
        .find(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["harness"] == "subagentBudget")
        })
        .expect("asked for its report");
    assert!(note.content.contains("budget of 2 tool calls"));
}

#[tokio::test]
async fn a_report_out_of_format_gets_one_repair_turn() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![
            (
                "MARK-F1",
                vec![Reply::Text("It's in a.txt."), Reply::Text(REPORT)],
            ),
            (
                "MARK-F2",
                vec![
                    Reply::Text("It's in a.txt."),
                    Reply::Text("Still just prose."),
                ],
            ),
        ],
        vec![
            Reply::Calls(vec![
                task_call("t1", "MARK-F1: where?"),
                task_call("t2", "MARK-F2: where?"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let repaired = h.task_result("t1").await;
    assert!(repaired.content.contains("Status: done"));
    assert!(repaired.content.ends_with(REPORT));
    let raw = h.task_result("t2").await;
    assert!(
        raw.content.contains("Status: unformatted"),
        "{}",
        raw.content
    );
    assert!(raw.content.ends_with("Still just prose."));
    let child = h.child_of("t2").await;
    assert!(h.messages_of(&child.id).await.iter().any(|m| m
        .meta
        .as_ref()
        .is_some_and(|m| m["harness"] == "subagentFormat")));
}

fn profile(id: &str, url: &str) -> Value {
    json!({ "id": id, "label": format!("Other/{id}"), "model": id, "baseUrl": url, "stream": true })
}

#[tokio::test]
async fn a_failing_model_hands_over_to_the_next_one() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model_routed(
        vec![("MARK-M", vec![Reply::Text(REPORT)])],
        vec![task("t1", "MARK-M: where?"), Reply::Text("Done.")],
    )
    .await;
    // The bad model first, then the chat's.
    let config = scouts_on(json!({ "models": [profile("bad", &bad), { "chat": true }] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("t1").await;
    let meta = &result.meta.as_ref().unwrap()["subagent"];
    assert_eq!(meta["status"], "done", "{}", result.content);
    assert_eq!(meta["modelsTried"], json!(["Other/bad", "Test/m"]));
    assert_eq!(meta["model"], "Test/m");
}

#[tokio::test]
async fn every_model_failing_ends_the_scout_as_failed() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model(vec![task("t1", "Where?"), Reply::Text("Done.")]).await;
    let config = scouts_on(json!({ "models": [profile("bad", &bad)] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: failed"),
        "{}",
        result.content
    );
    assert!(result.content.contains("Every model it tried failed"));
    assert!(result.content.contains("Models tried: Other/bad."));
}

#[tokio::test]
async fn stop_on_the_lead_stops_its_scouts() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-S", vec![Reply::Hang(None)])],
        vec![task("t1", "MARK-S: where?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let started = h
        .wait_for("subagentStarted", |e| e["type"] == "subagentStarted")
        .await;
    let child = started["childDialogId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let app = h.app.handle().clone();
    assert!(app.state::<AgentState>().is_running(&child));
    assert!(agent_stop(app.clone(), app.state(), h.dialog_id.clone()));
    assert_eq!(h.run_end().await, "stopped");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: stopped"),
        "{}",
        result.content
    );
    assert!(!app.state::<AgentState>().is_running(&child));
    assert!(!app.state::<AgentState>().is_running(&h.dialog_id));
}

#[tokio::test]
async fn a_scouts_approval_shows_in_the_lead_chat() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-P",
            vec![
                Reply::Call {
                    id: "s1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "MARK-P: where?"), Reply::Text("Done.")],
    )
    .await;
    let permissions = Permissions {
        tool_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    h.start(h.with_subagents(&url, "Go", permissions, scouts_on(json!({}))))
        .await;
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let request = &asked["request"];
    assert_eq!(request["toolCallId"], "s1");
    assert_eq!(request["parentDialogId"], h.dialog_id.as_str());
    assert_eq!(request["parentCallId"], "t1");
    let child = request["dialogId"].as_str().unwrap().to_string();
    assert_ne!(child, h.dialog_id);

    let app = h.app.handle().clone();
    assert!(agent_resolve_approval(
        app.clone(),
        app.state(),
        child,
        "s1".into(),
        ApprovalDecision::Allow
    ));
    assert_eq!(h.run_end().await, "done");
    assert!(h.task_result("t1").await.content.contains("Status: done"));
}

#[tokio::test]
async fn plan_mode_can_send_scouts() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, seen) = model_routed(
        vec![("MARK-Q", vec![Reply::Text(REPORT)])],
        vec![
            task("t1", "MARK-Q: where?"),
            Reply::Text("Here is what I found."),
        ],
    )
    .await;
    let mut request = h.with_subagents(&url, "Plan it", quiet_permissions(), scouts_on(json!({})));
    request.mode = Some("plan".into());
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let first = seen.lock().unwrap()[0].clone();
    let tools = tool_names(&first);
    assert!(tools.contains(&"task".to_string()));
    assert!(tools.contains(&"submit_plan".to_string()));
    assert!(!tools.contains(&"write".to_string()));
    assert!(h.task_result("t1").await.content.contains("Status: done"));
}

/// S00: two runs at once each get their own replies, in their order, every
/// time (the scripted provider used to answer in arrival order, K05).
#[tokio::test]
async fn routed_scripts_stay_apart() {
    for _ in 0..20 {
        let h = harness(&[("a.txt", "a\n"), ("b.txt", "b\n")]).await;
        let script = |path: &'static str, answer: &'static str| {
            vec![
                Reply::Call {
                    id: "r1",
                    name: "read",
                    args: json!({ "path": path }),
                },
                Reply::Call {
                    id: "r2",
                    name: "outline",
                    args: json!({ "path": path }),
                },
                Reply::Text(answer),
            ]
        };
        let (url, _) = model_routed(
            vec![
                ("ROUTE-1", script("a.txt", "## Answer\none")),
                ("ROUTE-2", script("b.txt", "## Answer\ntwo")),
            ],
            vec![
                Reply::Calls(vec![
                    task_call("t1", "ROUTE-1 question"),
                    task_call("t2", "ROUTE-2 question"),
                ]),
                Reply::Text("Done."),
            ],
        )
        .await;
        h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
            .await;
        assert_eq!(h.run_end().await, "done");
        assert!(h.task_result("t1").await.content.ends_with("one"));
        assert!(h.task_result("t2").await.content.ends_with("two"));
        let child = h.child_of("t2").await;
        let reads: Vec<String> = h
            .messages_of(&child.id)
            .await
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|c| c.arguments.clone())
            .collect();
        assert!(reads.iter().all(|a| a.contains("b.txt")), "{reads:?}");
    }
}

/*
 * The change reviewer (change_review.rs)
 */

const REVIEW: &str = "# Review this change";

const FINDING_A: &str = "## Findings\n\n1. [high] a.txt:1 — The greeting is wrong\n   - Scenario: anyone reading a.txt sees \"helo\".\n   - Fix: spell it \"hello\".\n   - Confidence: confirmed";
const FINDING_B: &str = "## Findings\n\n1. [medium] b.txt:1 — The farewell is missing\n   - Scenario: b.txt is empty.\n   - Fix: add one.\n   - Confidence: plausible";
const NO_FINDINGS: &str = "## Findings\n\nNone.";

fn reviewer_on(extra: Value) -> subagent::SubagentConfig {
    let mut reviewer = json!({ "enabled": true });
    if let (Some(r), Some(e)) = (reviewer.as_object_mut(), extra.as_object()) {
        r.extend(e.clone());
    }
    serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "reviewer": reviewer },
        "reviewMinLines": 1,
    }))
    .unwrap()
}

/// With checks on and none set up: each run ends in an "unverified" record.
fn checks_on() -> Permissions {
    Permissions {
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    }
}

fn reviews(messages: &[MessageRow]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("review").cloned())
        .collect()
}

/// What each record in the chat is, in order: "review:<status>",
/// "note" (findings sent to the agent), "verification:<status>".
fn record_order(messages: &[MessageRow]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| {
            let meta = m.meta.as_ref()?;
            if let Some(r) = meta.get("review") {
                return Some(format!("review:{}", r["status"].as_str().unwrap_or("")));
            }
            if meta["harness"] == "review" {
                return Some("note".into());
            }
            meta.get("verification")
                .map(|v| format!("verification:{}", v["status"].as_str().unwrap_or("")))
        })
        .collect()
}

#[tokio::test]
async fn findings_go_to_the_agent_then_a_second_review_then_the_checks() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Text(FINDING_A), Reply::Text(NO_FINDINGS)],
        )],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            // The findings came back: fix and answer.
            write_call("w2", "a.txt", "hello\n"),
            Reply::Text("1: fixed, it says hello now."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");

    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        [
            "review:findings",
            "note",
            "review:clean",
            "verification:unverified"
        ]
    );
    let note = messages
        .iter()
        .find(|m| m.meta.as_ref().is_some_and(|m| m["harness"] == "review"))
        .unwrap();
    assert!(note
        .content
        .contains("1. [high] a.txt:1 — The greeting is wrong"));
    assert!(note.content.contains("not this conversation"));

    let rounds = reviews(&messages);
    assert_eq!(rounds[0]["findings"][0]["severity"], "high");
    assert_eq!(rounds[0]["findings"][0]["line"], 1);
    assert_eq!(rounds[0]["round"], 1);
    assert_eq!(rounds[1]["round"], 2);
    assert_eq!(rounds[0]["files"], json!(["a.txt"]));
    // Its chat is kept (and the record says which), as the transcript.
    let child = rounds[0]["childDialogId"].as_str().unwrap().to_string();
    let work = h.messages_of(&child).await;
    assert!(work[0].content.starts_with(REVIEW));

    // The reviewer saw the request and the diff, never the lead's history.
    let briefs: Vec<String> = requests_with(&seen, REVIEW)
        .iter()
        .map(|b| {
            b["messages"][1]["content"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(briefs.len(), 2);
    assert!(briefs[0].contains("## The user's request\n\nGreet"));
    assert!(briefs[0].contains("+helo"));
    assert!(
        !briefs[0].contains("Done."),
        "nothing from the lead's conversation"
    );
    assert!(!briefs[0].contains("earlier review"));
    // Round 2: the earlier findings and the lead's answer.
    assert!(briefs[1].contains("## The earlier review"));
    assert!(briefs[1].contains("The greeting is wrong"));
    assert!(briefs[1].contains("1: fixed, it says hello now."));
    assert!(briefs[1].contains("+hello"));
    let tools = tool_names(&requests_with(&seen, REVIEW)[0]);
    assert!(tools.contains(&"read".to_string()) && tools.contains(&"callers".to_string()));
    for t in ["write", "bash", "task", "web_search"] {
        assert!(
            !tools.contains(&t.to_string()),
            "{t} offered to the reviewer"
        );
    }
    assert!(system_prompt(&requests_with(&seen, REVIEW)[0]).starts_with("You are a code reviewer"));
    // The lead isn't offered `task` for the reviewer: it's the IDE's.
    let first = seen.lock().unwrap()[0].clone();
    assert!(!tool_names(&first).contains(&"task".to_string()));
}

#[tokio::test]
async fn a_third_review_in_a_row_never_happens() {
    let h = harness(&[("a.txt", "old\n"), ("b.txt", "")]).await;
    let (url, _) = model_routed(
        vec![(REVIEW, vec![Reply::Text(FINDING_A), Reply::Text(FINDING_B)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            Reply::Text("1: fixed."),
            write_call("w3", "b.txt", "bye\n"),
            Reply::Text("1: added."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    assert_eq!(
        record_order(&h.messages().await),
        [
            "review:findings",
            "note",
            "review:findings",
            "note",
            "verification:unverified"
        ]
    );
}

#[tokio::test]
async fn a_repeated_finding_without_new_evidence_is_dropped() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(REVIEW, vec![Reply::Text(FINDING_A), Reply::Text(FINDING_A)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "helo there\n"),
            Reply::Text("1: not a bug, because helo is the brand name."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        [
            "review:findings",
            "note",
            "review:clean",
            "verification:unverified"
        ]
    );
    let second = &reviews(&messages)[1];
    assert_eq!(second["dropped"].as_array().unwrap().len(), 1);
    assert_eq!(second["dropped"][0]["title"], "The greeting is wrong");
}

#[tokio::test]
async fn a_failing_reviewer_is_skipped_and_the_run_goes_on() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model(vec![
        write_call("w1", "a.txt", "new\n"),
        Reply::Text("Done."),
    ])
    .await;
    let config = reviewer_on(json!({ "models": [profile("bad", &bad)] }));
    h.start(h.with_subagents(&url, "Change it", checks_on(), config))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        ["review:skipped", "verification:unverified"]
    );
    let r = &reviews(&messages)[0];
    assert!(
        r["reason"].as_str().unwrap().contains("model failed"),
        "{r}"
    );
    assert!(
        r.get("findings").is_none(),
        "skipped, never \"no findings\""
    );
}

#[tokio::test]
async fn an_unreadable_review_is_skipped_never_clean() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Text("Looks fine to me."),
                Reply::Text("Really, it's fine."),
            ],
        )],
        vec![write_call("w1", "a.txt", "new\n"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let messages = h.messages().await;
    let r = &reviews(&messages)[0];
    assert_eq!(r["status"], "skipped");
    assert!(r["reason"]
        .as_str()
        .unwrap()
        .contains("didn't follow the format"));
}

#[tokio::test]
async fn small_changes_and_runs_with_the_reviewer_off_get_no_review() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model(vec![
        write_call("w1", "a.txt", "new\n"),
        Reply::Text("Done."),
    ])
    .await;
    // The default threshold is 10 changed lines; this is 2.
    let mut config = reviewer_on(json!({}));
    config.review_min_lines = None;
    h.start(h.with_subagents(&url, "Change it", checks_on(), config))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    assert!(reviews(&h.messages().await).is_empty());

    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model(vec![
        write_call("w1", "a.txt", "new\n"),
        Reply::Text("Done."),
    ])
    .await;
    let mut off = reviewer_on(json!({}));
    off.enabled = false;
    h.start(h.with_subagents(&url, "Change it", checks_on(), off))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    assert!(reviews(&h.messages().await).is_empty());
}

#[tokio::test]
async fn the_review_goes_to_the_reviewers_model_and_its_fallback() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (strong, strong_seen) = model(vec![Reply::Text(NO_FINDINGS)]).await;
    let (url, seen) = model(vec![
        write_call("w1", "a.txt", "new\n"),
        Reply::Text("Done."),
    ])
    .await;
    let config =
        reviewer_on(json!({ "models": [profile("bad", &bad), profile("strong", &strong)] }));
    h.start(h.with_subagents(&url, "Change it", checks_on(), config))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let r = &reviews(&h.messages().await)[0];
    assert_eq!(r["status"], "clean");
    assert_eq!(r["model"], "Other/strong");
    assert_eq!(r["modelsTried"], json!(["Other/bad", "Other/strong"]));
    assert_eq!(requests_with(&strong_seen, REVIEW).len(), 1);
    assert!(
        requests_with(&seen, REVIEW).is_empty(),
        "not the chat's model"
    );
}

#[tokio::test]
async fn no_run_gets_more_than_three_reviews() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    // The checks always fail: every fix round goes back to the agent.
    let h = harness(&[
        ("a.txt", "old\n"),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(1)\"" } }"#,
        ),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Text(FINDING_A), Reply::Text(NO_FINDINGS)],
        )],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            Reply::Text("1: fixed."),
            write_call("w3", "a.txt", "hello 3\n"),
            Reply::Text("Done."),
            write_call("w4", "a.txt", "hello 4\n"),
            Reply::Text("Done."),
            write_call("w5", "a.txt", "hello 5\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verify-gave-up");
    assert_eq!(
        record_order(&h.messages().await),
        [
            "review:findings",
            "note",
            "review:clean",
            "verification:failed",
            // The checks ran in between: a review again, the third and last.
            "review:clean",
            "verification:failed",
            "verification:gave-up",
        ]
    );
}

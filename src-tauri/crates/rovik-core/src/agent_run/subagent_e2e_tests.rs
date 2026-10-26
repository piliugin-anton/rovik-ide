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
        let db = self.core.db.clone();
        db.read(move |c| store::child_for_call(c, &id, &call, None))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no subagent chat for {call_id}"))
    }

    async fn messages_of(&self, dialog_id: &str) -> Vec<MessageRow> {
        let id = dialog_id.to_string();
        let db = self.core.db.clone();
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
    let db = h.core.db.clone();
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
    let core = h.core.clone();
    assert!(!core.agents.is_running(&h.dialog_id));
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

/// P2.1: refused calls count for the budget: a scout that keeps calling a
/// tool it doesn't have runs out of it, in one reply.
#[tokio::test]
async fn refused_calls_use_up_a_subagents_budget() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let writes: Vec<(&'static str, &'static str, Value)> =
        ["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9"]
            .into_iter()
            .map(|id| (id, "write", json!({ "path": "a.txt", "content": "no" })))
            .collect();
    let (url, _) = model_routed(
        vec![(
            "MARK-RB",
            vec![
                Reply::Calls(writes),
                Reply::Text("## Answer\nI couldn't write.\n\n## Open questions\nNone"),
            ],
        )],
        vec![task("t1", "MARK-RB: write a.txt"), Reply::Text("Done.")],
    )
    .await;
    let config = scouts_on(json!({ "maxToolCalls": 3 }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: partial"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("ran out of budget"),
        "{}",
        result.content
    );
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
    let core = h.core.clone();
    assert!(core.agents.is_running(&child));
    assert!(agent_stop(&core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "stopped");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: stopped"),
        "{}",
        result.content
    );
    assert!(!core.agents.is_running(&child));
    assert!(!core.agents.is_running(&h.dialog_id));
}

/// 6.8: a move leaves the lead's scouts alone, and pauses the lead once
/// their reports are in.
#[tokio::test]
async fn a_move_lets_a_running_scout_finish_first() {
    let h = harness(&[("a.txt", "MARK-M lives here\n")]).await;
    let read = Reply::Call {
        id: "s1",
        name: "read",
        args: json!({ "path": "a.txt" }),
    };
    let (url, seen) = model_routed(
        vec![(
            "MARK-M",
            vec![Reply::Slow(400, Box::new(read)), Reply::Text(REPORT)],
        )],
        vec![task("t1", "Where is MARK-M defined?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), scouts_on(json!({}))))
        .await;
    // The scout's first request is out: the move is asked meanwhile.
    requests_seen(&seen, 2).await;
    assert_eq!(requests_with(&seen, "MARK-M").len(), 1);
    let pausing = h.pause_for_move("build-server").await;
    assert!(within("the pause", pausing).await.unwrap());
    assert_eq!(h.run_end().await, "paused-move");

    // The scout went on to its report, which the lead has; the lead asked
    // nothing more.
    assert_eq!(requests_with(&seen, "MARK-M").len(), 2);
    let result = h.task_result("t1").await;
    assert_eq!(result.meta.as_ref().unwrap()["subagent"]["status"], "done");
    assert!(result.content.ends_with(REPORT));
    assert_eq!(seen.lock().unwrap().len(), 3);
    let lead = h.messages().await;
    assert_eq!(roles(&lead), ["user", "assistant", "tool", "assistant"]);
    assert_eq!(lead[3].meta.as_ref().unwrap()["paused"]["kind"], "move");
    let child = h.child_of("t1").await;
    assert!(!h.core.agents.is_running(&child.id));
    assert!(!h.core.agents.is_running(&h.dialog_id));
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

    let core = h.core.clone();
    assert!(agent_resolve_approval(&core, child, "s1".into(), ApprovalDecision::Allow).await);
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

/// Plan mode changes no files: the kinds that write aren't offered, and a
/// call for one (or for a parallel group) is refused.
#[tokio::test]
async fn plan_mode_never_starts_a_writing_subagent() {
    let h = harness(&[("a.txt", "x\n"), (".rovik/plans/greet.md", STEP_PLAN)]).await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("t1", "task", json!({ "kind": "testWriter" })),
            ("t2", "task", json!({ "kind": "worker", "step": 1 })),
            ("t3", "task", json!({ "kind": "workers", "group": "pages" })),
        ]),
        Reply::Text("Planning only."),
    ])
    .await;
    let config: subagent::SubagentConfig = serde_json::from_value(json!({
        "enabled": true,
        "kinds": {
            "scout": { "enabled": true },
            "worker": { "enabled": true },
            "testWriter": { "enabled": true },
        },
        "workerMinSteps": 2,
        "parallelWorkers": true,
    }))
    .unwrap();
    let mut request = h.with_subagents(
        &url,
        "Implement the plan in `.rovik/plans/greet.md`.",
        quiet_permissions(),
        config,
    );
    request.mode = Some("plan".into());
    h.start(request).await;
    h.run_end().await;
    let first = seen.lock().unwrap()[0].clone();
    let task = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "task")
        .expect("scouts are still offered")
        .to_string();
    assert!(task.contains("scout"), "{task}");
    assert!(
        !task.contains("testWriter") && !task.contains("\"worker\""),
        "{task}"
    );
    for call in ["t1", "t2", "t3"] {
        let r = h.task_result(call).await.content;
        assert!(
            r.starts_with("Error: Plan mode changes no files"),
            "{call}: {r}"
        );
    }
    assert_eq!(h.file("a.txt").as_deref(), Some("x\n"));
    // No subagent chat was started for any of them.
    let db = h.core.db.clone();
    for call in ["t1", "t2", "t3"] {
        let (id, call) = (h.dialog_id.clone(), call.to_string());
        let child = db
            .read(move |c| store::child_for_call(c, &id, &call, None))
            .await
            .unwrap();
        assert!(child.is_none(), "{child:?}");
    }
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

/// The agent's answer to the change review's finding 1 (`answer_review`):
/// findings block in every mode, and free-text replies aren't answers.
fn answered(id: &'static str, status: &str, note: &str) -> Reply {
    Reply::Call {
        id,
        name: "answer_review",
        args: json!({ "responses": [{ "finding": 1, "status": status, "note": note }] }),
    }
}

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
    let resolved =
        "## Earlier findings\n\n- Earlier 1: resolved — a.txt says hello\n\n## Findings\n\nNone.";
    let (url, seen) = model_routed(
        vec![(REVIEW, vec![Reply::Text(FINDING_A), Reply::Text(resolved)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            // The findings came back: fix and answer.
            write_call("w2", "a.txt", "hello\n"),
            answered("a1", "fixed", "it says hello now."),
            Reply::Text("Fixed it."),
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
    // Round 2, a follow-up: the earlier findings and the lead's answer.
    assert!(briefs[1].contains("## The earlier review"));
    assert!(briefs[1].contains("The greeting is wrong"));
    assert!(
        briefs[1].contains("The agent's answer: fixed — it says hello now."),
        "{}",
        briefs[1]
    );
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
async fn every_finding_is_followed_up_before_the_checks() {
    let h = harness(&[("a.txt", "old\n"), ("b.txt", "")]).await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Text(FINDING_A),
                // A medium, plausible finding counts too: it's held.
                Reply::Text(FINDING_B),
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            answered("a1", "fixed", "spelled hello"),
            Reply::Text("Fixed."),
            write_call("w3", "b.txt", "bye\n"),
            answered("a2", "fixed", "added a farewell"),
            Reply::Text("Added."),
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
            "review:clean",
            "verification:unverified"
        ]
    );
}

/// Agent mode: a low finding the agent never resolves, still open after
/// the run's last review, keeps the change from being accepted: the run
/// ends `review-open`, before the checks.
#[tokio::test]
async fn a_low_finding_left_open_blocks_the_change_in_agent_mode() {
    let low = "## Findings\n\n1. [low] a.txt:1 — The greeting has a trailing space\n   - Scenario: a.txt ends with a space.\n   - Fix: trim it.\n   - Confidence: plausible";
    let not_resolved = "## Earlier findings\n\n- Earlier 1: not resolved — the space is still there\n\n## Findings\n\nNone.";
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Text(low),
                Reply::Text(not_resolved),
                Reply::Text(not_resolved),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "hello \n"),
            Reply::Text("Done."),
            answered("a1", "not-a-problem", "the space is harmless"),
            Reply::Text("Answered."),
            answered("a2", "not-a-problem", "still harmless"),
            Reply::Text("Answered again."),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "review-open");
    let messages = h.messages().await;
    let order = record_order(&messages);
    assert!(
        !order.iter().any(|r| r.starts_with("verification")),
        "never checked: {order:?}"
    );
    let last = reviews(&messages).pop().unwrap();
    assert_eq!(last["blocked"], true, "{last}");
    assert_eq!(last["open"], 1, "{last}");
    assert_eq!(last["findings"][0]["severity"], "low", "{last}");
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
            answered("a1", "not-a-problem", "helo is the brand name"),
            Reply::Text("Answered."),
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
            answered("a1", "fixed", "spelled hello"),
            Reply::Text("Fixed."),
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
            // While the checks fail, the review waits for green (R08).
            "verification:failed",
            "verification:gave-up",
        ]
    );
}

/*
 * S03b: a subagent's writes count as the lead's. No writing kind exists
 * yet, so a scout is given `write` (a test-only setting) to stand in.
 */

fn writing_scout() -> subagent::SubagentConfig {
    let mut config = scouts_on(json!({}));
    config.kinds.get_mut("scout").unwrap().extra_tools = vec!["write".into()];
    config
}

#[tokio::test]
async fn a_lead_that_only_delegates_writing_still_gets_its_changes_checked() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-WR",
            vec![
                write_call("s1", "a.txt", "child\n"),
                Reply::Text("## Answer\nChanged a.txt."),
            ],
        )],
        vec![task("t1", "MARK-WR: change a.txt"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", checks_on(), writing_scout()))
        .await;
    // Without S03b the lead changed nothing itself: "done", no checks.
    assert_eq!(h.run_end().await, "unverified");
    assert_eq!(h.file("a.txt").as_deref(), Some("child\n"));
    let messages = h.messages().await;
    let record = messages
        .iter()
        .find_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .expect("a verification record");
    assert_eq!(record["changedFiles"], json!(["a.txt"]));
}

#[tokio::test]
async fn the_lead_can_write_a_file_its_subagent_changed() {
    let h = harness(&[("a.txt", "old\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-WR2",
            vec![
                write_call("s1", "a.txt", "child\n"),
                Reply::Text("## Answer\nChanged a.txt."),
            ],
        )],
        vec![
            // The lead reads the file, its subagent changes it, then the
            // lead writes it whole: it counts as having seen the change.
            Reply::Call {
                id: "r1",
                name: "read",
                args: json!({ "path": "a.txt" }),
            },
            task("t1", "MARK-WR2: change a.txt"),
            write_call("w1", "a.txt", "lead\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Change it", quiet_permissions(), writing_scout()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h
        .messages()
        .await
        .into_iter()
        .find(|m| m.tool_call_id.as_deref() == Some("w1"))
        .unwrap();
    assert!(!result.content.starts_with("Error"), "{}", result.content);
    assert_eq!(h.file("a.txt").as_deref(), Some("lead\n"));
}

/// S09: ROVIK_API_LOG says who each request is for. (Other tests running
/// meanwhile write here too; their lines are told apart by chat.)
#[tokio::test]
async fn the_api_log_tags_each_request_with_its_role_and_kind() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let log = h._dir.0.join("api-log.jsonl");
    *crate::api_log::TEST_TARGET.lock().unwrap() = Some(log.clone());
    let (url, _) = model_routed(
        vec![("MARK-LOG", vec![Reply::Text(REPORT)])],
        vec![task("t1", "MARK-LOG: where?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let outcome = h.run_end().await;
    *crate::api_log::TEST_TARGET.lock().unwrap() = None;
    assert_eq!(outcome, "done");
    let child = h.child_of("t1").await;
    let tags: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        // Those the model answered: the lead's first went to `/responses`
        // before, which the test model doesn't have.
        .filter(|l| {
            l["dir"] == "request"
                && l["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/chat/completions"))
        })
        .map(|l| l["tags"].clone())
        .filter(|t| t["dialogId"] == h.dialog_id.as_str() || t["dialogId"] == child.id.as_str())
        .collect();
    assert_eq!(
        tags.iter().filter(|t| t["role"] == "lead").count(),
        2,
        "{tags:?}"
    );
    let sub: Vec<&Value> = tags.iter().filter(|t| t["role"] == "sub").collect();
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0]["kind"], "scout");
    assert_eq!(sub[0]["parentDialogId"], h.dialog_id.as_str());
}

/// S10: a rate limit too long to wait for goes to the next model at once.
#[tokio::test]
async fn a_long_rate_limit_goes_to_the_fallback_without_waiting() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (limited, limited_seen) = model(vec![Reply::RateLimited(3_600)]).await;
    let (url, _) = model_routed(
        vec![("MARK-RL", vec![Reply::Text(REPORT)])],
        vec![task("t1", "MARK-RL: where?"), Reply::Text("Done.")],
    )
    .await;
    let config = scouts_on(json!({ "models": [profile("limited", &limited), { "chat": true }] }));
    let started = std::time::Instant::now();
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "no wait for the limit"
    );
    assert_eq!(
        limited_seen.lock().unwrap().len(),
        1,
        "asked once, not retried"
    );
    let meta = &h.task_result("t1").await.meta.unwrap()["subagent"];
    assert_eq!(meta["status"], "done");
    assert_eq!(meta["modelsTried"], json!(["Other/limited", "Test/m"]));
}

/// S10: a request that doesn't fit the window is sent again, trimmed, to
/// the same model, once.
#[tokio::test]
async fn a_reply_too_long_for_the_window_is_trimmed_and_sent_again_once() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let overflow = r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#;
    let (url, seen) = model_routed(
        vec![
            (
                "MARK-CL1",
                vec![Reply::Status(400, overflow), Reply::Text(REPORT)],
            ),
            ("MARK-CL2", vec![Reply::Status(400, overflow)]),
        ],
        vec![
            Reply::Calls(vec![
                task_call("t1", "MARK-CL1: where?"),
                task_call("t2", "MARK-CL2: where?"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let once = &h.task_result("t1").await.meta.unwrap()["subagent"];
    assert_eq!(once["status"], "done");
    assert_eq!(once["modelsTried"], json!(["Test/m"]), "the same model");
    assert_eq!(requests_with(&seen, "MARK-CL1").len(), 2);
    // Still too long after trimming: no endless retries.
    let twice = &h.task_result("t2").await.meta.unwrap()["subagent"];
    assert_eq!(twice["status"], "failed");
    assert_eq!(requests_with(&seen, "MARK-CL2").len(), 2);
}

/// S12: a model that fails two subagents in a row is skipped for the run.
#[tokio::test]
async fn a_model_that_keeps_failing_is_skipped_for_the_run() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (bad, bad_seen) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model_routed(
        vec![
            ("MARK-C1", vec![Reply::Text(REPORT)]),
            ("MARK-C2", vec![Reply::Text(REPORT)]),
            ("MARK-C3", vec![Reply::Text(REPORT)]),
        ],
        vec![
            task("t1", "MARK-C1: where?"),
            task("t2", "MARK-C2: where?"),
            task("t3", "MARK-C3: where?"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let config = scouts_on(json!({ "models": [profile("bad", &bad), { "chat": true }] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let tried = |call: &'static str| {
        let h = &h;
        async move { h.task_result(call).await.meta.unwrap()["subagent"]["modelsTried"].clone() }
    };
    assert_eq!(tried("t1").await, json!(["Other/bad", "Test/m"]));
    assert_eq!(tried("t2").await, json!(["Other/bad", "Test/m"]));
    assert_eq!(
        tried("t3").await,
        json!(["Test/m"]),
        "straight to the fallback"
    );
    assert!(!requests_with(&bad_seen, "MARK-C2").is_empty());
    assert!(requests_with(&bad_seen, "MARK-C3").is_empty());
}

/// S12: with no model left, the lead is told to do the work itself.
#[tokio::test]
async fn with_every_model_failing_the_lead_is_told_to_work_alone() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (bad, bad_seen) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model(vec![
        task("t1", "First?"),
        task("t2", "Second?"),
        task("t3", "Third?"),
        Reply::Text("Done."),
    ])
    .await;
    let config = scouts_on(json!({ "models": [profile("bad", &bad)] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.task_result("t2").await.content.contains("Status: failed"));
    let third = h.task_result("t3").await.content;
    assert!(
        third.starts_with("Error: subagents are unavailable for the rest of this run"),
        "{third}"
    );
    assert!(third.contains("Do the work yourself."));
    assert!(!requests_with(&bad_seen, "Second?").is_empty());
    assert!(requests_with(&bad_seen, "Third?").is_empty());
}

/// S12: after a rate limit on a model, its subagents run one at a time.
#[tokio::test]
async fn a_rate_limited_model_runs_one_subagent_at_a_time() {
    let h = harness(&[("a.txt", "x\n")]).await;
    // Rate limited once, then slow answers.
    let (limited, _) = model(vec![
        Reply::RateLimited(3_600),
        Reply::Slow(400, Box::new(Reply::Text(REPORT))),
    ])
    .await;
    let (url, _) = model_routed(
        vec![("MARK-T0", vec![Reply::Text(REPORT)])],
        vec![
            task("t0", "MARK-T0: where?"),
            Reply::Calls(vec![
                task_call("t1", "MARK-TA: where?"),
                task_call("t2", "MARK-TB: where?"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    // t0's scout gets the 429 and falls back to the chat's model; t1's
    // and t2's ask the limited model again, but one at a time.
    let config = scouts_on(json!({ "models": [profile("limited", &limited), { "chat": true }] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let a = h.child_of("t1").await;
    let b = h.child_of("t2").await;
    let first_ask = |m: Vec<MessageRow>| m[1].created_at;
    let (a_at, b_at) = (
        first_ask(h.messages_of(&a.id).await),
        first_ask(h.messages_of(&b.id).await),
    );
    assert!(b_at.abs_diff(a_at) >= 300, "one at a time: {a_at} {b_at}");
}

/// S15: a chat's changes can't land or be discarded while its subagents
/// work (they run inside its run); Stop ends them with it.
#[tokio::test]
async fn landing_and_discarding_wait_for_running_subagents() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-LD", vec![Reply::Hang(None)])],
        vec![task("t1", "MARK-LD: where?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.wait_for("subagentStarted", |e| e["type"] == "subagentStarted")
        .await;
    let core = h.core.clone();
    let refused = crate::overlay::chats::overlay_discard(&core.agents, h.dialog_id.clone())
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        "this chat's run is still going, with 1 subagent at work: stop it first"
    );
    let refused = crate::overlay::chats::overlay_land(&core.agents, h.dialog_id.clone(), false)
        .await
        .err()
        .unwrap();
    assert!(refused.contains("1 subagent at work"));
    // Stop: the scout ends with the run, and nothing holds the chat.
    assert!(agent_stop(&core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "stopped");
    assert_eq!(core.agents.running_children(&h.dialog_id), 0);
    assert!(
        crate::overlay::chats::overlay_discard(&core.agents, h.dialog_id.clone())
            .await
            .is_ok()
    );
}

/// S16: Stop on one subagent stops only it; the others and the lead go on.
#[tokio::test]
async fn stopping_one_of_three_scouts_stops_only_it() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let slow = |text: &'static str| vec![Reply::Slow(600, Box::new(Reply::Text(text)))];
    let (url, _) = model_routed(
        vec![
            ("MARK-SA", slow("## Answer\nfrom A")),
            ("MARK-SB", vec![Reply::Hang(None)]),
            ("MARK-SC", slow("## Answer\nfrom C")),
        ],
        vec![
            Reply::Calls(vec![
                task_call("t1", "MARK-SA: where?"),
                task_call("t2", "MARK-SB: where?"),
                task_call("t3", "MARK-SC: where?"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let started = h
        .wait_for("subagentStarted t2", |e| {
            e["type"] == "subagentStarted" && e["toolCallId"] == "t2"
        })
        .await;
    let b = started["childDialogId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let core = h.core.clone();
    assert!(agent_stop(&core, b).await);
    assert_eq!(h.run_end().await, "done");
    let status = |c: &str, m: &MessageRow| {
        m.meta.as_ref().unwrap()["subagent"]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("{c}"))
            .to_string()
    };
    assert_eq!(status("t1", &h.task_result("t1").await), "done");
    assert_eq!(status("t2", &h.task_result("t2").await), "stopped");
    assert_eq!(status("t3", &h.task_result("t3").await), "done");
    // The lead's turn went on with all three results.
    assert_eq!(h.messages().await.last().unwrap().content, "Done.");
}

/// S14: a `task` call left unanswered when the app closed resumes its
/// subagent's chat, with what it had done, instead of starting over.
#[tokio::test]
async fn a_restart_during_a_scout_resumes_its_chat() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let db = h.core.db.clone();
    // As the app left it: the lead's task call, no result; the scout's
    // chat with its brief, one read done and one call that never ran.
    let lead = h.dialog_id.clone();
    let mut user = crate::agent_run::run::new_message("user", "Find it");
    user.meta = Some(json!({ "mode": "agent" }));
    let mut call = crate::agent_run::run::new_message("assistant", "");
    call.tool_calls = Some(vec![store::ToolCall {
        id: "t1".into(),
        name: "task".into(),
        arguments: json!({ "kind": "scout", "goal": "MARK-RS: where?" }).to_string(),
        extra_content: None,
    }]);
    let child = uuid::Uuid::new_v4().to_string();
    let brief =
        crate::agent_run::run::new_message("user", "# Task from the lead agent\n\nMARK-RS: where?");
    let mut read = crate::agent_run::run::new_message("assistant", "");
    read.tool_calls = Some(vec![store::ToolCall {
        id: "s1".into(),
        name: "read".into(),
        arguments: json!({ "path": "a.txt" }).to_string(),
        extra_content: None,
    }]);
    let mut result = crate::agent_run::run::new_message("tool", "EARLIER-READ-RESULT");
    result.tool_call_id = Some("s1".into());
    let mut dangling = crate::agent_run::run::new_message("assistant", "");
    dangling.tool_calls = Some(vec![store::ToolCall {
        id: "s2".into(),
        name: "outline".into(),
        arguments: json!({ "path": "a.txt" }).to_string(),
        extra_content: None,
    }]);
    let child_id = child.clone();
    db.write(move |c| {
        store::append_messages(c, &lead, &[user, call])?;
        store::create_child_dialog(
            c,
            &store::NewChildDialog {
                id: child_id.clone(),
                parent_id: lead.clone(),
                parent_call_id: "t1".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: store::now_ms(),
            },
        )?;
        store::append_messages(c, &child_id, &[brief, read, result, dangling])?;
        Ok(())
    })
    .await
    .unwrap();

    let (url, seen) = model_routed(
        vec![("MARK-RS", vec![Reply::Text(REPORT)])],
        vec![Reply::Text("Done.")],
    )
    .await;
    let mut request = h.with_subagents(&url, "unused", quiet_permissions(), scouts_on(json!({})));
    request.start = StartKind::Resume;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");

    let result = h.task_result("t1").await;
    let meta = &result.meta.as_ref().unwrap()["subagent"];
    assert_eq!(meta["status"], "done");
    assert_eq!(
        meta["childDialogId"],
        child.as_str(),
        "the same chat, resumed"
    );
    // P2.7: the calls it made before the restart count for its budget.
    assert_eq!(meta["toolCalls"], 2, "{meta}");
    // It picked up with its earlier work in its history.
    let asked = requests_with(&seen, "MARK-RS");
    assert_eq!(asked.len(), 1);
    assert!(asked[0].to_string().contains("EARLIER-READ-RESULT"));
    // Its call that never ran got a result first.
    let work = h.messages_of(&child).await;
    assert!(work
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("s2") && m.content.contains("interrupted")));
}

/// P1.1: a lead whose run fails while its subagents are queued leaves none
/// of them registered: the chat isn't "running" for good.
#[tokio::test]
async fn a_lead_that_fails_mid_batch_leaves_no_subagent_running() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-Q",
            (0..5)
                .map(|_| Reply::Slow(200, Box::new(Reply::Text(REPORT))))
                .collect(),
        )],
        vec![Reply::Calls(vec![
            task_call("q11-1", "MARK-Q: one"),
            task_call("q11-2", "MARK-Q: two"),
            task_call("q11-3", "MARK-Q: three"),
            task_call("q11-4", "MARK-Q: four"),
            task_call("q11-5", "MARK-Q: five"),
        ])],
    )
    .await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .insert("q11-1".into());
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let end = h.run_end().await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .remove("q11-1");
    assert_eq!(end, "error");
    let state = &h.core.agents;
    assert_eq!(state.running_children(&h.dialog_id), 0);
    assert!(!state.is_running(&h.dialog_id));
}

/// A follow-up asked just before the app closed is asked once: resumed,
/// the subagent goes on from the question already in its chat.
#[tokio::test]
async fn a_follow_up_resumed_after_a_restart_is_asked_once() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let db = h.core.db.clone();
    let lead = h.dialog_id.clone();
    let mut user = crate::agent_run::run::new_message("user", "Find it");
    user.meta = Some(json!({ "mode": "agent" }));
    let call = |id: &str, args: Value| {
        let mut m = crate::agent_run::run::new_message("assistant", "");
        m.tool_calls = Some(vec![store::ToolCall {
            id: id.into(),
            name: "task".into(),
            arguments: args.to_string(),
            extra_content: None,
        }]);
        m
    };
    let first = call("t1", json!({ "kind": "scout", "goal": "MARK-RF: where?" }));
    let mut answered = crate::agent_run::run::new_message("tool", "FIRST-ANSWER");
    answered.tool_call_id = Some("t1".into());
    let again = call(
        "t2",
        json!({ "continue": "t1", "goal": "And who calls it?" }),
    );
    let child = uuid::Uuid::new_v4().to_string();
    let brief =
        crate::agent_run::run::new_message("user", "# Task from the lead agent\n\nMARK-RF: where?");
    let report = crate::agent_run::run::new_message("assistant", "## Answer\nFIRST-ANSWER");
    let mut asked = crate::agent_run::run::new_message(
        "user",
        "# Follow-up from the lead agent\n\nAnd who calls it?\n\nAnswer it in the same report format.",
    );
    asked.meta = Some(json!({
        "subagentBrief": { "kind": "scout", "parentCallId": "t2", "followUp": true }
    }));
    let child_id = child.clone();
    db.write(move |c| {
        store::append_messages(c, &lead, &[user, first, answered, again])?;
        store::create_child_dialog(
            c,
            &store::NewChildDialog {
                id: child_id.clone(),
                parent_id: lead.clone(),
                parent_call_id: "t1".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: store::now_ms(),
            },
        )?;
        store::append_messages(c, &child_id, &[brief, report, asked])?;
        Ok(())
    })
    .await
    .unwrap();

    let (url, _) = model_routed(
        vec![("MARK-RF", vec![Reply::Text("## Answer\nSECOND-ANSWER")])],
        vec![Reply::Text("Done.")],
    )
    .await;
    let mut request = h.with_subagents(&url, "unused", quiet_permissions(), scouts_on(json!({})));
    request.start = StartKind::Resume;
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.task_result("t2").await.content.ends_with("SECOND-ANSWER"));
    let work = h.messages_of(&child).await;
    let questions = work
        .iter()
        .filter(|m| m.content.starts_with("# Follow-up from the lead agent"))
        .count();
    assert_eq!(questions, 1, "asked once");
}

/// S17: each subagent's reported tokens are kept with its result, and past
/// the run's limit no new subagent starts.
#[tokio::test]
async fn past_the_spend_limit_no_new_subagent_starts() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, seen) = model_routed(
        vec![
            (
                "MARK-U1",
                vec![Reply::Usage(600, 100, Box::new(Reply::Text(REPORT)))],
            ),
            ("MARK-U2", vec![Reply::Text(REPORT)]),
        ],
        vec![
            task("t1", "MARK-U1: where?"),
            task("t2", "MARK-U2: where?"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut config = scouts_on(json!({}));
    config.max_run_tokens = Some(500);
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let first = h.task_result("t1").await;
    assert_eq!(
        first.meta.unwrap()["subagent"]["usage"],
        json!({ "inputTokens": 600, "outputTokens": 100 })
    );
    let second = h.task_result("t2").await.content;
    assert!(
        second
            .starts_with("Error: this run's subagents have used 700 tokens, past the limit of 500"),
        "{second}"
    );
    assert!(requests_with(&seen, "MARK-U2").is_empty());
}

/// S18: the reviewer's repository map is around the change and smaller
/// than the lead's; a scout gets the lead's.
#[tokio::test]
async fn a_reviewer_gets_a_smaller_map_around_the_change() {
    let sources: Vec<(String, String)> = (0..60)
        .map(|i| {
            (
                format!("src/mod{i}.ts"),
                format!(
                    "export function handler{i}(input: string): number {{ return input.length + {i}; }}\nexport class Service{i} {{ run(x: number): number {{ return x * {i}; }} }}\n"
                ),
            )
        })
        .collect();
    let mut files: Vec<(&str, &str)> = sources
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    files.push((
        "src/changed.ts",
        "export function target(): number { return 1; }\n",
    ));
    let h = harness(&files).await;
    let (url, seen) = model_routed(
        vec![
            (REVIEW, vec![Reply::Text(NO_FINDINGS)]),
            ("MARK-MAP", vec![Reply::Text(REPORT)]),
        ],
        vec![
            task("t1", "MARK-MAP: where?"),
            write_call(
                "w1",
                "src/changed.ts",
                "export function target(): number { return 2; }\n",
            ),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut config = reviewer_on(json!({}));
    config
        .kinds
        .insert("scout".into(), scouts_on(json!({})).kinds["scout"].clone());
    let request: String = format!(
        "Change the target, the way {} do it.",
        (0..60)
            .map(|i| format!("handler{i} and Service{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    h.start(h.with_subagents(&url, &request, checks_on(), config))
        .await;
    assert_eq!(h.run_end().await, "unverified");

    fn map_of(body: &Value) -> String {
        // The map goes on the request (on a first request, the last
        // message), before its text, not in the system prompt. It has no
        // blank line.
        let tail = tail_text(body);
        let Some(at) = tail.find("## Repository map") else {
            return String::new();
        };
        let rest = &tail[at..];
        rest.split("\n\n").next().unwrap_or(rest).to_string()
    }
    let lead = map_of(&seen.lock().unwrap()[0]);
    let scout = map_of(&requests_with(&seen, "MARK-MAP")[0]);
    let reviewer = map_of(&requests_with(&seen, REVIEW)[0]);
    assert!(!lead.is_empty() && !reviewer.is_empty());
    assert_eq!(scout, lead, "a scout gets the lead's map");
    assert!(
        reviewer.len() * 10 < lead.len() * 6,
        "reviewer {} vs lead {}",
        reviewer.len(),
        lead.len()
    );
    assert!(reviewer.contains("changed.ts"), "{reviewer}");
}

/// A04: a follow-up to a finished scout goes on in its own chat, with what
/// it found before; an unknown or running one is refused.
#[tokio::test]
async fn a_follow_up_resumes_a_finished_scout_with_its_context() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            "MARK-FU",
            vec![
                Reply::Text("## Answer\nFIRST-ANSWER: in a.txt."),
                Reply::Text("## Answer\nSECOND-ANSWER: nobody calls it."),
            ],
        )],
        vec![
            task("t1", "MARK-FU: where is it?"),
            Reply::Call {
                id: "t2",
                name: "task",
                args: json!({ "continue": "t1", "goal": "And who calls it?" }),
            },
            Reply::Call {
                id: "t3",
                name: "task",
                args: json!({ "continue": "scout-ffffffff", "goal": "Anyone there?" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");

    let first = h.task_result("t1").await;
    let child = h.child_of("t1").await;
    let handle = format!("scout-{}", &child.id[..8]);
    assert!(
        first
            .content
            .contains(&format!("Follow-up: call task with continue: \"{handle}\"")),
        "{}",
        first.content
    );
    let second = h.task_result("t2").await;
    assert!(second.content.ends_with("SECOND-ANSWER: nobody calls it."));
    let meta = &second.meta.unwrap()["subagent"];
    assert_eq!(meta["childDialogId"], child.id.as_str(), "the same chat");
    // Its second request had the first exchange in its history.
    let asked = requests_with(&seen, "MARK-FU");
    assert_eq!(asked.len(), 2);
    let history = asked[1]["messages"].to_string();
    assert!(history.contains("FIRST-ANSWER") && history.contains("And who calls it?"));
    assert!(h
        .task_result("t3")
        .await
        .content
        .starts_with("Error: no subagent \"scout-ffffffff\" in this chat."));
}

#[tokio::test]
async fn a_follow_up_to_a_scout_still_working_is_refused() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-FR",
            vec![Reply::Slow(300, Box::new(Reply::Text(REPORT)))],
        )],
        vec![
            Reply::Calls(vec![
                task_call("t1", "MARK-FR: where?"),
                (
                    "t2",
                    "task",
                    json!({ "continue": "t1", "goal": "And then?" }),
                ),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.task_result("t1").await.content.contains("Status: done"));
    assert!(h
        .task_result("t2")
        .await
        .content
        .starts_with("Error: that subagent is still working"));
}

/*
 * C01–C03: failure triage. The project's test check is a script that
 * always fails, with a large output (over FAILURE_OUTPUT_CHARS) or a
 * small one.
 */

const TRIAGE: &str = "# Triage this failure";
const DIAGNOSIS: &str =
    "## Root cause\nThe greeting in a.txt:1 is checked by the tests and is wrong.\n\n\
    ## Files to fix\n- `a.txt`: write \"hello\"\n\n## Next step\nReplace the line in a.txt.\n\n\
    ## Confidence\nhigh: the only error names a.txt:1";

fn triage_on(extra: Value) -> subagent::SubagentConfig {
    let mut triage = json!({ "enabled": true });
    if let (Some(t), Some(e)) = (triage.as_object_mut(), extra.as_object()) {
        t.extend(e.clone());
    }
    serde_json::from_value(json!({ "enabled": true, "kinds": { "triage": triage } })).unwrap()
}

/// A project whose tests always fail: `lines` lines of noise, then the error.
async fn failing_project(lines: usize) -> Option<Harness> {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return None;
    }
    let script = format!(
        "for (let i = 1; i <= {lines}; i++) console.log('noise line ' + i + ' of the test run, nothing to see');\n\
         console.error('error: the greeting is wrong at a.txt:1');\nprocess.exit(1);\n"
    );
    Some(
        harness(&[
            ("a.txt", "old\n"),
            ("fail.js", &script),
            (
                "package.json",
                r#"{ "name": "t", "scripts": { "test": "node fail.js" } }"#,
            ),
        ])
        .await,
    )
}

/// The IDE's failure messages to the lead, in order.
fn fix_messages(messages: &[MessageRow]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["harness"] == "verification")
        })
        .map(|m| m.content.clone())
        .collect()
}

fn triages(messages: &[MessageRow]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification")?.get("triage").cloned())
        .collect()
}

#[tokio::test]
async fn a_large_failure_goes_to_triage_and_its_diagnosis_to_the_agent() {
    let Some(h) = failing_project(400).await else {
        return;
    };
    let (url, seen) = model_routed(
        vec![(
            TRIAGE,
            vec![
                Reply::Calls(vec![
                    (
                        "t1",
                        "read",
                        json!({ "path": "@checks/round-1-test.log", "start_line": 1, "end_line": 3 }),
                    ),
                    ("t2", "write", json!({ "path": "a.txt", "content": "triaged\n" })),
                ]),
                Reply::Text(DIAGNOSIS),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            // The failure came back; it gives up without changing anything.
            Reply::Text("I can't fix it."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), triage_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    let messages = h.messages().await;

    // C02: the brief names the saved output; triage reads it, can't write.
    let briefs = requests_with(&seen, TRIAGE);
    let brief = briefs[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        brief.contains("saved as `@checks/round-1-test.log`"),
        "{brief}"
    );
    assert!(
        brief.contains("Files the agent changed\n\na.txt"),
        "{brief}"
    );
    let tools = tool_names(&briefs[0]);
    assert!(tools.contains(&"read".to_string()));
    for t in ["write", "bash", "task"] {
        assert!(!tools.contains(&t.to_string()), "{t} offered to triage");
    }
    let t = &triages(&messages)[0];
    assert_eq!(t["status"], "done", "{t}");
    assert_eq!(t["reason"], "large output");
    let child = t["childDialogId"].as_str().unwrap().to_string();
    let work = h.messages_of(&child).await;
    let read = work
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("t1"))
        .unwrap();
    // Stderr first, as in the report.
    assert!(
        read.content
            .contains("error: the greeting is wrong at a.txt:1"),
        "{}",
        read.content
    );
    let write = work
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("t2"))
        .unwrap();
    assert!(write.content.starts_with("Error"), "{}", write.content);
    assert_eq!(h.file("a.txt").as_deref(), Some("helo\n"));

    // C03: the agent gets the diagnosis, a short excerpt and the file,
    // not the whole output.
    let fix = &fix_messages(&messages)[0];
    assert!(
        fix.contains("**Root cause:** The greeting in a.txt:1"),
        "{fix}"
    );
    assert!(fix.contains("**Confidence:** high"), "{fix}");
    assert!(
        fix.contains("error: the greeting is wrong at a.txt:1"),
        "{fix}"
    );
    assert!(fix.contains("saved as `@checks/round-1-test.log`"), "{fix}");
    assert!(fix.len() < 5_000, "{} characters", fix.len());
    // The record keeps the whole result.
    assert_eq!(t["result"]["files"][0], "`a.txt`: write \"hello\"");
}

#[tokio::test]
async fn a_small_failure_gets_the_usual_message_until_it_fails_again() {
    let Some(h) = failing_project(3).await else {
        return;
    };
    let (url, seen) = model_routed(
        vec![(TRIAGE, vec![Reply::Text(DIAGNOSIS)])],
        vec![
            write_call("w1", "a.txt", "helo\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "hello\n"),
            Reply::Text("Fixed."),
            Reply::Text("I can't fix it."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), triage_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    let messages = h.messages().await;
    let fixes = fix_messages(&messages);
    assert_eq!(fixes.len(), 2);
    // Round 1, small and new: today's message, the output whole.
    assert!(!fixes[0].contains("Root cause"), "{}", fixes[0]);
    assert!(fixes[0].contains("noise line 3 of the test run"));
    assert!(!fixes[0].contains("@checks/"));
    // Round 2, the same check failed again: triaged.
    assert!(fixes[1].contains("**Root cause:**"), "{}", fixes[1]);
    let briefs = requests_with(&seen, TRIAGE);
    assert_eq!(briefs.len(), 1);
    let brief = briefs[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        brief.contains("It failed in the round before too"),
        "{brief}"
    );
    assert!(brief.contains("## The output"), "{brief}");
    assert_eq!(triages(&messages)[0]["reason"], "failed again");
}

#[tokio::test]
async fn a_failing_triage_leaves_the_usual_message() {
    let Some(h) = failing_project(400).await else {
        return;
    };
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model(vec![
        write_call("w1", "a.txt", "helo\n"),
        Reply::Text("Done."),
        Reply::Text("I can't fix it."),
    ])
    .await;
    let config = triage_on(json!({ "models": [profile("bad", &bad)] }));
    h.start(h.with_subagents(&url, "Greet", checks_on(), config))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    let messages = h.messages().await;
    let t = &triages(&messages)[0];
    assert_eq!(t["status"], "skipped", "{t}");
    assert!(
        t["skipped"].as_str().unwrap().contains("model failed"),
        "{t}"
    );
    let fix = &fix_messages(&messages)[0];
    assert!(!fix.contains("Root cause"), "{fix}");
    assert!(
        fix.contains("error: the greeting is wrong at a.txt:1"),
        "{fix}"
    );
    // C01: the whole output is saved, and the message says where.
    assert!(
        fix.contains("The whole of it is saved as `@checks/round-1-test.log`"),
        "{fix}"
    );
}

#[tokio::test]
async fn with_triage_off_a_large_failure_is_saved_and_named() {
    let Some(h) = failing_project(400).await else {
        return;
    };
    let (url, seen) = model(vec![
        write_call("w1", "a.txt", "helo\n"),
        Reply::Text("Done."),
        // The lead reads the saved output itself, in pieces.
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "@checks/round-1-test.log", "start_line": 1, "end_line": 12 }),
        },
        Reply::Text("I can't fix it."),
    ])
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), Default::default()))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    let messages = h.messages().await;
    assert!(triages(&messages).is_empty());
    assert!(requests_with(&seen, TRIAGE).is_empty(), "no triage request");
    let read = result_of(&messages, "r1");
    assert!(read.content.contains("noise line 5 of"), "{}", read.content);
    assert!(
        !read.content.contains("noise line 300 of"),
        "{}",
        read.content
    );
    assert!(
        read.content.contains("error: the greeting is wrong"),
        "{}",
        read.content
    );
}

/*
 * B08–B09: the plan reviewer, on submit_plan in Plan mode.
 */

const PLAN_REVIEW: &str = "# Review this plan";
const PLAN_FINDING: &str = "## Findings\n\n1. [high] Step 2 — `list_users` is also called from `src/cli.rs`\n   - Scenario: the CLI keeps the old signature and won't build.\n   - Fix: add a step updating `src/cli.rs`.\n   - Confidence: confirmed";
const PLAN_FINDING_2: &str = "## Findings\n\n1. [medium] Step 1 — The table has no index on `email`\n   - Scenario: lookups by email scan the table.\n   - Fix: add the index in step 1.\n   - Confidence: plausible";
/// B13: follow-up reviews of `users_plan(.., 1)` after PLAN_FINDING held
/// `users_plan(.., 0)` back: the earlier finding resolved or not, and a new
/// medium finding about step 3, which the revision added.
const PLAN_RESOLVED: &str = "## Earlier findings\n\n- Earlier 1: resolved — step 3 updates `src/cli.rs`.\n\n## Findings\n\nNone.";
const PLAN_UNRESOLVED: &str = "## Earlier findings\n\n- Earlier 1: not resolved — `src/cli.rs` calls `list_users` twice, and step 3 updates one.\n\n## Findings\n\nNone.";
const PLAN_FINDING_3: &str = "## Earlier findings\n\n- Earlier 1: resolved — step 3 updates `src/cli.rs`.\n\n## Findings\n\n1. [medium] Step 3 — The CLI prints each user unformatted\n   - Scenario: `print` shows the struct's debug form.\n   - Fix: print the user's email in step 3.\n   - Confidence: plausible";

fn plan_reviewer_on(extra: Value) -> subagent::SubagentConfig {
    let mut kind = json!({ "enabled": true });
    if let (Some(k), Some(e)) = (kind.as_object_mut(), extra.as_object()) {
        k.extend(e.clone());
    }
    serde_json::from_value(json!({ "enabled": true, "kinds": { "planReviewer": kind } })).unwrap()
}

/// A plan the checker accepts in `plan_project()`; `extra` adds steps.
fn users_plan(title: &str, extra: usize) -> String {
    let mut steps = String::from(
        "1. **Table**\n   - Files: `src/db.rs`\n   - Add a `users` table with `id` and `email`.\n\
         2. **API**\n   - Files: `src/api.rs`\n   - Add `fn list_users()` returning every user.\n",
    );
    for i in 0..extra {
        steps.push_str(&format!(
            "{}. **Caller {i}**\n   - Files: `src/cli.rs`\n   - Update call site {i} to use `list_users()` and print each user.\n",
            i + 3
        ));
    }
    format!("# {title}\n\nAdd users.\n\n## Steps\n\n{steps}\n## Validation\n- `cargo test`\n")
}

async fn plan_project() -> Harness {
    harness(&[("src/db.rs", ""), ("src/api.rs", ""), ("src/cli.rs", "")]).await
}

fn submit(id: &'static str, plan: String) -> Reply {
    Reply::Call {
        id,
        name: "submit_plan",
        args: json!({ "plan": Box::leak(plan.into_boxed_str()) as &str }),
    }
}

/// B12: a submit after a hold, with the agent's answers.
fn submit_with(id: &'static str, plan: String, responses: Value) -> Reply {
    Reply::Call {
        id,
        name: "submit_plan",
        args: json!({ "plan": plan, "responses": responses }),
    }
}

/// The agent's answer to PLAN_FINDING.
fn added_the_step() -> Value {
    json!([{ "finding": 1, "action": "fixed", "note": "Added step 3, which updates `src/cli.rs`." }])
}

impl Harness {
    fn plan_request(&self, url: &str, subagents: subagent::SubagentConfig) -> StartRequest {
        let mut request = self.request(url, plan_send("Add users"), quiet_permissions());
        request.config.as_mut().unwrap().subagents = subagents;
        request
    }

    fn plan_files(&self) -> Vec<String> {
        std::fs::read_dir(self.project.join(".rovik/plans"))
            .map(|d| {
                d.flatten()
                    .map(|e| std::fs::read_to_string(e.path()).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[tokio::test]
async fn plan_findings_hold_the_plan_until_the_agent_fixes_it() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_RESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            // Held: the agent adds the missing step, says so, and submits again.
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved the plan."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let first = result_of(&messages, "p1");
    assert!(
        first
            .content
            .starts_with("Error: The plan wasn't saved yet"),
        "{}",
        first.content
    );
    assert!(first
        .content
        .contains("1. [high] Step 2 — `list_users` is also called"));
    // B11: how to answer, and that the next review is the last.
    assert!(
        first.content.contains("`responses`") && first.content.contains("this run's last review"),
        "{}",
        first.content
    );
    let review = &first.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (review["kind"].as_str(), review["status"].as_str()),
        (Some("plan"), Some("findings"))
    );
    assert_eq!(review["findings"][0]["step"], 2);
    assert!(first.meta.as_ref().unwrap().get("planFile").is_none());
    let second = result_of(&messages, "p2");
    assert!(
        second
            .content
            .starts_with(&format!("Saved the plan to {}", shown(".rovik/plans/"))),
        "{}",
        second.content
    );
    let review = &second.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (review["status"].as_str(), review["round"].as_u64()),
        (Some("clean"), Some(2))
    );
    // B13: the follow-up said the earlier finding is resolved, and why.
    assert_eq!(review["earlier"][0]["resolved"], true, "{review}");
    assert_eq!(
        review["earlier"][0]["reason"],
        "step 3 updates `src/cli.rs`."
    );
    assert_eq!(review["earlier"][0]["response"]["action"], "fixed");
    let files = h.plan_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].contains("**Caller 0**") && !files[0].contains("Open review findings"));

    // The reviewer saw the request and the plan, never the lead's
    // conversation; read-only tools; round 2 saw its earlier finding, the
    // agent's answer and what the revision changed.
    let briefs: Vec<String> = requests_with(&seen, PLAN_REVIEW)
        .iter()
        .map(|b| {
            b["messages"][1]["content"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(briefs.len(), 2);
    assert!(briefs[0].contains("## The user's request\n\nAdd users"));
    assert!(briefs[0].contains("## The plan\n\n````markdown\n# Users"));
    assert!(!briefs[0].contains("The plan wasn't saved"));
    assert!(!briefs[0].contains("## The earlier review"));
    assert!(briefs[1].contains("## The earlier review") && briefs[1].contains("Caller 0"));
    assert!(
        briefs[1]
            .contains("   - The agent's answer: fixed — Added step 3, which updates `src/cli.rs`."),
        "{}",
        briefs[1]
    );
    assert!(
        briefs[1].contains("## What the revision changed\n\n````diff\n")
            && briefs[1].contains("+3. **Caller 0**"),
        "{}",
        briefs[1]
    );
    let tools = tool_names(&requests_with(&seen, PLAN_REVIEW)[0]);
    assert!(tools.contains(&"callers".to_string()));
    for t in ["write", "bash", "task", "submit_plan", "web_search"] {
        assert!(
            !tools.contains(&t.to_string()),
            "{t} offered to the plan reviewer"
        );
    }
}

#[tokio::test]
async fn after_the_last_round_open_findings_go_in_the_plan_and_no_third_review() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_UNRESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            // A third submit in the run: saved without a review, and with
            // no answers owed (the save ended the hold).
            submit("p3", users_plan("Users v3", 3)),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let second = result_of(&messages, "p2");
    assert!(
        second
            .content
            .contains("still reported 1 finding after its last round"),
        "{}",
        second.content
    );
    let review = &second.meta.as_ref().unwrap()["review"];
    assert_eq!(review["open"], 1);
    // B13: the earlier finding is what's open, with the follow-up's reason.
    assert_eq!(review["earlier"][0]["resolved"], false, "{review}");
    assert_eq!(review["findings"][0]["step"], 2, "{review}");
    assert!(
        review["findings"][0]["newEvidence"]
            .as_str()
            .unwrap()
            .contains("step 3 updates one"),
        "{review}"
    );
    let third = result_of(&messages, "p3");
    assert!(
        third.content.starts_with("Saved the plan"),
        "{}",
        third.content
    );
    // P4.6: not reviewed, and the record says why.
    let review = &third.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "skipped");
    assert!(review["reason"]
        .as_str()
        .unwrap()
        .contains("plan reviews are used"));
    assert!(third
        .content
        .contains("Review skipped: this run's 2 plan reviews are used"));
    assert_eq!(requests_with(&seen, PLAN_REVIEW).len(), 2);
    // The second round's open finding was saved with that version; the third
    // replaced the file.
    let files = h.plan_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].starts_with("# Users v3"));
}

#[tokio::test]
async fn the_open_findings_section_is_in_the_saved_plan() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_FINDING_3)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let files = h.plan_files();
    assert!(files[0].contains("## Open review findings"), "{}", files[0]);
    // A new medium finding counts in a follow-up: step 3 is the revision's.
    assert!(files[0].contains("**[medium] Step 3 — The CLI prints each user unformatted**"));
    assert!(
        !files[0].contains("`list_users` is also called"),
        "{}",
        files[0]
    );
}

/// A follow-up review's new findings count wherever they are, whatever
/// their severity: one about a step the revision didn't change is still
/// open, and (Agent mode, the run's last plan review) saved at the end of
/// the plan for the user.
#[tokio::test]
async fn a_follow_up_holds_new_findings_even_where_the_revision_changed_nothing() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_FINDING_2)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let second = result_of(&messages, "p2");
    assert!(
        second.content.starts_with("Saved the plan to")
            && second.content.contains("still reported 1 finding"),
        "{}",
        second.content
    );
    let review = &second.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "findings", "{review}");
    assert!(review.get("notHeld").is_none(), "{review}");
    assert_eq!(review["open"], 1, "{review}");
    assert_eq!(review["findings"][0]["step"], 1, "{review}");
    // No verdict on the earlier one, even when asked: settled, and not checked.
    assert_eq!(
        (
            &review["earlier"][0]["resolved"],
            &review["earlier"][0]["checked"]
        ),
        (&json!(true), &json!(false)),
        "{review}"
    );
    let files = h.plan_files();
    assert!(files[0].contains("## Open review findings"), "{}", files[0]);
}

/// B13: a follow-up review whose report gives an earlier finding no
/// verdict is asked once, in a turn without tools, for it; its answer, in
/// a shape of its own, is the verdict. One that never gives it leaves the
/// finding settled but not checked.
#[tokio::test]
async fn a_follow_up_without_verdicts_is_asked_for_them() {
    for (answer, resolved, checked, reason) in [
        (
            "1: not resolved — `src/cli.rs` calls `list_users` twice.",
            false,
            true,
            "`src/cli.rs` calls `list_users` twice.",
        ),
        (
            "## Findings\n\nNone.",
            true,
            false,
            "the reviewer gave no verdict on it; taken as settled",
        ),
    ] {
        let h = plan_project().await;
        let (url, seen) = model_routed(
            vec![(
                PLAN_REVIEW,
                vec![
                    Reply::Text(PLAN_FINDING),
                    Reply::Text(NO_FINDINGS),
                    Reply::Text(answer),
                ],
            )],
            vec![
                submit("p1", users_plan("Users", 0)),
                submit_with("p2", users_plan("Users", 1), added_the_step()),
                Reply::Text("Saved."),
            ],
        )
        .await;
        h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
            .await;
        assert_eq!(h.run_end().await, "done");
        assert_eq!(
            requests_with(&seen, PLAN_REVIEW).len(),
            3,
            "the review, the follow-up and its ask"
        );
        let messages = h.messages().await;
        let review = &result_of(&messages, "p2").meta.as_ref().unwrap()["review"];
        let earlier = &review["earlier"][0];
        assert_eq!(
            (
                &earlier["resolved"],
                &earlier["checked"],
                &earlier["reason"]
            ),
            (&json!(resolved), &json!(checked), &json!(reason)),
            "{review}"
        );
        assert_eq!(
            review["open"].as_u64(),
            (!resolved).then_some(1),
            "{review}"
        );
        // The ask is in the reviewer's chat, naming the finding by number.
        let child = h.child_of("p2").await;
        let work = h.messages_of(&child.id).await;
        let asks: Vec<&MessageRow> = work
            .iter()
            .filter(|m| {
                m.meta
                    .as_ref()
                    .is_some_and(|m| m["harness"] == "subagentFormat")
            })
            .collect();
        assert_eq!(asks.len(), 1);
        assert!(
            asks[0]
                .content
                .contains("Your report gives no verdict on Earlier 1. Don't call any tools."),
            "{}",
            asks[0].content
        );
    }
}

/// B13: a follow-up review cut short by its budget is asked for its
/// verdicts with its findings, and a format repair's rewrite keeps the
/// verdicts its first report gave.
#[tokio::test]
async fn a_follow_ups_report_turns_keep_its_verdicts() {
    // Out of budget: the note asks for the verdicts too.
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![
                Reply::Text(PLAN_FINDING),
                Reply::Calls(vec![("r1", "read", json!({ "path": "src/cli.rs" }))]),
                Reply::Text(PLAN_RESOLVED),
            ],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({ "maxToolCalls": 1 }))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let child = h.child_of("p2").await;
    let work = h.messages_of(&child.id).await;
    let budget = work
        .iter()
        .find(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["harness"] == "subagentBudget")
        })
        .expect("asked for its report");
    assert!(
        budget.content.contains(
            "Start it with \"## Earlier findings\": one line for each earlier finding (Earlier 1)"
        ),
        "{}",
        budget.content
    );
    let messages = h.messages().await;
    let review = &result_of(&messages, "p2").meta.as_ref().unwrap()["review"];
    assert_eq!(
        (
            &review["earlier"][0]["checked"],
            &review["earlier"][0]["reason"]
        ),
        (&json!(true), &json!("step 3 updates `src/cli.rs`.")),
        "{review}"
    );

    // Out of format: the rewrite drops the verdict, which is kept.
    let h = plan_project().await;
    let unformatted = "## Earlier findings\n\n- Earlier 1: resolved — step 3 updates `src/cli.rs`.\n\n## Findings\n\n1. [high] Step 3 — No scenario";
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![
                Reply::Text(PLAN_FINDING),
                Reply::Text(unformatted),
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        requests_with(&seen, PLAN_REVIEW).len(),
        3,
        "no ask: the verdict was kept"
    );
    let child = h.child_of("p2").await;
    let work = h.messages_of(&child.id).await;
    let repair = work
        .iter()
        .find(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["harness"] == "subagentFormat")
        })
        .expect("the repair turn");
    assert!(
        repair
            .content
            .contains("Start it with \"## Earlier findings\""),
        "{}",
        repair.content
    );
    let messages = h.messages().await;
    let review = &result_of(&messages, "p2").meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "clean", "{review}");
    assert_eq!(
        (
            &review["earlier"][0]["checked"],
            &review["earlier"][0]["reason"]
        ),
        (&json!(true), &json!("step 3 updates `src/cli.rs`.")),
        "{review}"
    );
}

/// B12: after a hold, a submit that leaves a finding unanswered (none, or
/// "not a problem" without why) goes back without a review; one with the
/// answers, in any shape the parser takes, is reviewed.
#[tokio::test]
async fn an_unanswered_hold_goes_back_without_a_review() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_RESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            // Revised, but no answers.
            submit("p2", users_plan("Users", 1)),
            // Answers that don't say why, under other names.
            submit_with(
                "p3",
                users_plan("Users", 1),
                json!([{ "id": "1", "status": "not a problem" }]),
            ),
            submit_with(
                "p4",
                users_plan("Users", 1),
                json!("[{\"finding\": 1, \"action\": \"Fixed\", \"note\": \"Added step 3.\"}]"),
            ),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    for (id, tail) in [
        ("p2", "Finding 1 has no answer yet."),
        (
            "p3",
            "Finding 1 has no answer yet (a not_a_problem answer needs a note saying why).",
        ),
    ] {
        let r = result_of(&messages, id);
        assert!(
            r.content
                .starts_with("Error: the plan wasn't saved. Fix this and call submit_plan again")
                && r.content
                    .contains("Answer the review's findings in `responses`")
                && r.content.ends_with(tail),
            "{id}: {}",
            r.content
        );
        assert!(
            r.meta.as_ref().is_none_or(|m| m.get("review").is_none()),
            "{id}: no review record"
        );
    }
    assert_eq!(
        requests_with(&seen, PLAN_REVIEW).len(),
        2,
        "no review spent"
    );
    let fourth = result_of(&messages, "p4");
    assert!(
        fourth.content.starts_with("Saved the plan to"),
        "{}",
        fourth.content
    );
    let review = &fourth.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (review["round"].as_u64(), review["status"].as_str()),
        (Some(2), Some("clean"))
    );
    assert_eq!(review["earlier"][0]["response"]["note"], "Added step 3.");
    assert_eq!(h.plan_files().len(), 1);
}

/// B12: the answers are owed MAX_ANSWER_NUDGES times a hold; past that, the
/// plan sent again as it was, with nothing said, is reviewed, and the held
/// finding stays open whatever the review says (round 3).
#[tokio::test]
async fn hold_then_identical_resubmits_are_nudged_then_reviewed() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_RESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit("p2", users_plan("Users", 0)),
            submit("p3", users_plan("Users", 0)),
            submit("p4", users_plan("Users", 0)),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    for id in ["p2", "p3"] {
        let r = result_of(&messages, id);
        assert!(
            r.content.contains("Finding 1 has no answer yet."),
            "{id}: {}",
            r.content
        );
    }
    let briefs: Vec<String> = requests_with(&seen, PLAN_REVIEW)
        .iter()
        .map(|b| {
            b["messages"][1]["content"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(briefs.len(), 2, "p4 was reviewed, p2 and p3 weren't");
    assert!(
        briefs[1].contains("   - The agent's answer: none")
            && briefs[1].contains(
                "## What the revision changed\n\nNothing: the agent sent the plan again as it was."
            ),
        "{}",
        briefs[1]
    );
    let fourth = result_of(&messages, "p4");
    assert!(
        fourth.content.starts_with("Saved the plan to")
            && fourth
                .content
                .contains("still reported 1 finding after its last round"),
        "{}",
        fourth.content
    );
    let review = &fourth.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (&review["round"], &review["status"], &review["open"]),
        (&json!(2), &json!("findings"), &json!(1)),
        "{review}"
    );
    assert_eq!(review["earlier"][0]["resolved"], false, "{review}");
    assert_eq!(
        review["earlier"][0]["reason"],
        "the plan was sent again unchanged, with no answer"
    );
    assert!(h.plan_files()[0].contains("## Open review findings"));
}

#[tokio::test]
async fn a_failing_plan_reviewer_saves_the_plan_with_a_note() {
    let h = plan_project().await;
    let (bad, _) = model(vec![Reply::Status(
        400,
        r#"{"error":{"message":"no such model"}}"#,
    )])
    .await;
    let (url, _) = model(vec![
        submit("p1", users_plan("Users", 0)),
        Reply::Text("Saved."),
    ])
    .await;
    let config = plan_reviewer_on(json!({ "models": [profile("bad", &bad)] }));
    h.start(h.plan_request(&url, config)).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let r = result_of(&messages, "p1");
    assert!(r.content.starts_with("Saved the plan to"), "{}", r.content);
    assert!(
        r.content
            .contains("Review skipped: the reviewer's model failed"),
        "{}",
        r.content
    );
    assert_eq!(r.meta.as_ref().unwrap()["review"]["status"], "skipped");
    assert_eq!(h.plan_files().len(), 1);
}

/// A plan reviewer that answers with the plan it was given, twice (no
/// Findings section, nothing finding-shaped), counts as no findings: the
/// plan is saved, and the record says what was read as none.
#[tokio::test]
async fn a_plan_reviewer_sending_the_plan_back_counts_as_no_findings() {
    let h = plan_project().await;
    let plan: &'static str = Box::leak(users_plan("Users", 0).into_boxed_str());
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(plan), Reply::Text(plan)])],
        vec![submit("p1", plan.to_string()), Reply::Text("Saved.")],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let r = result_of(&messages, "p1");
    assert!(r.content.starts_with("Saved the plan to"), "{}", r.content);
    assert!(!r.content.contains("Review skipped"), "{}", r.content);
    let review = &r.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "clean", "{review}");
    assert!(
        review["reason"]
            .as_str()
            .unwrap()
            .contains("no Findings section"),
        "{review}"
    );
    assert!(
        review["report"].as_str().unwrap().contains("## Steps"),
        "{review}"
    );
    assert_eq!(h.plan_files().len(), 1);
}

/// A finding about the plan as a whole, located in words other than "Plan"
/// ("Validation section"), holds the plan like any other. Before, the
/// report couldn't be read, the repair turn got the same shape back, and
/// the plan was saved unreviewed: "Review skipped: its report didn't
/// follow the format".
#[tokio::test]
async fn a_plan_finding_located_without_the_word_plan_is_read() {
    let h = plan_project().await;
    let report = "## Findings\n\n1. [medium] Validation section — No test covers `list_users`\n   \
        - Scenario: a broken `list_users` passes `cargo test`.\n   \
        - Fix: add a test for `list_users` under Validation.\n   \
        - Confidence: confirmed";
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(report), Reply::Text(report)])],
        vec![
            submit("p1", users_plan("Users", 0)),
            Reply::Text("I'll add the test."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let r = result_of(&messages, "p1");
    assert!(
        r.content.starts_with("Error: The plan wasn't saved yet"),
        "{}",
        r.content
    );
    assert!(
        r.content
            .contains("1. [medium] Validation section — No test covers `list_users`"),
        "{}",
        r.content
    );
    let review = &r.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "findings", "{review}");
    assert_eq!(review["findings"][0]["location"], "Validation section");
    assert!(h.plan_files().is_empty());
    // Read the first time: no repair turn.
    let child = h.child_of("p1").await;
    let work = h.messages_of(&child.id).await;
    assert!(
        !work.iter().any(|m| m
            .meta
            .as_ref()
            .is_some_and(|m| m["harness"] == "subagentFormat")),
        "{work:#?}"
    );
}

/// A plan reviewer cut short by its budget writes its report like any
/// other: checked against the findings format, with the repair turn. Before,
/// the budget's report went to the parser unchecked, and a reviewer that
/// wrote prose (the budget note asked for "## Open questions", a section it
/// doesn't have) skipped the review: "it has no \"## Findings\" section".
#[tokio::test]
async fn a_plan_reviewer_out_of_budget_still_reports_findings() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![
                Reply::Calls(vec![
                    ("r1", "read", json!({ "path": "src/api.rs" })),
                    ("r2", "read", json!({ "path": "src/cli.rs" })),
                ]),
                // Asked for its report after the budget: prose, no heading.
                Reply::Text("I checked src/cli.rs: it calls list_users too, and the plan doesn't update it."),
                // The repair turn.
                Reply::Text(PLAN_FINDING),
                // Round 2, on the fixed plan.
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved the plan."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({ "maxToolCalls": 1 }))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let first = result_of(&messages, "p1");
    assert!(
        first
            .content
            .starts_with("Error: The plan wasn't saved yet"),
        "{}",
        first.content
    );
    assert!(first
        .content
        .contains("1. [high] Step 2 — `list_users` is also called"));
    let review = &first.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "findings");
    let second = result_of(&messages, "p2");
    assert!(
        second.content.starts_with("Saved the plan to"),
        "{}",
        second.content
    );
    assert_eq!(second.meta.as_ref().unwrap()["review"]["status"], "clean");

    // The budget note asked for findings, not "## Open questions", and the
    // prose got the repair turn.
    let child = h.child_of("p1").await;
    let work = h.messages_of(&child.id).await;
    let note = |harness: &str| {
        work.iter()
            .find(|m| m.meta.as_ref().is_some_and(|m| m["harness"] == harness))
    };
    let budget = note("subagentBudget").expect("asked for its report");
    assert!(
        budget.content.contains("budget of 1 tool calls"),
        "{}",
        budget.content
    );
    assert!(
        budget.content.contains("\"## Findings\""),
        "{}",
        budget.content
    );
    assert!(
        !budget.content.contains("Open questions"),
        "{}",
        budget.content
    );
    let format = note("subagentFormat").expect("got the repair turn");
    assert!(
        format.content.contains("no \"## Findings\" section"),
        "{}",
        format.content
    );
}

#[tokio::test]
async fn a_small_revision_is_saved_without_a_review_and_off_means_off() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![submit("p1", users_plan("Users", 0)), Reply::Text("Saved.")],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    // The user asks for a small change: a new run, a revision of a few lines.
    let (url2, seen2) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![
            submit("p2", users_plan("Users (revised)", 0)),
            Reply::Text("Revised."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.plan_request(&url2, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(requests_with(&seen, PLAN_REVIEW).len(), 1);
    assert!(
        requests_with(&seen2, PLAN_REVIEW).is_empty(),
        "a small revision isn't reviewed"
    );
    let files = h.plan_files();
    assert_eq!(files.len(), 1, "{files:#?}");
    assert!(files[0].starts_with("# Users (revised)"), "{files:#?}");

    // Subagents on, the plan reviewer off: no review.
    let h = plan_project().await;
    let (url, seen) = model(vec![
        submit("p1", users_plan("Users", 0)),
        Reply::Text("Saved."),
    ])
    .await;
    h.start(h.plan_request(&url, scouts_on(json!({})))).await;
    assert_eq!(h.run_end().await, "done");
    assert!(requests_with(&seen, PLAN_REVIEW).is_empty());
    assert!(result_of(&h.messages().await, "p1")
        .meta
        .as_ref()
        .unwrap()
        .get("review")
        .is_none());
}

/// P4.6: after a Hold, a small revert of the plan is still reviewed and
/// the held finding stays in front of the user.
#[tokio::test]
async fn a_small_revert_after_a_hold_is_still_reviewed() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![submit("p0", users_plan("Users", 0)), Reply::Text("Saved.")],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let again = format!("{PLAN_FINDING}\n   - New evidence: the revision dropped the step again.");
    let (url2, seen2) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![
                Reply::Text(PLAN_FINDING),
                Reply::Text(Box::leak(again.into_boxed_str())),
            ],
        )],
        vec![
            submit("p1", users_plan("Users", 6)),
            // Held; the agent reverts to (nearly) the saved plan.
            submit_with(
                "p2",
                users_plan("Users (again)", 0),
                json!([{ "finding": 1, "action": "not_a_problem", "note": "The CLI goes in a later change." }]),
            ),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.plan_request(&url2, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let briefs = requests_with(&seen2, PLAN_REVIEW);
    assert_eq!(briefs.len(), 2, "the revert was reviewed");
    let messages = h.messages().await;
    let second = result_of(&messages, "p2");
    let review = &second.meta.as_ref().unwrap()["review"];
    assert_eq!(review["round"], 2);
    // B13: a repeat with new evidence keeps the earlier finding open.
    assert_eq!(
        (
            &review["earlier"][0]["resolved"],
            &review["earlier"][0]["reason"]
        ),
        (
            &json!(false),
            &json!("the revision dropped the step again.")
        ),
        "{review}"
    );
    let files = h.plan_files();
    assert!(files[0].starts_with("# Users (again)"), "{files:#?}");
    assert!(
        files[0].contains("## Open review findings")
            && files[0].contains("`list_users` is also called"),
        "{}",
        files[0]
    );
}

/*
 * Milestone D: step workers. The lead implements a saved plan; workers are
 * routed by the heading of their brief, "# Your step: N.".
 */

const STEP_PLAN: &str = "# Greet\n\nSay hello in two files.\n\n## Steps\n\n\
    1. **First greeting**\n   - Files: `a.txt`\n   - Replace its line with \"hello\".\n\
    2. **Second greeting**\n   - Files: `a.txt`, `b.txt`\n   - Add a second line \"hello again\" to a.txt; write \"bye\" to b.txt.\n\
    3. **Notes**\n   - Files: `notes/` (new)\n   - Add a file with the date.\n\n\
    ## Decisions\n- Plain text, one greeting per line.\n\n## Validation\n- read the files\n";

fn workers_on(extra: Value) -> subagent::SubagentConfig {
    let mut worker = json!({ "enabled": true });
    if let (Some(w), Some(e)) = (worker.as_object_mut(), extra.as_object()) {
        w.extend(e.clone());
    }
    serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "worker": worker },
        "workerMinSteps": 2,
    }))
    .unwrap()
}

fn step(id: &'static str, n: usize) -> Reply {
    Reply::Call {
        id,
        name: "task",
        args: json!({ "kind": "worker", "step": n }),
    }
}

fn worker_report(summary: &str, files: &[&str]) -> Reply {
    let list = if files.is_empty() {
        "None".to_string()
    } else {
        files
            .iter()
            .map(|f| format!("- `{f}`: changed"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Reply::Text(Box::leak(
        format!("## Summary\n{summary}\n\n## Changed files\n{list}\n\n## Open questions\nNone")
            .into_boxed_str(),
    ))
}

async fn plan_harness(files: &[(&str, &str)]) -> Harness {
    let mut all: Vec<(&str, &str)> = files.to_vec();
    all.push((".rovik/plans/greet.md", STEP_PLAN));
    harness(&all).await
}

impl Harness {
    fn implement(&self, url: &str, subagents: subagent::SubagentConfig) -> StartRequest {
        self.implement_with(url, subagents, ChecklistMode::Auto)
    }

    /// Off: the run may end with steps open (a test that stops early).
    fn implement_with(
        &self,
        url: &str,
        subagents: subagent::SubagentConfig,
        checklist: ChecklistMode,
    ) -> StartRequest {
        let message = UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: "Implement the plan in `.rovik/plans/greet.md`.".into(),
            meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
            attachments: Vec::new(),
        };
        let mut request = self.request(
            url,
            StartKind::Send { message },
            Permissions {
                allow_any_command: true,
                checklist,
                ..quiet_permissions()
            },
        );
        request.config.as_mut().unwrap().subagents = subagents;
        request
    }
}

fn too_long() -> Reply {
    Reply::Status(
        400,
        r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#,
    )
}

fn bad() -> Reply {
    Reply::Status(400, r#"{"error":{"message":"no such model"}}"#)
}

/// The lead settles its checklist before it finishes.
fn checklist_closed() -> Reply {
    Reply::Call {
        id: "t9",
        name: "todo",
        args: json!({ "items": [
            { "title": "First greeting", "status": "done" },
            { "title": "Second greeting", "status": "skipped" },
            { "title": "Notes", "status": "skipped" },
        ] }),
    }
}

fn checklist_call() -> Reply {
    Reply::Call {
        id: "t1",
        name: "todo",
        args: json!({ "items": [
            { "title": "First greeting", "status": "pending" },
            { "title": "Second greeting", "status": "pending" },
            { "title": "Notes", "status": "pending" },
        ] }),
    }
}

#[tokio::test]
async fn workers_carry_out_the_plan_step_by_step() {
    let h = plan_harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 1.",
                vec![
                    write_call("w1", "a.txt", "hello\n"),
                    // A choice the plan didn't make, for the steps after it.
                    Reply::Text("## Summary\nReplaced the line with hello.\n\n## Changed files\n- `a.txt`: changed\n\n## Decisions\n- Greetings are lowercase, one a line.\n\n## Open questions\nNone"),
                ],
            ),
            (
                "# Your step: 2.",
                vec![
                    write_call("w2", "a.txt", "hello\nhello again\n"),
                    write_call("w3", "b.txt", "bye\n"),
                    // It lists a file it didn't change, and leaves out one it did.
                    worker_report(
                        "Added the second greeting and the farewell.",
                        &["a.txt", "c.txt"],
                    ),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w4", "notes/today.txt", "2026-10-06\n"),
                    worker_report("Added notes/today.txt.", &["notes/today.txt"]),
                ],
            ),
        ],
        vec![
            checklist_call(),
            step("s1", 1),
            step("s2", 2),
            step("s3", 3),
            Reply::Text("Done: the plan is implemented."),
        ],
    )
    .await;
    h.start(h.implement(&url, workers_on(json!({})))).await;
    // Step 2's report doesn't match its files: not marked done (P2.9).
    assert_eq!(h.run_end().await, "unfinished-checklist");
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\nhello again\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("bye\n"));
    assert_eq!(h.file("notes/today.txt").as_deref(), Some("2026-10-06\n"));

    // D05: the lead's prompt says when to use workers, with the threshold.
    let first = seen.lock().unwrap()[0].clone();
    assert!(
        system_prompt(&first).contains("plan of 2 or more steps"),
        "the threshold from the settings"
    );
    // D03: step 2's worker sees step 1's change, its summary, the plan's
    // head and decisions; its own step only.
    let briefs = requests_with(&seen, "# Your step: 2.");
    let brief = briefs[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        brief.contains("- Step 1. First greeting: Replaced the line with hello.\n  - Decided: Greetings are lowercase, one a line.\n"),
        "the step and the decision its worker made: {brief}"
    );
    // Step 3's too: step 1's decision holds for every step after it.
    let third = requests_with(&seen, "# Your step: 3.")[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        third.contains("  - Decided: Greetings are lowercase, one a line.\n"),
        "{third}"
    );
    assert!(
        brief.contains("-old") && brief.contains("+hello"),
        "the diff of its file: {brief}"
    );
    assert!(
        brief.contains("## The plan's decisions\n\n- Plain text"),
        "{brief}"
    );
    assert!(!brief.contains("**Notes**"), "{brief}");
    let tools = tool_names(&briefs[0]);
    for t in ["write", "bash", "extend_scope", "diagnostics"] {
        assert!(tools.contains(&t.to_string()), "{t} for a worker");
    }
    for t in ["task", "todo", "ask_user"] {
        assert!(!tools.contains(&t.to_string()), "{t} not for a worker");
    }
    // The lead's context: the workers' reports, not their tool calls.
    let last_lead = seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|b| route_of(b, &["# Your step:"]).is_none())
        .cloned()
        .unwrap();
    let context = last_lead["messages"].to_string();
    assert!(context.contains("Added the second greeting"));
    assert!(
        context.contains("Greetings are lowercase"),
        "the lead reads the decisions in the report"
    );
    assert!(
        !context.contains("hello again\\\\n"),
        "the workers' write calls stay in their chats"
    );

    // D04: a report that doesn't match the files gets a note.
    let messages = h.messages().await;
    let r2 = result_of(&messages, "s2");
    assert!(
        r2.content.contains("lists `c.txt`, which it didn't change"),
        "{}",
        r2.content
    );
    assert!(
        r2.content
            .contains("changed `b.txt`, which its report doesn't list"),
        "{}",
        r2.content
    );
    assert!(!result_of(&messages, "s1").content.contains("don't match"));
    // ...and its step isn't done (P2.9): partial, unticked; the others are.
    assert!(r2.content.contains("Status: partial"), "{}", r2.content);
    assert!(
        r2.content.contains("Step 2 isn't marked done"),
        "{}",
        r2.content
    );
    assert!(r2.meta.as_ref().unwrap().get("checklistUpdate").is_none());
    let recent = h.messages().await;
    let current = plan::current_checklist(&recent).unwrap();
    let statuses: Vec<&str> = current.items.iter().map(|i| i.status.as_str()).collect();
    assert_eq!(statuses, ["done", "pending", "done"], "{:?}", current.items);
    // The workers' changes are the lead's (S03b).
    assert_eq!(r2.meta.as_ref().unwrap()["subagent"]["worker"]["step"], 2);
}

/// P2.9: a worker that changed nothing isn't a step done, whatever it says.
#[tokio::test]
async fn a_worker_that_wrote_nothing_leaves_its_step_open() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![worker_report("All done.", &["a.txt"])],
        )],
        vec![
            checklist_call(),
            step("s1", 1),
            Reply::Text("Stopping here."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let r = result_of(&h.messages().await, "s1").clone();
    assert!(r.content.contains("Status: partial"), "{}", r.content);
    assert!(
        r.content
            .contains("it said it was done but changed no files"),
        "{}",
        r.content
    );
    assert!(r.meta.as_ref().unwrap().get("checklistUpdate").is_none());
}

/// A path in a worker's scope that is a link to a file outside it (granted
/// here) is checked by where it lands: the file behind it isn't written.
#[cfg(unix)]
#[tokio::test]
async fn a_link_in_scope_isnt_a_way_to_write_outside_it() {
    let h = plan_harness(&[("a.txt", "old\n"), ("c.txt", "keep\n")]).await;
    std::os::unix::fs::symlink("c.txt", h.project.join("link.txt")).unwrap();
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                Reply::Call {
                    id: "e1",
                    name: "extend_scope",
                    args: json!({ "paths": ["link.txt"], "reason": "mirrored there" }),
                },
                write_call("w1", "link.txt", "changed\n"),
                write_call("w2", "a.txt", "hello\n"),
                worker_report("Greeted.", &["a.txt"]),
            ],
        )],
        vec![
            checklist_call(),
            step("s1", 1),
            checklist_closed(),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    assert_eq!(h.run_end().await, "done");
    let child = h.child_of("s1").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content
            .clone()
    };
    assert!(result("e1").starts_with("Granted"), "{}", result("e1"));
    assert!(
        result("w1").contains("outside your write scope"),
        "{}",
        result("w1")
    );
    assert_eq!(h.file("c.txt").as_deref(), Some("keep\n"));
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\n"));
}

#[tokio::test]
async fn a_write_outside_the_scope_is_refused_until_extended() {
    let h = plan_harness(&[("a.txt", "old\n"), ("c.txt", "keep\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                write_call("w1", "c.txt", "changed\n"),
                write_call("w2", "src/new.txt", "x\n"),
                Reply::Call {
                    id: "e1",
                    name: "extend_scope",
                    args: json!({ "paths": ["c.txt"], "reason": "the greeting is mirrored there" }),
                },
                write_call("w3", "c.txt", "hello\n"),
                write_call("w4", "a.txt", "hello\n"),
                worker_report("Greeted.", &["a.txt", "c.txt"]),
            ],
        )],
        vec![
            checklist_call(),
            step("s1", 1),
            checklist_closed(),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    assert_eq!(h.run_end().await, "done");
    let child = h.child_of("s1").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content
            .clone()
    };
    let refused = result("w1");
    assert!(
        refused.starts_with("Error: `c.txt` is outside your write scope"),
        "{refused}"
    );
    assert!(
        refused.contains("You may write: `a.txt`; and new files in the project root"),
        "{refused}"
    );
    assert!(
        result("w2").contains("outside your write scope"),
        "a new file in another folder"
    );
    assert!(result("e1").starts_with("Granted"));
    assert!(!result("w3").starts_with("Error"), "{}", result("w3"));
    assert_eq!(h.file("c.txt").as_deref(), Some("hello\n"));
    assert!(h.file("src/new.txt").is_none());
    let lead = result_of(&h.messages().await, "s1").content.clone();
    assert!(
        lead.contains("It was granted, beyond its step: c.txt."),
        "{lead}"
    );
}

#[tokio::test]
async fn a_failed_worker_is_rolled_back_and_a_partial_one_kept() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "a.txt", "half\n"),
                    write_call("w2", "b.txt", "half\n"),
                    bad(),
                    bad(),
                ],
            ),
            (
                "# Your step: 1.",
                vec![
                    write_call("w3", "a.txt", "one\n"),
                    write_call("w4", "a.txt", "two\n"),
                    write_call("w5", "a.txt", "three\n"),
                    // Out of budget (3 calls): the result turn.
                    worker_report("Half done.", &["a.txt"]),
                ],
            ),
        ],
        vec![
            checklist_call(),
            step("s2", 2),
            step("s1", 1),
            checklist_closed(),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(
        &url,
        workers_on(json!({ "maxToolCalls": 3 })),
        ChecklistMode::Off,
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let failed = result_of(&messages, "s2").content.clone();
    assert!(failed.contains("Status: failed"), "{failed}");
    assert!(
        failed.contains("Its changes were undone: a.txt, b.txt put back as they were."),
        "{failed}"
    );
    let partial = result_of(&messages, "s1").content.clone();
    assert!(partial.contains("Status: partial"), "{partial}");
    assert!(
        partial.contains("Its changes are kept: a.txt."),
        "{partial}"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("three\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("b\n"));
    // A failed or partial step isn't marked done.
    assert!(result_of(&messages, "s1")
        .meta
        .as_ref()
        .unwrap()
        .get("checklistUpdate")
        .is_none());
}

/// P0.3: a file a worker changed in a fix round goes back byte for byte
/// when the round is rolled back, UTF-8 or not (Latin-1 here: text to the
/// write tool, but not UTF-8).
#[tokio::test]
async fn a_rolled_back_round_puts_a_workers_non_utf8_file_back_exactly() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let plan = "# Data\n\nFix the data.\n\n## Steps\n\n\
        1. **Greeting**\n   - Files: `a.txt`\n   - Say good in it.\n\
        2. **Data**\n   - Files: `data.txt`, `b.txt`\n   - Rewrite data.txt and update b.txt.\n\n\
        ## Validation\n- `npm test`\n";
    let check = "const fs = require('fs');\n\
        if (!fs.readFileSync('a.txt', 'utf8').includes('good')) { console.log('test t::one ... FAILED'); process.exit(1); }\n\
        if (fs.readFileSync('b.txt', 'utf8').includes('broken')) { console.log('test t::one ... ok'); console.log('test t::two ... FAILED'); process.exit(1); }\n\
        console.log('test t::one ... ok'); console.log('test t::two ... ok');\n";
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        ("check.js", check),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
        (".rovik/plans/greet.md", plan),
    ])
    .await;
    let original: Vec<u8> = b"caf\xe9 cr\xe8me\n".to_vec();
    std::fs::write(h.project.join("data.txt"), &original).unwrap();
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                write_call("w1", "data.txt", "text now\n"),
                write_call("w2", "b.txt", "broken\n"),
                worker_report("Rewrote both.", &["data.txt", "b.txt"]),
            ],
        )],
        vec![
            write_call("l1", "notes.txt", "looking\n"),
            Reply::Text("Done."),
            // The fix round: a worker, then the real fix.
            step("s2", 2),
            write_call("l2", "a.txt", "good\n"),
            Reply::Text("Done."),
            // Rolled back: the fix again, alone.
            write_call("l3", "a.txt", "good\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut request = h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off);
    request.config.as_mut().unwrap().permissions.verify_changes = true;
    h.start(request).await;
    assert_eq!(h.run_end().await, "verified");
    let statuses: Vec<Value> = h
        .messages()
        .await
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .map(|v| v["status"].clone())
        .collect();
    assert!(statuses.contains(&json!("rolled-back")), "{statuses:?}");
    assert_eq!(
        std::fs::read(h.project.join("data.txt")).unwrap(),
        original,
        "the bytes the round started with"
    );
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
}

/// Round 2, P0.3: in an isolated chat a worker's original is the
/// project's copy. For a file the chat never changed, that is what the
/// round started with, so the round's rollback removes the file the
/// worker created (it used to "restore" the worker's own write).
#[tokio::test]
async fn an_isolated_step_workers_new_file_is_removed_by_the_rounds_rollback() {
    if !crate::vfs::mount_works()
        || std::process::Command::new("npm")
            .arg("--version")
            .output()
            .is_err()
    {
        return;
    }
    let plan = "# Extra\n\nFix the greeting.\n\n## Steps\n\n\
        1. **Greeting**\n   - Files: `a.txt`\n   - Say good in it.\n\
        2. **Extra**\n   - Files: `src/extra.rs` (new)\n   - Add the helper there.\n\n\
        ## Validation\n- `npm test`\n";
    let check = "const fs = require('fs');\n\
        if (!fs.readFileSync('a.txt', 'utf8').includes('good')) { console.log('test t::one ... FAILED'); process.exit(1); }\n\
        if (fs.existsSync('src/extra.rs')) { console.log('test t::one ... ok'); console.log('test t::two ... FAILED'); process.exit(1); }\n\
        console.log('test t::one ... ok'); console.log('test t::two ... ok');\n";
    let mut h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        ("check.js", check),
        ("a.txt", "bad\n"),
        ("src/lib.rs", "// lib\n"),
        (".rovik/plans/greet.md", plan),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                write_call("w1", "src/extra.rs", "pub fn extra() {}\n"),
                worker_report("Added the helper.", &["src/extra.rs"]),
            ],
        )],
        vec![
            write_call("l1", "notes.txt", "looking\n"),
            Reply::Text("Done."),
            // The fix round: a worker, then the real fix.
            step("s2", 2),
            write_call("l2", "a.txt", "good\n"),
            Reply::Text("Done."),
            // Rolled back: the fix again, alone.
            write_call("l3", "a.txt", "good\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut request = h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off);
    request.isolate = Some(true);
    request.config.as_mut().unwrap().permissions.verify_changes = true;
    h.start(request).await;
    assert_eq!(h.run_end().await, "verified");
    let records: Vec<Value> = h
        .messages()
        .await
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect();
    let rb = records
        .iter()
        .find(|r| r["status"] == "rolled-back")
        .unwrap_or_else(|| panic!("{records:#?}"));
    assert!(
        rb["rolledBack"]["restored"]
            .as_array()
            .unwrap()
            .contains(&json!("src/extra.rs")),
        "{rb:#}"
    );
    assert!(
        !crate::overlay::chats::changes(&h.dialog_id)
            .iter()
            .any(|c| c.path() == "src/extra.rs"),
        "the worker's file went with the round"
    );
}

/// P2.3: extend_scope grants files, not folders beyond the step's own nor
/// another step's files; a continued worker keeps what it was granted.
#[tokio::test]
async fn scope_grants_are_bounded_and_kept_when_continued() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let extend = |id: &'static str, paths: Value| Reply::Call {
        id,
        name: "extend_scope",
        args: json!({ "paths": paths, "reason": "needed" }),
    };
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                extend("e1", json!(["notes/"])),
                extend("e2", json!(["notes/x.txt"])),
                extend("e3", json!(["c.txt"])),
                write_call("w1", "c.txt", "one\n"),
                worker_report("Wrote c.txt.", &["c.txt"]),
                // Continued: it still may write c.txt.
                write_call("w2", "c.txt", "two\n"),
                worker_report("Changed c.txt again.", &["c.txt"]),
            ],
        )],
        vec![
            step("s2", 2),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "s2", "goal": "make it say two" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let child = h.child_of("s2").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .map(|m| m.content.clone())
            .unwrap_or_default()
    };
    assert!(
        result("e1").starts_with("Error: not granted: `notes/` is a folder"),
        "{}",
        result("e1")
    );
    assert!(
        result("e2").contains("is step 3's to change"),
        "{}",
        result("e2")
    );
    assert!(result("e3").starts_with("Granted"), "{}", result("e3"));
    assert!(!result("w2").starts_with("Error"), "{}", result("w2"));
    assert_eq!(h.file("c.txt").as_deref(), Some("two\n"));
}

/// Round 2, P2.3: a folder the step may create files in can't be granted
/// when another step's file is in it.
#[tokio::test]
async fn a_folder_grant_cant_cover_another_steps_file() {
    let plan = "# API\n\nUsers and posts.\n\n## Steps\n\n\
        1. **Base**\n   - Files: `src/lib.rs`\n   - Declare the api module.\n\
        2. **Users**\n   - Files: `src/api/users.rs` (new)\n   - Add the users endpoint.\n\
        3. **Posts**\n   - Files: `src/api/posts.rs`\n   - Add the posts endpoint.\n\n\
        ## Validation\n- `cargo test`\n";
    let h = harness(&[
        ("src/lib.rs", "// lib\n"),
        ("src/api/posts.rs", "// posts\n"),
        (".rovik/plans/greet.md", plan),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                Reply::Call {
                    id: "e1",
                    name: "extend_scope",
                    args: json!({ "paths": ["src/api/"], "reason": "the whole module" }),
                },
                write_call("w1", "src/api/posts.rs", "// posts by step 2\n"),
                write_call("w2", "src/api/users.rs", "// users\n"),
                worker_report("Users.", &["src/api/users.rs"]),
            ],
        )],
        vec![step("s2", 2), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let child = h.child_of("s2").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .map(|m| m.content.clone())
            .unwrap_or_default()
    };
    assert!(
        result("e1").contains("is step 3's to change"),
        "{}",
        result("e1")
    );
    assert!(result("w1").starts_with("Error"), "{}", result("w1"));
    assert!(!result("w2").starts_with("Error"), "{}", result("w2"));
    assert_eq!(h.file("src/api/posts.rs").as_deref(), Some("// posts\n"));
}

/// P2.8: continuing a worker by the call that started it writes as much
/// as by its handle: in a batch with scouts, it runs on its own.
#[tokio::test]
async fn a_worker_continued_by_its_call_runs_alone() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 1.",
                vec![
                    write_call("w1", "a.txt", "hello\n"),
                    worker_report("Said hello.", &["a.txt"]),
                    worker_report("Nothing else.", &[]),
                ],
            ),
            ("MARK-P8", vec![Reply::Text(REPORT), Reply::Text(REPORT)]),
        ],
        vec![
            step("s1", 1),
            Reply::Calls(vec![
                (
                    "c1",
                    "task",
                    json!({ "continue": "s1", "goal": "anything else?" }),
                ),
                task_call("q1", "MARK-P8: one"),
                task_call("q2", "MARK-P8: two"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut config = workers_on(json!({}));
    config.kinds.insert(
        "scout".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    h.start(h.implement_with(&url, config, ChecklistMode::Off))
        .await;
    h.run_end().await;
    let batches = crate::agent_run::parallel::batches_of(&h.dialog_id);
    assert!(
        batches.iter().any(|b| b == &vec!["c1".to_string()]),
        "{batches:?}"
    );
    assert!(!batches.iter().any(|b| b.len() > 1), "{batches:?}");
}

/// P2.2: a subagent's command runs no longer than its time budget.
#[tokio::test]
async fn a_subagents_command_ends_with_its_time_budget() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = plan_harness(&[
        ("a.txt", "old\n"),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "sleep 60" } }"#,
        ),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                Reply::Call {
                    id: "b1",
                    name: "bash",
                    args: json!({ "command": "npm test", "timeout_seconds": 600 }),
                },
                worker_report("Ran out of time.", &[]),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Done.")],
    )
    .await;
    let started = std::time::Instant::now();
    // Three seconds: the command (a minute long) is cut at the budget.
    h.start(h.implement_with(
        &url,
        workers_on(json!({ "maxMinutes": 0.05 })),
        ChecklistMode::Off,
    ))
    .await;
    h.run_end().await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(40),
        "{:?}",
        started.elapsed()
    );
    let r = result_of(&h.messages().await, "s1").content.clone();
    assert!(r.contains("Status: partial"), "{r}");
}

#[tokio::test]
async fn a_file_changed_meanwhile_is_left_alone_when_a_worker_is_rolled_back() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                write_call("w1", "a.txt", "half\n"),
                write_call("w2", "b.txt", "half\n"),
                Reply::Slow(1_500, Box::new(bad())),
                bad(),
            ],
        )],
        vec![
            checklist_call(),
            step("s2", 2),
            checklist_closed(),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    // The user edits b.txt in the editor while the worker waits for its model.
    let b = h.project.join("b.txt");
    for _ in 0..100 {
        if std::fs::read_to_string(&b).ok().as_deref() == Some("half\n") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::write(&b, "the user's\n").unwrap();
    assert_eq!(h.run_end().await, "done");
    let failed = result_of(&h.messages().await, "s2").content.clone();
    assert!(failed.contains("a.txt put back as they were"), "{failed}");
    assert!(
        failed.contains("left as they are, because they changed outside it meanwhile: b.txt"),
        "{failed}"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("old\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("the user's\n"));
}

#[tokio::test]
async fn workers_need_a_plan_and_a_real_step() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model(vec![step("s1", 1), Reply::Text("Done.")]).await;
    h.start(h.with_subagents(&url, "Do it", checks_on(), workers_on(json!({}))))
        .await;
    h.run_end().await;
    assert!(result_of(&h.messages().await, "s1")
        .content
        .starts_with("Error: workers carry out steps of a saved plan"));

    let h = plan_harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model(vec![checklist_call(), step("s9", 9), Reply::Text("Done.")]).await;
    h.start(h.implement(&url, workers_on(json!({})))).await;
    h.run_end().await;
    let r = result_of(&h.messages().await, "s9").content.clone();
    assert!(r.contains("has no step 9; its steps are 1, 2, 3"), "{r}");
}

/*
 * Milestone E: the test writer.
 */

const TEST_PLAN: &str = "# Greeter\n\nAdd a greeter.\n\n## Steps\n\n\
    1. **Greeter**\n   - Files: `src/greet.ts`\n   - Add `export function greet(name: string): string` returning `Hello, <name>!`.\n\
    2. **Use it**\n   - Files: `src/main.ts`\n   - Print `greet(\"you\")`.\n\n## Validation\n- `npm test`\n";

fn test_writer_on() -> subagent::SubagentConfig {
    serde_json::from_value(
        json!({ "enabled": true, "kinds": { "testWriter": { "enabled": true } } }),
    )
    .unwrap()
}

fn test_task(id: &'static str) -> Reply {
    Reply::Call {
        id,
        name: "task",
        args: json!({ "kind": "testWriter" }),
    }
}

const TEST_REPORT: &str = "## Summary\nTested greet.\n\n## Test files\n- `src/greet.test.ts`: greet's output\n\n## Assumptions\nNone";

#[tokio::test]
async fn the_test_writer_writes_tests_only_and_sees_outlines_only() {
    let h = harness(&[
        (
            "src/greet.ts",
            "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
        ),
        ("src/main.ts", "console.log('hi');\n"),
        ("src/other.ts", "export const other = 1;\n"),
        ("src/helper.ts", "export const h = greet('x');\n"),
        (".rovik/plans/greeter.md", TEST_PLAN),
    ])
    .await;
    let (url, seen) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                Reply::Calls(vec![
                    ("r1", "read", json!({ "path": "src/greet.ts" })),
                    ("o1", "outline", json!({ "path": "src/greet.ts" })),
                    ("r2", "read", json!({ "path": "src/other.ts" })),
                    ("s1", "search_code", json!({ "query": "greet" })),
                    // P2.4: other spellings, other tools.
                    ("r3", "read", json!({ "path": "src/./greet.ts" })),
                    ("r4", "read", json!({ "path": "src//greet.ts" })),
                    ("c1", "callers", json!({ "symbol": "greet" })),
                    ("d1", "diagnostics", json!({ "path": "src/greet.ts" })),
                    ("n1", "neighbours", json!({ "path": "src/greet.ts" })),
                ]),
                write_call("w1", "src/greet.ts", "hacked\n"),
                write_call("w2", "src/greet.test.ts", "test('greets', () => {});\n"),
                Reply::Call { id: "b1", name: "bash", args: json!({ "command": "cargo build" }) },
                Reply::Call { id: "b2", name: "bash", args: json!({ "command": list("src") }) },
                // Bash can't read the code under test, or write a non-test file.
                Reply::Call { id: "b3", name: "bash", args: json!({ "command": "cat src/greet.ts" }) },
                Reply::Call { id: "b4", name: "bash", args: json!({ "command": "sed -i s/Hello/Bye/ src/greet.ts" }) },
                // A light check that may write: it holds the project's
                // commands for its lead, not for itself.
                Reply::Call { id: "b5", name: "bash", args: json!({ "command": "ruff check src/x.py" }) },
                Reply::Text(TEST_REPORT),
            ],
        )],
        vec![
            Reply::Call {
                id: "t1",
                name: "todo",
                args: json!({ "items": [{ "title": "Greeter", "status": "done" }, { "title": "Use it", "status": "done" }] }),
            },
            test_task("tw"),
            // E03: the lead changes the test writer's test.
            write_call("w9", "src/greet.test.ts", "test('greets politely', () => {});\n"),
            Reply::Text("Implemented; the greeting test now follows the plan's wording."),
        ],
    )
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/greeter.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    let mut request = h.request(
        &url,
        StartKind::Send { message },
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");

    let child = h.child_of("tw").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no result for {id}"))
            .content
            .clone()
    };
    assert!(
        result("r1").starts_with("Error: `src/greet.ts` is one of the files the change touches"),
        "{}",
        result("r1")
    );
    assert!(result("r1").contains("Use outline on it"));
    assert!(
        result("o1").contains("greet"),
        "outline works: {}",
        result("o1")
    );
    assert!(
        result("r2").contains("export const other"),
        "other files can be read"
    );
    let search = result("s1");
    assert!(!search.contains("src/greet.ts"), "{search}");
    for id in ["r3", "r4", "d1", "n1"] {
        assert!(
            result(id).starts_with("Error: `src/greet.ts` is one of the files the change touches"),
            "{id}: {}",
            result(id)
        );
    }
    let callers = result("c1");
    assert!(
        !callers.contains("src/greet.ts") && callers.contains("src/helper.ts"),
        "{callers}"
    );
    assert!(
        search.contains("in the files the change touches left out"),
        "{search}"
    );
    assert!(
        result("w1").starts_with("Error: `src/greet.ts` isn't a test file"),
        "{}",
        result("w1")
    );
    assert!(result("w1").contains("`src/greet.test.ts`"));
    assert!(!result("w2").starts_with("Error"), "{}", result("w2"));
    assert!(
        result("b1").starts_with("Error: `cargo build` is a heavy command"),
        "{}",
        result("b1")
    );
    assert!(result("b2").contains("greet.ts"), "{}", result("b2"));
    for id in ["b3", "b4"] {
        assert!(
            result(id).starts_with("Error: `") && result(id).contains("The test writer runs only"),
            "{}",
            result(id)
        );
    }
    assert!(!result("b5").contains("another chat"), "{}", result("b5"));
    // The run ended: its hold on the project's commands went with it.
    assert_eq!(
        crate::overlay::chats::claim_direct_writes(&h.project, "another-chat"),
        Ok(())
    );
    crate::overlay::chats::release_direct_writes("another-chat");
    assert_eq!(
        h.file("src/greet.ts").as_deref(),
        Some("export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n")
    );

    // Its brief: the plan and the outlines, never the code.
    let brief = requests_with(&seen, "# Write tests for this change")[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        brief.contains("## The plan (.rovik/plans/greeter.md)"),
        "{brief}"
    );
    assert!(brief.contains("### `src/greet.ts`"), "{brief}");
    assert!(!brief.contains("return `Hello"), "no code: {brief}");
    assert!(
        brief.contains("`src/greet.test.ts`"),
        "where tests go: {brief}"
    );

    // E03: the lead's final message lists the test-writer test it changed.
    let last = h
        .messages()
        .await
        .into_iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.content.is_empty())
        .unwrap();
    assert!(
        last.content
            .contains("Tests written by the test writer that I changed: `src/greet.test.ts`."),
        "{}",
        last.content
    );
    // The lead's prompt has the rule.
    let lead = seen.lock().unwrap()[0].clone();
    assert!(system_prompt(&lead)
        .contains("Change a test-writer test only when it contradicts the request or the plan"));
}

/*
 * Milestone F: parallel workers, in child overlays of an isolated chat.
 * They need FUSE: skipped where mounts don't work.
 */

const GROUP_PLAN: &str = "# Two pages\n\nUsers and posts.\n\n## Steps\n\n\
    1. **Contract**\n   - Files: `src/api.rs`\n   - Declare `fn users()` and `fn posts()` with stub bodies.\n\
    2. **Users**\n   - Files: `src/users.rs`, `src/api.rs` (read)\n   - Implement the users page with `fn users()`.\n   - Group: pages\n\
    3. **Posts**\n   - Files: `src/posts.rs`, `src/api.rs` (read)\n   - Implement the posts page with `fn posts()`.\n   - Group: pages\n\n\
    ## Validation\n- `npm test`\n";

fn groups_on() -> subagent::SubagentConfig {
    serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "worker": { "enabled": true } },
        "workerMinSteps": 2,
        "parallelWorkers": true,
    }))
    .unwrap()
}

fn group_task(id: &'static str) -> Reply {
    Reply::Call {
        id,
        name: "task",
        args: json!({ "kind": "workers", "group": "pages" }),
    }
}

fn all_done() -> Reply {
    Reply::Call {
        id: "t1",
        name: "todo",
        args: json!({ "items": [
            { "title": "Contract", "status": "done" },
            { "title": "Users", "status": "pending" },
            { "title": "Posts", "status": "pending" },
        ] }),
    }
}

async fn group_harness(extra: &[(&str, &str)]) -> Harness {
    let mut files = vec![
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        (".rovik/plans/greet.md", GROUP_PLAN),
    ];
    files.extend_from_slice(extra);
    let mut h = harness(&files).await;
    h.isolate().await;
    h
}

impl Harness {
    fn implement_isolated(&self, url: &str, verify: bool) -> StartRequest {
        let mut request = self.implement_with(url, groups_on(), ChecklistMode::Off);
        request.isolate = Some(true);
        request.config.as_mut().unwrap().permissions.verify_changes = verify;
        request
    }

    /// Waits until the call has its result, and returns it: a minute at
    /// most, as a group's workers take long on a busy machine.
    async fn result_when_ready(&self, call: &str) -> MessageRow {
        for _ in 0..2400 {
            if let Some(m) = self
                .messages()
                .await
                .into_iter()
                .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call))
            {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no result for {call}");
    }
}

#[tokio::test]
async fn a_group_runs_in_parallel_and_joins_into_the_chat_not_the_project() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    Reply::Calls(vec![
                        ("b1", "bash", json!({ "command": "cargo build" })),
                        ("b2", "bash", json!({ "command": list("src") })),
                        (
                            "e1",
                            "extend_scope",
                            json!({ "paths": ["src/posts.rs"], "reason": "shared" }),
                        ),
                        (
                            "b3",
                            "bash",
                            json!({ "command": "ruff check src/users.rs" }),
                        ),
                        (
                            "b4",
                            "bash",
                            json!({ "command": "python -m pytest tests/" }),
                        ),
                    ]),
                    write_call("w1", "src/users.rs", "pub fn users_page() {}\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "pub fn posts_page() {}\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Slow(2_000, Box::new(Reply::Text("Done."))),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let result = h.result_when_ready("g1").await;
    // Joined into the chat's view; the project unchanged until the chat lands.
    assert_eq!(h.file("src/users.rs").as_deref(), Some("// users\n"));
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("// posts\n"));
    let mut changed: Vec<String> = crate::overlay::chats::changes(&h.dialog_id)
        .iter()
        .map(|c| c.path().to_string())
        .collect();
    changed.sort();
    assert_eq!(changed, ["src/posts.rs", "src/users.rs"]);
    assert!(
        result
            .content
            .contains("Parallel group \"pages\" (2 workers)"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("### Step 2 (w1): Users\nLanded: src/users.rs."),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("### Step 3 (w2): Posts\nLanded: src/posts.rs."),
        "{}",
        result.content
    );
    let workers = &result.meta.as_ref().unwrap()["group"]["workers"];
    assert_eq!(workers.as_array().unwrap().len(), 2);

    // In w1's chat: no build (F05), ls runs, no one else's file (D07).
    let w1 = workers[0]["childDialogId"].as_str().unwrap().to_string();
    let work = h.messages_of(&w1).await;
    let res = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content
            .clone()
    };
    assert!(
        res("b1").starts_with("Error: `cargo build` is a heavy command"),
        "{}",
        res("b1")
    );
    assert!(res("b2").contains("users.rs"), "{}", res("b2"));
    assert!(
        res("e1").contains("`src/posts.rs` (w2) belongs to another part"),
        "{}",
        res("e1")
    );
    // F05b: a light check on its file runs (whether ruff is installed or
    // not); a test run doesn't.
    assert!(!res("b3").contains("is a heavy command"), "{}", res("b3"));
    assert!(
        res("b4").starts_with("Error: `python -m pytest tests/` is a heavy command"),
        "{}",
        res("b4")
    );
    // Each worker's brief says who else is in the group.
    let brief = requests_with(&seen, "# Your step: 2.")[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        brief.contains("You are worker w1 of the parallel group \"pages\""),
        "{brief}"
    );
    assert!(brief.contains("- w2, step 3: `src/posts.rs`"), "{brief}");
    // The lead's prompt offers the group, and its tools the workers kind.
    let lead = seen
        .lock()
        .unwrap()
        .iter()
        .find(|b| route_of(b, &["# Your step:"]).is_none())
        .cloned()
        .unwrap();
    assert!(system_prompt(&lead).contains("### Parallel groups"));

    assert_eq!(h.run_end().await, "done");
    // The chat's run finished: its changes, the workers' included, landed.
    assert_eq!(
        h.file("src/users.rs").as_deref(),
        Some("pub fn users_page() {}\n")
    );
    assert_eq!(
        h.file("src/posts.rs").as_deref(),
        Some("pub fn posts_page() {}\n")
    );
}

/// P2.6: what a worker's copy has outside its scope (a light check's side
/// effect on a sibling's file) doesn't land, and the lead is told.
#[tokio::test]
async fn a_side_effect_outside_a_workers_scope_doesnt_land() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[
        (
            "node_modules/.bin/eslint",
            "#!/bin/sh\nprintf 'from w1\\n' > src/posts.rs\n",
        ),
        ("scripts/side.js", "x\n"),
    ])
    .await;
    runnable(&h.project.join("node_modules/.bin/eslint"));
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    Reply::Call {
                        id: "b1",
                        name: "bash",
                        args: json!({ "command": "npx eslint scripts/side.js" }),
                    },
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let result = h.result_when_ready("g1").await;
    assert!(
        result.content.contains(
            "Not landed, outside its step's scope (a command's side effect?): src/posts.rs."
        ),
        "{}",
        result.content
    );
    assert!(!result.content.contains("conflict"), "{}", result.content);
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    assert_eq!(
        std::fs::read_to_string(view.join("src/posts.rs")).unwrap(),
        "from w2\n",
        "w1's side effect didn't land; w2's change did"
    );
    h.run_end().await;
}

/// Waits until a worker has written `theirs` at `path` in its copy, then
/// changes the same file in the chat's view, as the user editing it
/// meanwhile: a conflict when that worker lands.
async fn edit_while_workers_run(h: &Harness, path: &str, theirs: &str, ours: &str) {
    // Up to 30 s: the whole suite at once can hold a worker back for more
    // than the few seconds it takes alone.
    for _ in 0..3000 {
        let written = crate::overlay::chats::children_of(&h.dialog_id)
            .iter()
            .filter_map(|c| crate::overlay::chats::get(c))
            .any(|o| std::fs::read_to_string(o.view.join(path)).ok().as_deref() == Some(theirs));
        if written {
            let view = crate::overlay::chats::get(&h.dialog_id)
                .unwrap()
                .view
                .clone();
            std::fs::write(view.join(path), ours).unwrap();
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the worker never wrote {path}");
}

#[tokio::test]
async fn a_conflict_while_joining_goes_to_the_lead_with_both_versions() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    Reply::Slow(1_500, Box::new(worker_report("Posts.", &["src/posts.rs"]))),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Slow(2_000, Box::new(Reply::Text("Merged by hand."))),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/posts.rs", "from w2\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    assert!(
        result.content.contains("## The join stopped: a conflict"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("Worker w2 (step 3) changed the same lines"),
        "{}",
        result.content
    );
    // Both versions are saved for the lead to read.
    for (file, want) in [("chat", "from the user\n"), ("worker", "from w2\n")] {
        let path = format!("@checks/conflict-w2-src-posts-rs-{file}.log");
        assert!(result.content.contains(&path), "{}", result.content);
        let saved = crate::agent_run::check_output::resolve(&h.dialog_id, &path).unwrap();
        assert_eq!(std::fs::read_to_string(saved).unwrap(), want);
    }
    assert_eq!(result.meta.as_ref().unwrap()["group"]["conflict"], true);
    h.run_end().await;
}

/// P2.7: retries count against the run's workers: a group of six whose
/// every worker fails once starts at most the steps plus two retries.
#[tokio::test]
async fn retried_workers_count_against_the_limit() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut steps = String::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for n in 1..=6 {
        steps.push_str(&format!(
            "{n}. **Part {n}**\n   - Files: `src/part{n}.rs`\n   - Implement part {n} with `fn part{n}()`.\n   - Group: all\n"
        ));
        files.push((format!("src/part{n}.rs"), format!("// part {n}\n")));
    }
    let plan = format!(
        "# Six parts\n\nSix parts at once.\n\n## Steps\n\n{steps}\n## Validation\n- `npm test`\n"
    );
    let mut all: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    all.push((".rovik/plans/greet.md", plan.as_str()));
    let mut h = harness(&all).await;
    h.isolate().await;
    let routes: Vec<(&'static str, Vec<Reply>)> = (1..=6)
        .map(|n| {
            let marker: &'static str = Box::leak(format!("# Your step: {n}.").into_boxed_str());
            let path: &'static str = Box::leak(format!("src/part{n}.rs").into_boxed_str());
            (
                marker,
                // Too long for the model, twice (trimmed once): it fails,
                // without counting against the model (S12's breaker).
                vec![
                    too_long(),
                    too_long(),
                    write_call("w", path, "done\n"),
                    worker_report("Done.", &[path]),
                ],
            )
        })
        .collect();
    let (url, _) = model_routed(
        routes,
        vec![
            Reply::Call {
                id: "g1",
                name: "task",
                args: json!({ "kind": "workers", "group": "all" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    h.run_end().await;
    let started = h.events_of("subagentStarted").len();
    assert_eq!(started, 8, "six, and two retries");
    let result = result_of(&h.messages().await, "g1").content.clone();
    assert!(
        result.contains("wasn't retried: this run has already started 8 workers, the plan's 6 steps plus 2 retries"),
        "{result}"
    );
}

const GROUP3_PLAN: &str = "# Three pages\n\nUsers, posts and a feed.\n\n## Steps\n\n\
    1. **Contract**\n   - Files: `src/api.rs`\n   - Declare `fn users()` and `fn posts()` with stub bodies.\n\
    2. **Users**\n   - Files: `src/users.rs`, `src/api.rs` (read)\n   - Implement the users page with `fn users()`.\n   - Group: pages\n\
    3. **Posts**\n   - Files: `src/posts.rs`, `src/extra.rs`, `src/api.rs` (read)\n   - Implement the posts page with `fn posts()` and its helpers.\n   - Group: pages\n\
    4. **Feed**\n   - Files: `src/feed.rs`, `src/api.rs` (read)\n   - Implement the feed with `fn posts()`.\n   - Group: pages\n\n\
    ## Validation\n- `npm test`\n";

/// Round 2, P2.3: a group worker can't be granted the file of a step
/// outside its group either.
#[tokio::test]
async fn a_group_worker_cant_be_granted_a_non_member_steps_file() {
    if !crate::vfs::mount_works() {
        return;
    }
    let plan = GROUP3_PLAN.replace(
        "\n## Validation",
        "5. **Second feed**\n   - Files: `src/feed2.rs`\n   - Add a second feed there.\n\n## Validation",
    );
    let plan: &'static str = Box::leak(plan.into_boxed_str());
    let mut h = harness(&[
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/extra.rs", "// extra\n"),
        ("src/feed.rs", "// feed\n"),
        ("src/feed2.rs", "// feed2\n"),
        (".rovik/plans/greet.md", plan),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    Reply::Call {
                        id: "e1",
                        name: "extend_scope",
                        args: json!({ "paths": ["src/feed2.rs"], "reason": "shared" }),
                    },
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("w3", "src/feed.rs", "feed\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    h.result_when_ready("g1").await;
    h.run_end().await;
    let w1 = h.child_of("g1#w1").await;
    let granted = h
        .messages_of(&w1.id)
        .await
        .into_iter()
        .find(|m| m.tool_call_id.as_deref() == Some("e1"))
        .map(|m| m.content)
        .unwrap_or_default();
    assert!(granted.contains("is step 5's to change"), "{granted}");
}

/// Round 2, P1.2: the conflict of `a_conflict_lands_the_rest_and_keeps_every_copy`,
/// the run over: w2's copy (its posts.rs) and w3's (feed.rs) are kept.
async fn kept_after_a_conflict() -> Harness {
    let mut h = harness(&[
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/extra.rs", "// extra\n"),
        ("src/feed.rs", "// feed\n"),
        (".rovik/plans/greet.md", GROUP3_PLAN),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    write_call("w3", "src/extra.rs", "extra from w2\n"),
                    Reply::Slow(
                        1_500,
                        Box::new(worker_report("Posts.", &["src/posts.rs", "src/extra.rs"])),
                    ),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("w4", "src/feed.rs", "feed from w3\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/posts.rs", "from w2\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    assert!(
        result.content.contains("## The join stopped: a conflict"),
        "{}",
        result.content
    );
    h.run_end().await;
    h
}

/// Round 2, P6.8: after a conflict, the snapshot blobs left are exactly
/// the saved group's: none for the paths that didn't land.
#[tokio::test]
async fn unlanded_snapshot_blobs_are_forgotten() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = kept_after_a_conflict().await;
    let groups = crate::overlay::chats::saved_groups(&h.dialog_id);
    let referenced: usize = groups
        .iter()
        .flat_map(|g| &g.before)
        .map(|(_, b)| match b {
            crate::overlay::chats::Before::File { base, .. } => 1 + usize::from(base.is_some()),
            _ => 0,
        })
        .sum();
    assert_eq!(
        crate::overlay::chats::group_blob_count(&h.dialog_id),
        referenced,
        "{groups:?}"
    );
}

/// Round 2, P2.6: a kept copy whose plan step can't be read any more when
/// it's done (the plan was changed meanwhile) lands nothing, and says why.
#[tokio::test]
async fn a_kept_copy_whose_step_cant_be_read_doesnt_land_everything() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = kept_after_a_conflict().await;
    let w3 = h.child_of("g1#w3").await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 4.",
            vec![Reply::Slow(
                1_500,
                Box::new(worker_report("Nothing more to do.", &["src/feed.rs"])),
            )],
        )],
        vec![
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w3", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_isolated(&url, false)).await;
    // While it works, the plan loses its step 4.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let cut = GROUP3_PLAN.split("4. **Feed**").next().unwrap().to_string()
        + "\n## Validation\n- `npm test`\n";
    let plan_in_view =
        crate::overlay::chats::get(&h.dialog_id).map(|o| o.view.join(".rovik/plans/greet.md"));
    std::fs::write(h.project.join(".rovik/plans/greet.md"), &cut).unwrap();
    if let Some(p) = plan_in_view {
        let _ = std::fs::write(p, &cut);
    }
    let continued = h.result_when_ready("c1").await;
    h.run_end().await;
    assert!(
        continued
            .content
            .contains("the scope it may land in can't be determined"),
        "{}",
        continued.content
    );
    // Nothing of it reached the chat's files (nor, through them, the
    // project's).
    assert_eq!(h.file("src/feed.rs").as_deref(), Some("// feed\n"));
    if crate::overlay::chats::get(&h.dialog_id).is_some() {
        assert_eq!(in_view(&h, "src/feed.rs").as_deref(), Some("// feed\n"));
    }
    assert!(continued.content.contains("Its copy is kept"));
    assert!(continued
        .content
        .contains(&subagent::handle(subagent::Kind::Worker, &w3.id)));
}

/// Round 2, P1.2: after the app restarts, `continue` on a kept copy goes
/// on in that copy (opened again from disk) and lands it.
#[tokio::test]
async fn a_kept_copy_is_landed_by_continue_after_a_restart() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = kept_after_a_conflict().await;
    let w3 = h.child_of("g1#w3").await;
    crate::overlay::chats::forget_child_as_if_restarted(&w3.id);
    assert!(crate::overlay::chats::get(&w3.id).is_none());
    let (url, _) = model_routed(
        vec![(
            "# Your step: 4.",
            vec![worker_report("Nothing more to do.", &["src/feed.rs"])],
        )],
        vec![
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w3", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_isolated(&url, false)).await;
    let continued = h.result_when_ready("c1").await;
    h.run_end().await;
    assert!(
        continued.content.contains("Its copy landed: src/feed.rs."),
        "{}",
        continued.content
    );
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    assert_eq!(
        std::fs::read_to_string(view.join("src/feed.rs"))
            .ok()
            .as_deref(),
        Some("feed from w3\n")
    );
    assert!(!crate::overlay::chats::children_of(&h.dialog_id).contains(&w3.id));
}

/// Round 2, P1.2: once the lead has merged the conflicting file, "land it"
/// lands the copy's other files and keeps the lead's merge, instead of
/// conflicting again for ever.
#[tokio::test]
async fn a_kept_copy_lands_after_the_lead_merges_the_conflict() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = kept_after_a_conflict().await;
    let w2 = h.child_of("g1#w2").await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 3.",
            vec![worker_report(
                "Nothing more to do.",
                &["src/posts.rs", "src/extra.rs"],
            )],
        )],
        vec![
            write_call("l1", "src/posts.rs", "from the user\nfrom w2\n"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w2", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_isolated(&url, false)).await;
    let continued = h.result_when_ready("c1").await;
    h.run_end().await;
    assert!(
        continued.content.contains("Its copy landed"),
        "{}",
        continued.content
    );
    assert!(
        continued
            .content
            .contains("Its version of `src/posts.rs` isn't landed"),
        "{}",
        continued.content
    );
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    assert_eq!(
        std::fs::read_to_string(view.join("src/posts.rs"))
            .ok()
            .as_deref(),
        Some("from the user\nfrom w2\n"),
        "the lead's merge stays"
    );
    assert!(
        crate::overlay::chats::get(&w2.id).is_none(),
        "landed: the copy is gone"
    );
}

/// Round 2, P1.2: the only member that landed anything conflicted on one
/// file: the group is still saved (Redo, owners for the repair loop), with
/// what its other file was.
#[tokio::test]
async fn a_conflict_in_the_only_landing_member_still_saves_the_group() {
    if !crate::vfs::mount_works() {
        return;
    }
    let plan = GROUP3_PLAN.replace(
        "   - Files: `src/users.rs`, `src/api.rs` (read)\n",
        "   - Files: `src/users.rs`, `src/api_users.rs`, `src/api.rs` (read)\n",
    );
    let plan: &'static str = Box::leak(plan.into_boxed_str());
    let mut h = harness(&[
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/api_users.rs", "// api users\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/extra.rs", "// extra\n"),
        ("src/feed.rs", "// feed\n"),
        (".rovik/plans/greet.md", plan),
    ])
    .await;
    h.isolate().await;
    let blocked = "## Summary\nStuck.\n\n## Changed files\nNone\n\n## Open questions\nNone\n\n## Blocked\nThe posts API isn't there.";
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users from w1\n"),
                    write_call("w2", "src/api_users.rs", "route from w1\n"),
                    Reply::Slow(
                        1_000,
                        Box::new(worker_report(
                            "Users.",
                            &["src/users.rs", "src/api_users.rs"],
                        )),
                    ),
                ],
            ),
            ("# Your step: 3.", vec![Reply::Text(blocked)]),
            ("# Your step: 4.", vec![Reply::Text(blocked)]),
        ],
        vec![
            // The chat's own version of the file the group then lands.
            write_call("l0", "src/api_users.rs", "// api users, by the lead\n"),
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/users.rs", "users from w1\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    h.run_end().await;
    assert!(
        result
            .content
            .contains("Its other files landed: src/api_users.rs."),
        "{}",
        result.content
    );
    let groups = crate::overlay::chats::saved_groups(&h.dialog_id);
    assert_eq!(groups.len(), 1, "{}", result.content);
    assert!(
        groups[0]
            .before
            .iter()
            .any(|(p, _)| p == "src/api_users.rs"),
        "{:?}",
        groups[0].before
    );
    // What api_users.rs was (the lead's version) is kept; nothing of the
    // conflicting users.rs, which didn't land.
    let referenced: usize = groups[0]
        .before
        .iter()
        .map(|(_, b)| match b {
            crate::overlay::chats::Before::File { base, .. } => 1 + usize::from(base.is_some()),
            _ => 0,
        })
        .sum();
    assert!(referenced > 0, "{:?}", groups[0].before);
    assert_eq!(
        crate::overlay::chats::group_blob_count(&h.dialog_id),
        referenced
    );
}

/// P1.2: a conflict (the user edited the file meanwhile) lands the rest of
/// the worker, keeps its copy and the later workers' copies (none
/// discarded), and saves both versions; a later worker lands when the
/// lead continues it.
#[tokio::test]
async fn a_conflict_lands_the_rest_and_keeps_every_copy() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/extra.rs", "// extra\n"),
        ("src/feed.rs", "// feed\n"),
        (".rovik/plans/greet.md", GROUP3_PLAN),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    write_call("w3", "src/extra.rs", "extra from w2\n"),
                    Reply::Slow(
                        1_500,
                        Box::new(worker_report("Posts.", &["src/posts.rs", "src/extra.rs"])),
                    ),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("w4", "src/feed.rs", "feed from w3\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                    // Continued once the conflict is resolved: it lands.
                    worker_report("Nothing more to do.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w3", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/posts.rs", "from w2\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    let text = result.content.clone();
    assert!(
        text.contains("Its other files landed: src/extra.rs."),
        "{text}"
    );
    assert!(
        text.contains("@checks/conflict-w2-src-posts-rs-chat.log")
            && text.contains("@checks/conflict-w2-src-posts-rs-worker.log"),
        "{text}"
    );
    assert!(
        text.contains("Not landed yet: the join stopped at the conflict below. Its copy is kept"),
        "{text}"
    );
    let saved = crate::agent_run::check_output::resolve(
        &h.dialog_id,
        "@checks/conflict-w2-src-posts-rs-worker.log",
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "from w2\n");
    // w2's copy is kept (its posts.rs waits there).
    let w2 = h.child_of("g1#w2").await;
    assert!(
        crate::overlay::chats::get(&w2.id).is_some(),
        "w2's copy is kept"
    );
    let continued = h.result_when_ready("c1").await;
    assert!(
        continued.content.contains("Its copy landed: src/feed.rs."),
        "{}",
        continued.content
    );
    // In the chat's view (the run ends with its checklist open, unlanded).
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    let in_view = |p: &str| std::fs::read_to_string(view.join(p)).ok();
    assert_eq!(in_view("src/extra.rs").as_deref(), Some("extra from w2\n"));
    assert_eq!(in_view("src/feed.rs").as_deref(), Some("feed from w3\n"));
    assert_eq!(in_view("src/posts.rs").as_deref(), Some("from the user\n"));
    h.run_end().await;
    // P6.8: the next run runs the group again: w2's stale copy, still
    // kept, is discarded first, never landed over the new work.
    assert!(crate::overlay::chats::children_of(&h.dialog_id).contains(&w2.id));
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("v1", "src/users.rs", "users again\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("v2", "src/posts.rs", "posts again\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("v3", "src/feed.rs", "feed again\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![group_task("g2"), Reply::Text("Done again.")],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_isolated(&url, false)).await;
    let again = h.result_when_ready("g2").await;
    assert!(
        again
            .content
            .contains("Copies kept from an earlier run of these steps were discarded, not landed:")
            && again.content.contains("(step 3)"),
        "{}",
        again.content
    );
    assert!(crate::overlay::chats::get(&w2.id).is_none());
    assert!(!crate::overlay::chats::children_of(&h.dialog_id).contains(&w2.id));
    // This run lands as it ends, right after, and the view closes with it:
    // the new work is in the project.
    h.run_end().await;
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts again\n"));
}

/// A group run whose step 3 (w2) fails half way through landing
/// `src/extra.rs` into the chat's view, the failure still injected. The
/// failure hooks are process-wide: the lock comes back with it, to hold
/// for the whole test.
#[allow(clippy::await_holding_lock)]
async fn half_landed_group() -> (Harness, std::sync::MutexGuard<'static, ()>) {
    use crate::overlay::land;
    let hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut h = harness(&[
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/extra.rs", "// extra\n"),
        ("src/feed.rs", "// feed\n"),
        (".rovik/plans/greet.md", GROUP3_PLAN),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts from w2\n"),
                    write_call("w3", "src/extra.rs", "extra from w2\n"),
                    worker_report("Posts.", &["src/posts.rs", "src/extra.rs"]),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("w4", "src/feed.rs", "feed\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![
            Reply::Slow(800, Box::new(all_done())),
            group_task("g1"),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    // The chat's view is open now: w2's extra.rs fails to land, and so
    // does putting it back.
    let view = loop {
        if let Some(o) = crate::overlay::chats::get(&h.dialog_id) {
            break o.view.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    *land::FAIL_WRITING.lock().unwrap() = Some((view.clone(), "src/extra.rs".into()));
    land::FAIL_WRITING_ALWAYS.store(true, std::sync::atomic::Ordering::SeqCst);
    (h, hook)
}

fn stop_failing_landings() {
    use crate::overlay::land;
    land::FAIL_WRITING_ALWAYS.store(false, std::sync::atomic::Ordering::SeqCst);
    *land::FAIL_WRITING.lock().unwrap() = None;
}

/// Whether a landing into the chat's view stopped half way (its journal).
fn landing_interrupted(dialog_id: &str) -> bool {
    let data = crate::overlay::chats::data_dir().unwrap();
    crate::overlay::land::interrupted_into(
        &data,
        dialog_id,
        &crate::overlay::chats::mountpoint(dialog_id),
    )
}

fn land_refused_unfinished(dialog_id: &str) -> bool {
    matches!(
        crate::overlay::chats::land_now(dialog_id, false),
        Err(crate::overlay::land::LandError::Failed(ref e)) if e.contains("couldn't be finished")
    )
}

/// P1.3: a worker's landing that fails half way keeps its copy and its
/// journal; Land refuses until the landing is finished, then lands it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_landing_that_fails_half_way_keeps_the_copy_and_holds_land() {
    if !crate::vfs::mount_works() {
        return;
    }
    let (h, _hook) = half_landed_group().await;
    let result = h.result_when_ready("g1").await;
    h.run_end().await;
    let text = result.content.clone();
    assert!(
        text.contains("### Step 3 (w2): Posts\nIt couldn't land ("),
        "{text}"
    );
    assert!(text.contains("Its copy is kept"), "{text}");
    let w2 = h.child_of("g1#w2").await;
    assert!(
        crate::overlay::chats::get(&w2.id).is_some(),
        "the copy is kept"
    );
    // Land waits for the landing to be finished.
    assert!(land_refused_unfinished(&h.dialog_id));
    stop_failing_landings();
    crate::overlay::chats::land_now(&h.dialog_id, false).unwrap();
    assert_eq!(h.file("src/extra.rs").as_deref(), Some("extra from w2\n"));
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts from w2\n"));
}

/// Round 2, P1.3: running the group again discards the kept copies of
/// its steps, and with them their landing journals: a landing that
/// stopped half way is finished first, and while it can't be the group
/// doesn't start, so Land still refuses.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_rerun_of_the_group_finishes_an_interrupted_landing_before_discarding_stale_copies() {
    if !crate::vfs::mount_works() {
        return;
    }
    let (h, _hook) = half_landed_group().await;
    h.result_when_ready("g1").await;
    h.run_end().await;
    let w2 = h.child_of("g1#w2").await;
    assert!(landing_interrupted(&h.dialog_id));
    let (url, _) = model_routed(
        Vec::new(),
        vec![group_task("g2"), Reply::Text("Done again.")],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_isolated(&url, false)).await;
    let again = h.result_when_ready("g2").await;
    h.run_end().await;
    assert!(
        again.content.starts_with("Error: the group can't start"),
        "{}",
        again.content
    );
    assert!(
        crate::overlay::chats::get(&w2.id).is_some(),
        "the copy is kept"
    );
    assert!(landing_interrupted(&h.dialog_id), "its journal is kept");
    assert!(land_refused_unfinished(&h.dialog_id));
    stop_failing_landings();
    crate::overlay::chats::land_now(&h.dialog_id, false).unwrap();
    assert_eq!(h.file("src/extra.rs").as_deref(), Some("extra from w2\n"));
}

#[tokio::test]
async fn after_the_join_the_checks_run_once_and_name_the_worker() {
    if !crate::vfs::mount_works() {
        return;
    }
    let check = "const fs = require('fs');\n\
        if (fs.readFileSync('src/posts.rs', 'utf8').includes('bad')) {\n\
          console.error('error[E0425]: cannot find value `x`\\n --> src/posts.rs:1:5');\n  console.error('noise\\n'.repeat(3000));\n  process.exit(1);\n}\n";
    let h = group_harness(&[
        ("check.js", check),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
    ])
    .await;
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    // The first w1 fails; its retry (F09) succeeds.
                    bad(),
                    bad(),
                    write_call("w1", "src/users.rs", "pub fn users_page() {}\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            Reply::Text("I'll leave it."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, true)).await;
    h.run_end().await;
    let messages = h.messages().await;
    let result = result_of(&messages, "g1").content.clone();
    assert!(
        result.contains("### Step 2 (w1): Users\nStatus: failed. Its copy was discarded"),
        "{result}"
    );
    assert!(
        result.contains("### Step 2 (w1'): Users\nLanded: src/users.rs."),
        "the retry: {result}"
    );
    assert!(
        result.contains("- src/posts.rs (step 3, worker w2)"),
        "{result}"
    );
    // One check run after the join (the mid-run record), before the end's.
    let mid: Vec<_> = messages
        .iter()
        .filter(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["verification"]["midRun"] == true)
        })
        .collect();
    assert_eq!(mid.len(), 1);
    assert_eq!(
        mid[0].meta.as_ref().unwrap()["verification"]["attribution"][0]["owner"],
        "step 3, worker w2"
    );
    // Its whole output is saved under its own name, not the round's.
    let saved = mid[0].meta.as_ref().unwrap()["verification"]["checks"][0]["savedOutput"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(saved.starts_with("@checks/round-0-midrun-"), "{saved}");
    // P4.7: the end's checks ran after it, and it's still readable.
    assert!(messages.iter().any(|m| m.meta.as_ref().is_some_and(|m| {
        m["verification"].is_object() && m["verification"]["midRun"] != true
    })));
    let path = crate::agent_run::check_output::resolve(&h.dialog_id, &saved).unwrap();
    assert!(
        std::fs::read_to_string(path).is_ok_and(|t| t.contains("error[E0425]")),
        "the mid-run output was removed by the end's checks"
    );
}

#[tokio::test]
async fn a_step_that_fails_twice_goes_to_the_lead() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            ("# Your step: 2.", vec![bad(), bad(), bad(), bad()]),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("I'll do step 2.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    h.run_end().await;
    let result = result_of(&h.messages().await, "g1").content.clone();
    assert!(
        result.contains("Step 2 failed twice: do it yourself"),
        "{result}"
    );
    assert!(
        result.contains("### Step 3 (w2): Posts\nLanded: src/posts.rs."),
        "{result}"
    );
}

#[tokio::test]
async fn a_group_worker_continued_works_in_the_chat_within_its_old_scope() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Next run fixes it."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, false);
    // Keep the chat's changes in its overlay: land only when told.
    request.config.as_mut().unwrap().permissions.checklist = ChecklistMode::Off;
    h.start(request).await;
    h.run_end().await;
    let messages = h.messages().await;
    let w2 = result_of(&messages, "g1").meta.as_ref().unwrap()["group"]["workers"][1]
        ["childDialogId"]
        .as_str()
        .unwrap()
        .to_string();
    let handle: &'static str =
        Box::leak(subagent::handle(subagent::Kind::Worker, &w2).into_boxed_str());
    // F07: the lead continues w2 to fix its step, now in the chat's view.
    let (url, _) = model_routed(
        vec![(
            // Its chat starts with its step's brief; the follow-up is later.
            "# Your step: 3.",
            vec![
                write_call("w3", "src/posts.rs", "posts, fixed\n"),
                write_call("w4", "src/users.rs", "not mine\n"),
                worker_report("Fixed.", &["src/posts.rs"]),
            ],
        )],
        vec![
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": handle, "goal": "posts.rs fails: fix it" }),
            },
            Reply::Text("Fixed."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    let mut request = h.request(
        &url,
        Harness::send("Fix the posts page"),
        Permissions {
            allow_any_command: true,
            checklist: ChecklistMode::Off,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = groups_on();
    request.isolate = Some(true);
    h.start(request).await;
    h.run_end().await;
    let work = h.messages_of(&w2).await;
    let res = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content
            .clone()
    };
    assert!(!res("w3").starts_with("Error"), "{}", res("w3"));
    assert!(
        res("w4").contains("outside your write scope"),
        "{}",
        res("w4")
    );
    assert_eq!(
        h.file("src/posts.rs").as_deref(),
        Some("posts, fixed\n"),
        "landed with the chat"
    );
}

/*
 * E04: the red check, on the project (the code before the change) of an
 * isolated chat. Go: one test package per file.
 */

#[tokio::test]
async fn the_red_check_flags_a_test_that_passes_on_the_old_code() {
    if !crate::vfs::mount_works() {
        return;
    }
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_err()
    {
        return;
    }
    let mut h = harness(&[
        ("go.mod", "module x\n\ngo 1.21\n"),
        ("a/a.go", "package a\n\nfunc A() int { return 1 }\n"),
        ("b/b.go", "package b\n\nfunc B() int { return 1 }\n"),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                write_call("t1", "a/a_test.go", "package a\n\nimport \"testing\"\n\nfunc TestA(t *testing.T) { if A() != 1 { t.Fatal(A()) } }\n"),
                write_call("t2", "b/b_test.go", "package b\n\nimport \"testing\"\n\nfunc TestB(t *testing.T) { if B() != 2 { t.Fatal(B()) } }\n"),
                Reply::Text("## Summary\nTests.\n\n## Test files\n- `a/a_test.go`\n- `b/b_test.go`\n\n## Assumptions\nNone"),
            ],
        )],
        vec![
            write_call("w1", "b/b.go", "package b\n\nfunc B() int { return 2 }\n"),
            Reply::Call { id: "tw", name: "task", args: json!({ "kind": "testWriter" }) },
            Reply::Text("Done."),
        ],
    )
    .await;
    let config: subagent::SubagentConfig = serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "testWriter": { "enabled": true } },
        "redCheck": true,
    }))
    .unwrap();
    let mut request = h.request(
        &url,
        Harness::send("Make B return 2"),
        Permissions {
            allow_any_command: true,
            checklist: ChecklistMode::Off,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = config;
    request.isolate = Some(true);
    h.start(request).await;
    // `go test` builds Go's standard library first on a fresh machine.
    h.run_end_within(Duration::from_secs(120)).await;
    let r = result_of(&h.messages().await, "tw").content.clone();
    assert!(
        r.contains("Red check: `a/a_test.go` already passes on the code before the change"),
        "{r}"
    );
    assert!(!r.contains("`b/b_test.go` already"), "{r}");
}

/*
 * R04–R07: the repair loop after a parallel group. The check fails on
 * "bad" in a page, with errors pointing at it: one for users.rs, two for
 * posts.rs (so posts' cause explains more), one for api.rs.
 */

const REPAIR_CHECK: &str = "const fs = require('fs');\n\
    let failed = false;\n\
    if (process.env.NOISE) { console.log('noise line for the saved output\\n'.repeat(400)); }\n\
    if (fs.readFileSync('src/api.rs', 'utf8').includes('bad')) {\n\
      console.error('error[E0412]: cannot find type `Api`\\n --> src/api.rs:1:1\\n'); failed = true;\n}\n\
    if (fs.readFileSync('src/users.rs', 'utf8').includes('bad')) {\n\
      console.error('error[E0425]: cannot find value `u`\\n --> src/users.rs:1:1\\n'); failed = true;\n}\n\
    if (fs.readFileSync('src/posts.rs', 'utf8').includes('bad')) {\n\
      console.error('error[E0425]: cannot find value `p`\\n --> src/posts.rs:1:1\\n');\n\
      console.error('error[E0308]: mismatched types\\n --> src/posts.rs:2:1\\n'); failed = true;\n}\n\
    if (failed) process.exit(1);\n";

async fn repair_harness(noise: bool) -> Option<Harness> {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm isn't installed: skipped");
        return None;
    }
    let script = if noise {
        r#"{ "name": "t", "scripts": { "test": "NOISE=1 node check.js" } }"#
    } else {
        r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#
    };
    Some(group_harness(&[("check.js", REPAIR_CHECK), ("package.json", script)]).await)
}

/// The end-of-run verification records (not F06's mid-run one).
fn end_records(messages: &[MessageRow]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .filter(|v| v["midRun"] != true)
        .collect()
}

fn statuses(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .map(|r| r["status"].as_str().unwrap_or("").to_string())
        .collect()
}

/// The first request whose body has all of `parts`.
fn first_request_with(seen: &Arc<Mutex<Vec<Value>>>, parts: &[&str]) -> Option<usize> {
    seen.lock().unwrap().iter().position(|b| {
        let text = b.to_string();
        parts.iter().all(|p| text.contains(p))
    })
}

#[tokio::test]
async fn two_causes_go_to_their_workers_in_order_then_one_check_run() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    // R04: resumed to fix its cause.
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, true)).await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let records = end_records(&messages);
    // One failing round, the workers' fixes, one check run: green.
    assert_eq!(statuses(&records), ["failed", "passed"]);
    let repair = &records[0]["repair"];
    assert_eq!(repair["status"], "recheck", "{repair}");
    assert_eq!(repair["parallel"], false);
    let causes = repair["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2, "{repair}");
    // Most failures first: posts (two), then users (one).
    assert_eq!(causes[0]["owner"]["label"], "w2");
    assert_eq!(causes[1]["owner"]["label"], "w1");
    for c in causes {
        assert_eq!(c["fixer"]["status"], "done", "{c}");
    }
    let w2 =
        first_request_with(&seen, &["# Fix from the lead's checks", "# Your step: 3."]).unwrap();
    let w1 =
        first_request_with(&seen, &["# Fix from the lead's checks", "# Your step: 2."]).unwrap();
    assert!(w2 < w1, "the fixers run in order, one at a time");
    // The lead never got a failure report: the owners fixed it all.
    assert!(!messages
        .iter()
        .any(|m| m.content.contains("Your changes are not done yet")));
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts, fixed\n"));
}

/// P3.10: fixers draw on the run's shared subagent limit (D11), not the
/// plan's workers: with one place left, the second fixer doesn't start.
#[tokio::test]
async fn fixers_count_against_the_runs_subagent_limit() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            Reply::Text("I'll leave the rest."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    request.config.as_mut().unwrap().subagents.max_per_run = Some(1);
    h.start(request).await;
    h.run_end().await;
    let records = end_records(&h.messages().await);
    let causes = records[0]["repair"]["causes"].as_array().unwrap().clone();
    let fixers: Vec<&Value> = causes.iter().map(|c| &c["fixer"]).collect();
    assert_eq!(fixers[0]["status"], "done", "{causes:?}");
    assert_eq!(fixers[1]["status"], "not started", "{causes:?}");
    assert!(
        fixers[1]["said"]
            .as_str()
            .unwrap_or("")
            .contains("this run has already started 1 subagents"),
        "{causes:?}"
    );
}

/// Round 2 (§4.2): fixers and scouts share the run's limit: with 3, two
/// scouts leave one place, so the first fixer starts and the second
/// doesn't.
#[tokio::test]
async fn fixers_share_the_limit_with_scouts() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, _) = model_routed(
        vec![
            ("MARK-S1", vec![Reply::Text(REPORT)]),
            ("MARK-S2", vec![Reply::Text(REPORT)]),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            task("s1", "MARK-S1: where are the pages?"),
            task("s2", "MARK-S2: where is the router?"),
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            Reply::Text("I'll leave the rest."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let subagents = &mut request.config.as_mut().unwrap().subagents;
    subagents.max_per_run = Some(3);
    subagents.kinds.insert(
        "scout".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    h.start(request).await;
    h.run_end().await;
    let messages = h.messages().await;
    for call in ["s1", "s2"] {
        assert!(
            !result_of(&messages, call).content.starts_with("Error"),
            "{}",
            result_of(&messages, call).content
        );
    }
    let records = end_records(&messages);
    let causes = records[0]["repair"]["causes"].as_array().unwrap().clone();
    let fixers: Vec<&Value> = causes.iter().map(|c| &c["fixer"]).collect();
    assert_eq!(fixers[0]["status"], "done", "{causes:?}");
    assert_eq!(fixers[1]["status"], "not started", "{causes:?}");
    assert!(
        fixers[1]["said"]
            .as_str()
            .unwrap_or("")
            .contains("this run has already started 3 subagents"),
        "{causes:?}"
    );
}

/// Round 2 (§4.2): the subagents a run started count on after its checks
/// waited for approval: with 3 per run, two before the wait leave one.
#[tokio::test]
async fn the_ledger_survives_approval() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = harness(&[
        ("a.txt", "x\n"),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node -e \"process.exit(require('fs').existsSync('ok.txt') ? 0 : 1)\"" } }"#,
        ),
    ])
    .await;
    let (url, _) = model_routed(
        vec![
            ("MARK-L1", vec![Reply::Text(REPORT)]),
            ("MARK-L2", vec![Reply::Text(REPORT)]),
            ("MARK-L3", vec![Reply::Text(REPORT)]),
            ("MARK-L4", vec![Reply::Text(REPORT)]),
        ],
        vec![
            task("s1", "MARK-L1: where?"),
            task("s2", "MARK-L2: where?"),
            write_call("w1", "x.txt", "x\n"),
            Reply::Text("Done."),
            // After "Run checks": the checks failed.
            task("s3", "MARK-L3: why?"),
            task("s4", "MARK-L4: and?"),
            write_call("w2", "ok.txt", "ok\n"),
            Reply::Text("Fixed."),
        ],
    )
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    let mut subagents = scouts_on(json!({}));
    subagents.max_per_run = Some(3);
    h.start(h.with_subagents(
        &url,
        "Look, then write",
        permissions.clone(),
        subagents.clone(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let mut outcome = String::new();
    for _ in 0..2 {
        let record = h.messages().await.last().unwrap().id.clone();
        h.events.lock().unwrap().clear();
        let mut request = h.request(
            &url,
            StartKind::RunChecks { record_id: record },
            permissions.clone(),
        );
        request.config.as_mut().unwrap().subagents = subagents.clone();
        h.start(request).await;
        outcome = h.run_end().await;
        if outcome != "verify-needs-approval" {
            break;
        }
    }
    assert_eq!(outcome, "verified");
    let messages = h.messages().await;
    for call in ["s1", "s2", "s3"] {
        let r = &result_of(&messages, call).content;
        assert!(!r.starts_with("Error"), "{call}: {r}");
    }
    assert!(
        result_of(&messages, "s4")
            .content
            .contains("this run has already started 3 subagents"),
        "{}",
        result_of(&messages, "s4").content
    );
}

#[tokio::test]
async fn three_failing_rounds_give_up_and_redo_runs_the_steps_one_at_a_time() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    // Its fix doesn't fix it, and adds a file of its own;
                    // and changes one the chat changed in an earlier run.
                    write_call("w2f", "src/posts.rs", "still bad\n"),
                    write_call("w2g", "src/posts_extra.rs", "extra\n"),
                    Reply::Call {
                        id: "w2e",
                        name: "extend_scope",
                        args: json!({ "paths": ["src/util.rs"], "reason": "a helper" }),
                    },
                    write_call("w2h", "src/util.rs", "util by the fixer\n"),
                    worker_report("Fixed, I think.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            // Round 2 made no progress: the lead's alone now.
            write_call("l1", "src/posts.rs", "bad again\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    // P1.4: an earlier run of the chat changed src/util.rs (new in its view).
    let chat = crate::overlay::chats::open(&h.dialog_id, &h.project, Box::new(|_, _| {})).unwrap();
    std::fs::write(chat.view.join("src/util.rs"), "util from run 1\n").unwrap();
    chat.fs.flush();
    h.start(h.implement_isolated(&url, true)).await;
    assert_eq!(h.run_end().await, "verify-gave-up");
    let messages = h.messages().await;
    let records = end_records(&messages);
    assert_eq!(statuses(&records), ["failed", "failed", "gave-up"]);
    assert_eq!(records[0]["repair"]["status"], "recheck");
    // R06: after no progress, no worker is resumed: the lead fixes it all.
    let second = &records[1]["repair"];
    assert!(second["leadOnly"].is_string(), "{second}");
    assert!(second["causes"][0].get("fixer").is_none(), "{second}");
    let report = messages
        .iter()
        .find(|m| m.content.contains("## Root causes"))
        .expect("the lead's report names the causes");
    assert!(
        report.content.contains("the lead fixes everything"),
        "{}",
        report.content
    );
    assert!(
        first_request_with(&seen, &["Fix from the lead's checks (round 2"]).is_none()
            && !seen
                .lock()
                .unwrap()
                .iter()
                .any(|b| b.to_string().contains("round 2). These failures")),
        "no fixer in round 2"
    );
    // The gave-up record: what's left.
    let last = &records[2];
    assert!(
        last["remaining"].as_array().is_some_and(|r| r
            .iter()
            .any(|k| k.as_str().unwrap().contains("src/posts.rs"))),
        "{last}"
    );
    // Nothing landed.
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("// posts\n"));
    assert_eq!(h.file("src/users.rs").as_deref(), Some("// users\n"));

    // Redo sequentially: the merge is thrown away; the steps run one at a
    // time, by step workers.
    let saved = crate::overlay::chats::saved_groups(&h.dialog_id);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].steps, vec![2, 3]);
    // The fixer's own file is among what Redo puts back.
    assert!(
        saved[0]
            .before
            .iter()
            .any(|(p, _)| p == "src/posts_extra.rs"),
        "{:?}",
        saved[0].before
    );
    let fixed = std::fs::read_to_string(chat.view.join("src/util.rs")).unwrap();
    assert_eq!(fixed, "util by the fixer\n", "the fixer changed it");
    assert!(crate::overlay::chats::group_blob_count(&h.dialog_id) > 0);
    let request = crate::overlay::chats::redo_group(&h.dialog_id, "pages").unwrap();
    assert!(request.contains("Carry out its steps (2, 3)"), "{request}");
    // P6.8: what the group's paths were is spent once it's redone.
    assert_eq!(crate::overlay::chats::group_blob_count(&h.dialog_id), 0);
    assert!(crate::overlay::chats::redo_group(&h.dialog_id, "pages")
        .unwrap_err()
        .contains("redone already"));
    // P1.4: the chat's own version from before the group, not the
    // project's (it had none).
    assert_eq!(
        std::fs::read_to_string(chat.view.join("src/util.rs"))
            .ok()
            .as_deref(),
        Some("util from run 1\n")
    );
    assert!(
        !crate::overlay::chats::changes(&h.dialog_id)
            .iter()
            .any(|c| c.path() == "src/posts_extra.rs"),
        "the fixer's file went with the group"
    );
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("s2w", "src/users.rs", "users, again\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("s3w", "src/posts.rs", "posts, again\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            group_task("g2"),
            step("s2", 2),
            step("s3", 3),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    let mut next = h.request(
        &url,
        Harness::send(&request),
        Permissions {
            allow_any_command: true,
            checklist: ChecklistMode::Off,
            ..quiet_permissions()
        },
    );
    next.config.as_mut().unwrap().subagents = groups_on();
    next.isolate = Some(true);
    h.start(next).await;
    h.run_end().await;
    let messages = h.messages().await;
    assert!(
        result_of(&messages, "g2")
            .content
            .contains("being redone one step at a time"),
        "{}",
        result_of(&messages, "g2").content
    );
    for call in ["s2", "s3"] {
        let r = &result_of(&messages, call).content;
        assert!(!r.starts_with("Error"), "{r}");
    }
    // The workers started from the code before the group.
    let brief = seen
        .lock()
        .unwrap()
        .iter()
        .map(|b| b.to_string())
        .find(|b| b.contains("# Your step: 3."))
        .unwrap();
    assert!(!brief.contains("still bad") && !brief.contains("bad again"));
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts, again\n"));
}

const TRIAGE_TWO_CAUSES: &str = "## Root cause\n`p` is never declared in src/posts.rs:1.\n\n\
    ## Files to fix\n- `src/posts.rs`: declare it\n\n\
    ## Next step\nDeclare `p`.\n\n\
    ## Confidence\nhigh: the error says so.\n\n\
    ## Other causes\n- `src/users.rs` | high | `u` is never declared";

#[tokio::test]
async fn high_confidence_causes_in_different_scopes_are_fixed_in_parallel() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(true).await else {
        return;
    };
    let (url, _) = model_routed(
        vec![
            (
                "# Triage this failure",
                vec![Reply::Text(TRIAGE_TWO_CAUSES)],
            ),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let mut config = groups_on();
    config.parallel_repair = true;
    config.kinds.insert(
        "triage".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    assert_eq!(h.run_end().await, "verified");
    let records = end_records(&h.messages().await);
    assert_eq!(
        statuses(&records),
        ["failed", "passed"],
        "one check run after the fixes"
    );
    let repair = &records[0]["repair"];
    assert_eq!(repair["parallel"], true, "{repair}");
    let causes = repair["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    for c in causes {
        assert_eq!(c["confidence"], "high");
        assert_eq!(c["fixer"]["status"], "done", "{c}");
        assert_eq!(c["fixer"]["parallel"], true);
    }
    assert_eq!(h.file("src/users.rs").as_deref(), Some("users, fixed\n"));
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts, fixed\n"));
}

/// Round 2, P1.3: a fixer whose landing fails half way keeps its copy
/// and its landing journal, so Land refuses until it's finished instead
/// of landing a half-fixed view.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_fixers_failed_landing_keeps_its_copy_and_journal() {
    if !crate::vfs::mount_works() {
        return;
    }
    use crate::overlay::land;
    let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(h) = repair_harness(true).await else {
        return;
    };
    let (url, _) = model_routed(
        vec![
            (
                "# Triage this failure",
                vec![Reply::Text(TRIAGE_TWO_CAUSES)],
            ),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let mut config = groups_on();
    config.parallel_repair = true;
    config.kinds.insert(
        "triage".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    // Once the group has joined, posts.rs fails to land, and to go back.
    h.result_when_ready("g1").await;
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    *land::FAIL_WRITING.lock().unwrap() = Some((view, "src/posts.rs".into()));
    land::FAIL_WRITING_ALWAYS.store(true, std::sync::atomic::Ordering::SeqCst);
    h.run_end().await;
    let records = end_records(&h.messages().await);
    let fixer = records
        .iter()
        .flat_map(|r| {
            r["repair"]["causes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|c| c["fixer"].clone())
        .find(|f| f["status"] == "not landed")
        .unwrap_or_else(|| panic!("{records:#?}"));
    let said = fixer.to_string();
    assert!(!said.contains("discarded"), "{said}");
    assert!(said.contains("its copy is kept"), "{said}");
    let child = fixer["childDialogId"].as_str().unwrap().to_string();
    assert!(
        crate::overlay::chats::get(&child).is_some(),
        "the fixer's copy is kept"
    );
    assert!(landing_interrupted(&h.dialog_id), "its journal is kept");
    assert!(land_refused_unfinished(&h.dialog_id));
    stop_failing_landings();
    crate::overlay::chats::land_now(&h.dialog_id, false).unwrap();
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts, fixed\n"));
}

#[tokio::test]
async fn a_cause_the_lead_owns_is_never_fixed_in_parallel() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(true).await else {
        return;
    };
    let triage: &'static str = Box::leak(
        TRIAGE_TWO_CAUSES
            .replace("`src/users.rs` | high | `u`", "`src/api.rs` | high | `Api`")
            .into_boxed_str(),
    );
    let (url, _) = model_routed(
        vec![
            ("# Triage this failure", vec![Reply::Text(triage)]),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            // The contract step breaks the contract.
            write_call("l0", "src/api.rs", "bad api\n"),
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            write_call("l1", "src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
            Reply::Text("Fixed the contract."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let mut config = groups_on();
    config.parallel_repair = true;
    config.kinds.insert(
        "triage".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    assert_eq!(h.run_end().await, "verified");
    let records = end_records(&h.messages().await);
    let repair = &records[0]["repair"];
    assert_eq!(repair["parallel"], false, "{repair}");
    let causes = repair["causes"].as_array().unwrap();
    // w2's cause (two failures) first, by itself; then the lead's.
    assert_eq!(causes[0]["owner"]["label"], "w2");
    assert_eq!(causes[0]["fixer"]["parallel"], false);
    assert_eq!(causes[1]["owner"]["kind"], "lead");
    assert!(causes[1].get("fixer").is_none());
    assert_eq!(statuses(&records), ["failed", "passed"]);
}

/*
 * R08: a review after green, within the run's change-review budget.
 */

const FINDING_C: &str = "## Findings\n\n1. [high] b.txt:40 — The greeting has no name\n   - Scenario: the user is greeted as nobody.\n   - Fix: add the name.\n   - Confidence: confirmed";

/// P3.6: the fix for a review after green is checkpointed (a fix that
/// breaks what passes is rolled back), and a reply with no change after
/// the findings ends verified.
#[tokio::test]
async fn the_fix_after_a_review_after_green_is_protected() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let files = [
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        (
            "check.js",
            "const fs = require('fs'); if (!fs.readFileSync('a.txt', 'utf8').includes('ok')) { console.log('test t::greeting ... FAILED'); process.exit(1); } console.log('test t::greeting ... ok');",
        ),
        ("a.txt", "no\n"),
    ];
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    for breaks in [true, false] {
        let h = harness(&files).await;
        let mut lead = vec![
            write_call("w1", "a.txt", "nope\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "ok\n"),
            Reply::Text("Fixed."),
        ];
        if breaks {
            // The findings' fix breaks the check: rolled back; then it stops.
            lead.push(write_call("w3", "a.txt", "fixed the greeting\n"));
            lead.push(answered("a1", "fixed", "fixed the greeting"));
            lead.push(Reply::Text("Fixed."));
            lead.push(Reply::Text("I'll leave it."));
        } else {
            lead.push(answered("a1", "not-a-problem", "the spelling is intended"));
            lead.push(Reply::Text("Answered."));
        }
        let (url, _) = model_routed(
            vec![(
                REVIEW,
                // Its follow-up finds the answer right.
                vec![
                    Reply::Text(NO_FINDINGS),
                    Reply::Text(FINDING_A),
                    Reply::Text(NO_FINDINGS),
                ],
            )],
            lead,
        )
        .await;
        h.start(h.with_subagents(&url, "Greet", permissions.clone(), reviewer_on(json!({}))))
            .await;
        let end = h.run_end().await;
        let records: Vec<Value> = h
            .messages()
            .await
            .iter()
            .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
            .collect();
        let statuses: Vec<&Value> = records.iter().map(|v| &v["status"]).collect();
        if breaks {
            // Rolled back (the round was the run's last: given up too).
            let rb = records.iter().find(|r| r["rolledBack"].is_object());
            assert!(rb.is_some(), "{records:?}");
            assert_eq!(h.file("a.txt").as_deref(), Some("ok\n"));
        } else {
            assert_eq!(end, "verified", "{statuses:?}");
        }
    }
}

/// Round 2, P3.6: findings after green answered with a command alone
/// (no write): the checks run again on the changed code before the run is
/// verified.
#[tokio::test]
async fn a_bash_only_fix_after_green_reruns_the_checks() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        (
            "check.js",
            "const fs = require('fs'); const a = fs.readFileSync('a.txt', 'utf8'); if (!a.includes('ok') || a.includes('broken')) { console.log('test t::greeting ... FAILED'); process.exit(1); } console.log('test t::greeting ... ok');",
        ),
        ("a.txt", "no\n"),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Text(NO_FINDINGS),
                Reply::Text(FINDING_A),
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "nope\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "ok\n"),
            Reply::Text("Fixed."),
            // The findings' fix, by command only: it breaks the check. (Not
            // `sed -i`, whose argument macOS's sed reads as a suffix.)
            Reply::Call {
                id: "b1",
                name: "bash",
                args: json!({ "command": "printf 'ok broken\\n' > a.txt" }),
            },
            answered("a1", "fixed", "changed the greeting"),
            Reply::Text("Fixed."),
            Reply::Text("I'll leave it."),
        ],
    )
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    h.start(h.with_subagents(&url, "Greet", permissions, reviewer_on(json!({}))))
        .await;
    let end = h.run_end().await;
    let records: Vec<Value> = h
        .messages()
        .await
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect();
    assert!(
        records.len() >= 3,
        "the checks ran after the command: {records:#?}"
    );
    // It broke the passing check: a regression (rolled back where it can
    // be, the command's change listed as not undone).
    assert_eq!(
        records[2]["progress"]["verdict"], "regression",
        "{:#}",
        records[2]
    );
    assert_ne!(end, "verified");
}

/// R08 with the strict review: each green after a fix is reviewed, the
/// findings followed up; a finding the run's last review raises can't be
/// resolved any more, so the change isn't accepted: `review-open`.
#[tokio::test]
async fn green_after_a_fix_is_reviewed_until_the_review_budget_is_used_up() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        (
            "check.js",
            "const fs = require('fs'); if (!fs.readFileSync('a.txt', 'utf8').includes('ok')) { console.error('a.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("a.txt", "no\n"),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                // B03, before the first checks.
                Reply::Text(NO_FINDINGS),
                // R08, after each green that followed a fix.
                Reply::Text(FINDING_A),
                Reply::Text(FINDING_C),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "nope\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "ok\n"),
            Reply::Text("Fixed."),
            write_call("w3", "a.txt", "ok hello\n"),
            answered("a1", "fixed", "spelled hello"),
            Reply::Text("Fixed."),
            write_call("w4", "a.txt", "ok hello, Ann\n"),
            answered("a2", "fixed", "added the name"),
            Reply::Text("Fixed."),
        ],
    )
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    h.start(h.with_subagents(&url, "Greet", permissions, reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "review-open");
    let messages = h.messages().await;
    assert_eq!(
        record_order(&messages),
        [
            "review:clean",
            "verification:failed",
            "verification:passed",
            "review:findings",
            "note",
            // The follow-up comes before the checks run again.
            "review:findings",
        ]
    );
    let records: Vec<Value> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect();
    assert_eq!(records[1]["reviewAfterGreen"]["status"], "findings");
    let all = reviews(&messages);
    // The review after green; its follow-up runs as the agent finishes.
    assert_eq!(all[1]["afterGreen"], true, "{:#}", all[1]);
    // The third review, the run's last, found a new problem: it blocks.
    let last = all.last().unwrap();
    assert_eq!(last["blocked"], true, "{last}");
    assert_eq!(
        last["findings"][0]["title"], "The greeting has no name",
        "{last}"
    );
}

/// Round 2 (§4.2): the third round is the first green; its review's fix
/// gets a fourth round past the usual three, which passes: verified, at
/// round 4 of 4. The notes before it said "of 3".
#[tokio::test]
async fn review_after_green_fix_that_passes_ends_verified_with_round_4_of_4() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        (
            "check.js",
            "const fs = require('fs'); if (!fs.readFileSync('a.txt', 'utf8').includes('ok')) { console.error('a.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("a.txt", "no\n"),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![
                Reply::Text(NO_FINDINGS),
                Reply::Text(FINDING_A),
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![
            write_call("w1", "a.txt", "nope\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "still no\n"),
            Reply::Text("Fixed."),
            write_call("w3", "a.txt", "ok\n"),
            Reply::Text("Fixed."),
            write_call("w4", "a.txt", "ok hello\n"),
            Reply::Text("1: fixed."),
        ],
    )
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    h.start(h.with_subagents(&url, "Greet", permissions, reviewer_on(json!({}))))
        .await;
    let end = h.run_end().await;
    let messages = h.messages().await;
    assert_eq!(end, "verified", "{:?}", record_order(&messages));
    let records: Vec<Value> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect();
    let rounds: Vec<(&Value, &Value)> = records
        .iter()
        .map(|r| (&r["round"], &r["status"]))
        .collect();
    assert_eq!(
        rounds,
        [
            (&json!(1), &json!("failed")),
            (&json!(2), &json!("failed")),
            (&json!(3), &json!("passed")),
            (&json!(4), &json!("passed")),
        ]
    );
    assert_eq!(records[2]["reviewAfterGreen"]["status"], "findings");
    assert_eq!(records[3]["maxRounds"], 4);
    let notes: Vec<&str> = messages
        .iter()
        .filter(|m| {
            m.meta
                .as_ref()
                .is_some_and(|m| m["harness"] == "verification")
        })
        .map(|m| m.content.as_str())
        .collect();
    assert!(notes[0].contains("(attempt 1 of 3)"), "{}", notes[0]);
    assert!(notes[1].contains("(attempt 2 of 3)"), "{}", notes[1]);
}

#[tokio::test]
async fn a_review_after_green_that_couldnt_read_the_report_isnt_done() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
        ),
        (
            "check.js",
            "const fs = require('fs'); if (!fs.readFileSync('a.txt', 'utf8').includes('ok')) { console.error('a.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("a.txt", "no\n"),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            REVIEW,
            vec![Reply::Text(NO_FINDINGS), Reply::Text("Looks fine to me.")],
        )],
        vec![
            write_call("w1", "a.txt", "nope\n"),
            Reply::Text("Done."),
            write_call("w2", "a.txt", "ok\n"),
            Reply::Text("Fixed."),
        ],
    )
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    h.start(h.with_subagents(&url, "Greet", permissions, reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let last = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .next_back()
        .unwrap();
    assert_eq!(last["reviewAfterGreen"]["status"], "skipped", "{last}");
    // The reason says what was wrong.
    let reason = last["reviewAfterGreen"]["reason"].as_str().unwrap();
    assert!(
        reason
            .starts_with("its report didn't follow the format: it has no \"## Findings\" section"),
        "{reason}"
    );
}

/*
 * A sequential subagent in an isolated chat works under the chat's
 * isolation: its commands get the chat's variables and confinement.
 */

#[tokio::test]
async fn a_workers_commands_run_under_the_chats_isolation() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = plan_harness(&[("a.txt", "old\n")]).await;
    h.isolate().await;
    let project = h.project.to_string_lossy().into_owned();
    let read_real: &'static str = Box::leak(format!("cat {project}/a.txt").into_boxed_str());
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                write_call("w1", "a.txt", "hello\n"),
                Reply::Calls(vec![
                    (
                        "e1",
                        "bash",
                        json!({ "command": "echo target=$CARGO_TARGET_DIR" }),
                    ),
                    ("e2", "bash", json!({ "command": read_real })),
                    ("e3", "bash", json!({ "command": "git commit -am x" })),
                ]),
                worker_report("First greeting.", &["a.txt"]),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Stopping here.")],
    )
    .await;
    let mut request = h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off);
    request.isolate = Some(true);
    h.start(request).await;
    h.run_end().await;
    let child = h.child_of("s1").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no result for {id}"))
            .content
            .clone()
    };
    // The chat's build folder, as the lead's commands get.
    assert!(
        result("e1").contains("overlays") && result("e1").contains("targets"),
        "{}",
        result("e1")
    );
    // Confined where the platform can: the project's path shows the
    // chat's view, with the worker's change.
    if crate::overlay::chats::get(&h.dialog_id).is_some_and(|o| o.confine().is_some()) {
        assert!(result("e2").contains("hello"), "{}", result("e2"));
    }
    assert!(result("e3").starts_with("Error"), "{}", result("e3"));
}

/// A test writer asked more (A04) keeps its limits: test files only, the
/// change's code hidden, checks only with bash.
#[tokio::test]
async fn a_test_writer_continued_keeps_its_limits() {
    let h = harness(&[
        (
            "src/greet.ts",
            "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
        ),
        ("src/main.ts", "console.log('hi');\n"),
        (".rovik/plans/greeter.md", TEST_PLAN),
    ])
    .await;
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                write_call("w1", "src/greet.test.ts", "test('greets', () => {});\n"),
                Reply::Text(TEST_REPORT),
            ],
        )],
        vec![test_task("tw"), Reply::Text("Tests are written.")],
    )
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/greeter.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..quiet_permissions()
    };
    let mut request = h.request(&url, StartKind::Send { message }, permissions.clone());
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    h.run_end().await;
    let child = h.child_of("tw").await;
    let handle: &'static str =
        Box::leak(subagent::handle(subagent::Kind::TestWriter, &child.id).into_boxed_str());

    // Asked to "also fix the bug": it still can't touch or read the code.
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                Reply::Calls(vec![
                    (
                        "f1",
                        "write",
                        json!({ "path": "src/greet.ts", "content": "fixed\n" }),
                    ),
                    ("f2", "read", json!({ "path": "src/greet.ts" })),
                    ("f3", "bash", json!({ "command": "cat src/greet.ts" })),
                ]),
                Reply::Text(TEST_REPORT),
            ],
        )],
        vec![
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": handle, "goal": "also fix the bug in greet" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    let mut request = h.request(&url, Harness::send("Fix greet"), permissions);
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    h.run_end().await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no result for {id}"))
            .content
            .clone()
    };
    assert!(
        result("f1").contains("isn't a test file"),
        "{}",
        result("f1")
    );
    assert!(
        result("f2").contains("one of the files the change touches"),
        "{}",
        result("f2")
    );
    assert!(
        result("f3").contains("The test writer runs only"),
        "{}",
        result("f3")
    );
    assert!(h.file("src/greet.ts").unwrap().contains("Hello"));
}

/// The light-command limit fails closed: a heavy command behind a wrapper
/// is refused too.
#[tokio::test]
async fn a_heavy_command_behind_a_wrapper_is_refused_to_the_test_writer() {
    let h = harness(&[("src/greet.ts", "export const greet = 1;\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                Reply::Calls(vec![
                    ("b1", "bash", json!({ "command": "timeout 600 cargo test" })),
                    (
                        "b2",
                        "bash",
                        json!({ "command": "env CI=1 timeout 60 npm test" }),
                    ),
                ]),
                Reply::Text(TEST_REPORT),
            ],
        )],
        vec![test_task("tw"), Reply::Text("Done.")],
    )
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..quiet_permissions()
    };
    let mut request = h.request(&url, Harness::send("Test greet"), permissions);
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    h.run_end().await;
    let child = h.child_of("tw").await;
    let work = h.messages_of(&child.id).await;
    for id in ["b1", "b2"] {
        let r = &work
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .content;
        assert!(r.contains("is a heavy command"), "{id}: {r}");
    }
}

/// The tool-call budget holds within one reply: calls past it aren't run.
#[tokio::test]
async fn calls_past_the_budget_in_one_reply_are_not_run() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let read = |id: &'static str| (id, "read", json!({ "path": "a.txt" }));
    let (url, _) = model_routed(
        vec![(
            "MARK-B",
            vec![
                Reply::Calls(vec![
                    read("r1"),
                    read("r2"),
                    read("r3"),
                    read("r4"),
                    read("r5"),
                ]),
                Reply::Text(REPORT),
            ],
        )],
        vec![
            Reply::Call {
                id: "t1",
                name: "task",
                args: json!({ "kind": "scout", "goal": "MARK-B: where?", "max_tool_calls": 3 }),
            },
            Reply::Text("Found it."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.run_end().await;
    let child = h.child_of("t1").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no result for {id}"))
            .content
            .clone()
    };
    for id in ["r1", "r2", "r3"] {
        assert!(result(id).contains('x'), "{id}: {}", result(id));
    }
    for id in ["r4", "r5"] {
        assert!(
            result(id).starts_with("Error: not run"),
            "{id}: {}",
            result(id)
        );
    }
    let r = h.task_result("t1").await.content;
    assert!(r.contains("Status: partial"), "{r}");
    assert!(r.contains("3 tool calls"), "{r}");
}

/// A worker whose report starts repeating itself is cut there, not taken
/// as stopped by the user: its report stands and its changes are kept.
#[tokio::test]
async fn a_worker_cut_for_repeating_itself_keeps_its_work() {
    let h = plan_harness(&[("a.txt", "old\n")]).await;
    let looping: &'static str = Box::leak(
        format!(
            "## Summary\nReplaced the line with hello.\n\n## Changed files\n- `a.txt`: changed\n\n## Open questions\nNone\n\n{}",
            "Sigh. I'll just output it. ".repeat(80)
        )
        .into_boxed_str(),
    );
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            // Still streaming when it's cut: the request is cancelled.
            vec![
                write_call("w1", "a.txt", "hello\n"),
                Reply::Hang(Some(looping)),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Stopping here.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let r = h.task_result("s1").await.content;
    assert!(!r.contains("stopped"), "{r}");
    assert!(r.contains("Status: done"), "{r}");
    assert!(r.contains("Replaced the line with hello."), "{r}");
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\n"));
}

/*
 * R06: Redo sequentially puts each path back exactly as the chat had it
 * before the join: a folder the group made goes, a file the chat had
 * changed gets its bytes and its merge base back, a file it hadn't
 * touched shows the project's again.
 */

const REDO_PLAN: &str = "# Pages\n\nUsers and posts.\n\n## Steps\n\n\
    1. **Contract**\n   - Files: `src/api.rs`\n   - Declare the pages there.\n\
    2. **Users**\n   - Files: `src/users/` (new)\n   - Add the users page.\n   - Group: pages\n\
    3. **Posts**\n   - Files: `src/posts.rs`, `assets/x.bin`\n   - Add the posts page and its data.\n   - Group: pages\n\n\
    ## Validation\n- read the files\n";

#[tokio::test]
async fn redo_puts_each_path_back_as_the_chat_had_it() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[
        ("src/api.rs", "pub fn pages() {}\n"),
        ("src/posts.rs", "// posts\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("assets/x.bin", "orig\n"),
        (".rovik/plans/greet.md", REDO_PLAN),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users/mod.rs", "pub fn users() {}\n"),
                    worker_report("Users.", &["src/users/mod.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts by w2\n"),
                    write_call("w3", "assets/x.bin", "text by w2\n"),
                    worker_report("Posts.", &["src/posts.rs", "assets/x.bin"]),
                ],
            ),
        ],
        vec![
            // Before the group, the lead changes posts.rs, and writes bytes
            // that aren't UTF-8 into x.bin.
            write_call("l1", "src/posts.rs", "posts by the lead\n"),
            Reply::Call {
                id: "l2",
                name: "bash",
                args: json!({ "command": "printf '\\377\\376bin' > assets/x.bin" }),
            },
            Reply::Call {
                id: "t1",
                name: "todo",
                args: json!({ "items": [
                    { "title": "Contract", "status": "done" },
                    { "title": "Users", "status": "pending" },
                    { "title": "Posts", "status": "pending" },
                ] }),
            },
            group_task("g1"),
            Reply::Hang(None),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let joined = h.result_when_ready("g1").await;
    assert!(
        joined.content.contains("Landed: src/users"),
        "{}",
        joined.content
    );
    let core = h.core.clone();
    agent_stop(&core, h.dialog_id.clone()).await;
    assert_eq!(h.run_end().await, "stopped");

    let id = h.dialog_id.clone();
    // The join's changes are in the chat before the redo.
    let before: Vec<String> = crate::overlay::chats::changes(&id)
        .iter()
        .map(|c| c.path().to_string())
        .collect();
    for p in [
        "src/users",
        "src/users/mod.rs",
        "src/posts.rs",
        "assets/x.bin",
    ] {
        assert!(before.iter().any(|b| b == p), "{p}: {before:?}");
    }
    let request = crate::overlay::chats::redo_group(&id, "pages").unwrap();
    assert!(request.contains("Carry out its steps (2, 3)"), "{request}");
    let changes = crate::overlay::chats::changes(&id);
    let change = |p: &str| changes.iter().rev().find(|c| c.path() == p).cloned();
    // The folder the group made, and its file: gone, nothing to land.
    assert!(change("src/users").is_none(), "{changes:?}");
    assert!(change("src/users/mod.rs").is_none(), "{changes:?}");
    // posts.rs: the lead's version, merged against the project's file as
    // it was when the lead changed it.
    let version = |p: &str| -> (Vec<u8>, Option<Vec<u8>>) {
        match change(p).unwrap_or_else(|| panic!("{p} isn't the chat's: {changes:?}")) {
            crate::overlay::fs::OverlayChange::Write {
                upper, base_copy, ..
            } => (
                std::fs::read(upper.unwrap()).unwrap(),
                base_copy.map(|b| std::fs::read(b).unwrap()),
            ),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        version("src/posts.rs"),
        (
            b"posts by the lead\n".to_vec(),
            Some(b"// posts\n".to_vec())
        )
    );
    // x.bin: its bytes exactly.
    assert_eq!(version("assets/x.bin").0, b"\xff\xfebin".to_vec());
    // Untouched files stay untouched.
    assert!(change("src/main.rs").is_none());
    // Nothing to land is left from the group, and the project is as it was.
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("// posts\n"));
    assert!(!h.project.join("src/users").exists());
}

/// A fixer that fails is rolled back: it changed nothing, and the lead
/// isn't told the workers' fixes are in.
#[tokio::test]
async fn a_failed_fixer_is_reported_as_having_changed_nothing() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    // Resumed to fix it: writes, then its model fails.
                    write_call("w2f", "src/posts.rs", "posts, half fixed\n"),
                    bad(),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            Reply::Text("I'll leave it."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, true)).await;
    h.run_end().await;
    let messages = h.messages().await;
    let records = end_records(&messages);
    let repair = &records[0]["repair"];
    assert_eq!(repair["status"], "lead", "{repair}");
    let fixer = &repair["causes"][0]["fixer"];
    assert_eq!(fixer["status"], "failed", "{fixer}");
    assert_eq!(fixer["changed"], json!([]), "{fixer}");
    let report = messages
        .iter()
        .find(|m| m.content.contains("## Root causes"))
        .expect("the lead's report");
    assert!(
        !report.content.contains("fixes are in your files"),
        "{}",
        report.content
    );
    assert!(
        report.content.contains("couldn't fix it"),
        "{}",
        report.content
    );
}

/// A worker resumed after the app closed knows what it wrote before then:
/// those files count as changed, and a failure says they're kept.
#[tokio::test]
async fn a_worker_resumed_after_a_restart_knows_what_it_wrote() {
    let h = plan_harness(&[("a.txt", "hello\n")]).await;
    let db = h.core.db.clone();
    // As the app left it: the lead's task call, no result; the worker's
    // chat with its brief and one write done (a.txt says hello now).
    let lead = h.dialog_id.clone();
    let mut user = crate::agent_run::run::new_message(
        "user",
        "Implement the plan in `.rovik/plans/greet.md`.",
    );
    user.meta = Some(json!({ "mode": "agent", "implementsPlan": true }));
    let mut call = crate::agent_run::run::new_message("assistant", "");
    call.tool_calls = Some(vec![store::ToolCall {
        id: "s1".into(),
        name: "task".into(),
        arguments: json!({ "kind": "worker", "step": 1 }).to_string(),
        extra_content: None,
    }]);
    let child = uuid::Uuid::new_v4().to_string();
    let brief =
        crate::agent_run::run::new_message("user", "# Your step: 1. First greeting\n\nDo it.");
    let mut write = crate::agent_run::run::new_message("assistant", "");
    write.tool_calls = Some(vec![store::ToolCall {
        id: "w1".into(),
        name: "write".into(),
        arguments: json!({ "path": "a.txt", "content": "hello\n" }).to_string(),
        extra_content: None,
    }]);
    let mut wrote = crate::agent_run::run::new_message("tool", "wrote 6 bytes to a.txt");
    wrote.tool_call_id = Some("w1".into());
    let child_id = child.clone();
    db.write(move |c| {
        store::append_messages(c, &lead, &[user, call])?;
        store::create_child_dialog(
            c,
            &store::NewChildDialog {
                id: child_id.clone(),
                parent_id: lead.clone(),
                parent_call_id: "s1".into(),
                kind: "worker".into(),
                title: "Worker".into(),
                created_at: store::now_ms(),
            },
        )?;
        store::append_messages(c, &child_id, &[brief, write, wrote])?;
        Ok(())
    })
    .await
    .unwrap();

    // Resumed, its model fails.
    let (url, _) = model_routed(
        vec![("# Your step: 1.", vec![bad(), bad(), bad()])],
        vec![Reply::Text("I'll look at a.txt myself.")],
    )
    .await;
    let mut request = h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off);
    request.start = StartKind::Resume;
    h.start(request).await;
    h.run_end().await;
    let r = h.task_result("s1").await.content;
    assert!(
        r.contains("What it wrote before the app closed is kept, not undone: a.txt"),
        "{r}"
    );
    assert!(!r.contains("It changed no files"), "{r}");
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\n"));
}

/// Two steps started in one reply: the second's brief is made once the
/// first is done, so it has the first's work.
#[tokio::test]
async fn steps_started_together_brief_each_after_the_one_before() {
    let h = plan_harness(&[("a.txt", "old\n")]).await;
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 1.",
                vec![
                    write_call("w1", "a.txt", "hello\n"),
                    worker_report("Replaced the line with hello.", &["a.txt"]),
                ],
            ),
            (
                "# Your step: 2.",
                vec![
                    write_call("w2", "a.txt", "hello\nhello again\n"),
                    write_call("w3", "b.txt", "bye\n"),
                    worker_report("Added the second greeting.", &["a.txt", "b.txt"]),
                ],
            ),
        ],
        vec![
            Reply::Calls(vec![
                ("s1", "task", json!({ "kind": "worker", "step": 1 })),
                ("s2", "task", json!({ "kind": "worker", "step": 2 })),
            ]),
            Reply::Text("Two steps done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let brief = requests_with(&seen, "# Your step: 2.")[0].to_string();
    assert!(
        brief.contains("Replaced the line with hello."),
        "step 1's summary: {brief}"
    );
    assert!(
        brief.contains("+hello"),
        "step 1's change to a.txt: {brief}"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("hello\nhello again\n"));
}

/// Stop during a group: the stopped workers aren't retried as failures.
#[tokio::test]
async fn stop_during_a_group_retries_no_worker() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            ("# Your step: 2.", vec![Reply::Hang(None)]),
            ("# Your step: 3.", vec![Reply::Hang(None)]),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    // Both workers are at work.
    for _ in 0..400 {
        if h.events_of("subagentStarted").len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(h.events_of("subagentStarted").len(), 2);
    let core = h.core.clone();
    agent_stop(&core, h.dialog_id.clone()).await;
    assert_eq!(h.run_end().await, "stopped");
    // No third worker was started for a retry.
    assert_eq!(h.events_of("subagentStarted").len(), 2);
    let messages = h.messages().await;
    let result = result_of(&messages, "g1").content.clone();
    assert!(
        !result.contains("w1'") && !result.contains("w2'"),
        "{result}"
    );
    assert!(!result.contains("failed twice"), "{result}");
}

/*
 * Review-fixes Phase 7: behaviours that had no test.
 */

/// Stop on the lead while its scout waits for an approval: both end, and
/// no approval is left waiting.
#[tokio::test]
async fn stop_on_the_lead_while_its_scout_waits_for_an_approval() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-W",
            vec![
                Reply::Call {
                    id: "s1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "MARK-W: where?"), Reply::Text("Done.")],
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
    let child = asked["request"]["dialogId"].as_str().unwrap().to_string();
    let core = h.core.clone();
    assert!(agent_stop(&core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "stopped");
    let result = h.task_result("t1").await;
    assert!(
        result.content.contains("Status: stopped"),
        "{}",
        result.content
    );
    let state = &core.agents;
    assert!(!state.is_running(&child) && !state.is_running(&h.dialog_id));
    let status = serde_json::to_value(agent_status(&core).await).unwrap();
    assert_eq!(status["approvals"], json!([]), "no approval left waiting");
    assert_eq!(status["subagents"], json!([]));
}

/// A subagent that panics mid-batch: its call is answered, the others
/// finish, and nothing stays registered as running.
#[tokio::test]
async fn a_subagent_that_panics_mid_batch_leaves_nothing_running() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-X",
            (0..2)
                .map(|_| Reply::Slow(200, Box::new(Reply::Text(REPORT))))
                .collect(),
        )],
        vec![
            Reply::Calls(vec![
                task_call("x1", "MARK-X: one"),
                task_call("x2", "MARK-X: two"),
                task_call("x3", "MARK-X: three"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    *crate::agent_run::subagent_run::PANIC_SUB.lock().unwrap() = Some("x2".into());
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let end = h.run_end().await;
    *crate::agent_run::subagent_run::PANIC_SUB.lock().unwrap() = None;
    assert_eq!(end, "done");
    for ok in ["x1", "x3"] {
        assert!(
            h.task_result(ok).await.content.contains("Status: done"),
            "{ok}"
        );
    }
    let broken = h.task_result("x2").await;
    assert!(broken.content.starts_with("Error"), "{}", broken.content);
    let state = &h.core.agents;
    assert_eq!(state.running_children(&h.dialog_id), 0);
    assert!(!state.is_running(&h.dialog_id));
}

/// A path no member owns, granted to one member, isn't granted to a
/// second: two copies of it would merge silently at the join (no line
/// conflict) into a file neither worker saw whole.
#[tokio::test]
async fn a_path_granted_to_one_group_member_isnt_granted_to_another() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[("src/shared.rs", "a\nb\nc\nd\ne\n")]).await;
    let extend = |id: &'static str| Reply::Call {
        id,
        name: "extend_scope",
        args: json!({ "paths": ["src/shared.rs"], "reason": "a helper" }),
    };
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    extend("e1"),
                    write_call("w1", "src/shared.rs", "A\nb\nc\nd\ne\n"),
                    write_call("w1b", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs", "src/shared.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    // After w1 has its grant.
                    Reply::Slow(800, Box::new(extend("e2"))),
                    write_call("w2", "src/shared.rs", "a\nb\nc\nd\nE\n"),
                    write_call("w2b", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
            (
                "# Your step: 4.",
                vec![
                    write_call("w3", "src/feed.rs", "feed\n"),
                    worker_report("Feed.", &["src/feed.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    h.result_when_ready("g1").await;
    // What the join landed in the chat's view lands in the project as the
    // lead ends right after, its view closing with it: read there.
    h.run_end().await;
    let shared = h.file("src/shared.rs").unwrap();
    let w2 = h.child_of("g1#w2").await;
    let work = h.messages_of(&w2.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .map(|m| m.content.clone())
            .unwrap_or_default()
    };
    assert!(
        result("e2").starts_with("Error: not granted") && result("e2").contains("w1"),
        "{}",
        result("e2")
    );
    assert!(result("w2").starts_with("Error"), "{}", result("w2"));
    assert_eq!(shared, "A\nb\nc\nd\ne\n", "only w1's change");
}

/// Depth one: no subagent is offered `task` (on the request's tool list,
/// not only refused), even a kind configured with it, and one that calls
/// it anyway starts nothing.
#[tokio::test]
async fn no_subagent_is_offered_task_even_when_configured_with_it() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, seen) = model_routed(
        vec![(
            "MARK-D",
            vec![task("inner", "MARK-NESTED: go deeper"), Reply::Text(REPORT)],
        )],
        vec![task("t1", "MARK-D: where?"), Reply::Text("Done.")],
    )
    .await;
    let mut config = scouts_on(json!({}));
    config.kinds.get_mut("scout").unwrap().extra_tools = vec!["task".into()];
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let scout = requests_with(&seen, "MARK-D");
    assert!(!scout.is_empty());
    for body in &scout {
        assert!(
            !tool_names(body).contains(&"task".to_string()),
            "offered task"
        );
    }
    let starts = h.events_of("subagentStarted");
    assert_eq!(starts.len(), 1, "only the lead's scout: {starts:?}");
    let child = h.child_of("t1").await;
    let inner = h
        .messages_of(&child.id)
        .await
        .into_iter()
        .find(|m| m.tool_call_id.as_deref() == Some("inner"))
        .unwrap();
    assert!(inner.content.starts_with("Error"), "{}", inner.content);
}

/// The run's subagent limit, reached through the lead's own calls: the
/// call past it is refused, with nothing started, and the run goes on.
#[tokio::test]
async fn the_lead_is_refused_past_the_runs_subagent_limit() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-L", vec![Reply::Text(REPORT), Reply::Text(REPORT)])],
        vec![
            task("l1", "MARK-L: one"),
            task("l2", "MARK-L: two"),
            task("l3", "MARK-L: three"),
            Reply::Text("Done alone."),
        ],
    )
    .await;
    let mut config = scouts_on(json!({}));
    config.max_per_run = Some(2);
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    for ok in ["l1", "l2"] {
        assert!(
            h.task_result(ok).await.content.contains("Status: done"),
            "{ok}"
        );
    }
    let third = h.task_result("l3").await;
    assert!(
        third
            .content
            .contains("already started 2 subagents, the limit"),
        "{}",
        third.content
    );
    assert_eq!(h.events_of("subagentStarted").len(), 2);
}

/// A fallback model whose request doesn't fit either gets its own trim:
/// the first model's doesn't use it up.
#[tokio::test]
async fn a_fallback_model_gets_its_own_trim() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let overflow = r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#;
    let (small, small_seen) = model(vec![
        Reply::Status(400, overflow),
        Reply::Status(400, overflow),
    ])
    .await;
    let (url, seen) = model_routed(
        vec![(
            "MARK-FT",
            vec![Reply::Status(400, overflow), Reply::Text(REPORT)],
        )],
        vec![task("t1", "MARK-FT: where?"), Reply::Text("Done.")],
    )
    .await;
    let config = scouts_on(json!({ "models": [profile("small", &small), { "chat": true }] }));
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    assert_eq!(h.run_end().await, "done");
    let meta = &h.task_result("t1").await.meta.unwrap()["subagent"];
    assert_eq!(meta["status"], "done", "{meta}");
    assert_eq!(meta["modelsTried"], json!(["Other/small", "Test/m"]));
    assert_eq!(
        small_seen.lock().unwrap().len(),
        2,
        "trimmed once, then the next model"
    );
    assert_eq!(
        requests_with(&seen, "MARK-FT").len(),
        2,
        "the fallback trimmed once too"
    );
}

/// The repair turn for a report out of format fails (the provider errors):
/// the subagent ends with what it had, and the lead is told.
#[tokio::test]
async fn an_error_on_the_report_turn_keeps_what_it_had() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-RT",
            vec![
                Reply::Text("I looked around; a.txt holds x."),
                Reply::Status(400, r#"{"error":{"message":"bad request"}}"#),
            ],
        )],
        vec![task("t1", "MARK-RT: where?"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let result = h.task_result("t1").await;
    let meta = &result.meta.as_ref().unwrap()["subagent"];
    assert_ne!(meta["status"], "done", "{}", result.content);
    assert!(
        result.content.contains("a.txt holds x"),
        "what it had reaches the lead: {}",
        result.content
    );
    assert!(!h.core.agents.is_running(&h.dialog_id));
}

/// Stop while a repair round's fixer works: the run ends stopped, the next
/// fixer never starts, no recheck runs, and nothing is left running.
#[tokio::test]
async fn stop_in_the_middle_of_a_repair_round() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let (url, seen) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "bad users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                    write_call("w1f", "src/users.rs", "users, fixed\n"),
                    worker_report("Fixed the users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    // The first fixer (posts) hangs until the Stop.
                    Reply::Hang(None),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, true)).await;
    let fixer = h
        .wait_for("a fixer started", |e| {
            e["type"] == "subagentStarted"
                && e["toolCallId"]
                    .as_str()
                    .is_some_and(|c| c.contains(":fix:"))
        })
        .await;
    let fixer_chat = fixer["childDialogId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let core = h.core.clone();
    assert!(agent_stop(&core, h.dialog_id.clone()).await);
    assert_eq!(h.run_end().await, "verify-stopped");
    let state = &core.agents;
    assert!(!state.is_running(&fixer_chat) && !state.is_running(&h.dialog_id));
    assert_eq!(state.running_children(&h.dialog_id), 0);
    // The users fixer never started, and no recheck ran.
    assert!(
        first_request_with(&seen, &["# Fix from the lead's checks", "# Your step: 2."]).is_none(),
        "the second fixer started"
    );
    let records = end_records(&h.messages().await);
    assert_eq!(records.len(), 1, "no recheck: {records:#?}");
    assert_ne!(records[0]["repair"]["status"], "recheck");
}

/// A fixer's change breaks what passed before: the recheck round is a
/// regression, and the fixers' changes are rolled back like the lead's.
#[tokio::test]
async fn a_regression_from_the_fixers_is_rolled_back_in_the_recheck() {
    if !crate::vfs::mount_works() {
        return;
    }
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    // Lint finds posts.rs's error (a located cause: its worker fixes it);
    // the test `users` fails when posts.rs says "v2", and passed before:
    // the tests ran then, so it's a regression, not a hidden failure.
    let lint = "const fs = require('fs');\n\
        if (fs.readFileSync('src/posts.rs', 'utf8').includes('bad')) {\n\
          console.error('src/posts.rs:1:1: error  no-undef  p is not defined');\n\
          console.error('\\n1 problem (1 error, 0 warnings)'); process.exit(1);\n}\n";
    let test = "const fs = require('fs');\n\
        const broken = fs.readFileSync('src/posts.rs', 'utf8').includes('v2');\n\
        console.log('test t::users ... ' + (broken ? 'FAILED' : 'ok'));\n\
        console.log('test result: ' + (broken ? 'FAILED. 0 passed; 1 failed' : 'ok. 1 passed; 0 failed'));\n\
        if (broken) process.exit(1);\n";
    let h = group_harness(&[
        ("lint.js", lint),
        ("check.js", test),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "lint": "node lint.js", "test": "node check.js" } }"#,
        ),
    ])
    .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    // Its fix: posts pass, users break.
                    write_call("w2f", "src/posts.rs", "posts v2\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Text("Done."),
            // Told the fix was undone: the lead fixes posts itself.
            write_call("l1", "src/posts.rs", "posts fixed\n"),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, true)).await;
    let end = h.run_end().await;
    let messages = h.messages().await;
    let records = end_records(&messages);
    assert_eq!(end, "verified", "{:#?}", statuses(&records));
    assert_eq!(statuses(&records), ["failed", "rolled-back", "passed"]);
    let rb = &records[1]["rolledBack"];
    assert_eq!(rb["restored"], json!(["src/posts.rs"]), "{rb}");
    assert_eq!(rb["broke"], json!(["test|test|t::users"]), "{rb}");
    assert!(messages.iter().any(|m| m
        .content
        .contains("Your last fix broke something that didn't fail before")));
}

/// B07 in an isolated chat: a file only the chat's view has (an earlier
/// run made it, unlanded) is there for the plan's checks; one nowhere is
/// still a problem.
#[tokio::test]
async fn plan_checks_see_the_chats_view_in_an_isolated_chat() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = plan_project().await;
    h.isolate().await;
    // Stopped after the write: a stopped run doesn't land.
    let (url, _) = model(vec![
        write_call("w1", "src/only_here.rs", "pub fn here() {}\n"),
        Reply::Hang(None),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Make a file"), quiet_permissions());
    request.isolate = Some(true);
    h.start(request).await;
    h.wait_for("the write", |e| e["type"] == "fileWritten")
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let core = h.core.clone();
    agent_stop(&core, h.dialog_id.clone()).await;
    assert_eq!(h.run_end().await, "stopped");
    assert!(h.file("src/only_here.rs").is_none(), "not in the project");

    let plan = |extra: &str| {
        format!(
            "# Use it\n\nCall it.\n\n## Steps\n\n1. **Call**\n   - Files: `src/api.rs`, `src/only_here.rs`\n   - Call `here()` from the api; export it.\n{extra}\n## Validation\n- `cargo test`\n"
        )
    };
    let (url, _) = model(vec![
        submit(
            "p1",
            plan("2. **Gone**\n   - Files: `src/missing.rs`\n   - Use it in the loader.\n"),
        ),
        submit(
            "p2",
            plan("2. **Store**\n   - Files: `src/db.rs`\n   - Keep what it returns.\n"),
        ),
        Reply::Text("Saved."),
    ])
    .await;
    h.events.lock().unwrap().clear();
    let mut request = h.plan_request(&url, subagent::SubagentConfig::default());
    request.isolate = Some(true);
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let first = result_of(&messages, "p1").content.clone();
    assert!(
        first.contains("`src/missing.rs`, which isn't in the project"),
        "{first}"
    );
    assert!(!first.contains("only_here"), "{first}");
    let second = result_of(&messages, "p2").content.clone();
    assert!(second.starts_with("Saved the plan"), "{second}");
}

/// The plan reviewer waits for an approval of its own read: the review
/// goes on once it's answered, and the plan is saved with its record.
#[tokio::test]
async fn a_plan_review_waits_for_its_approval_and_goes_on() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![
                Reply::Call {
                    id: "r1",
                    name: "read",
                    args: json!({ "path": "src/api.rs" }),
                },
                Reply::Text(NO_FINDINGS),
            ],
        )],
        vec![submit("p1", users_plan("Users", 0)), Reply::Text("Saved.")],
    )
    .await;
    let mut request = h.plan_request(&url, plan_reviewer_on(json!({})));
    request.config.as_mut().unwrap().permissions.tool_approval = permissions::ApprovalRule {
        required: true,
        by: permissions::Approver::User,
    };
    h.start(request).await;
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert_eq!(asked["request"]["toolCallId"], "r1");
    assert_eq!(asked["request"]["parentCallId"], "p1");
    // Waiting: nothing saved yet.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.plan_files().is_empty());
    let core = h.core.clone();
    let child = asked["request"]["dialogId"].as_str().unwrap().to_string();
    assert!(agent_resolve_approval(&core, child, "r1".into(), ApprovalDecision::Allow).await);
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let saved = result_of(&messages, "p1");
    assert!(
        saved.content.starts_with("Saved the plan"),
        "{}",
        saved.content
    );
    assert_eq!(saved.meta.as_ref().unwrap()["review"]["status"], "clean");
    assert_eq!(h.plan_files().len(), 1);
}

/// A plan held by its review, and the run ends: the next run's revision
/// is reviewed afresh (its own rounds), not saved unreviewed.
#[tokio::test]
async fn a_plan_held_in_one_run_is_reviewed_in_the_next() {
    let h = plan_project().await;
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(PLAN_FINDING)])],
        vec![
            submit("p1", users_plan("Users", 0)),
            Reply::Text("I'll add the step next time."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.plan_files().is_empty(), "held, never saved");

    let (url2, seen2) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![submit("p2", users_plan("Users", 1)), Reply::Text("Saved.")],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.plan_request(&url2, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(requests_with(&seen2, PLAN_REVIEW).len(), 1, "reviewed");
    let messages = h.messages().await;
    let second = result_of(&messages, "p2");
    let review = &second.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (review["round"].as_u64(), review["status"].as_str()),
        (Some(1), Some("clean"))
    );
    let files = h.plan_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].contains("**Caller 0**"));
}

/// Deleting a project removes its chats' subagents' files too (their
/// copies and landing records), and their rows.
#[tokio::test]
async fn deleting_a_project_removes_its_subagents_chats_and_files() {
    let mut h = harness(&[("a.txt", "x\n")]).await;
    h.isolate().await;
    let db = h.core.db.clone();
    let root = h.project.clone();
    let project_id = db
        .write(move |c| crate::chat_db::projects::open_project(c, &root))
        .await
        .unwrap()
        .project
        .id;
    let (parent, pid) = (h.dialog_id.clone(), project_id.clone());
    let kid = uuid::Uuid::new_v4().to_string();
    let grandkid = uuid::Uuid::new_v4().to_string();
    let (k, g) = (kid.clone(), grandkid.clone());
    db.write(move |c| {
        c.execute(
            "UPDATE dialogs SET project_id = ?1 WHERE id = ?2",
            rusqlite::params![pid, parent],
        )
        .map_err(|e| e.to_string())?;
        for (id, of) in [(&k, &parent), (&g, &k)] {
            store::create_child_dialog(
                c,
                &store::NewChildDialog {
                    id: id.clone(),
                    parent_id: of.clone(),
                    parent_call_id: "c".into(),
                    kind: "worker".into(),
                    title: "Worker".into(),
                    created_at: store::now_ms(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    // What a kept copy and a landing leave on disk.
    let data = h._dir.0.join("data");
    let left = |id: &str| {
        [
            data.join("overlays").join(id),
            data.join("landings").join(id),
        ]
    };
    for id in [&kid, &grandkid] {
        for dir in left(id) {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("project.txt"), "x").unwrap();
        }
    }
    let core = h.core.clone();
    crate::chat_db::api::chat_delete_project(&core, project_id, Vec::new())
        .await
        .unwrap();
    for id in [&kid, &grandkid] {
        for dir in left(id) {
            assert!(!dir.exists(), "{} left", dir.display());
        }
    }
    let ids = [h.dialog_id.clone(), kid, grandkid];
    let rows: i64 = db
        .read(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM dialogs WHERE id IN (?1, ?2, ?3)",
                rusqlite::params![ids[0], ids[1], ids[2]],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

/// Round 2 (§4.2): a brief that can't be stored leaves nothing behind:
/// no child running or registered, no child chat, and its place back.
#[tokio::test]
async fn a_failed_brief_append_leaves_nothing_running() {
    let h = harness(&[("a.txt", "a\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-FB", vec![Reply::Text(REPORT)])],
        vec![
            task("fb1", "MARK-FB: first"),
            task("fb2", "MARK-FB: second"),
            Reply::Text("Done."),
        ],
    )
    .await;
    crate::agent_run::subagent_run::FAIL_BRIEF
        .lock()
        .unwrap()
        .insert("fb1".into());
    let mut config = scouts_on(json!({}));
    config.max_per_run = Some(1);
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    let end = h.run_end().await;
    crate::agent_run::subagent_run::FAIL_BRIEF
        .lock()
        .unwrap()
        .remove("fb1");
    assert_eq!(end, "done");
    let messages = h.messages().await;
    assert!(
        result_of(&messages, "fb1")
            .content
            .contains("the brief couldn't be stored"),
        "{}",
        result_of(&messages, "fb1").content
    );
    let (id, call) = (h.dialog_id.clone(), "fb1".to_string());
    let db = h.core.db.clone();
    let child = db
        .read(move |c| store::child_for_call(c, &id, &call, None))
        .await
        .unwrap();
    assert!(child.is_none(), "no child chat left: {child:?}");
    assert_eq!(h.core.agents.running_children(&h.dialog_id), 0);
    assert!(
        h.task_result("fb2").await.content.contains("Status: done"),
        "its place went back"
    );
}

/// Round 2, P1.1: a subagent admitted but never launched gives its place
/// back: with a limit of one, the next `task` still starts.
#[tokio::test]
async fn a_failed_launch_gives_its_slot_back() {
    let h = harness(&[("a.txt", "a\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-SLOT", vec![Reply::Text(REPORT)])],
        vec![
            task("f1", "MARK-SLOT: first"),
            task("f2", "MARK-SLOT: second"),
            Reply::Text("Done."),
        ],
    )
    .await;
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .insert("f1".into());
    let mut config = scouts_on(json!({}));
    config.max_per_run = Some(1);
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    h.run_end().await;
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .remove("f1");
    let first = h.task_result("f1").await;
    assert!(
        first.content.contains("already running"),
        "{}",
        first.content
    );
    let second = h.task_result("f2").await;
    assert!(
        second.content.contains("Status: done"),
        "{}",
        second.content
    );
}

/// Round 2 (§4.2): a step worker can't change files through a wrapper:
/// each command is refused and the files stay as they were.
#[tokio::test]
async fn a_worker_cant_write_through_a_wrapper() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let bash = |id: &'static str, command: &str| Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": command }),
    };
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                bash("c1", "env sed -i s/old/new/ a.txt"),
                bash("c2", "timeout 5 rm b.txt"),
                bash("c3", "env -S \"rm b.txt\""),
                bash("c4", "nice -n 5 cp a.txt c.txt"),
                worker_report("Tried.", &[]),
            ],
        )],
        vec![step("s2", 2), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let lead = h.messages().await;
    let step = result_of(&lead, "s2");
    assert!(!step.content.starts_with("Error"), "{}", step.content);
    let child = h.child_of("s2").await;
    let work = h.messages_of(&child.id).await;
    for id in ["c1", "c2", "c3", "c4"] {
        let result = work
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        assert!(result.starts_with("Error"), "{id}: {result}");
    }
    assert_eq!(h.file("a.txt").as_deref(), Some("old\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("b\n"));
    assert_eq!(h.file("c.txt"), None);
}

/*
 * Round 2 (section 4.2): child lifecycle and budgets.
 */

/// A test writer run for the greeter plan: `script` is its replies, `task`
/// the lead's call.
async fn test_writer_run(script: Vec<Reply>, task: Value) -> Harness {
    let h = harness(&[
        (
            "src/greet.ts",
            "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
        ),
        ("src/main.ts", "console.log('hi');\n"),
        (".rovik/plans/greeter.md", TEST_PLAN),
    ])
    .await;
    let (url, _) = model_routed(
        vec![("# Write tests for this change", script)],
        vec![
            Reply::Call {
                id: "tw",
                name: "task",
                args: task,
            },
            Reply::Text("Tests are written."),
        ],
    )
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/greeter.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    let mut request = h.request(
        &url,
        StartKind::Send { message },
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    h.run_end().await;
    h
}

impl Harness {
    /// The results in a child's chat, by call id.
    async fn child_results(&self, call: &str) -> std::collections::HashMap<String, String> {
        let child = self.child_of(call).await;
        self.messages_of(&child.id)
            .await
            .into_iter()
            .filter_map(|m| Some((m.tool_call_id.clone()?, m.content)))
            .collect()
    }
}

/// Refusals of every kind (a non-test write, a hidden read, a refused
/// command, a path outside the project) each use up the budget; past it,
/// the rest of the reply isn't run.
#[tokio::test]
async fn refusals_of_every_kind_count() {
    let h = test_writer_run(
        vec![
            Reply::Calls(vec![
                (
                    "x1",
                    "write",
                    json!({ "path": "src/greet.ts", "content": "x\n" }),
                ),
                ("x2", "read", json!({ "path": "src/greet.ts" })),
                ("x3", "bash", json!({ "command": "cat src/greet.ts" })),
                ("x4", "read", json!({ "path": "../outside.txt" })),
                (
                    "x5",
                    "write",
                    json!({ "path": "README.md", "content": "x\n" }),
                ),
                ("x6", "bash", json!({ "command": "rm -rf src" })),
            ]),
            Reply::Text(TEST_REPORT),
        ],
        json!({ "kind": "testWriter", "max_tool_calls": 4 }),
    )
    .await;
    let results = h.child_results("tw").await;
    for id in ["x1", "x2", "x3", "x4"] {
        assert!(results[id].starts_with("Error"), "{id}: {}", results[id]);
        assert!(
            !results[id].starts_with("Error: not run"),
            "{id}: {}",
            results[id]
        );
    }
    for id in ["x5", "x6"] {
        assert!(
            results[id].starts_with("Error: not run"),
            "{id}: {}",
            results[id]
        );
    }
    let result = h.task_result("tw").await;
    assert!(
        result.content.contains("Status: partial"),
        "{}",
        result.content
    );
    assert_eq!(result.meta.as_ref().unwrap()["subagent"]["toolCalls"], 4);
    assert_eq!(
        h.file("src/greet.ts")
            .as_deref()
            .map(|s| s.contains("Hello")),
        Some(true)
    );
}

/// Exactly at the budget: the calls ran, and the subagent still gets its
/// report turn (the budget is checked before a call, `>=`), so its report
/// stands.
#[tokio::test]
async fn budget_boundary_exactly_reached() {
    let h = test_writer_run(
        vec![
            Reply::Calls(vec![
                ("x1", "read", json!({ "path": "src/greet.ts" })),
                ("x2", "read", json!({ "path": "src/main.ts" })),
            ]),
            Reply::Text(TEST_REPORT),
        ],
        json!({ "kind": "testWriter", "max_tool_calls": 2 }),
    )
    .await;
    let results = h.child_results("tw").await;
    assert!(
        !results["x1"].starts_with("Error: not run"),
        "{}",
        results["x1"]
    );
    assert!(
        !results["x2"].starts_with("Error: not run"),
        "{}",
        results["x2"]
    );
    let result = h.task_result("tw").await;
    assert_eq!(result.meta.as_ref().unwrap()["subagent"]["toolCalls"], 2);
    assert!(
        result.content.contains("Tested greet."),
        "{}",
        result.content
    );
}

/// The same refused call made again: the refusal already said not to
/// retry, so the guard pauses the subagent at once (no warning first),
/// and its result says so.
#[tokio::test]
async fn a_refused_call_repeated_is_detected_as_a_loop() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                write_call("w1", "b.txt", "mine\n"),
                write_call("w2", "b.txt", "mine\n"),
                write_call("w3", "b.txt", "mine\n"),
                write_call("w4", "b.txt", "mine\n"),
                worker_report("Stuck.", &[]),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let results = h.child_results("s1").await;
    assert!(results["w1"].starts_with("Error"), "{}", results["w1"]);
    assert!(
        results["w2"].contains("the run was paused here for the user, because write (b.txt) was made again after it was rejected"),
        "{results:#?}"
    );
    assert!(
        !results.contains_key("w3"),
        "stopped at its second try: {results:#?}"
    );
    assert_eq!(h.file("b.txt").as_deref(), Some("b\n"));
}

/// extend_scope uses the budget like any call, granted or refused.
#[tokio::test]
async fn scope_grants_count_as_calls_not_refusals() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let extend = |id: &'static str, paths: Value| Reply::Call {
        id,
        name: "extend_scope",
        args: json!({ "paths": paths, "reason": "needed" }),
    };
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                extend("e1", json!(["c.txt"])),
                extend("e2", json!(["notes/"])),
                write_call("w1", "c.txt", "c\n"),
                worker_report("Ran out.", &[]),
            ],
        )],
        vec![
            Reply::Call {
                id: "s1",
                name: "task",
                args: json!({ "kind": "worker", "step": 1, "max_tool_calls": 2 }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let results = h.child_results("s1").await;
    assert!(results["e1"].starts_with("Granted"), "{}", results["e1"]);
    assert!(
        results["e2"].starts_with("Error: not granted"),
        "{}",
        results["e2"]
    );
    // Its budget used, the write is never run.
    assert!(
        results
            .get("w1")
            .is_none_or(|r| r.starts_with("Error: not run")),
        "{results:#?}"
    );
    let result = h.task_result("s1").await;
    assert_eq!(result.meta.as_ref().unwrap()["subagent"]["toolCalls"], 2);
    assert_eq!(h.file("c.txt"), None);
}

/// Reads that run side by side, refused or not, use the budget one each.
#[tokio::test]
async fn refused_parallel_reads_count_once_each() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let read = |id: &'static str| (id, "read", json!({ "path": format!("../{id}.txt") }));
    let (url, _) = model_routed(
        vec![(
            "MARK-PR",
            vec![
                Reply::Calls(vec![
                    read("r1"),
                    read("r2"),
                    read("r3"),
                    read("r4"),
                    read("r5"),
                ]),
                Reply::Text(REPORT),
            ],
        )],
        vec![
            Reply::Call {
                id: "t1",
                name: "task",
                args: json!({ "kind": "scout", "goal": "MARK-PR: look", "max_tool_calls": 3 }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Look", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.run_end().await;
    let results = h.child_results("t1").await;
    let not_run = results
        .values()
        .filter(|r| r.starts_with("Error: not run"))
        .count();
    assert_eq!(not_run, 2, "{results:#?}");
    let result = h.task_result("t1").await;
    assert_eq!(result.meta.as_ref().unwrap()["subagent"]["toolCalls"], 3);
}

/// A follow-up's goal is capped like a first one's.
#[tokio::test]
async fn a_follow_up_goal_is_capped() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let long: &'static str = Box::leak("y".repeat(subagent::MAX_GOAL_CHARS + 1).into_boxed_str());
    let ok: &'static str = Box::leak("y".repeat(subagent::MAX_GOAL_CHARS).into_boxed_str());
    let (url, _) = model_routed(
        vec![("MARK-FG", vec![Reply::Text(REPORT), Reply::Text(REPORT)])],
        vec![
            task("t1", "MARK-FG: where?"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "t1", "goal": long }),
            },
            Reply::Call {
                id: "c2",
                name: "task",
                args: json!({ "continue": "t1", "goal": ok }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.run_end().await;
    let too_long = h.task_result("c1").await.content;
    assert!(
        too_long.starts_with("Error: \"goal\" is too long"),
        "{too_long}"
    );
    let fine = h.task_result("c2").await.content;
    assert!(fine.contains("Status: done"), "{fine}");
}

/// A follow-up's own max_tool_calls is its budget.
#[tokio::test]
async fn a_follow_up_max_tool_calls_is_honoured() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let read = |id: &'static str| (id, "read", json!({ "path": "a.txt" }));
    let (url, _) = model_routed(
        vec![(
            "MARK-FM",
            vec![
                Reply::Text(REPORT),
                Reply::Calls(vec![read("f1"), read("f2"), read("f3")]),
                Reply::Text(REPORT),
            ],
        )],
        vec![
            task("t1", "MARK-FM: where?"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "t1", "goal": "and more?", "max_tool_calls": 1 }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Find it", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.run_end().await;
    let results = h.child_results("t1").await;
    assert!(!results["f1"].starts_with("Error"), "{}", results["f1"]);
    for id in ["f2", "f3"] {
        assert!(
            results[id].starts_with("Error: not run"),
            "{id}: {}",
            results[id]
        );
    }
}

/// The same step given to a worker a third time in a run is refused,
/// retries counted.
#[tokio::test]
async fn same_brief_limit_counts_retries_across_the_run() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![("# Your step: 1.", vec![worker_report("Nothing.", &[])])],
        vec![
            step("s1", 1),
            step("s2", 1),
            step("s3", 1),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let third = h.task_result("s3").await.content;
    assert!(
        third.contains("step 1 was already given to a worker 2 times"),
        "{third}"
    );
}

/// Calls in a reply that came back after the time budget ran out aren't
/// run, and the subagent ends soon after.
#[tokio::test]
async fn remaining_calls_in_a_reply_are_refused_once_time_is_up() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-WALL",
            vec![
                // Past the time budget (three seconds).
                Reply::Slow(
                    4_500,
                    Box::new(Reply::Calls(vec![
                        ("r1", "read", json!({ "path": "a.txt" })),
                        ("r2", "read", json!({ "path": "a.txt" })),
                    ])),
                ),
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "MARK-WALL: where?"), Reply::Text("Done.")],
    )
    .await;
    let started = std::time::Instant::now();
    h.start(h.with_subagents(
        &url,
        "Go",
        quiet_permissions(),
        scouts_on(json!({ "maxMinutes": 0.05 })),
    ))
    .await;
    h.run_end().await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    let results = h.child_results("t1").await;
    for id in ["r1", "r2"] {
        assert!(
            results[id].starts_with("Error: not run") && results[id].contains("time budget"),
            "{id}: {}",
            results[id]
        );
    }
    assert!(h
        .task_result("t1")
        .await
        .content
        .contains("Status: partial"));
}

/// Time spent waiting for the user's approval isn't the subagent's: one
/// approved after its whole budget still runs the call.
#[tokio::test]
async fn waiting_for_an_approval_doesnt_spend_the_wall() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-WAIT",
            vec![
                Reply::Call {
                    id: "s1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Text(REPORT),
            ],
        )],
        vec![task("t1", "MARK-WAIT: where?"), Reply::Text("Done.")],
    )
    .await;
    let permissions = Permissions {
        tool_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    h.start(h.with_subagents(
        &url,
        "Go",
        permissions,
        scouts_on(json!({ "maxMinutes": 0.05 })),
    ))
    .await;
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let child = asked["request"]["dialogId"].as_str().unwrap().to_string();
    // Longer than its whole budget (three seconds).
    tokio::time::sleep(Duration::from_secs(4)).await;
    let core = h.core.clone();
    assert!(agent_resolve_approval(&core, child, "s1".into(), ApprovalDecision::Allow).await);
    h.run_end().await;
    let results = h.child_results("t1").await;
    assert!(results["s1"].contains('x'), "{}", results["s1"]);
    assert!(h.task_result("t1").await.content.contains("Status: done"));
}

/// A subagent of a batch that can't be launched gets an error; the others
/// run, and none is left registered.
#[tokio::test]
async fn an_error_before_the_batch_starts_unregisters_launched_children() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-EB", vec![Reply::Text(REPORT), Reply::Text(REPORT)])],
        vec![
            Reply::Calls(vec![
                task_call("eb1", "MARK-EB: one"),
                task_call("eb2", "MARK-EB: two"),
                task_call("eb3", "MARK-EB: three"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .insert("eb2".into());
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let end = h.run_end().await;
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .remove("eb2");
    assert_eq!(end, "done");
    assert!(h.task_result("eb2").await.content.starts_with("Error"));
    for ok in ["eb1", "eb3"] {
        assert!(
            h.task_result(ok).await.content.contains("Status: done"),
            "{ok}"
        );
    }
    let state = &h.core.agents;
    assert_eq!(state.running_children(&h.dialog_id), 0);
    let status = serde_json::to_value(agent_status(&h.core).await).unwrap();
    assert_eq!(status["subagents"], json!([]), "{status}");
}

/// The lead failing while subagents wait for a place (more than
/// maxAtOnce): every one started is ended, and the lead is free soon.
#[tokio::test]
async fn queued_children_past_max_at_once_are_stopped_with_the_lead() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "MARK-QM",
            (0..5)
                .map(|_| Reply::Slow(300, Box::new(Reply::Text(REPORT))))
                .collect(),
        )],
        vec![Reply::Calls(vec![
            task_call("qm-1", "MARK-QM: one"),
            task_call("qm-2", "MARK-QM: two"),
            task_call("qm-3", "MARK-QM: three"),
            task_call("qm-4", "MARK-QM: four"),
            task_call("qm-5", "MARK-QM: five"),
        ])],
    )
    .await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .insert("qm-1".into());
    let mut config = scouts_on(json!({}));
    config.max_at_once = Some(2);
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), config))
        .await;
    let end = h.run_end().await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .remove("qm-1");
    assert_eq!(end, "error");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let state = &h.core.agents;
    while state.is_running(&h.dialog_id) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!state.is_running(&h.dialog_id));
    assert_eq!(state.running_children(&h.dialog_id), 0);
    let started = h.events_of("subagentStarted").len();
    let ended = h.events_of("subagentEnded").len();
    assert_eq!(started, ended, "every one started is ended");
}

/// After a batch whose lead failed, the next run in the chat starts.
#[tokio::test]
async fn a_second_run_in_the_same_chat_starts_after_a_failed_batch() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-SR", vec![Reply::Text(REPORT), Reply::Text(REPORT)])],
        vec![Reply::Calls(vec![
            task_call("sr-1", "MARK-SR: one"),
            task_call("sr-2", "MARK-SR: two"),
        ])],
    )
    .await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .insert("sr-1".into());
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "error");
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .remove("sr-1");
    let (url, _) = model(vec![Reply::Text("Second run.")]).await;
    h.events.lock().unwrap().clear();
    h.start(h.with_subagents(&url, "Again", quiet_permissions(), scouts_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
}

/// A test writer continued by the call that started it writes, so it runs
/// on its own, not batched with scouts.
#[tokio::test]
async fn continue_by_call_id_of_a_test_writer_runs_alone() {
    let h = harness(&[
        (
            "src/greet.ts",
            "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
        ),
        ("src/main.ts", "console.log('hi');\n"),
        (".rovik/plans/greeter.md", TEST_PLAN),
    ])
    .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Write tests for this change",
                vec![Reply::Text(TEST_REPORT), Reply::Text(TEST_REPORT)],
            ),
            ("MARK-TA", vec![Reply::Text(REPORT)]),
        ],
        vec![
            test_task("tw"),
            Reply::Calls(vec![
                (
                    "c1",
                    "task",
                    json!({ "continue": "tw", "goal": "one more case?" }),
                ),
                task_call("q1", "MARK-TA: where?"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/greeter.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    let mut request = h.request(
        &url,
        StartKind::Send { message },
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    let mut config = test_writer_on();
    config.kinds.insert(
        "scout".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    h.run_end().await;
    let batches = crate::agent_run::parallel::batches_of(&h.dialog_id);
    assert!(
        batches.iter().any(|b| b == &vec!["c1".to_string()]),
        "{batches:?}"
    );
    assert!(!batches.iter().any(|b| b.len() > 1), "{batches:?}");
}

/// An unknown handle in a batch: one named like a writing kind's runs
/// alone (it might write) and errors cleanly; any other is batched with
/// the scouts and errors cleanly, the scouts unharmed.
#[tokio::test]
async fn continue_with_an_unknown_handle_in_a_batch() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![("MARK-UH", vec![Reply::Text(REPORT), Reply::Text(REPORT)])],
        vec![
            Reply::Calls(vec![
                (
                    "u1",
                    "task",
                    json!({ "continue": "worker-deadbeef", "goal": "go on" }),
                ),
                task_call("q1", "MARK-UH: one"),
            ]),
            Reply::Calls(vec![
                (
                    "u2",
                    "task",
                    json!({ "continue": "nobody", "goal": "go on" }),
                ),
                task_call("q2", "MARK-UH: two"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    h.run_end().await;
    for id in ["u1", "u2"] {
        let r = h.task_result(id).await.content;
        assert!(r.starts_with("Error"), "{id}: {r}");
    }
    for id in ["q1", "q2"] {
        assert!(
            h.task_result(id).await.content.contains("Status: done"),
            "{id}"
        );
    }
    let batches = crate::agent_run::parallel::batches_of(&h.dialog_id);
    assert!(
        batches.iter().any(|b| b == &vec!["u1".to_string()]),
        "{batches:?}"
    );
    assert!(
        batches
            .iter()
            .any(|b| b == &vec!["u2".to_string(), "q2".to_string()]),
        "{batches:?}"
    );
}

/// A scout's chat as the app left it at a restart: the lead's task call
/// (`max_tool_calls` of `budget`) with no result, and in the scout's chat
/// its brief and `made` calls, each with its result.
async fn scout_left_at_a_restart(h: &Harness, made: usize, budget: u32) -> String {
    let db = h.core.db.clone();
    let lead = h.dialog_id.clone();
    let mut user = crate::agent_run::run::new_message("user", "Find it");
    user.meta = Some(json!({ "mode": "agent" }));
    let mut call = crate::agent_run::run::new_message("assistant", "");
    call.tool_calls = Some(vec![store::ToolCall {
        id: "t1".into(),
        name: "task".into(),
        arguments: json!({ "kind": "scout", "goal": "MARK-RB: where?", "max_tool_calls": budget })
            .to_string(),
        extra_content: None,
    }]);
    let child = uuid::Uuid::new_v4().to_string();
    let mut work = vec![crate::agent_run::run::new_message(
        "user",
        "# Task from the lead agent\n\nMARK-RB: where?",
    )];
    for n in 0..made {
        let mut read = crate::agent_run::run::new_message("assistant", "");
        read.tool_calls = Some(vec![store::ToolCall {
            id: format!("e{n}"),
            name: "read".into(),
            arguments: json!({ "path": "a.txt" }).to_string(),
            extra_content: None,
        }]);
        let mut result = crate::agent_run::run::new_message("tool", "x");
        result.tool_call_id = Some(format!("e{n}"));
        work.push(read);
        work.push(result);
    }
    let child_id = child.clone();
    db.write(move |c| {
        store::append_messages(c, &lead, &[user, call])?;
        store::create_child_dialog(
            c,
            &store::NewChildDialog {
                id: child_id.clone(),
                parent_id: lead.clone(),
                parent_call_id: "t1".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: store::now_ms(),
            },
        )?;
        store::append_messages(c, &child_id, &work)?;
        Ok(())
    })
    .await
    .unwrap();
    child
}

async fn resume_scout(h: &Harness, script: Vec<Reply>) {
    let (url, _) = model_routed(vec![("MARK-RB", script)], vec![Reply::Text("Done.")]).await;
    let mut request = h.with_subagents(&url, "unused", quiet_permissions(), scouts_on(json!({})));
    request.start = StartKind::Resume;
    h.start(request).await;
    h.run_end().await;
}

/// A resumed scout's earlier calls count: all of its budget used, it runs
/// nothing more; one left, one more call runs.
#[tokio::test]
async fn resumed_tool_calls_boundary() {
    let read = |id: &'static str| (id, "read", json!({ "path": "a.txt" }));
    let h = harness(&[("a.txt", "x\n")]).await;
    let child = scout_left_at_a_restart(&h, 3, 3).await;
    resume_scout(
        &h,
        vec![Reply::Calls(vec![read("n1")]), Reply::Text(REPORT)],
    )
    .await;
    let meta = h.task_result("t1").await.meta.unwrap()["subagent"].clone();
    assert_eq!(meta["toolCalls"], 3, "{meta}");
    assert!(!h.messages_of(&child).await.iter().any(
        |m| m.tool_call_id.as_deref() == Some("n1") && !m.content.starts_with("Error: not run")
    ));

    let h = harness(&[("a.txt", "x\n")]).await;
    let child = scout_left_at_a_restart(&h, 2, 3).await;
    resume_scout(
        &h,
        vec![
            Reply::Calls(vec![read("n1"), read("n2")]),
            Reply::Text(REPORT),
        ],
    )
    .await;
    let results: std::collections::HashMap<String, String> = h
        .messages_of(&child)
        .await
        .into_iter()
        .filter_map(|m| Some((m.tool_call_id.clone()?, m.content)))
        .collect();
    assert_eq!(results["n1"], "x\n");
    assert!(
        results["n2"].starts_with("Error: not run"),
        "{}",
        results["n2"]
    );
}

/// Round 2, P2.7: a resumed subagent's calls are counted however long its
/// chat is, not from its latest page of messages.
#[tokio::test]
async fn resume_counts_calls_beyond_one_page() {
    let h = harness(&[("a.txt", "x\n")]).await;
    scout_left_at_a_restart(&h, 600, 200).await;
    resume_scout(&h, vec![Reply::Text(REPORT)]).await;
    let meta = h.task_result("t1").await.meta.unwrap()["subagent"].clone();
    assert_eq!(meta["toolCalls"], 600, "{meta}");
}

/// After a batch whose lead failed, landing doesn't wait for subagents:
/// none is left at work.
#[tokio::test]
async fn landing_waits_only_for_live_children() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("a.txt", "x\n")]).await;
    h.isolate().await;
    let (url, _) = model_routed(
        vec![(
            "MARK-LL",
            (0..3)
                .map(|_| Reply::Slow(200, Box::new(Reply::Text(REPORT))))
                .collect(),
        )],
        vec![
            write_call("l1", "b.txt", "from the lead\n"),
            Reply::Calls(vec![
                task_call("ll-1", "MARK-LL: one"),
                task_call("ll-2", "MARK-LL: two"),
                task_call("ll-3", "MARK-LL: three"),
            ]),
        ],
    )
    .await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .insert("ll-1".into());
    let mut request = h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({})));
    request.isolate = Some(true);
    h.start(request).await;
    let end = h.run_end().await;
    crate::agent_run::run::FAIL_TOOL_RESULT
        .lock()
        .unwrap()
        .remove("ll-1");
    assert_eq!(end, "error");
    let core = h.core.clone();
    let landed =
        crate::overlay::chats::overlay_land(&core.agents, h.dialog_id.clone(), false).await;
    if let Err(e) = &landed {
        assert!(!e.contains("at work"), "{e}");
    }
    assert!(landed.is_ok(), "{:?}", landed.err());
    assert_eq!(h.file("b.txt").as_deref(), Some("from the lead\n"));
}

/// A resumed subagent's time budget starts when it's resumed, not when
/// its chat was started (an hour before, here).
#[tokio::test]
async fn a_resumed_subagents_wall_starts_at_resume() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let child = scout_left_at_a_restart(&h, 0, 10).await;
    let db = h.core.db.clone();
    let (id, lead) = (child.clone(), h.dialog_id.clone());
    db.write(move |c| {
        // An hour ago, the lead's reply that started it too: a child is
        // never older than the reply whose call started it (S14).
        for (sql, key) in [
            (
                "UPDATE dialogs SET created_at = created_at - 3600000 WHERE id = ?1",
                &id,
            ),
            (
                "UPDATE messages SET created_at = created_at - 3600000 WHERE dialog_id = ?1",
                &lead,
            ),
        ] {
            c.execute(sql, rusqlite::params![key])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let (url, _) = model_routed(
        vec![(
            "MARK-RB",
            vec![
                Reply::Call {
                    id: "n1",
                    name: "read",
                    args: json!({ "path": "a.txt" }),
                },
                Reply::Text(REPORT),
            ],
        )],
        vec![Reply::Text("Done.")],
    )
    .await;
    let mut request = h.with_subagents(
        &url,
        "unused",
        quiet_permissions(),
        scouts_on(json!({ "maxMinutes": 0.25 })),
    );
    request.start = StartKind::Resume;
    h.start(request).await;
    h.run_end().await;
    let work = h.messages_of(&child).await;
    let read = work
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("n1"))
        .unwrap();
    assert!(!read.content.starts_with("Error"), "{}", read.content);
    assert!(h.task_result("t1").await.content.contains("Status: done"));
}

/// The test writer can't see the code under test in any spelling, by any
/// tool: reads, neighbours and diagnostics (with or without a path), and
/// search_code says how many lines it left out.
#[tokio::test]
async fn hidden_paths_in_every_spelling_by_every_tool() {
    let h = test_writer_run(
        vec![
            Reply::Calls(vec![
                ("r1", "read", json!({ "path": "src/greet.ts/." })),
                ("r2", "read", json!({ "path": "./src/greet.ts" })),
                ("r3", "read", json!({ "path": "SRC/GREET.TS" })),
                ("r4", "read", json!({ "path": "src/../src/greet.ts" })),
                ("n1", "neighbours", json!({ "path": "src//greet.ts" })),
                ("d1", "diagnostics", json!({ "path": "./src/greet.ts" })),
                ("d2", "diagnostics", json!({})),
                ("s1", "search_code", json!({ "query": "Hello" })),
            ]),
            Reply::Text(TEST_REPORT),
        ],
        json!({ "kind": "testWriter" }),
    )
    .await;
    let results = h.child_results("tw").await;
    for (id, r) in &results {
        assert!(!r.contains("Hello, ${name}"), "{id} shows the code: {r}");
    }
    for id in ["r1", "r2", "n1", "d1"] {
        assert!(
            results[id].contains("one of the files the change touches"),
            "{id}: {}",
            results[id]
        );
    }
    assert!(results["r4"].starts_with("Error"), "{}", results["r4"]);
    assert!(
        results["s1"].contains("[1 match in the files the change touches left out"),
        "{}",
        results["s1"]
    );
}

/// extend_scope grants at most 8 paths beyond the step; a path granted
/// before is granted again (it adds nothing); what was granted is in the
/// result's meta only when granted.
#[tokio::test]
async fn grants_cap_boundary() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let mut script: Vec<Reply> = (1..=9)
        .map(|n| Reply::Call {
            id: Box::leak(format!("e{n}").into_boxed_str()),
            name: "extend_scope",
            args: json!({ "paths": [format!("extra/e{n}.txt")], "reason": "needed" }),
        })
        .collect();
    script.push(Reply::Call {
        id: "again",
        name: "extend_scope",
        args: json!({ "paths": ["extra/e1.txt"], "reason": "needed" }),
    });
    script.push(worker_report("Asked.", &[]));
    let (url, _) = model_routed(
        vec![("# Your step: 1.", script)],
        vec![step("s1", 1), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let child = h.child_of("s1").await;
    let work = h.messages_of(&child.id).await;
    let result = |id: &str| {
        work.iter()
            .find(|m| m.tool_call_id.as_deref() == Some(id))
            .unwrap()
    };
    for n in 1..=8 {
        let r = result(&format!("e{n}"));
        assert!(r.content.starts_with("Granted"), "e{n}: {}", r.content);
        assert!(
            r.meta.as_ref().is_some_and(|m| m["granted"].is_array()),
            "e{n}"
        );
    }
    let ninth = result("e9");
    assert!(
        ninth.content.contains("at most 8 paths"),
        "{}",
        ninth.content
    );
    assert!(ninth
        .meta
        .as_ref()
        .is_none_or(|m| m.get("granted").is_none()));
    assert!(
        result("again").content.starts_with("Granted"),
        "{}",
        result("again").content
    );
}

/// Grants are the worker's: kept when it's continued in a later run, not
/// given to another worker of the same step.
#[tokio::test]
async fn grants_survive_a_restart_and_a_second_run() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                Reply::Call {
                    id: "e1",
                    name: "extend_scope",
                    args: json!({ "paths": ["extra.txt"], "reason": "needed" }),
                },
                worker_report("Granted, not written yet.", &[]),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    // A second run: the same worker continued writes it; a new worker of
    // the same step can't.
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                write_call("w1", "extra.txt", "by the first\n"),
                worker_report("Wrote it.", &["extra.txt"]),
                write_call("w2", "extra.txt", "by the second\n"),
                worker_report("Tried.", &[]),
            ],
        )],
        vec![
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "s1", "goal": "write extra.txt now" }),
            },
            step("s2", 1),
            Reply::Text("Done."),
        ],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let first = h.child_results("s1").await;
    assert!(!first["w1"].starts_with("Error"), "{}", first["w1"]);
    let second = h.child_results("s2").await;
    assert!(second["w2"].starts_with("Error"), "{}", second["w2"]);
    assert_eq!(h.file("extra.txt").as_deref(), Some("by the first\n"));
}

/// A file in the chat's view, while it's open (None when it isn't, or
/// the file isn't there).
fn in_view(h: &Harness, path: &str) -> Option<String> {
    let view = crate::overlay::chats::get(&h.dialog_id)?.view.clone();
    std::fs::read_to_string(view.join(path)).ok()
}

/// A script the test wrote, made a program: Unix's mode bits (Windows has
/// none to set).
fn runnable(script: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = script;
}

/// A group harness whose `npx eslint` runs `script` (a side effect).
async fn group_harness_with_side_effect(
    plan: &'static str,
    script: &str,
    extra: &[(&str, &str)],
) -> Harness {
    let body = format!("#!/bin/sh\n{script}\n");
    let mut files: Vec<(&str, &str)> = vec![
        ("src/api.rs", "pub fn users() {}\npub fn posts() {}\n"),
        ("src/users.rs", "// users\n"),
        ("src/posts.rs", "// posts\n"),
        (".rovik/plans/greet.md", plan),
        ("node_modules/.bin/eslint", body.as_str()),
    ];
    files.extend_from_slice(extra);
    let mut h = harness(&files).await;
    runnable(&h.project.join("node_modules/.bin/eslint"));
    h.isolate().await;
    h
}

fn eslint(id: &'static str) -> Reply {
    Reply::Call {
        id,
        name: "bash",
        // One file: a light check a worker may run.
        args: json!({ "command": "npx eslint src/api.rs" }),
    }
}

/// A new folder that holds the step's file lands with it; a file a
/// command made in a folder below it doesn't, and the lead is told.
#[tokio::test]
async fn out_of_scope_folder_with_in_scope_file_lands() {
    if !crate::vfs::mount_works() {
        return;
    }
    let plan: &'static str = Box::leak(
        GROUP_PLAN
            .replace("Files: `src/users.rs`,", "Files: `src/api/users.rs` (new),")
            .into_boxed_str(),
    );
    let h = group_harness_with_side_effect(
        plan,
        "mkdir -p src/api/cache && printf 'tmp\\n' > src/api/cache/x.tmp",
        &[],
    )
    .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/api/users.rs", "users\n"),
                    eslint("b1"),
                    worker_report("Users.", &["src/api/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let result = h.result_when_ready("g1").await;
    assert_eq!(in_view(&h, "src/api/users.rs").as_deref(), Some("users\n"));
    assert!(crate::overlay::chats::get(&h.dialog_id).is_some());
    assert_eq!(in_view(&h, "src/api/cache/x.tmp"), None);
    assert!(
        result
            .content
            .contains("Not landed, outside its step's scope"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("src/api/cache"),
        "{}",
        result.content
    );
    h.run_end().await;
}

/// A path granted to a worker is in its scope when it lands; another
/// worker's side effect on it isn't.
#[tokio::test]
async fn grants_make_a_path_in_scope_at_landing() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness_with_side_effect(
        GROUP_PLAN,
        "printf 'by w2s command\\n' > src/shared.rs",
        &[("src/shared.rs", "// shared\n")],
    )
    .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    Reply::Call {
                        id: "e1",
                        name: "extend_scope",
                        args: json!({ "paths": ["src/shared.rs"], "reason": "a helper" }),
                    },
                    write_call("w1", "src/shared.rs", "by w1\n"),
                    write_call("w1b", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs", "src/shared.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    eslint("b2"),
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let result = h.result_when_ready("g1").await;
    // The lead ends right after: what landed in its view lands in the
    // project, the view closing with it, so it's read there.
    h.run_end().await;
    assert_eq!(
        h.file("src/shared.rs").as_deref(),
        Some("by w1\n"),
        "{}",
        result.content
    );
    assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts\n"));
}

/// A worker with both a conflict and a side effect: both are named, the
/// conflicting file stays as the chat has it, and landing it later still
/// leaves the side effect out.
#[tokio::test]
async fn out_of_scope_and_conflict_together() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness_with_side_effect(GROUP_PLAN, "printf 'side effect\\n' > src/api.rs", &[])
        .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    eslint("b2"),
                    Reply::Slow(1_000, Box::new(worker_report("Posts.", &["src/posts.rs"]))),
                    worker_report("Nothing more.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            write_call("l1", "src/posts.rs", "from the user\nfrom w2\n"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w2", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/posts.rs", "from w2\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    assert!(
        result.content.contains("## The join stopped: a conflict"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("outside its step's scope"),
        "{}",
        result.content
    );
    let continued = h.result_when_ready("c1").await;
    assert_eq!(
        in_view(&h, "src/api.rs").as_deref(),
        Some("pub fn users() {}\npub fn posts() {}\n"),
        "the side effect never lands: {}",
        continued.content
    );
    assert_eq!(
        in_view(&h, "src/posts.rs").as_deref(),
        Some("from the user\nfrom w2\n")
    );
    h.run_end().await;
}

/// A worker that says it's done but reports another file than it changed:
/// partial, the step left open, and the lead told why.
#[tokio::test]
async fn a_done_worker_whose_report_names_the_wrong_file_is_partial() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let (url, _) = model_routed(
        vec![(
            "# Your step: 1.",
            vec![
                write_call("w1", "a.txt", "hello\n"),
                worker_report("Said hello.", &["b.txt"]),
            ],
        )],
        vec![step("s1", 1), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let r = h.task_result("s1").await;
    assert!(r.content.contains("Status: partial"), "{}", r.content);
    assert!(
        r.content.contains("what it changed isn't what it reported"),
        "{}",
        r.content
    );
    assert!(r.meta.as_ref().unwrap().get("checklistUpdate").is_none());
    assert_eq!(
        h.file("a.txt").as_deref(),
        Some("hello\n"),
        "its change is kept"
    );
}

/// A blocked worker keeps what it did of its step, and the lead hears why
/// it stopped.
#[tokio::test]
async fn a_blocked_worker_keeps_its_half_step_with_the_note() {
    let h = plan_harness(&[("a.txt", "old\n"), ("b.txt", "b\n")]).await;
    let blocked = "## Summary\nHalf of it.\n\n## Changed files\n- `a.txt`: changed\n\n## Open questions\nNone\n\n## Blocked\nb.txt is locked by another step.";
    let (url, _) = model_routed(
        vec![(
            "# Your step: 2.",
            vec![
                write_call("w1", "a.txt", "hello again\n"),
                Reply::Text(blocked),
            ],
        )],
        vec![step("s2", 2), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_with(&url, workers_on(json!({})), ChecklistMode::Off))
        .await;
    h.run_end().await;
    let r = h.task_result("s2").await;
    assert!(r.content.contains("Status: blocked"), "{}", r.content);
    assert!(
        r.content.contains("b.txt is locked by another step"),
        "{}",
        r.content
    );
    assert!(
        r.content.contains("Its changes are kept: a.txt."),
        "{}",
        r.content
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("hello again\n"));
}

/// A group worker whose report is out of format isn't landed at the join;
/// its copy waits until the lead says to land it.
#[tokio::test]
async fn an_unformatted_group_worker_isnt_landed_until_the_lead_says_so() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    Reply::Text("did stuff"),
                    Reply::Text("still no format"),
                    worker_report("Landing it.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            all_done(),
            group_task("g1"),
            Reply::Call {
                id: "c1",
                name: "task",
                args: json!({ "continue": "g1#w1", "goal": "land it" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let joined = h.result_when_ready("g1").await;
    assert!(
        joined
            .content
            .contains("Status: unformatted. Its copy is kept, not landed"),
        "{}",
        joined.content
    );
    let continued = h.result_when_ready("c1").await;
    assert!(
        continued.content.contains("Its copy landed: src/users.rs."),
        "{}",
        continued.content
    );
    // The lead ends right after: its changes land in the project, and its
    // view closes with them, maybe before it could be read. The landed
    // copy is in what landed, the project's file at its run's end.
    h.run_end().await;
    assert_eq!(h.file("src/users.rs").as_deref(), Some("users\n"));
}

/// A group worker whose only change was outside its scope changed nothing
/// of its step: partial, and nothing of it lands.
#[tokio::test]
async fn a_group_worker_with_only_out_of_scope_changes_is_partial() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness_with_side_effect(GROUP_PLAN, "printf 'side effect\\n' > src/api.rs", &[])
        .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![eslint("b1"), worker_report("Users done.", &[])],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    worker_report("Posts.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let joined = h.result_when_ready("g1").await;
    let w1 = joined
        .content
        .split("### Step 3")
        .next()
        .unwrap()
        .to_string();
    assert!(w1.contains("Status: partial"), "{w1}");
    assert!(w1.contains("outside its step's scope"), "{w1}");
    assert_eq!(
        in_view(&h, "src/api.rs").as_deref(),
        Some("pub fn users() {}\npub fn posts() {}\n")
    );
    h.run_end().await;
}

/// Busy landings (the chat's files changing under them) are tried again
/// a few times: once is retried and lands; for good, each worker is tried
/// exactly 4 times and its copy kept, with no landing journal left.
#[tokio::test]
async fn a_busy_landing_is_retried_then_kept() {
    if !crate::vfs::mount_works() {
        return;
    }
    for (busy, lands) in [(1usize, true), (10, false)] {
        let h = group_harness(&[]).await;
        let (url, _) = model_routed(
            vec![
                (
                    "# Your step: 2.",
                    vec![
                        write_call("w1", "src/users.rs", "users\n"),
                        Reply::Slow(500, Box::new(worker_report("Users.", &["src/users.rs"]))),
                    ],
                ),
                (
                    "# Your step: 3.",
                    vec![
                        write_call("w2", "src/posts.rs", "posts\n"),
                        Reply::Slow(500, Box::new(worker_report("Posts.", &["src/posts.rs"]))),
                    ],
                ),
            ],
            vec![all_done(), group_task("g1"), Reply::Text("Done.")],
        )
        .await;
        h.start(h.implement_isolated(&url, false)).await;
        let view = loop {
            if let Some(o) = crate::overlay::chats::get(&h.dialog_id) {
                break o.view.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        crate::overlay::land::BUSY_TIMES
            .lock()
            .unwrap()
            .insert(view.clone(), (busy, 0));
        let result = h.result_when_ready("g1").await;
        let (_, calls) = crate::overlay::land::BUSY_TIMES
            .lock()
            .unwrap()
            .remove(&view)
            .unwrap();
        if lands {
            assert_eq!(calls, 3, "one retry");
            // The copies landed in the chat's view, which lands in the
            // project as the lead ends right after, closing: read there.
            h.run_end().await;
            assert_eq!(
                h.file("src/users.rs").as_deref(),
                Some("users\n"),
                "{}",
                result.content
            );
            assert_eq!(h.file("src/posts.rs").as_deref(), Some("posts\n"));
        } else {
            assert_eq!(calls, 8, "4 tries for each of the two");
            assert!(
                result
                    .content
                    .contains("the chat's files kept changing under it"),
                "{}",
                result.content
            );
            let w1 = h.child_of("g1#w1").await;
            assert!(
                crate::overlay::chats::get(&w1.id).is_some(),
                "its copy is kept"
            );
            assert_eq!(in_view(&h, "src/users.rs").as_deref(), Some("// users\n"));
            assert!(!landing_interrupted(&h.dialog_id));
            h.run_end().await;
        }
    }
}

/// A conflict in one of a worker's three files names only that file; the
/// other two land, and one pair of versions is saved.
#[tokio::test]
async fn a_conflict_in_one_of_n_files_names_only_that_file() {
    if !crate::vfs::mount_works() {
        return;
    }
    let plan: &'static str = Box::leak(
        GROUP_PLAN
            .replace(
                "Files: `src/posts.rs`, `src/api.rs` (read)\n",
                "Files: `src/posts.rs`, `src/extra.rs`, `src/extra2.rs`, `src/api.rs` (read)\n",
            )
            .into_boxed_str(),
    );
    let h = group_harness_with_side_effect(
        plan,
        "true",
        &[
            ("src/extra.rs", "// extra\n"),
            ("src/extra2.rs", "// extra2\n"),
        ],
    )
    .await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "from w2\n"),
                    write_call("w3", "src/extra.rs", "extra\n"),
                    write_call("w4", "src/extra2.rs", "extra2\n"),
                    Reply::Slow(
                        1_000,
                        Box::new(worker_report(
                            "Posts.",
                            &["src/posts.rs", "src/extra.rs", "src/extra2.rs"],
                        )),
                    ),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    edit_while_workers_run(&h, "src/posts.rs", "from w2\n", "from the user\n").await;
    let result = h.result_when_ready("g1").await;
    let text = &result.content;
    assert!(
        text.contains("changed the same lines as the chat's view in `src/posts.rs`."),
        "{text}"
    );
    assert!(
        text.contains("Its other files landed: src/extra.rs, src/extra2.rs."),
        "{text}"
    );
    assert_eq!(
        text.matches("@checks/conflict-").count(),
        2,
        "one pair: {text}"
    );
    let w2 = result.meta.as_ref().unwrap()["group"]["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["label"] == "w2")
        .cloned()
        .unwrap();
    assert_eq!(w2["landed"].as_array().unwrap().len(), 2, "{w2}");
    assert_eq!(in_view(&h, "src/extra2.rs").as_deref(), Some("extra2\n"));
    assert_eq!(
        in_view(&h, "src/posts.rs").as_deref(),
        Some("from the user\n")
    );
    h.run_end().await;
}

/// The user's edit in Latin-1 conflicting with a worker's: the version
/// saved for the lead is text (lossy), while the chat's file keeps its
/// exact bytes and the worker's copy its own.
#[tokio::test]
async fn non_utf8_contents_land_and_roll_back_exactly() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts from w2\n"),
                    Reply::Slow(1_000, Box::new(worker_report("Posts.", &["src/posts.rs"]))),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    let latin1: Vec<u8> = b"caf\xe9 by the user\n".to_vec();
    // As the user, once w2 has written its version.
    for _ in 0..400 {
        let written = crate::overlay::chats::children_of(&h.dialog_id)
            .iter()
            .filter_map(|c| crate::overlay::chats::get(c))
            .any(|o| {
                std::fs::read_to_string(o.view.join("src/posts.rs"))
                    .ok()
                    .as_deref()
                    == Some("posts from w2\n")
            });
        if written {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let view = crate::overlay::chats::get(&h.dialog_id)
        .unwrap()
        .view
        .clone();
    std::fs::write(view.join("src/posts.rs"), &latin1).unwrap();
    let result = h.result_when_ready("g1").await;
    assert!(
        result.content.contains("## The join stopped: a conflict"),
        "{}",
        result.content
    );
    assert_eq!(
        std::fs::read(view.join("src/posts.rs")).unwrap(),
        latin1,
        "exact bytes"
    );
    let saved = crate::agent_run::check_output::resolve(
        &h.dialog_id,
        "@checks/conflict-w2-src-posts-rs-chat.log",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        "caf\u{fffd} by the user\n"
    );
    let w2 = h.child_of("g1#w2").await;
    let copy = crate::overlay::chats::get(&w2.id).unwrap().view.clone();
    assert_eq!(
        std::fs::read(copy.join("src/posts.rs")).unwrap(),
        b"posts from w2\n"
    );
    h.run_end().await;
}

/// Stop while a group's other worker is still at work: the worker that
/// finished lands, the stopped one is discarded, and none is retried.
#[tokio::test]
async fn stop_during_the_join_keeps_finished_copies() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = group_harness(&[]).await;
    let (url, _) = model_routed(
        vec![
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "posts\n"),
                    Reply::Hang(None),
                ],
            ),
        ],
        vec![all_done(), group_task("g1"), Reply::Text("Done.")],
    )
    .await;
    h.start(h.implement_isolated(&url, false)).await;
    // Once w1 has reported and w2 has written, Stop.
    for _ in 0..400 {
        let ended = h.events_of("subagentEnded").len();
        let w2_wrote = crate::overlay::chats::children_of(&h.dialog_id)
            .iter()
            .filter_map(|c| crate::overlay::chats::get(c))
            .any(|o| {
                std::fs::read_to_string(o.view.join("src/posts.rs"))
                    .ok()
                    .as_deref()
                    == Some("posts\n")
            });
        if ended >= 1 && w2_wrote {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let core = h.core.clone();
    assert!(agent_stop(&core, h.dialog_id.clone()).await);
    let result = h.result_when_ready("g1").await;
    assert_eq!(h.run_end().await, "stopped");
    let text = &result.content;
    assert!(
        text.contains("### Step 2 (w1): Users\nLanded: src/users.rs."),
        "{text}"
    );
    assert!(
        text.contains("Status: stopped. Its copy was discarded"),
        "{text}"
    );
    assert_eq!(h.events_of("subagentStarted").len(), 2, "no retry");
    let w2 = h.child_of("g1#w2").await;
    assert!(crate::overlay::chats::get(&w2.id).is_none(), "discarded");
}

/// A triage that names a folder among the files to fix doesn't break the
/// fixer's snapshot; Redo then puts the fixed file back as the chat had it
/// before the group (the lead's version).
#[tokio::test]
async fn a_folder_in_cause_files_doesnt_break_the_snapshot() {
    if !crate::vfs::mount_works() {
        return;
    }
    let Some(h) = repair_harness(false).await else {
        return;
    };
    let triage = "## Root cause\n`p` is never declared in src/posts.rs:1.\n\n\
        ## Files to fix\n- `src/posts.rs`: declare it\n- `src/`: the module\n\n\
        ## Next step\nDeclare `p`.\n\n## Confidence\nhigh: the error says so.";
    let (url, _) = model_routed(
        vec![
            ("# Triage this failure", vec![Reply::Text(triage)]),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs"]),
                ],
            ),
        ],
        vec![
            write_call("l1", "src/posts.rs", "posts by the lead\n"),
            // A step of the lead's own left open: the chat doesn't land
            // at the end, so its group can still be redone.
            Reply::Call {
                id: "t1",
                name: "todo",
                args: json!({ "items": [
                    { "title": "Contract", "status": "done" },
                    { "title": "Users", "status": "pending" },
                    { "title": "Posts", "status": "pending" },
                    { "title": "Docs", "status": "pending" },
                ] }),
            },
            group_task("g1"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let mut config = groups_on();
    config.kinds.insert(
        "triage".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    h.run_end().await;
    let records = end_records(&h.messages().await);
    let fixer = &records[0]["repair"]["causes"][0]["fixer"];
    assert_eq!(fixer["status"], "done", "{:#}", records[0]);
    let groups = crate::overlay::chats::saved_groups(&h.dialog_id);
    assert!(
        groups[0].unavailable.is_none(),
        "{:?}",
        groups[0].unavailable
    );
    crate::overlay::chats::redo_group(&h.dialog_id, &groups[0].name).unwrap();
    assert_eq!(
        in_view(&h, "src/posts.rs").as_deref(),
        Some("posts by the lead\n")
    );
}

/// A fixer that also writes a file of its step outside its cause's files:
/// Redo puts that file back as the chat had it (the lead's version).
#[tokio::test]
async fn a_fixer_writing_outside_paths_is_recorded_from_the_chats_own_version() {
    if !crate::vfs::mount_works()
        || std::process::Command::new("npm")
            .arg("--version")
            .output()
            .is_err()
    {
        return;
    }
    let plan: &'static str = Box::leak(
        GROUP_PLAN
            .replace(
                "Files: `src/posts.rs`, `src/api.rs` (read)\n",
                "Files: `src/posts.rs`, `src/extra.rs`, `src/api.rs` (read)\n",
            )
            .into_boxed_str(),
    );
    let h = group_harness_with_side_effect(
        plan,
        "true",
        &[
            ("src/extra.rs", "// extra\n"),
            ("check.js", REPAIR_CHECK),
            (
                "package.json",
                r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#,
            ),
        ],
    )
    .await;
    let triage = "## Root cause\n`p` is never declared in src/posts.rs:1.\n\n\
        ## Files to fix\n- `src/posts.rs`: declare it\n\n\
        ## Next step\nDeclare `p`.\n\n## Confidence\nhigh: the error says so.";
    let (url, _) = model_routed(
        vec![
            ("# Triage this failure", vec![Reply::Text(triage)]),
            (
                "# Your step: 2.",
                vec![
                    write_call("w1", "src/users.rs", "users\n"),
                    worker_report("Users page.", &["src/users.rs"]),
                ],
            ),
            (
                "# Your step: 3.",
                vec![
                    write_call("w2", "src/posts.rs", "bad posts\n"),
                    worker_report("Posts page.", &["src/posts.rs"]),
                    write_call("w2f", "src/posts.rs", "posts, fixed\n"),
                    write_call("w2g", "src/extra.rs", "extra by the fixer\n"),
                    worker_report("Fixed the posts page.", &["src/posts.rs", "src/extra.rs"]),
                ],
            ),
        ],
        vec![
            write_call("l1", "src/extra.rs", "extra by the lead\n"),
            Reply::Call {
                id: "t1",
                name: "todo",
                args: json!({ "items": [
                    { "title": "Contract", "status": "done" },
                    { "title": "Users", "status": "pending" },
                    { "title": "Posts", "status": "pending" },
                    { "title": "Docs", "status": "pending" },
                ] }),
            },
            group_task("g1"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let mut request = h.implement_isolated(&url, true);
    let mut config = groups_on();
    config.kinds.insert(
        "triage".into(),
        serde_json::from_value(json!({ "enabled": true })).unwrap(),
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    h.run_end().await;
    assert_eq!(
        in_view(&h, "src/extra.rs").as_deref(),
        Some("extra by the fixer\n")
    );
    let groups = crate::overlay::chats::saved_groups(&h.dialog_id);
    assert!(
        groups[0]
            .before
            .iter()
            .any(|(p, b)| p == "src/extra.rs"
                && matches!(b, crate::overlay::chats::Before::File { .. })),
        "{:?}",
        groups[0].before
    );
    crate::overlay::chats::redo_group(&h.dialog_id, &groups[0].name).unwrap();
    assert_eq!(
        in_view(&h, "src/extra.rs").as_deref(),
        Some("extra by the lead\n")
    );
}

/// P4.6: a plan submitted once the run's plan reviews are used is saved
/// with exactly this note and record (no round, time or reviewer).
#[tokio::test]
async fn budget_used_record_shape() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_RESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            submit("p3", users_plan("Users v3", 3)),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(requests_with(&seen, PLAN_REVIEW).len(), 2);
    let messages = h.messages().await;
    let file = result_of(&messages, "p2").meta.as_ref().unwrap()["planFile"].clone();
    assert!(file.is_string(), "{file}");
    let third = result_of(&messages, "p3");
    assert_eq!(
        third.content,
        format!(
            "Saved the plan to {}. Review skipped: this run's 2 plan reviews are used.",
            shown(".rovik/plans/2026-10-06-add-users.md")
        )
    );
    let mut third_meta = third.meta.clone().unwrap();
    // Step 7: the plan as checked is kept with the result.
    assert!(third_meta["plan"]
        .as_str()
        .is_some_and(|p| p.starts_with("# Users")));
    third_meta.as_object_mut().unwrap().remove("plan");
    assert_eq!(
        &third_meta,
        &json!({
            "planFile": file,
            "review": {
                "kind": "plan",
                "status": "skipped",
                "reason": "this run's 2 plan reviews are used",
                "maxRounds": 2,
            },
        })
    );
    assert!(h.plan_files()[0].starts_with("# Users v3"));
}

/// Pins the per-run reset: a run that used both plan reviews doesn't use
/// up the next run's. Its first review is round 1 again, and its brief
/// carries none of the earlier run's findings (they're in the saved plan's
/// open-findings section, which the reviewed revision replaces).
#[tokio::test]
async fn plan_review_across_a_second_run() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_UNRESOLVED)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            submit_with("p2", users_plan("Users", 1), added_the_step()),
            Reply::Text("Saved, with one finding open."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(requests_with(&seen, PLAN_REVIEW).len(), 2);
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "p2").meta.as_ref().unwrap()["review"]["open"],
        1
    );
    assert!(h.plan_files()[0].contains("## Open review findings"));

    // The next run: more than a small revision, so it's reviewed, afresh.
    let (url2, seen2) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text(NO_FINDINGS)])],
        vec![submit("p3", users_plan("Users", 6)), Reply::Text("Saved.")],
    )
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.plan_request(&url2, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let briefs = requests_with(&seen2, PLAN_REVIEW);
    assert_eq!(briefs.len(), 1, "reviewed: the budget is the run's");
    let brief = briefs[0]["messages"][1]["content"].as_str().unwrap();
    assert!(!brief.contains("## The earlier review"), "{brief}");
    assert!(brief.contains("**Caller 5**"), "{brief}");
    let messages = h.messages().await;
    let third = result_of(&messages, "p3");
    assert!(
        third.content.starts_with("Saved the plan to") && !third.content.contains("Review skipped"),
        "{}",
        third.content
    );
    let review = &third.meta.as_ref().unwrap()["review"];
    assert_eq!(
        (review["round"].as_u64(), review["status"].as_str()),
        (Some(1), Some("clean"))
    );
    let files = h.plan_files();
    assert_eq!(files.len(), 1);
    assert!(
        files[0].contains("**Caller 5**") && !files[0].contains("Open review findings"),
        "{}",
        files[0]
    );
}

/// P4.7 without FUSE: `verify_now` runs only after a parallel group, which
/// needs an isolated chat, so it's driven on a run prepared in the project
/// itself. Its saved output is still readable after the end's checks (the
/// run's first round, which cleaned before P4.7); an earlier run's output
/// goes at the run's first checks, mid-run ones included.
#[tokio::test]
async fn verify_now_output_survives_verify_at_end_without_fuse() {
    let Some(h) = failing_project(400).await else {
        return;
    };
    let output = |path: &str| crate::agent_run::check_output::resolve(&h.dialog_id, path);
    // Left by an earlier run of the chat.
    let earlier =
        crate::agent_run::check_output::save(&h.dialog_id, "round-1", "lint", "earlier\n").unwrap();
    let (url, seen) = model(vec![Reply::Text("Not asked.")]).await;
    let core = h.core.clone();
    let state = core.agents.clone();
    let request = h.request(&url, Harness::send("Greet"), checks_on());
    state.set_config(request.config.clone().unwrap());
    let controls = state.register(&h.dialog_id, "p::m", None).unwrap();
    let _registered = state.registered(&h.dialog_id, &controls.run_id);
    let (mut run, _entry) = crate::agent_run::run::Run::prepare(&core, request, controls)
        .await
        .unwrap()
        .unwrap();
    run.changed_files = vec!["a.txt".into()];

    let said = run.verify_now(&[]).await.unwrap();
    assert!(said.contains("and some fail"), "{said}");
    assert!(
        output(earlier.as_str()).is_err(),
        "an earlier run's output goes"
    );
    let messages = h.messages().await;
    let mid = messages
        .iter()
        .find_map(|m| {
            let v = m.meta.as_ref()?.get("verification")?;
            (v["midRun"] == true).then(|| v.clone())
        })
        .expect("a mid-run record");
    assert_eq!(mid["status"], "failed", "{mid}");
    let saved = mid["checks"][0]["savedOutput"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(saved.starts_with("@checks/round-0-midrun-"), "{saved}");
    assert!(said.contains(&format!("saved as `{saved}`")), "{said}");

    // The end's checks: round 1, which cleaned the folder before P4.7.
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert!(output("@checks/round-1-test.log").is_ok());
    let path = output(saved.as_str()).expect("the mid-run output was removed by the end's checks");
    assert!(std::fs::read_to_string(path)
        .unwrap()
        .contains("error: the greeting is wrong at a.txt:1"));
    assert!(seen.lock().unwrap().is_empty(), "no model turn ran");
    drop(run);
    crate::agent_run::check_output::clean(&h.dialog_id);
}

/// P4.7, across runs (documents the semantics): saved output stays until
/// the chat's next run runs its first checks. That run can still read it
/// before then; its checks remove it (they pass here, so nothing new is
/// saved), while the earlier run's record keeps naming the file.
#[tokio::test]
async fn second_run_cleans_earlier_output() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let check = "const fs = require('fs');\n\
        if (!fs.readFileSync('a.txt', 'utf8').includes('hello')) {\n\
          for (let i = 1; i <= 400; i++) console.log('noise line ' + i + ' of the test run, nothing to see');\n\
          console.error('error: the greeting is wrong at a.txt:1');\n\
          process.exit(1);\n\
        }\n";
    let h = harness(&[
        ("a.txt", "old\n"),
        ("greet.js", check),
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node greet.js" } }"#,
        ),
    ])
    .await;
    let saved = "@checks/round-1-test.log";
    let output = |path: &str| crate::agent_run::check_output::resolve(&h.dialog_id, path);
    let (url, _) = model(vec![
        write_call("w1", "a.txt", "helo\n"),
        Reply::Text("Done."),
        Reply::Text("I can't fix it."),
    ])
    .await;
    h.start(h.with_subagents(&url, "Greet", checks_on(), Default::default()))
        .await;
    assert_eq!(h.run_end().await, "verify-unresolved");
    assert!(output(saved).is_ok(), "kept after its run ended");

    let (url, _) = model(vec![
        // Before this run's checks: the earlier run's output is there.
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": saved, "start_line": 1, "end_line": 12 }),
        },
        write_call("w2", "a.txt", "hello\n"),
        Reply::Text("Fixed."),
    ])
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.with_subagents(&url, "Try again", checks_on(), Default::default()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let read = result_of(&messages, "r1");
    assert!(
        read.content.contains("error: the greeting is wrong"),
        "{}",
        read.content
    );
    // Its first checks emptied the folder; passing, they saved nothing.
    let gone = output(saved).unwrap_err();
    assert!(
        gone.contains("no such saved output (it's kept until the chat's next run runs its checks)"),
        "{gone}"
    );
    // The earlier run's record still names the file.
    let first = messages
        .iter()
        .find_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .unwrap();
    assert_eq!(first["checks"][0]["savedOutput"], saved, "{first}");
}

/// Round 3: held, the agent revises the plan and sends it again without a
/// word. Past the answer nudges (B12) it's reviewed: the brief says it
/// answered nothing and shows what it changed, and a finding the reviewer
/// repeats without new evidence is dropped as before (B04): the plan is
/// saved clean.
#[tokio::test]
async fn hold_then_silent_revision_drops_a_repeat() {
    let h = plan_project().await;
    let (url, seen) = model_routed(
        vec![(
            PLAN_REVIEW,
            vec![Reply::Text(PLAN_FINDING), Reply::Text(PLAN_FINDING)],
        )],
        vec![
            submit("p1", users_plan("Users", 0)),
            // Held: it adds the step the finding asked for, saying nothing.
            submit("p2", users_plan("Users", 1)),
            submit("p3", users_plan("Users", 1)),
            submit("p4", users_plan("Users", 1)),
            Reply::Text("Saved."),
        ],
    )
    .await;
    h.start(h.plan_request(&url, plan_reviewer_on(json!({}))))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "p1").meta.as_ref().unwrap()["review"]["held"],
        true
    );
    for id in ["p2", "p3"] {
        assert!(
            result_of(&messages, id)
                .meta
                .as_ref()
                .is_none_or(|m| m.get("review").is_none()),
            "{id}: not reviewed"
        );
    }
    let briefs: Vec<String> = requests_with(&seen, PLAN_REVIEW)
        .iter()
        .map(|b| {
            b["messages"][1]["content"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(briefs.len(), 2);
    assert!(
        briefs[1].contains("   - The agent's answer: none")
            && !briefs[1].contains("The agent sent no answers with the plan")
            && briefs[1].contains("+3. **Caller 0**"),
        "{}",
        briefs[1]
    );
    let fourth = result_of(&messages, "p4");
    assert!(
        fourth
            .content
            .starts_with(&format!("Saved the plan to {}", shown(".rovik/plans/"))),
        "{}",
        fourth.content
    );
    let review = &fourth.meta.as_ref().unwrap()["review"];
    assert_eq!(review["status"], "clean", "{review}");
    assert_eq!(
        review["dropped"].as_array().map(Vec::len),
        Some(1),
        "{review}"
    );
    assert_eq!(review["earlier"][0]["resolved"], true, "{review}");
    assert!(review.get("open").is_none(), "{review}");
    let files = h.plan_files();
    assert_eq!(files.len(), 1);
    assert!(
        !files[0].contains("## Open review findings"),
        "{}",
        files[0]
    );
}

/// Round 2: a conflict's two versions, saved for the lead during a join,
/// are this run's: the run's first checks (at the end here, as after a
/// group with a conflict) remove only an earlier run's output, not them.
/// Driven on a run prepared in the project, as joins need FUSE.
#[tokio::test]
async fn a_conflicts_saved_versions_outlive_the_runs_checks() {
    let Some(h) = failing_project(400).await else {
        return;
    };
    let output = |path: &str| crate::agent_run::check_output::resolve(&h.dialog_id, path);
    let earlier =
        crate::agent_run::check_output::save(&h.dialog_id, "round-1", "lint", "earlier\n").unwrap();
    let (url, seen) = model(vec![Reply::Text("Not asked.")]).await;
    let core = h.core.clone();
    let state = core.agents.clone();
    let request = h.request(&url, Harness::send("Greet"), checks_on());
    state.set_config(request.config.clone().unwrap());
    let controls = state.register(&h.dialog_id, "p::m", None).unwrap();
    let _registered = state.registered(&h.dialog_id, &controls.run_id);
    let (mut run, _entry) = crate::agent_run::run::Run::prepare(&core, request, controls)
        .await
        .unwrap()
        .unwrap();
    run.changed_files = vec!["a.txt".into()];
    run.writes_since_verify = true;

    let said = run.save_conflict(
        "w2",
        None,
        &[crate::overlay::land::Conflict {
            path: "a.txt".into(),
            reason: "both changed line 1".into(),
        }],
    );
    let saved: Vec<String> = said
        .split('`')
        .filter(|s| s.starts_with("@checks/"))
        .map(str::to_string)
        .collect();
    assert_eq!(saved.len(), 2, "{said}");
    assert!(
        output(earlier.as_str()).is_err(),
        "an earlier run's output goes"
    );

    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert!(output("@checks/round-1-test.log").is_ok());
    for path in &saved {
        assert!(
            output(path).is_ok(),
            "{path} was removed by the run's checks"
        );
    }
    assert!(seen.lock().unwrap().is_empty(), "no model turn ran");
    drop(run);
    crate::agent_run::check_output::clean(&h.dialog_id);
}

/// Round 2 (§4.2), P6.7: deleting a lead's chat removes what its
/// subagent's chat left outside the database: its saved check output,
/// its kept copy and landing records, and the checks state it carried.
#[tokio::test]
async fn deleting_a_lead_removes_its_subagents_checks_landings_and_carry() {
    let mut h = harness(&[("a.txt", "x\n")]).await;
    h.isolate().await;
    let db = h.core.db.clone();
    let kid = uuid::Uuid::new_v4().to_string();
    let (parent, k) = (h.dialog_id.clone(), kid.clone());
    db.write(move |c| {
        store::create_child_dialog(
            c,
            &store::NewChildDialog {
                id: k,
                parent_id: parent,
                parent_call_id: "c".into(),
                kind: "worker".into(),
                title: "Worker".into(),
                created_at: store::now_ms(),
            },
        )
    })
    .await
    .unwrap();
    // What the child's run leaves: saved check output, a kept copy, a
    // landing record, and checks waiting with their state.
    let saved =
        crate::agent_run::check_output::save(&kid, "round-1", "test", "1 failed\n").unwrap();
    let checks = crate::agent_run::check_output::resolve(&kid, &saved)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let data = h._dir.0.join("data");
    let left = [
        data.join("overlays").join(&kid),
        data.join("landings").join(&kid),
    ];
    for dir in &left {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("project.txt"), "x").unwrap();
    }
    let state = &h.core.agents;
    state.put_carry(
        &kid,
        crate::agent_run::verify::VerifyCarry::for_test("rec", Duration::ZERO, vec![]),
    );
    assert!(state.has_carry(&kid));

    let core = h.core.clone();
    crate::chat_db::api::chat_delete_dialog(&core, h.dialog_id.clone())
        .await
        .unwrap();
    assert!(!checks.exists(), "{} left", checks.display());
    for dir in &left {
        assert!(!dir.exists(), "{} left", dir.display());
    }
    assert!(!state.has_carry(&kid));
    let ids = [h.dialog_id.clone(), kid];
    let rows: i64 = db
        .read(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM dialogs WHERE id IN (?1, ?2)",
                rusqlite::params![ids[0], ids[1]],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

/// Round 2 (§4.2): diagnostics with no path (the editor's summary for the
/// whole project) leaves out the lines of the files the change touches,
/// says how many went, and keeps the rest.
#[tokio::test]
async fn diagnostics_without_a_path_hides_the_files() {
    let h = harness(&[
        (
            "src/greet.ts",
            "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
        ),
        ("src/main.ts", "console.log('hi');\n"),
        ("src/other.ts", "export const other = 1;\n"),
        (".rovik/plans/greeter.md", TEST_PLAN),
    ])
    .await;
    // What the editor reported: an error in a hidden file and in another.
    let root = h.project.to_string_lossy().into_owned();
    for path in ["src/greet.ts", "src/other.ts"] {
        let markers: Vec<crate::agent_tools::EditorMarker> = serde_json::from_value(json!([{
            "startLine": 1, "startCol": 1, "endLine": 1, "endCol": 2,
            "severity": "error", "message": "something is off",
        }]))
        .unwrap();
        crate::agent_tools::report_editor_diagnostics(root.clone(), path.into(), markers).unwrap();
    }
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                Reply::Call {
                    id: "d1",
                    name: "diagnostics",
                    args: json!({}),
                },
                Reply::Text(TEST_REPORT),
            ],
        )],
        vec![test_task("tw"), Reply::Text("Tests are written.")],
    )
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `.rovik/plans/greeter.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true })),
        attachments: Vec::new(),
    };
    let mut request = h.request(
        &url,
        StartKind::Send { message },
        Permissions {
            allow_any_command: true,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = test_writer_on();
    h.start(request).await;
    h.run_end().await;
    let d1 = h.child_results("tw").await["d1"].clone();
    assert!(!d1.contains("src/greet.ts"), "{d1}");
    assert!(d1.contains("- src/other.ts: 1 error"), "{d1}");
    assert!(
        d1.contains("[1 line in the files the change touches left out"),
        "{d1}"
    );
}

/// Round 2 (§4.2): two members of a group can't both change one path: a
/// plan that gives `src/shared.rs` to both is refused at the group's start
/// (F03), before any worker. The other half, one member's side effect on
/// a path another changes, is left out at landing with no conflict
/// (`a_side_effect_outside_a_workers_scope_doesnt_land`).
#[tokio::test]
async fn two_members_landing_the_same_path_without_a_line_conflict() {
    if !crate::vfs::mount_works() {
        return;
    }
    let plan: &'static str = Box::leak(
        GROUP_PLAN
            .replace(
                "Files: `src/users.rs`,",
                "Files: `src/shared.rs`, `src/users.rs`,",
            )
            .replace(
                "Files: `src/posts.rs`,",
                "Files: `src/shared.rs`, `src/posts.rs`,",
            )
            .into_boxed_str(),
    );
    let h = group_harness_with_side_effect(plan, "true", &[("src/shared.rs", "// shared\n")]).await;
    let (url, _) = model(vec![all_done(), group_task("g1"), Reply::Text("Done.")]).await;
    h.start(h.implement_isolated(&url, false)).await;
    let result = h.result_when_ready("g1").await;
    assert!(
        result
            .content
            .starts_with("Error: the group can't run in parallel as planned"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("Steps 2 and 3 are both in group \"pages\" but both name `src/shared.rs`"),
        "{}",
        result.content
    );
    assert!(h.events_of("subagentStarted").is_empty());
    h.run_end().await;
    assert_eq!(h.file("src/shared.rs").as_deref(), Some("// shared\n"));
}

/// A subagent of a batch the IDE starts itself (Thorough's planners) that
/// can't be launched leaves no child chat behind; the others run.
#[tokio::test]
async fn a_batch_member_that_cant_launch_leaves_no_chat() {
    let h = harness(&[("a.txt", "a\n")]).await;
    let (url, _) = model_routed(vec![("MARK-BL", vec![Reply::Text(REPORT)])], vec![]).await;
    let core = h.core.clone();
    let state = core.agents.clone();
    let request = h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({})));
    state.set_config(request.config.clone().unwrap());
    let controls = state.register(&h.dialog_id, "p::m", None).unwrap();
    let _registered = state.registered(&h.dialog_id, &controls.run_id);
    let (mut run, _entry) = crate::agent_run::run::Run::prepare(&core, request, controls)
        .await
        .unwrap()
        .unwrap();
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .insert("bl1".into());
    let jobs = ["bl1", "bl2"]
        .iter()
        .enumerate()
        .map(|(i, call)| crate::agent_run::subagent_run::IdeJob {
            call_id: call.to_string(),
            goal: format!("Job {call}"),
            message: "MARK-BL: where?".into(),
            models_from: i,
        })
        .collect();
    let results = run
        .run_ide_subagents(crate::agent_run::subagent::Kind::Scout, jobs)
        .await;
    crate::agent_run::subagent_run::FAIL_LAUNCH
        .lock()
        .unwrap()
        .remove("bl1");
    assert!(
        results[0].is_err(),
        "{:?}",
        results[0].as_ref().map(|r| &r.0)
    );
    assert!(results[1].is_ok());
    let child = |call: &'static str| {
        let (id, db) = (h.dialog_id.clone(), h.core.db.clone());
        async move {
            db.read(move |c| store::child_for_call(c, &id, call, None))
                .await
                .unwrap()
        }
    };
    assert!(
        child("bl1").await.is_none(),
        "no chat for the one that failed"
    );
    assert!(child("bl2").await.is_some());
    assert_eq!(h.core.agents.running_children(&h.dialog_id), 0);
    drop(run);
}

/// A lead that panics while its subagents work stops them: they're tasks
/// of their own, and none is left running for a run that's gone.
#[tokio::test]
async fn a_lead_that_panics_stops_its_subagents() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let (url, _) = model_routed(
        vec![
            ("MARK-QUICK", vec![Reply::Text(REPORT)]),
            (
                "MARK-LONG",
                vec![Reply::Slow(10_000, Box::new(Reply::Text(REPORT)))],
            ),
        ],
        vec![
            Reply::Calls(vec![
                task_call("lp1", "MARK-QUICK: one"),
                task_call("lp2", "MARK-LONG: two"),
            ]),
            Reply::Text("Done."),
        ],
    )
    .await;
    *crate::agent_run::subagent_run::PANIC_LEAD_AT
        .lock()
        .unwrap() = Some("lp1".into());
    h.start(h.with_subagents(&url, "Go", quiet_permissions(), scouts_on(json!({}))))
        .await;
    let end = h.run_end().await;
    *crate::agent_run::subagent_run::PANIC_LEAD_AT
        .lock()
        .unwrap() = None;
    assert_eq!(end, "error");
    // Stopped well before its slow reply would have come.
    let waited = std::time::Instant::now();
    while h.core.agents.running_children(&h.dialog_id) > 0 {
        assert!(
            waited.elapsed() < Duration::from_secs(5),
            "the subagent is still running"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

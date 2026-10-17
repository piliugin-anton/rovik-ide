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
    use super::autopilot::{add_stage, end_stage, load, records_of};
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

/*
 * The driver (A03)
 */

use super::autopilot::{crash_at, task_stages, Crash, Effort, Stage, StageRecord, Status};

/// A project whose test (`npm test`) passes once ok.txt says "ok".
async fn task_project() -> Harness {
    harness(&[
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        (
            "check.js",
            "const fs = require('fs'); if (fs.readFileSync('ok.txt', 'utf8').trim() !== 'ok') { console.error('ok.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("ok.txt", "no"),
    ])
    .await
}

const TASK_PLAN: &str = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
    2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `npm test`\n";

/// The plan reviewer's brief starts so (plan_review.rs).
const PLAN_REVIEW: &str = "# Review this plan";

/// The task's runs, in order: the plan (reviewed clean), then its
/// implementation, which makes the test pass.
fn task_script() -> Vec<Reply> {
    vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": TASK_PLAN }),
        },
        Reply::Text("Saved the plan."),
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [
                { "title": "Write ok", "status": "in_progress" },
                { "title": "Check", "status": "pending" },
            ] }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "ok.txt", "content": "ok" }),
        },
        Reply::Call {
            id: "t2",
            name: "todo",
            args: json!({ "items": [
                { "title": "Write ok", "status": "done" },
                { "title": "Check", "status": "done" },
            ] }),
        },
        Reply::Text("Done: ok.txt says ok."),
    ]
}

/// A task's start: in Autopilot, with the plan reviewer on, checks on.
fn task_request(h: &Harness, url: &str) -> StartRequest {
    let mut request = h.request(
        url,
        autopilot_send("Make the test pass"),
        Permissions::default(),
    );
    request.mode = Some("autopilot".into());
    request.config.as_mut().unwrap().subagents = serde_json::from_value(
        json!({ "enabled": true, "kinds": { "planReviewer": { "enabled": true } } }),
    )
    .unwrap();
    request
}

/// A Resume, as the app sends it when the chat is opened.
fn resume_request(h: &Harness, url: &str) -> StartRequest {
    let mut request = h.request(url, StartKind::Resume, Permissions::default());
    request.mode = Some("autopilot".into());
    request
}

/// The chat's stage records, oldest first.
async fn stages(h: &Harness) -> Vec<StageRecord> {
    super::autopilot::load(&h.core, &h.dialog_id)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.record)
        .collect()
}

/// The stage records once `done` says they're as awaited.
async fn stages_until(
    h: &Harness,
    what: &str,
    done: impl Fn(&[StageRecord]) -> bool,
) -> Vec<StageRecord> {
    for _ in 0..1200 {
        let records = stages(h).await;
        if done(&records) {
            return records;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what}: {:#?}", stages(h).await);
}

/// Over: delivered (or ended), nothing running.
fn delivered(records: &[StageRecord]) -> bool {
    records
        .last()
        .is_some_and(|r| r.name == Stage::Deliver && r.status != Status::Running)
}

fn names(records: &[StageRecord]) -> Vec<(Stage, Status)> {
    records.iter().map(|r| (r.name, r.status)).collect()
}

/// What a Balanced task records once it went well: every stage, those not
/// built yet skipped.
const BALANCED_OK: &[(Stage, Status)] = &[
    (Stage::Plan, Status::Done),
    (Stage::PlanReview, Status::Done),
    (Stage::Prepare, Status::Skipped),
    (Stage::Tests, Status::Skipped),
    (Stage::Implement, Status::Done),
    (Stage::Review, Status::Skipped),
    (Stage::Checks, Status::Done),
    (Stage::Acceptance, Status::Skipped),
    (Stage::Deliver, Status::Done),
];

/// The IDE's messages that started an implementation, in order.
fn implement_messages(messages: &[MessageRow]) -> Vec<&MessageRow> {
    messages
        .iter()
        .filter(|m| {
            m.role == "user" && m.meta.as_ref().is_some_and(|x| x["harness"] == "autopilot")
        })
        .collect()
}

/// A03: one message runs the task to its delivery: the plan run (its
/// review clean), then the implementation, started by the IDE with the
/// plan as "Implement this plan" sends it; its checks pass, and every stage
/// left a record in order, pointing at what it produced.
#[tokio::test]
async fn a_task_runs_from_its_plan_to_its_delivery() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, seen) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    h.start(task_request(&h, &url)).await;
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(names(&records), BALANCED_OK);
    assert_eq!(h.file("ok.txt").as_deref(), Some("ok"));

    // Each record points at what its stage produced.
    let messages = h.messages().await;
    let by_id = |id: &Option<String>| {
        messages
            .iter()
            .find(|m| Some(&m.id) == id.as_ref())
            .unwrap_or_else(|| panic!("no row {id:?}"))
    };
    let plan = &records[0];
    assert_eq!(plan.task, messages[0].id, "the task is its message's");
    assert!(by_id(&plan.reference).meta.as_ref().unwrap()["planFile"].is_string());
    assert!(
        plan.summary
            .as_deref()
            .unwrap()
            .starts_with("Saved the plan to .rovik/plans/"),
        "{plan:?}"
    );
    let review = &records[1];
    assert_eq!(review.summary.as_deref(), Some("Clean"));
    assert_eq!(by_id(&review.reference).tool_call_id.as_deref(), Some("p1"));
    let checks = &records[6];
    assert_eq!(checks.summary.as_deref(), Some("1 check passed"));
    let verification = &by_id(&checks.reference).meta.as_ref().unwrap()["verification"];
    assert_eq!(verification["status"], "passed");
    assert_eq!(records[4].outcome.as_deref(), Some("verified"));
    let deliver = &records[8];
    assert_eq!(deliver.outcome.as_deref(), Some("verified"));
    assert_eq!(
        deliver.summary.as_deref(),
        Some("Delivered: the changes are in the project")
    );
    assert!(records.iter().all(|r| r.task == plan.task));

    // The implementation began with the IDE's message: the plan as saved.
    let starts = implement_messages(&messages);
    assert_eq!(starts.len(), 1);
    let file = by_id(&plan.reference).meta.as_ref().unwrap()["planFile"]
        .as_str()
        .unwrap()
        .to_string();
    let shown = plan::relative_plan_path(&file, Some(&h.project.to_string_lossy()));
    let text = plan::implementation_request(&std::fs::read_to_string(&file).unwrap(), Some(&shown));
    assert_eq!(starts[0].content, text);
    let requests = seen.lock().unwrap().clone();
    let implementing = requests
        .iter()
        .find(|r| r["messages"].to_string().contains("Implement this plan"))
        .expect("the implementation's first request");
    assert_eq!(
        implementing["tool_choice"],
        json!({ "type": "function", "function": { "name": "todo" } }),
        "its first turn makes the checklist"
    );
    let tools = tool_names(implementing);
    assert!(tools.iter().any(|t| t == "write"), "Agent mode: {tools:?}");
    // Two runs, each marked with its stage.
    assert_eq!(h.events_of("runStarted").len(), 2);
    let runs = log_lines(&h, "run").await;
    let stage_of: Vec<&str> = runs.iter().filter_map(|r| r["stage"].as_str()).collect();
    assert_eq!(stage_of, ["plan", "implement"]);

    // Over: a Resume starts nothing.
    assert!(h.start(resume_request(&h, &url)).await.is_none());
    assert_eq!(h.events_of("runStarted").len(), 2);
}

/// A06: a Thorough task has Thorough's stages: Break and Pick, after the
/// checks, recorded skipped until T builds them; every record carries the
/// effort the task started with, which a change of the setting meanwhile
/// doesn't change.
#[tokio::test]
async fn a_thorough_task_records_its_own_stages_skipped_until_built() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    let mut request = task_request(&h, &url);
    request.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    h.start(request).await;
    // The setting goes back to Balanced while the plan runs: the task stays Thorough.
    stages_until(&h, "the plan's start", |r| !r.is_empty()).await;
    let mut balanced = h.core.agents.config();
    balanced.autopilot.effort = Effort::Balanced;
    h.core.agents.set_config(balanced);
    let records = stages_until(&h, "the delivery", delivered).await;
    let thorough: Vec<(Stage, Status)> = [
        &BALANCED_OK[..7],
        &[
            (Stage::Break, Status::Skipped),
            (Stage::Pick, Status::Skipped),
        ],
        &BALANCED_OK[7..],
    ]
    .concat();
    assert_eq!(names(&records), thorough);
    assert!(
        records.iter().all(|r| r.effort == Effort::Thorough),
        "{records:#?}"
    );
    for record in records.iter().filter(|r| r.name.thorough_only()) {
        assert_eq!(record.summary.as_deref(), Some("Not available yet"));
    }
    assert_eq!(h.file("ok.txt").as_deref(), Some("ok"));
}

/// A03: the driver dies right after it ended the plan's stage, before the
/// next was recorded. A Resume goes on from there; a second one, meanwhile,
/// starts nothing twice.
#[tokio::test]
async fn a_task_cut_between_two_stages_goes_on_once_resumed() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    crash_at(&h.dialog_id, Crash::AfterEnd);
    h.start(task_request(&h, &url)).await;
    let cut = stages_until(&h, "the plan's end", |r| {
        r.first().is_some_and(|p| p.status == Status::Done)
    })
    .await;
    // Nothing else happens on its own.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        names(&stages(&h).await),
        [(Stage::Plan, Status::Done)],
        "{cut:?}"
    );
    assert!(!h.core.agents.is_running(&h.dialog_id));

    assert!(h.start(resume_request(&h, &url)).await.is_none());
    // The implementation runs; a second Resume now starts nothing.
    assert!(h.start(resume_request(&h, &url)).await.is_none());
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(names(&records), BALANCED_OK);
    assert_eq!(implement_messages(&h.messages().await).len(), 1);
    assert_eq!(h.events_of("runStarted").len(), 2);
}

/// A03: the driver dies after it recorded the implementation, before its
/// run started (the stage is recorded running first, for this). A Resume
/// starts that stage's run, once, under the same record.
#[tokio::test]
async fn a_stage_cut_before_its_run_started_is_started_once_resumed() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    crash_at(&h.dialog_id, Crash::BeforeRun);
    h.start(task_request(&h, &url)).await;
    stages_until(&h, "the implementation's record", |r| {
        r.last().is_some_and(|i| i.name == Stage::Implement)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.core.agents.is_running(&h.dialog_id));
    assert!(implement_messages(&h.messages().await).is_empty());

    assert!(h.start(resume_request(&h, &url)).await.is_none());
    assert!(h.start(resume_request(&h, &url)).await.is_none());
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(names(&records), BALANCED_OK);
    assert_eq!(implement_messages(&h.messages().await).len(), 1);
    assert_eq!(h.events_of("runStarted").len(), 2);
}

/// A03: a Resume that comes as a stage's run ends (the app hears the end
/// and opens the chat) finds the driver at work, and starts nothing.
#[tokio::test]
async fn a_resume_as_a_stage_ends_starts_nothing_more() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    h.start(task_request(&h, &url)).await;
    for _ in 0..1200 {
        if !h.events_of("runEnded").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(h.start(resume_request(&h, &url)).await.is_none());
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(names(&records), BALANCED_OK);
    assert_eq!(h.events_of("runStarted").len(), 2);
}

/// A03: a plan run that saves no plan fails the task, which delivers
/// nothing, with why; a message of the user's after a task takes over from
/// it, and a Resume then starts nothing for the old one.
#[tokio::test]
async fn a_plan_run_without_a_plan_fails_the_task() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Text("I looked, and wrote nothing down.")]).await;
    h.start(h.request(&url, autopilot_send("Do something"), quiet_permissions()))
        .await;
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(
        names(&records),
        [
            (Stage::Plan, Status::Failed),
            (Stage::Deliver, Status::Failed)
        ]
    );
    assert_eq!(records[0].outcome.as_deref(), Some("plan-failed"));
    assert_eq!(records[0].summary.as_deref(), Some("No plan was saved"));
    assert_eq!(records[1].outcome.as_deref(), Some("plan-failed"));
    assert_eq!(records[1].summary.as_deref(), Some("Nothing to deliver"));
}

/// A03: a user message after an unfinished task takes over: the old task
/// isn't resumed, and its records stay as they were. A05: the strip reads
/// the task while it's the chat's current request, and none after.
#[tokio::test]
async fn a_new_message_takes_over_from_an_unfinished_task() {
    let h = harness(&[]).await;
    let (url, _) = model(vec![Reply::Text("Plan saved.")]).await;
    crash_at(&h.dialog_id, Crash::AfterEnd);
    h.start(h.request(&url, autopilot_send("Do something"), quiet_permissions()))
        .await;
    stages_until(&h, "the plan's end", |r| {
        r.first().is_some_and(|p| p.status != Status::Running)
    })
    .await;
    let shown = task_stages(&h.core, &h.dialog_id).await.unwrap().unwrap();
    let rows = stages(&h).await;
    assert_eq!(
        shown.iter().map(|r| &r.stage).collect::<Vec<_>>(),
        rows.iter().collect::<Vec<_>>()
    );
    let json = serde_json::to_value(&shown[0]).unwrap();
    assert_eq!(json["stage"]["name"], "plan", "{json}");
    assert_eq!(json["id"], json!(shown[0].id));

    h.start(h.request(&url, Harness::send("Never mind"), quiet_permissions()))
        .await;
    for _ in 0..200 {
        if h.events_of("runEnded").len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(task_stages(&h.core, &h.dialog_id).await.unwrap(), None);
    let before = stages(&h).await;
    assert!(h.start(resume_request(&h, &url)).await.is_none());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(stages(&h).await, before);
    assert_eq!(h.events_of("runStarted").len(), 2);
}

/*
 * The plan and implement stages (A04)
 */

/// The text of a request's last user-role message: the current request.
fn last_user_text(request: &Value) -> String {
    let message = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap();
    match &message["content"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A04: the plan stage asks nobody: `ask_user` isn't offered, a call to it
/// anyway is refused with the reason, nothing waits for an answer, and the
/// plan goes in with its assumptions under "## Decisions". What the model
/// is told says so, in Plan mode's place.
#[tokio::test]
async fn a_plan_that_asks_is_refused_and_submits_with_assumptions() {
    let h = harness(&[("src/a.rs", "fn main() {}\n")]).await;
    let plan = "# Flag\n\nAdd a flag.\n\n## Steps\n\n\
        1. **Flag**\n   - Files: `src/a.rs`\n   - Add a `verbose` flag that `main` reads and prints with.\n\
        2. **Use it**\n   - Files: `src/a.rs`\n   - Print each step when the `verbose` flag is set.\n\n\
        ## Decisions\n- Assumed the flag is `--verbose`: nobody was there to ask.\n\n\
        ## Validation\n- `cargo test`\n";
    let (url, seen) = model(vec![
        Reply::Call {
            id: "q1",
            name: "ask_user",
            args: json!({ "questions": [{ "question": "Which flag?", "options": [{ "label": "--verbose" }] }] }),
        },
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": plan }),
        },
        Reply::Text("Saved the plan."),
    ])
    .await;
    h.start(h.request(&url, autopilot_send("Add a flag"), quiet_permissions()))
        .await;
    let records = stages_until(&h, "the plan's end", |r| {
        r.first().is_some_and(|p| p.status != Status::Running)
    })
    .await;
    assert_eq!(records[0].status, Status::Done, "{records:?}");
    assert!(h.events_of("questionsAsked").is_empty());
    let messages = h.messages().await;
    assert_eq!(
        result_of(&messages, "q1").content,
        super::autopilot::NO_QUESTIONS
    );
    let saved = result_of(&messages, "p1");
    assert!(
        saved.content.starts_with("Saved the plan to"),
        "{}",
        saved.content
    );

    let first = seen.lock().unwrap()[0].clone();
    let tools = tool_names(&first);
    assert!(!tools.iter().any(|t| t == "ask_user"), "{tools:?}");
    assert!(tools.iter().any(|t| t == "submit_plan"), "{tools:?}");
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Nobody is there to answer questions"),
        "{system}"
    );
    assert!(!system.contains("Only ask the user a clarifying question"));
    assert!(system.contains("Nobody answers questions in Autopilot"));
    assert!(!system.contains("ask_user"), "{system}");
    stages_until(&h, "the delivery", delivered).await;
}

/// A04: the implementation starts without a message of the user's, and the
/// model gets the text "Implement this plan" sends for the same plan file:
/// the button sends the plan's path, and the core writes the text for both.
#[tokio::test]
async fn the_button_and_the_driver_send_the_same_implementation_request() {
    let h = task_project().await;
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let (url, seen) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        task_script(),
    )
    .await;
    h.start(task_request(&h, &url)).await;
    let records = stages_until(&h, "the delivery", delivered).await;
    let messages = h.messages().await;
    let users_before = messages.iter().filter(|m| m.role == "user").count();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.role == "user" && !plan::is_harness(m))
            .count(),
        1,
        "the task's message alone is the user's"
    );
    let plan_reply = messages
        .iter()
        .find(|m| Some(&m.id) == records[0].reference.as_ref())
        .unwrap();
    let file = plan_reply.meta.as_ref().unwrap()["planFile"]
        .as_str()
        .unwrap()
        .to_string();
    let by_driver = {
        let requests = seen.lock().unwrap();
        let first = requests
            .iter()
            .find(|r| last_user_text(r).starts_with("Implement this plan."))
            .expect("the implementation's first request");
        last_user_text(first)
    };

    // "Implement this plan" on the same file, as the app sends it now.
    let shown = plan::relative_plan_path(&file, Some(&h.project.to_string_lossy()));
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: format!("Implement the plan in `{shown}`."),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true, "planPath": file })),
        attachments: Vec::new(),
    };
    let asked = seen.lock().unwrap().len();
    h.start(h.request(&url, StartKind::Send { message }, Permissions::default()))
        .await;
    for _ in 0..400 {
        if h.events_of("runEnded").len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The same text, kept with the message for its later turns and a
    // Retry. The button's message starts its request, so the repository
    // map goes before it; the IDE's doesn't (the task's message has it).
    let messages = h.messages().await;
    let sent = messages
        .iter()
        .filter(|m| m.role == "user")
        .nth(users_before)
        .unwrap();
    assert_eq!(
        sent.meta.as_ref().unwrap()["sentContent"],
        by_driver.as_str()
    );
    let by_button = last_user_text(&seen.lock().unwrap()[asked]);
    assert!(by_button.ends_with(&by_driver), "{by_button}");
    assert!(by_button.starts_with("## Repository map"), "{by_button}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(by_driver, plan::implementation_request(&text, Some(&shown)));
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

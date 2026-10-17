//! What a task's earlier stages left for its later ones: the plan its plan
//! run saved, the tests its Tests stage wrote first (S04), and the
//! implementation's message built from them (A04).

use super::config::MODE;
use super::rows::{current_task, task_chat};
use super::stage::{Recorded, Stage};
use crate::agent_run::{plan, UserMessage};
use crate::chat_db::store;
use crate::core::Core;
use serde_json::json;

/// The plan the run after row `since` saved: the latest row that names its
/// file (the reply that shows it), and the file.
pub(super) async fn saved_plan(
    core: &Core,
    dialog_id: &str,
    since: i64,
) -> Result<Option<(String, String)>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "planFile", Some(since)))
        .await?;
    Ok(rows.iter().rev().find_map(|m| {
        let file = m.meta.as_ref()?.get("planFile")?.as_str()?;
        Some((m.id.clone(), file.to_string()))
    }))
}

/// S04: the file of the plan the current task's plan run saved, which
/// the Tests stage writes the tests from; a candidate's is its task's
/// (T05).
pub async fn task_plan_file(core: &Core, dialog_id: &str) -> Result<Option<String>, String> {
    let chat = task_chat(core, dialog_id).await?;
    let Some((_, records)) = current_task(core, &chat).await? else {
        return Ok(None);
    };
    let Some(plan_run) = records.iter().rev().find(|r| r.record.name == Stage::Plan) else {
        return Ok(None);
    };
    Ok(saved_plan(core, &chat, plan_run.seq)
        .await?
        .map(|(_, file)| file))
}

/// S04: what a task's Tests stage wrote, from its latest record: the test
/// files, and those of them that passed before the change (the red check).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskTests {
    pub files: Vec<String>,
    pub passing: Vec<String>,
}

/// `TaskTests` of the current task; a candidate's are its task's (T05).
pub async fn task_tests(core: &Core, dialog_id: &str) -> Result<TaskTests, String> {
    let chat = task_chat(core, dialog_id).await?;
    match current_task(core, &chat).await? {
        Some((_, records)) => tests_of(core, &chat, &records).await,
        None => Ok(TaskTests::default()),
    }
}

/// `TaskTests` of a task's records; the red check's from the stage's
/// record row (`meta.tests.passing`).
pub(super) async fn tests_of(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<TaskTests, String> {
    let Some(stage) = records.iter().rev().find(|r| r.record.name == Stage::Tests) else {
        return Ok(TaskTests::default());
    };
    let files = stage.record.files.clone();
    let passing = match (&stage.record.reference, files.is_empty()) {
        (Some(row), false) => {
            let (id, since) = (dialog_id.to_string(), stage.seq);
            let rows = core
                .db
                .read(move |c| store::messages_with_meta(c, &id, "tests", Some(since)))
                .await?;
            rows.iter()
                .find(|m| &m.id == row)
                .and_then(|m| {
                    m.meta
                        .as_ref()?
                        .pointer("/tests/passing")?
                        .as_array()
                        .cloned()
                })
                .unwrap_or_default()
                .iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .filter(|p| files.contains(p))
                .collect()
        }
        _ => Vec::new(),
    };
    Ok(TaskTests { files, passing })
}

/// S04 (4.4): the line of the implementation's request that names the
/// tests written first. None: none were.
pub fn tests_line(tests: &TaskTests) -> Option<String> {
    let named = |files: &[&String]| {
        files
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (passing, failing): (Vec<&String>, Vec<&String>) =
        tests.files.iter().partition(|f| tests.passing.contains(f));
    let already = |files: &[&String]| {
        let one = files.len() == 1;
        format!(
            "{}, which {} already, before the change: check that {} what the change does.",
            named(files),
            if one { "passes" } else { "pass" },
            if one { "it tests" } else { "they test" },
        )
    };
    match (failing.is_empty(), passing.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!(
            "Tests that must pass: {}; {} now on purpose.",
            named(&failing),
            if failing.len() == 1 {
                "it fails"
            } else {
                "they fail"
            },
        )),
        (false, false) => Some(format!(
            "Tests that must pass: {}; {} now on purpose. Also {}",
            named(&failing),
            if failing.len() == 1 {
                "it fails"
            } else {
                "they fail"
            },
            already(&passing),
        )),
        (true, false) => Some(format!("Tests that must pass: {}", already(&passing))),
    }
}

/// S02: the review row that blocked the plan the run after row `since`
/// wrote, if one did (`meta.review.blocked`).
pub(super) async fn blocked_review(
    core: &Core,
    dialog_id: &str,
    since: i64,
) -> Result<Option<String>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "review", Some(since)))
        .await?;
    Ok(rows.iter().rev().find_map(|m| {
        let review = m.meta.as_ref()?.get("review")?;
        (review["kind"] == "plan" && review["blocked"] == true).then(|| m.id.clone())
    }))
}

/// The IDE's message that starts an implementation of the task whose
/// records are `records` (in `dialog_id`, the task's chat): the plan the
/// task saved, as "Implement this plan" sends it, named from `root`, and
/// the tests written first, which it makes pass (S04). Its `meta`
/// names the stage record `record` it's for, and a candidate's letter
/// (T05). Ok(Err): why there's none (the plan isn't saved, or its file
/// can't be read).
pub(in crate::agent_run) async fn implementation_message(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    record: &str,
    root: Option<&str>,
    candidate: Option<&str>,
) -> Result<Result<UserMessage, String>, String> {
    let task = &records[0].record.task;
    let plan_run = records.iter().rev().find(|r| r.record.name == Stage::Plan);
    let saved = match plan_run {
        Some(r) => saved_plan(core, dialog_id, r.seq).await?,
        None => None,
    };
    let Some((_, file)) = saved else {
        return Ok(Err("The task's plan isn't in the chat".into()));
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Ok(Err(format!("The plan's file {file} can't be read")));
    };
    let shown = plan::relative_plan_path(&file, root);
    let mut content = plan::implementation_request(&text, Some(&shown));
    // S04: the tests written first, which it makes pass.
    if let Some(line) = tests_line(&tests_of(core, dialog_id, records).await?) {
        content.push_str("\n\n");
        content.push_str(&line);
    }
    let mut autopilot =
        json!({ "task": task, "stage": Stage::Implement.id(), "record": record, "plan": file });
    if let Some(letter) = candidate {
        autopilot["candidate"] = json!(letter);
    }
    Ok(Ok(UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content,
        meta: Some(json!({
            "harness": MODE,
            "implementsPlan": true,
            "autopilot": autopilot,
        })),
        attachments: Vec::new(),
    }))
}

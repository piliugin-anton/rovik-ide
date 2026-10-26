//! What a task's earlier stages left for its later ones: the plan its plan
//! run saved, the tests its Tests stage wrote first (S04), and the
//! implementation's message built from them (A04).

use super::config::MODE;
use super::rows::{current_task, task_chat};
use super::stage::{Recorded, Stage};
use crate::agent_run::{plan, UserMessage};
use crate::chat_db::store;
use crate::core::Core;
use serde_json::{json, Value};

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
        // A plan's file only: the rows may be a server's (`is_plan_file`).
        plan::is_plan_file(file).then(|| (m.id.clone(), file.to_string()))
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
    // From the task's first plan run: a plan saved by one whose stage was
    // retried after is the task's still (S02).
    let Some(plan_run) = records.iter().find(|r| r.record.name == Stage::Plan) else {
        return Ok(None);
    };
    Ok(saved_plan(core, &chat, plan_run.seq)
        .await?
        .map(|(_, file)| file))
}

/// S04: what a task's Tests stage wrote, from its latest record: the test
/// files, those of them that passed before the change, and those the red
/// check saw fail then. The rest it didn't run: the files the stage
/// changed rather than made, or ones whose run said nothing (R03).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskTests {
    pub files: Vec<String>,
    pub passing: Vec<String>,
    pub failing: Vec<String>,
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
/// record row (`meta.tests.passing`, `meta.tests.failing`).
pub(super) async fn tests_of(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<TaskTests, String> {
    let Some(stage) = records.iter().rev().find(|r| r.record.name == Stage::Tests) else {
        return Ok(TaskTests::default());
    };
    let files = stage.record.files.clone();
    let row = match files.is_empty() {
        true => None,
        false => stage_tests_row(core, dialog_id, stage).await?,
    };
    // The files of the row's `key` list, those of the stage's only.
    let listed = |key: &str| -> Vec<String> {
        row.as_ref()
            .and_then(|tests| tests[key].as_array())
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .filter(|p| files.contains(p))
            .collect()
    };
    let (passing, failing) = (listed("passing"), listed("failing"));
    Ok(TaskTests {
        files,
        passing,
        failing,
    })
}

/// S04: the row of the current task's Tests stage (`meta.tests`); a
/// candidate's is its task's (T05). None: no Tests stage wrote one.
pub async fn tests_row(core: &Core, dialog_id: &str) -> Result<Option<Value>, String> {
    let chat = task_chat(core, dialog_id).await?;
    let Some((_, records)) = current_task(core, &chat).await? else {
        return Ok(None);
    };
    match records.iter().rev().find(|r| r.record.name == Stage::Tests) {
        Some(stage) => stage_tests_row(core, &chat, stage).await,
        None => Ok(None),
    }
}

/// The `meta.tests` of the row a Tests stage's record points at.
async fn stage_tests_row(
    core: &Core,
    dialog_id: &str,
    stage: &Recorded,
) -> Result<Option<Value>, String> {
    let Some(row) = stage.record.reference.clone() else {
        return Ok(None);
    };
    let (id, since) = (dialog_id.to_string(), stage.seq);
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "tests", Some(since)))
        .await?;
    Ok(rows
        .into_iter()
        .find(|m| m.id == row)
        .and_then(|m| m.meta?.get("tests").cloned()))
}

/// S04 (4.4): the line of the implementation's request that names the
/// tests written first: those the red check saw fail fail on purpose,
/// those that pass already are to be checked, and the rest (unrun) only
/// have to pass. None: none were written.
pub fn tests_line(tests: &TaskTests) -> Option<String> {
    let named = |files: &[&String]| {
        files
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let of = |list: &[String]| -> Vec<&String> {
        tests.files.iter().filter(|f| list.contains(f)).collect()
    };
    let (failing, passing) = (of(&tests.failing), of(&tests.passing));
    let unrun: Vec<&String> = (tests.files.iter())
        .filter(|f| !failing.contains(f) && !passing.contains(f))
        .collect();
    let mut clauses = Vec::new();
    if !failing.is_empty() {
        let fail = if failing.len() == 1 {
            "it fails"
        } else {
            "they fail"
        };
        clauses.push(format!("{}; {fail} now on purpose.", named(&failing)));
    }
    if !unrun.is_empty() {
        clauses.push(format!("{}.", named(&unrun)));
    }
    if !passing.is_empty() {
        let one = passing.len() == 1;
        clauses.push(format!(
            "{}, which {} already, before the change: check that {} what the change does.",
            named(&passing),
            if one { "passes" } else { "pass" },
            if one { "it tests" } else { "they test" },
        ));
    }
    (!clauses.is_empty()).then(|| format!("Tests that must pass: {}", clauses.join(" Also ")))
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
    let plan_run = records.iter().find(|r| r.record.name == Stage::Plan);
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
    // Autopilot's stages run with Rovik's tools, on any model.
    let mut content = plan::implementation_request(&text, Some(&shown), true);
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

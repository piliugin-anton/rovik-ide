//! Autopilot (docs/design/autopilot-plan.md): a chat mode in which one
//! message is a whole task. The agent plans it, the plan is reviewed, the
//! plan is implemented and the change checked, and the result delivered,
//! with nobody at the keyboard.
//!
//! A task is a pipeline of runs in one chat (D2), not one long run. Its
//! first run is a Plan-mode run, started by the user's message.
//!
//! Every stage leaves a record (D3): an empty assistant message with
//! `meta.stage`, which is never sent to the model (empty assistant
//! messages aren't) and goes wherever the chat's rows go, the mirror of a
//! remote chat included. The pipeline's state *is* its records: what ran,
//! how it went, and so what comes next (`next_stage`). Nothing else holds
//! it, so an app that reopens, or a daemon that restarts, reads the rows
//! and goes on (A03).

use crate::chat_db::store::{self, MessagePatch, MessageRow, NewMessage};
use crate::core::Core;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use rovik_protocol::autopilot::{AutopilotConfig, Budget, Effort};

/// The mode of a message sent in Autopilot (`meta.mode`, `StartRequest.mode`).
pub const MODE: &str = "autopilot";

/*
 * ---------------------------------------------------------------------------
 * Stage records
 * ---------------------------------------------------------------------------
 */

/// A stage of a task (3.1).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// A Plan-mode run writes the plan (4.1).
    Plan,
    /// The plan reviewer checks it against the code (4.2).
    PlanReview,
    /// The dependencies a lockfile implies are installed (4.3).
    Prepare,
    /// The test writer writes tests from the plan, before the code (4.4).
    Tests,
    /// An "Implement this plan" run (4.5).
    Implement,
    /// The change reviewer reads the change (4.6).
    Review,
    /// The verification gate runs the project's checks (4.7).
    Checks,
    /// Thorough: an adversarial tester tries to break the change (4.8).
    Break,
    /// Thorough: the better of two candidates is picked (4.9).
    Pick,
    /// The acceptance checker judges the change by the criteria (4.10).
    Acceptance,
    /// The result lands or goes to its branch, with the report (4.11).
    Deliver,
}

impl Stage {
    /// Every stage, in the order a task runs them.
    pub const ALL: &'static [Stage] = &[
        Stage::Plan,
        Stage::PlanReview,
        Stage::Prepare,
        Stage::Tests,
        Stage::Implement,
        Stage::Review,
        Stage::Checks,
        Stage::Break,
        Stage::Pick,
        Stage::Acceptance,
        Stage::Deliver,
    ];

    /// Its name in a record (`meta.stage.name`) and in the log.
    pub fn id(self) -> &'static str {
        match self {
            Stage::Plan => "plan",
            Stage::PlanReview => "planReview",
            Stage::Prepare => "prepare",
            Stage::Tests => "tests",
            Stage::Implement => "implement",
            Stage::Review => "review",
            Stage::Checks => "checks",
            Stage::Break => "break",
            Stage::Pick => "pick",
            Stage::Acceptance => "acceptance",
            Stage::Deliver => "deliver",
        }
    }

    /// Only Thorough has it (3.2).
    pub fn thorough_only(self) -> bool {
        matches!(self, Stage::Break | Stage::Pick)
    }
}

/// The stages of a task of this effort, in order.
pub fn pipeline(effort: Effort) -> Vec<Stage> {
    Stage::ALL
        .iter()
        .copied()
        .filter(|s| effort == Effort::Thorough || !s.thorough_only())
        .collect()
}

/// How a stage went.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Under way, or cut by a crash while it was (resumed: A03).
    Running,
    Done,
    /// It ended the task, with `outcome`.
    Failed,
    /// It didn't run, and `summary` says why; the task goes on.
    Skipped,
}

/// A stage record (3.3): `meta.stage` of its row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageRecord {
    /// The task: the id of the message that started it.
    pub task: String,
    pub name: Stage,
    pub status: Status,
    /// The stage's how-many-th run in the task, from 1.
    #[serde(default = "first_round")]
    pub round: u32,
    /// The task's effort, as it started.
    #[serde(default)]
    pub effort: Effort,
    /// Thorough: the working copy a stage ran in, when it ran per candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    /// A failed stage's: the task's outcome ("plan-failed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The record this stage produced (a review, a check round, a report),
    /// by message id.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// One line for the strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn first_round() -> u32 {
    1
}

impl StageRecord {
    /// A stage of `task` starting now.
    pub fn running(task: &str, name: Stage, round: u32, effort: Effort) -> StageRecord {
        StageRecord {
            task: task.to_string(),
            name,
            status: Status::Running,
            round,
            effort,
            candidate: None,
            outcome: None,
            reference: None,
            summary: None,
        }
    }

    /// A stage of `task` that ended as soon as it came: one that was done
    /// inside the run before it, or one that doesn't run.
    pub fn ended(
        task: &str,
        name: Stage,
        round: u32,
        effort: Effort,
        status: Status,
    ) -> StageRecord {
        StageRecord {
            status,
            ..StageRecord::running(task, name, round, effort)
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> StageRecord {
        self.summary = Some(summary.into());
        self
    }
}

/// A stage record as stored: its row and the record.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub id: String,
    pub seq: i64,
    pub record: StageRecord,
}

/// The stage record a row holds, if it holds one that reads. A record from
/// a later version this one can't read is left out, never guessed at.
pub fn record_of(row: &MessageRow) -> Option<StageRecord> {
    if row.role != "assistant" {
        return None;
    }
    serde_json::from_value(row.meta.as_ref()?.get("stage")?.clone()).ok()
}

/// The stage records among `rows`, oldest first.
pub fn recorded(rows: &[MessageRow]) -> Vec<Recorded> {
    rows.iter()
        .filter_map(|row| {
            Some(Recorded {
                id: row.id.clone(),
                seq: row.seq,
                record: record_of(row)?,
            })
        })
        .collect()
}

/// The records of `task`, oldest first.
pub fn records_of<'a>(all: &'a [Recorded], task: &str) -> Vec<&'a Recorded> {
    all.iter().filter(|r| r.record.task == task).collect()
}

/// The chat's latest task: the one its last stage record belongs to.
pub fn latest_task(all: &[Recorded]) -> Option<&str> {
    all.last().map(|r| r.record.task.as_str())
}

/*
 * ---------------------------------------------------------------------------
 * What comes next
 * ---------------------------------------------------------------------------
 */

/// What a task does next, from its records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// The stage is under way: nothing else starts.
    Running(Stage),
    /// The stage to start, and its round.
    Start(Stage, u32),
    /// The task is over.
    Over,
}

/// Outcomes that end a task where it is, without delivering it: the user
/// stopped it, and is there to say what happens next.
const UNDELIVERED: &[&str] = &["stopped"];

/// What comes after `records` (a task's, oldest first) in a task of this
/// effort. Nothing recorded: the plan. A failed stage ends the task, and the
/// task still delivers what it has, with why, unless the user stopped it;
/// any other stage that ended leads to the next in the pipeline; the
/// delivery is the last.
pub fn next_stage(records: &[StageRecord], effort: Effort) -> Next {
    let Some(last) = records.last() else {
        return Next::Start(Stage::Plan, 1);
    };
    let round = |stage: Stage| records.iter().filter(|r| r.name == stage).count() as u32 + 1;
    match last.status {
        Status::Running => Next::Running(last.name),
        _ if last.name == Stage::Deliver => Next::Over,
        Status::Failed
            if last
                .outcome
                .as_deref()
                .is_some_and(|o| UNDELIVERED.contains(&o)) =>
        {
            Next::Over
        }
        Status::Failed => Next::Start(Stage::Deliver, round(Stage::Deliver)),
        Status::Done | Status::Skipped => {
            // The first of this effort's stages after it: a stage of the
            // other effort's (a record from before a change) is passed.
            let at = Stage::ALL.iter().position(|s| *s == last.name).unwrap_or(0);
            let stages = pipeline(effort);
            match Stage::ALL[at + 1..].iter().find(|s| stages.contains(s)) {
                Some(next) => Next::Start(*next, round(*next)),
                None => Next::Over,
            }
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * The rows
 * ---------------------------------------------------------------------------
 */

/// The chat's stage records, every task's, oldest first: read from the
/// database, so they're whatever the chat holds now, however long its runs.
pub async fn load(core: &Core, dialog_id: &str) -> Result<Vec<Recorded>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "stage", None))
        .await?;
    Ok(recorded(&rows))
}

/// Adds a stage record at the end of the chat, and shows it: the row's id.
pub async fn add_stage(
    core: &Core,
    dialog_id: &str,
    record: &StageRecord,
) -> Result<String, String> {
    let mut row = super::run::new_message("assistant", "");
    row.meta = Some(json!({ "stage": record }));
    let id = row.id.clone();
    append(core, dialog_id, row).await?;
    super::log::write(
        core,
        json!({
            "event": "stage",
            "dialogId": dialog_id,
            "task": record.task,
            "stage": record.name.id(),
            "status": record.status,
            "round": record.round,
        }),
    );
    Ok(id)
}

/// Changes the stage record in row `id` (its end: the status, outcome,
/// what it produced, its summary), and shows it.
pub async fn end_stage(
    core: &Core,
    dialog_id: &str,
    id: &str,
    change: impl FnOnce(&mut StageRecord),
) -> Result<StageRecord, String> {
    let ids = vec![id.to_string()];
    let row = core
        .db
        .read(move |c| store::messages_by_id(c, &ids))
        .await?
        .into_iter()
        .next()
        .filter(|row| row.dialog_id == dialog_id)
        .ok_or_else(|| format!("stage record {id} isn't in chat {dialog_id}"))?;
    let mut record =
        record_of(&row).ok_or_else(|| format!("message {id} holds no stage record"))?;
    change(&mut record);
    let mut meta = row
        .meta
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    meta["stage"] = json!(record);
    let (row_id, patch) = (
        id.to_string(),
        MessagePatch {
            meta: Some(meta.clone()),
            ..Default::default()
        },
    );
    core.db
        .write(move |c| store::update_message(c, &row_id, &patch))
        .await?;
    core.emit(super::events::AgentEvent::MessagePatched {
        dialog_id: dialog_id.to_string(),
        message_id: id.to_string(),
        patch: super::events::Patch {
            meta: Some(meta),
            ..Default::default()
        },
    });
    super::log::write(
        core,
        json!({
            "event": "stage",
            "dialogId": dialog_id,
            "task": record.task,
            "stage": record.name.id(),
            "status": record.status,
            "round": record.round,
            "outcome": record.outcome,
        }),
    );
    Ok(record)
}

/// Stores a row at the end of the chat and shows it.
async fn append(core: &Core, dialog_id: &str, row: NewMessage) -> Result<(), String> {
    let (id, stored) = (dialog_id.to_string(), vec![row.clone()]);
    let seqs = core
        .db
        .write(move |c| store::append_messages(c, &id, &stored))
        .await?;
    core.emit(super::events::AgentEvent::MessagesAdded {
        dialog_id: dialog_id.to_string(),
        messages: vec![super::events::out_message(dialog_id, seqs[0], &row, false)],
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: Stage, status: Status) -> StageRecord {
        StageRecord::ended("t", name, 1, Effort::Balanced, status)
    }

    fn failed(name: Stage, outcome: &str) -> StageRecord {
        StageRecord {
            outcome: Some(outcome.into()),
            ..rec(name, Status::Failed)
        }
    }

    /// The records of a task that went well up to and including `stage`.
    fn up_to(stage: Stage, effort: Effort) -> Vec<StageRecord> {
        let stages = pipeline(effort);
        let at = stages.iter().position(|s| *s == stage).unwrap();
        stages[..=at]
            .iter()
            .map(|s| rec(*s, Status::Done))
            .collect()
    }

    /// The JSON of 3.3, field for field, and a record from a later version
    /// with a stage this one doesn't know isn't read as any.
    #[test]
    fn a_record_reads_as_the_design_writes_it() {
        let written = json!({
            "task": "t_8f3a",
            "name": "planReview",
            "status": "running",
            "round": 2,
            "effort": "thorough",
            "candidate": "b",
            "outcome": "plan-blocked",
            "ref": "m1",
            "summary": "3 findings, 1 high",
        });
        let record: StageRecord = serde_json::from_value(written.clone()).unwrap();
        assert_eq!(record.name, Stage::PlanReview);
        assert_eq!(record.reference.as_deref(), Some("m1"));
        assert_eq!(serde_json::to_value(&record).unwrap(), written);

        let bare: StageRecord =
            serde_json::from_value(json!({ "task": "t", "name": "plan", "status": "done" }))
                .unwrap();
        assert_eq!((bare.round, bare.effort), (1, Effort::Balanced));
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({ "task": "t", "name": "plan", "status": "done", "round": 1, "effort": "balanced" })
        );

        let row = |meta: Value| MessageRow {
            id: "m".into(),
            dialog_id: "d".into(),
            seq: 1,
            role: "assistant".into(),
            content: String::new(),
            body_external: false,
            body_bytes: 0,
            tool_calls: None,
            tool_call_id: None,
            meta: Some(meta),
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 0,
        };
        assert_eq!(record_of(&row(json!({ "stage": written }))), Some(record));
        let later = json!({ "stage": { "task": "t", "name": "deploy", "status": "done" } });
        assert_eq!(record_of(&row(later)), None);
        assert_eq!(record_of(&row(json!({ "paused": {} }))), None);
        for id in Stage::ALL.iter().map(|s| s.id()) {
            let stage: Stage = serde_json::from_value(json!(id)).unwrap();
            assert_eq!(stage.id(), id);
        }
    }

    #[test]
    fn thorough_has_every_stage_and_balanced_all_but_its_own() {
        assert_eq!(pipeline(Effort::Thorough), Stage::ALL);
        let balanced = pipeline(Effort::Balanced);
        assert!(!balanced.contains(&Stage::Break) && !balanced.contains(&Stage::Pick));
        assert_eq!(balanced.len(), Stage::ALL.len() - 2);
        assert_eq!(balanced.first(), Some(&Stage::Plan));
        assert_eq!(balanced.last(), Some(&Stage::Deliver));
    }

    /// A02: for every stage of either effort and every way it can stand:
    /// running waits for it; done or skipped leads to the next stage of
    /// the effort; failed goes to the delivery, stopped to nothing; the
    /// delivery, however it went, ends the task.
    #[test]
    fn next_stage_for_every_status_of_every_stage_in_both_efforts() {
        for effort in [Effort::Balanced, Effort::Thorough] {
            let stages = pipeline(effort);
            for (i, stage) in stages.iter().copied().enumerate() {
                let before = |last: StageRecord| {
                    let mut records = up_to(stage, effort);
                    *records.last_mut().unwrap() = last;
                    records
                };
                let at = |records: Vec<StageRecord>| next_stage(&records, effort);
                let context = format!("{effort:?} {stage:?}");

                assert_eq!(
                    at(before(rec(stage, Status::Running))),
                    Next::Running(stage),
                    "{context}"
                );
                let after = match stages.get(i + 1) {
                    Some(next) => Next::Start(*next, 1),
                    None => Next::Over,
                };
                for status in [Status::Done, Status::Skipped] {
                    assert_eq!(
                        at(before(rec(stage, status))),
                        after,
                        "{context} {status:?}"
                    );
                }
                let failure = at(before(failed(stage, "plan-failed")));
                let stopped = at(before(failed(stage, "stopped")));
                if stage == Stage::Deliver {
                    assert_eq!(failure, Next::Over, "{context}");
                } else {
                    assert_eq!(failure, Next::Start(Stage::Deliver, 1), "{context}");
                }
                assert_eq!(stopped, Next::Over, "{context}");
            }
        }
    }

    #[test]
    fn a_task_starts_with_its_plan_and_rounds_count_up() {
        assert_eq!(
            next_stage(&[], Effort::Balanced),
            Next::Start(Stage::Plan, 1)
        );
        assert_eq!(
            next_stage(&[], Effort::Thorough),
            Next::Start(Stage::Plan, 1)
        );
        // A second delivery, after a failed stage that came after one.
        let records = vec![
            rec(Stage::Plan, Status::Done),
            failed(Stage::PlanReview, "plan-blocked"),
            rec(Stage::Deliver, Status::Done),
            failed(Stage::Plan, "error"),
        ];
        assert_eq!(
            next_stage(&records, Effort::Balanced),
            Next::Start(Stage::Deliver, 2)
        );
    }

    /// Balanced passes Thorough's stages, Thorough takes them.
    #[test]
    fn after_the_checks_balanced_goes_to_acceptance_and_thorough_breaks_it() {
        assert_eq!(
            next_stage(&up_to(Stage::Checks, Effort::Balanced), Effort::Balanced),
            Next::Start(Stage::Acceptance, 1)
        );
        assert_eq!(
            next_stage(&up_to(Stage::Checks, Effort::Thorough), Effort::Thorough),
            Next::Start(Stage::Break, 1)
        );
        // A Thorough-only stage in a Balanced task's records is passed.
        let mut records = up_to(Stage::Checks, Effort::Balanced);
        records.push(rec(Stage::Break, Status::Skipped));
        assert_eq!(
            next_stage(&records, Effort::Balanced),
            Next::Start(Stage::Acceptance, 1)
        );
    }
}

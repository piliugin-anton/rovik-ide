//! The stages of a task and their records (3.1, 3.3): what each stage is
//! and who carries it out (`Stage::carrier`), a stage record as a row's
//! `meta.stage` holds it, and what comes next from a task's records
//! (`next_stage`). Pure: no database, no runs.

use super::outcome::undelivered;
use crate::agent_run::budget::Spent;
use crate::chat_db::store::MessageRow;
use rovik_protocol::autopilot::Effort;
use serde::{Deserialize, Serialize};

/// A stage of a task (3.1).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// A Plan-mode run writes the plan (4.1).
    Plan,
    /// The plan reviewer checks it against the code (4.2).
    PlanReview,
    /// The dependencies the packages need are installed (4.3).
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

    /// Who carries it out. A new stage says so here, and the driver takes
    /// it from there: whether a run starts for it, whose, and where its
    /// record comes from.
    pub fn carrier(self) -> Carrier {
        let inside = |run, key| Carrier::Inside { run, key };
        match self {
            Stage::Plan | Stage::Implement => Carrier::Agent,
            Stage::Prepare | Stage::Tests | Stage::Pick | Stage::Deliver => Carrier::Ide,
            Stage::PlanReview => inside(Stage::Plan, "review"),
            Stage::Review => inside(Stage::Implement, "review"),
            Stage::Checks => inside(Stage::Implement, "verification"),
            Stage::Break => inside(Stage::Implement, "breaker"),
            Stage::Acceptance => inside(Stage::Implement, "acceptance"),
        }
    }

    /// A run of its own carries it out; the driver records the others.
    pub fn has_run(self) -> bool {
        !matches!(self.carrier(), Carrier::Inside { .. })
    }

    /// Its run is the IDE's own work (`Carrier::Ide`).
    pub fn is_ide_work(self) -> bool {
        self.carrier() == Carrier::Ide
    }

    /// Whether starting it starts work, which a task past its budget
    /// doesn't (U04): what a run did inside itself is recorded whatever
    /// the budget, the pick takes what the candidates did (T06), and the
    /// delivery always comes.
    pub fn starts_work(self) -> bool {
        self.has_run() && !matches!(self, Stage::Pick | Stage::Deliver)
    }

    /// D2: its run starts in a context fresh from the task's artifacts
    /// (`history_window`), not the plan's conversation: the implementation,
    /// and the delivery's report, written in the implementer's context.
    pub fn fresh_context(self) -> bool {
        matches!(self, Stage::Implement | Stage::Deliver)
    }

    /// T05: a candidate's chat has it: its implementation, and what that
    /// run does inside itself. The pick, and what comes after it, are the
    /// task's chat's.
    pub fn in_candidates(self) -> bool {
        self == Stage::Implement
            || matches!(
                self.carrier(),
                Carrier::Inside {
                    run: Stage::Implement,
                    ..
                }
            )
    }
}

/// Who carries a stage out (3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// A run of the agent's, which a message starts: the plan (the user's
    /// message), the implementation (the IDE's, with the plan).
    Agent,
    /// A run of the IDE's own work: the installs (S03), the tests written
    /// first (S04), the pick of a candidate (T06), the delivery and its
    /// report (S09). A run, so it can be stopped, its subagents started
    /// and its time counted, like any.
    Ide,
    /// Done inside the latest run of stage `run`, and recorded once that
    /// run ends, from the rows it left in the chat with `key` in their meta.
    Inside { run: Stage, key: &'static str },
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
    /// What the stage's runs spent: their time and their replies' cost
    /// (U04), which the task's budget adds up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent: Option<Spent>,
    /// The files the stage wrote: the Tests stage's test files (S04),
    /// which the implementation is told to make pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// T05: a Thorough task's implementation by candidates, by letter
    /// ("a", "b"): each implements the plan in a working copy of its own,
    /// in a chat of its own, and the stage's run is the IDE's, which runs
    /// them (candidates.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// H07: the provider's rate limit holds the stage up: its run goes on
    /// when the wait is over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitWait>,
    /// U04: what the run under way spent so far, kept as it goes, so that a
    /// crash keeps it. Its end puts all it spent in `spent` instead; the
    /// stage's next run, after a crash, adds what was kept to `spent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spending: Option<Spent>,
}

/// H07: a stage's waits for the provider's rate limit, kept in its record
/// so that a restart keeps them: when its run goes on, and how many waits
/// it had, how long in all (`RATE_LIMIT_PATIENCE` at most).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RateLimitWait {
    /// When the stage's run goes on, in ms since the epoch; None once it
    /// has gone on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    pub waits: u32,
    pub waited_ms: u64,
}

impl StageRecord {
    /// What the stage's runs spent (U04): the ended runs' (`spent`), and
    /// what the one under way, or one a crash cut, kept as it went.
    pub fn spent_so_far(&self) -> Option<Spent> {
        match (self.spent, self.spending) {
            (None, None) => None,
            (spent, spending) => Some(spent.unwrap_or_default().plus(spending.unwrap_or_default())),
        }
    }

    /// When the stage's run goes on after a wait for the provider's rate
    /// limit, while it waits for one (H07).
    pub fn waits_until(&self) -> Option<i64> {
        let waiting = self.status == Status::Running;
        self.rate_limit
            .as_ref()
            .and_then(|w| w.until)
            .filter(|_| waiting)
    }
}

/// What `records`' stages spent so far, all together (U04).
pub fn total_spent<'a>(records: impl IntoIterator<Item = &'a StageRecord>) -> Spent {
    add_spent(Spent::default(), records)
}

/// `spent`, and what `records`' stages spent so far.
pub(in crate::agent_run) fn add_spent<'a>(
    spent: Spent,
    records: impl IntoIterator<Item = &'a StageRecord>,
) -> Spent {
    records
        .into_iter()
        .filter_map(StageRecord::spent_so_far)
        .fold(spent, Spent::plus)
}

pub(super) fn first_round() -> u32 {
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
            spent: None,
            files: Vec::new(),
            candidates: Vec::new(),
            rate_limit: None,
            spending: None,
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

/// H08: where the latest task among a chat's stage rows is: its latest
/// stage record, and what its runs spent. None: the chat never ran one.
pub fn task_state(rows: &[MessageRow]) -> Option<rovik_protocol::autopilot::TaskState> {
    let all = recorded(rows);
    let task = latest_task(&all)?;
    let records = records_of(&all, task);
    let last = &records.last()?.record;
    let spent = total_spent(records.iter().map(|r| &r.record));
    let status = serde_json::to_value(last.status).ok()?;
    Some(rovik_protocol::autopilot::TaskState {
        id: task.to_string(),
        stage: last.name.id().to_string(),
        status: status.as_str().unwrap_or_default().to_string(),
        outcome: last.outcome.clone(),
        summary: last.summary.clone(),
        ms: spent.ms,
        cost: spent.cost,
        uncosted: spent.uncosted as u64,
        tokens: spent.tokens,
    })
}

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

/// What comes after `records` (a chat's of one task, oldest first) in a
/// task of this effort. Nothing recorded: the plan. A failed stage ends
/// the task, and the task still delivers what it has, with why, unless the
/// user stopped it; any other stage that ended leads to the next stage the
/// chat has; the delivery is the last. T05: a candidate's chat has its
/// implementation and what that run does inside itself
/// (`Stage::in_candidates`), and a failed stage ends its records: the
/// task's chat goes on from there.
pub fn next_stage(records: &[StageRecord], effort: Effort) -> Next {
    let Some(last) = records.last() else {
        return Next::Start(Stage::Plan, 1);
    };
    let candidate = records[0].candidate.is_some();
    let start = |stage: Stage| Next::Start(stage, next_round(records, stage));
    match last.status {
        Status::Running => Next::Running(last.name),
        Status::Failed if candidate => Next::Over,
        _ if last.name == Stage::Deliver => Next::Over,
        Status::Failed if last.outcome.as_deref().is_some_and(undelivered) => Next::Over,
        Status::Failed => start(Stage::Deliver),
        Status::Done | Status::Skipped => {
            // The first stage after it that the chat has: a stage of the
            // other effort's (a record from before a change) is passed.
            let stages = pipeline(effort);
            let has = |s: &Stage| match candidate {
                true => s.in_candidates(),
                false => stages.contains(s),
            };
            let at = Stage::ALL.iter().position(|s| *s == last.name).unwrap_or(0);
            match Stage::ALL[at + 1..].iter().copied().find(has) {
                Some(next) => start(next),
                None => Next::Over,
            }
        }
    }
}

/// The round a new record of `stage` would be, after `records`: one more
/// than they have.
pub(super) fn next_round<'a>(
    records: impl IntoIterator<Item = &'a StageRecord>,
    stage: Stage,
) -> u32 {
    records.into_iter().filter(|r| r.name == stage).count() as u32 + 1
}

/// What comes after a task's stored records (`next_stage`), at the effort
/// it started with.
pub fn next_after(records: &[Recorded]) -> Next {
    let list: Vec<StageRecord> = records.iter().map(|r| r.record.clone()).collect();
    let effort = list.first().map_or(Effort::default(), |first| first.effort);
    next_stage(&list, effort)
}

/// The stage a run carries out.
#[derive(Debug, Clone, PartialEq)]
pub struct StageRun {
    pub task: String,
    pub stage: Stage,
    /// Its record's row.
    pub record: String,
    pub effort: Effort,
    /// T05: an implementation by candidates, whose run is the IDE's.
    pub candidates: bool,
}

impl StageRun {
    /// The run of `record`, stored in row `id`.
    pub(super) fn new(id: String, record: &StageRecord) -> StageRun {
        StageRun {
            task: record.task.clone(),
            stage: record.name,
            record: id,
            effort: record.effort,
            candidates: !record.candidates.is_empty(),
        }
    }

    pub(super) fn of(r: &Recorded) -> StageRun {
        StageRun::new(r.id.clone(), &r.record)
    }

    /// Whether its run is the IDE's own work (`Stage::is_ide_work`), as an
    /// implementation by candidates is (T05).
    pub fn harness(&self) -> bool {
        self.stage.is_ide_work() || self.candidates
    }
}

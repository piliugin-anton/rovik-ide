//! The driver (A03): which stage a starting run carries out
//! (`stage_for`), what happens when a stage's run ends (`after_run`), the
//! stages it records and starts next (`advance`), and how a task goes on
//! after a crash, a restart or a closed app (`resume`). One driver at a
//! time in a chat (`drive_lock`).

use super::artifacts::{blocked_review, implementation_message, saved_plan};
use super::config::MODE;
use super::delivery::deliver;
use super::inside::inside_record;
use super::outcome::{
    failure_of, failure_summary, reopens, run_continues, task_outcome, undelivered,
};
use super::rate_limit::{rate_limit_wait, retry_later};
use super::rows::{add_stage, current_task, end_stage, load, settle_spent, task_chat, task_spent};
use super::stage::{
    next_after, next_round, total_spent, Carrier, Next, Recorded, Stage, StageRecord, StageRun,
    Status,
};
use crate::agent::OpenAIChatOptions;
use crate::agent_run::budget::{self, Spent};
use crate::agent_run::recovery::{self, Recovery};
use crate::agent_run::{plan, StartKind, StartRequest, UserMessage};
use crate::chat_db::store;
use crate::core::Core;
use rovik_protocol::agent::ModelProfile;
use rovik_protocol::autopilot::Effort;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// What a stage's run starts with: the model and settings of the run
/// before it, which for a task's first run are the user's start's.
#[derive(Clone, Debug)]
pub struct Settings {
    pub profile: ModelProfile,
    pub options: Option<OpenAIChatOptions>,
    pub context_mode: Option<String>,
    pub root: Option<String>,
    pub local_date: Option<String>,
    pub isolate: Option<bool>,
}

impl Settings {
    pub fn of(request: &StartRequest) -> Settings {
        Settings {
            profile: request.profile.clone(),
            options: request.options.clone(),
            context_mode: request.context_mode.clone(),
            root: request.root.clone(),
            local_date: request.local_date.clone(),
            isolate: request.isolate,
        }
    }

    /// A start of the chat's run with these settings. The configuration is
    /// the one the runs follow already.
    pub(in crate::agent_run) fn request(&self, dialog_id: &str, start: StartKind) -> StartRequest {
        StartRequest {
            dialog_id: dialog_id.to_string(),
            start,
            profile: self.profile.clone(),
            options: self.options.clone(),
            mode: Some(MODE.into()),
            context_mode: self.context_mode.clone(),
            root: self.root.clone(),
            current_user_content: None,
            local_date: self.local_date.clone(),
            config: None,
            isolate: self.isolate,
            project_settings: None,
        }
    }
}

/// Records a stage that starts now, its run to come: the run's stage.
pub(super) async fn record_run(
    core: &Core,
    dialog_id: &str,
    record: &StageRecord,
) -> Result<StageRun, String> {
    let id = add_stage(core, dialog_id, record).await?;
    Ok(StageRun::new(id, record))
}

/// A message of the user's that starts a task: sent in Autopilot mode.
pub(super) fn starts_task(message: &UserMessage) -> bool {
    let meta = message.meta.as_ref();
    meta.and_then(|m| m.get("mode")).and_then(Value::as_str) == Some(MODE)
        && meta.and_then(|m| m.get("harness")).is_none()
}

/// The record row a message the driver wrote is for (`meta.autopilot.record`).
pub(super) fn record_named(meta: Option<&Value>) -> Option<&str> {
    meta?.pointer("/autopilot/record")?.as_str()
}

/// Whether a run of stage record `record` stored its message, the one that
/// names the record (`meta.autopilot.record`), however far back it is.
async fn run_started(core: &Core, dialog_id: &str, record: &Recorded) -> Result<bool, String> {
    let (id, since) = (dialog_id.to_string(), record.seq);
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "autopilot", Some(since)))
        .await?;
    Ok(rows
        .iter()
        .any(|m| record_named(m.meta.as_ref()) == Some(record.id.as_str())))
}

/// The stage a run starting now carries out, if it's one of the chat's
/// task: the plan of a task its message starts (recorded here, before the
/// run), the stage the driver started it for, or the one a Retry, a
/// Continue or a Resume picks up. A run stage that ended in a failure is
/// opened again, as its next round, by the user's Retry or Continue only
/// (never by a Resume, which nobody asked for). The daemon's checks of a
/// merge (6.6) are the result's, never a stage's. None: the run is no
/// stage's.
pub async fn stage_for(
    core: &Core,
    dialog_id: &str,
    start: &StartKind,
    effort: Effort,
) -> Result<Option<StageRun>, String> {
    if let StartKind::Send { message } = start {
        if starts_task(message) {
            let record = StageRecord::running(&message.id, Stage::Plan, 1, effort);
            return record_run(core, dialog_id, &record).await.map(Some);
        }
        let Some(record) = record_named(message.meta.as_ref()) else {
            return Ok(None);
        };
        let all = load(core, dialog_id).await?;
        return Ok(all
            .iter()
            .find(|r| r.id == record && r.record.status == Status::Running)
            .map(StageRun::of));
    }
    if matches!(start, StartKind::CheckMerged { .. }) {
        return Ok(None);
    }
    let Some((_, records)) = current_task(core, dialog_id).await? else {
        return Ok(None);
    };
    let last = records.last().expect("a task has a record");
    if last.record.status == Status::Running {
        return Ok(last.record.name.has_run().then(|| StageRun::of(last)));
    }
    let asked = matches!(
        start,
        StartKind::Retry | StartKind::ContinueReply | StartKind::ContinueRun
    );
    if !asked {
        return Ok(None);
    }
    // The latest run stage, when it failed and only the delivery came after.
    let Some(at) = records
        .iter()
        .rposition(|r| r.record.name != Stage::Deliver)
    else {
        return Ok(None);
    };
    let failed = &records[at].record;
    let reopened = failed.name.has_run()
        && failed.status == Status::Failed
        && failed.outcome.as_deref().is_some_and(reopens);
    if !reopened {
        return Ok(None);
    }
    // T06: a Pick that took its winner's change failed with that winner's
    // work, and its copies are gone: the work is done again, not the pick.
    let again = match failed.name {
        Stage::Pick
            if crate::agent_run::candidates::took_winner(core, dialog_id, failed).await? =>
        {
            Stage::Implement
        }
        name => name,
    };
    let round = next_round(records.iter().map(|r| &r.record), again);
    // T05: an implementation by candidates is done by candidates again.
    let candidates = records
        .iter()
        .rev()
        .find(|r| r.record.name == again)
        .map_or_else(Vec::new, |r| r.record.candidates.clone());
    let record = StageRecord {
        candidates,
        ..StageRecord::running(&failed.task, again, round, failed.effort)
    };
    record_run(core, dialog_id, &record).await.map(Some)
}

pub(super) type Driving = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The chat's driver's turn: taken before a stage's run's end is told, so
/// a Resume that hears of it can't start the stage again meanwhile.
pub(in crate::agent_run) type DriveGuard = tokio::sync::OwnedMutexGuard<()>;

/// After a stage's run ended, with its outcome: the stage ends, and the
/// task goes on to its next stages. Called where the run's end is told,
/// after its landing (`agent_start`), with the driver's turn taken before
/// it was told. Boxed: what it starts is a run whose end comes back here.
/// `spent`: what the run spent (U04), which its stage's record keeps.
pub(in crate::agent_run) fn after_run(
    core: Core,
    dialog_id: String,
    run: StageRun,
    outcome: String,
    spent: Spent,
    settings: Settings,
    driving: DriveGuard,
) -> Driving {
    Box::pin(async move {
        let _driving = driving;
        let result = async {
            // U04: what the run spent is its record's from here, all of it.
            settle_spent(&core, &dialog_id, &run.record, spent).await?;
            if run_continues(&outcome) {
                // Under way still: what it spent so far is kept with it.
                return Ok(());
            }
            // H07: the provider's rate limit holds the stage up until it
            // resets, then its run goes on.
            if let Some(wait) = rate_limit_wait(&core, &dialog_id, &run, &outcome).await? {
                let again = crate::agent_run::failure::format_duration(wait.as_millis() as i64);
                let until = store::now_ms() + wait.as_millis() as i64;
                end_stage(&core, &dialog_id, &run.record, |r| {
                    r.summary = Some(format!("Rate limited by the provider: again in {again}"));
                    let kept = r.rate_limit.get_or_insert_with(Default::default);
                    kept.until = Some(until);
                    kept.waits += 1;
                    kept.waited_ms += wait.as_millis() as u64;
                })
                .await?;
                retry_later(
                    core.clone(),
                    dialog_id.clone(),
                    run.record.clone(),
                    settings,
                    until,
                );
                return Ok(());
            }
            end_run_stage(&core, &dialog_id, &run, &outcome, &settings).await?;
            #[cfg(test)]
            if crashes(&dialog_id, Crash::AfterEnd) {
                return Ok(());
            }
            advance(&core, &dialog_id, &run.task, &settings).await
        }
        .await;
        if let Err(e) = result {
            stopped_by(&core, &dialog_id, &e);
        }
    })
}

/// The driver couldn't go on (the database failed it): said in the log and
/// to the user; the task's records stay as they are, for a resume.
pub(super) fn stopped_by(core: &Core, dialog_id: &str, error: &str) {
    eprintln!("autopilot: chat {dialog_id} stopped: {error}");
    crate::agent_run::log::write(
        core,
        json!({ "event": "autopilot", "dialogId": dialog_id, "error": error }),
    );
    core.emit(crate::agent_run::events::AgentEvent::Notice {
        dialog_id: Some(dialog_id.to_string()),
        kind: "error".into(),
        title: "Autopilot stopped".into(),
        message: format!("The task couldn't go on: {error}"),
    });
}

/// One driver at a time in a chat: a run's end and a resume never both
/// start the next stage.
pub(super) fn lock_of(dialog_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks
        .get_or_insert_with(HashMap::new)
        .entry(dialog_id.to_string())
        .or_default()
        .clone()
}

pub(in crate::agent_run) async fn drive_lock(dialog_id: &str) -> DriveGuard {
    lock_of(dialog_id).lock_owned().await
}

pub(in crate::agent_run) fn try_drive_lock(dialog_id: &str) -> Option<DriveGuard> {
    lock_of(dialog_id).try_lock_owned().ok()
}

/// Why a start of the chat's run was refused: its driver is between two
/// of its task's stages, which takes longer than a moment only while it
/// delivers the task.
pub const DRIVER_BUSY: &str =
    "Autopilot is between two stages of this chat's task. Try again in a moment.";

/// The chat's driver's turn, for a start of its run that isn't the
/// driver's (the user's, the daemon's): once a driver between two stages
/// is done with it, so the two never interleave; `wait` at most.
pub(in crate::agent_run) async fn driver_turn(
    dialog_id: &str,
    wait: std::time::Duration,
) -> Result<DriveGuard, String> {
    tokio::time::timeout(wait, drive_lock(dialog_id))
        .await
        .map_err(|_| DRIVER_BUSY.to_string())
}

/// Ends the stage a run carried out. A plan's is done once a plan was
/// saved; the implementation's once the run finished, whatever its checks
/// said (the checks' stage says that). Either keeps its run's outcome.
pub(super) async fn end_run_stage(
    core: &Core,
    dialog_id: &str,
    run: &StageRun,
    outcome: &str,
    settings: &Settings,
) -> Result<(), String> {
    let all = load(core, dialog_id).await?;
    let Some(record) = all.iter().find(|r| r.id == run.record) else {
        return Err(format!("its stage record {} is gone", run.record));
    };
    // The line of a stage its run failed; past the budget, by how much, as
    // the run found.
    let failed_line = |failure: &str| {
        let mut line = failure_summary(failure);
        if failure == "budget" {
            let task = all.iter().filter(|r| r.record.task == run.task);
            let spent = total_spent(task.map(|r| &r.record));
            let budget = core.agents.config_for(dialog_id).autopilot.budget;
            if let Some(why) = budget::over(&budget, &spent) {
                line.push_str(&format!(": {why}"));
            }
        }
        line
    };
    // The IDE's own stages end their records themselves; they never end
    // a task, but for a Stop and the budget.
    if run.harness() {
        let ended = record.record.status != Status::Running;
        let failure = failure_of(outcome).filter(|f| matches!(*f, "stopped" | "budget"));
        let summary = match failure {
            Some(f) => Some(failed_line(f)),
            None if ended => None,
            None => Some(format!("It didn't finish ({outcome})")),
        };
        end_stage(core, dialog_id, &run.record, |r| {
            if let Some(f) = failure {
                r.status = Status::Failed;
                r.outcome = Some(f.to_string());
            } else if !ended {
                r.status = Status::Skipped;
            }
            if let Some(summary) = summary {
                r.summary = Some(summary);
            }
        })
        .await?;
        return Ok(());
    }
    let (status, outcome, summary, reference) = match failure_of(outcome) {
        Some(failure) => (
            Status::Failed,
            failure.to_string(),
            failed_line(failure),
            None,
        ),
        None if run.stage == Stage::Plan => match saved_plan(core, dialog_id, record.seq).await? {
            Some((row, file)) => (
                Status::Done,
                outcome.to_string(),
                format!(
                    "Saved the plan to {}",
                    plan::relative_plan_path(&file, settings.root.as_deref())
                ),
                Some(row),
            ),
            // S02: written, and held back by its last review: the review's
            // stage ends the task.
            None => match blocked_review(core, dialog_id, record.seq).await? {
                Some(row) => (
                    Status::Done,
                    outcome.to_string(),
                    "Written; its review held it back".into(),
                    Some(row),
                ),
                None => (
                    Status::Failed,
                    "plan-failed".to_string(),
                    failure_summary("plan-failed"),
                    None,
                ),
            },
        },
        // S06: its review held it back: the review's stage ends the task.
        None if outcome == "review-open" => (
            Status::Done,
            outcome.to_string(),
            "Finished; its review held it back".into(),
            None,
        ),
        // S08: the acceptance check found it short: its stage ends the task.
        None if outcome == "unmet" => (
            Status::Done,
            outcome.to_string(),
            "Finished; a criterion is unmet".into(),
            None,
        ),
        None => (Status::Done, outcome.to_string(), "Finished".into(), None),
    };
    end_stage(core, dialog_id, &run.record, |r| {
        r.status = status;
        r.outcome = Some(outcome);
        r.summary = Some(summary);
        r.reference = reference;
    })
    .await?;
    Ok(())
}

/// Records and starts the task's stages after its last record, until one
/// is a run of its own or the task is over. Each pass goes from the records
/// as they are, so a resume goes on from wherever this stopped.
pub(super) async fn advance(
    core: &Core,
    dialog_id: &str,
    task: &str,
    settings: &Settings,
) -> Result<(), String> {
    // Once round the pipeline at most: a record that can't be written
    // doesn't go round for ever.
    for _ in 0..=Stage::ALL.len() {
        let Some((current, records)) = current_task(core, dialog_id).await? else {
            return Ok(());
        };
        if current != task {
            return Ok(());
        }
        let (stage, round) = match next_after(&records) {
            Next::Start(stage, round) => (stage, round),
            Next::Running(_) | Next::Over => return Ok(()),
        };
        let effort = records[0].record.effort;
        // Past the task's budget, no stage starts work: it delivers (U04).
        if stage.starts_work() {
            if let Some(why) = over_budget(core, dialog_id, &records).await? {
                let mut record = StageRecord::ended(task, stage, round, effort, Status::Failed)
                    .with_summary(format!("Not started: over the task's budget: {why}"));
                record.outcome = Some("budget".into());
                add_stage(core, dialog_id, &record).await?;
                continue;
            }
        }
        let record = match stage.carrier() {
            Carrier::Inside { run, key } => {
                inside_record(core, dialog_id, &records, stage, run, key).await?
            }
            Carrier::Agent | Carrier::Ide => {
                match start_stage(core, dialog_id, &records, stage, round, settings).await? {
                    Started::Run => return Ok(()),
                    Started::Recorded => continue,
                    Started::Ended(record) => *record,
                }
            }
        };
        // T05: a candidate's records carry its letter.
        add_stage(
            core,
            dialog_id,
            &StageRecord {
                task: task.to_string(),
                effort,
                candidate: records[0].record.candidate.clone(),
                ..record
            },
        )
        .await?;
    }
    Ok(())
}

/// Why the task of `records` is past its budget (U04), if it is: by its
/// records; a candidate's chat counts its task's, all of it (T05).
pub(super) async fn over_budget(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<Option<String>, String> {
    let spent = match records[0].record.candidate.is_some() {
        true => task_spent(core, dialog_id).await?,
        false => total_spent(records.iter().map(|r| &r.record)),
    };
    let budget = core.agents.config_for(dialog_id).autopilot.budget;
    Ok(budget::over(&budget, &spent))
}

/// What became of a stage that has a run of its own, as the driver
/// reached it (`start_stage`).
pub(super) enum Started {
    /// A run carries it out, and its end goes on with the task.
    Run,
    /// It couldn't start, or it was done at once, and its record says
    /// so: the task goes on.
    Recorded,
    /// It ended as it came, with this record, which the driver adds.
    Ended(Box<StageRecord>),
}

impl Started {
    /// `Run` when its run started, else `Recorded`.
    fn when(ran: bool) -> Started {
        match ran {
            true => Started::Run,
            false => Started::Recorded,
        }
    }
}

/// Starts a stage that has a run of its own (`Stage::has_run`) as the
/// pipeline reaches it: its record and its run, or the record of why it
/// has none.
pub(super) async fn start_stage(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
    round: u32,
    settings: &Settings,
) -> Result<Started, String> {
    let first = &records[0].record;
    let (task, effort) = (first.task.as_str(), first.effort);
    let skipped = |summary: String| {
        let record = StageRecord::ended(task, stage, round, effort, Status::Skipped);
        Started::Ended(Box::new(record.with_summary(summary)))
    };
    Ok(match stage {
        // A task's plan starts with its message, never here.
        Stage::Plan => Started::Run,
        Stage::Implement => {
            // T05: Thorough's implementation is two candidates', each in a
            // working copy of its own, where copies can be made; else one
            // implementation, as Balanced's.
            if effort == Effort::Thorough {
                let started =
                    crate::agent_run::candidates::start(core, dialog_id, records, round, settings)
                        .await?;
                if let Some(ran) = started {
                    return Ok(Started::when(ran));
                }
            }
            Started::when(start_implement(core, dialog_id, records, round, settings, None).await?)
        }
        // S03: the installs, unless the settings turned them off.
        Stage::Prepare if !core.agents.config_for(dialog_id).autopilot.prepare => {
            skipped("Off in the settings".into())
        }
        // S03, S04: the installs, and the tests written first; T06: the
        // better candidate, when there were candidates.
        Stage::Prepare | Stage::Tests => Started::when(
            start_harness_stage(core, dialog_id, records, stage, round, settings).await?,
        ),
        Stage::Pick if crate::agent_run::candidates::ran(records) => Started::when(
            start_harness_stage(core, dialog_id, records, stage, round, settings).await?,
        ),
        // With one implementer, the record says why there was one.
        Stage::Pick => {
            skipped(crate::agent_run::candidates::single_summary(core, dialog_id, task).await?)
        }
        // S09: the delivery's record and the report, in a run of its own;
        // a task the user stopped has no report: they're there.
        Stage::Deliver => {
            let record = StageRecord::running(task, stage, round, effort);
            let id = add_stage(core, dialog_id, &record).await?;
            if !undelivered(&task_outcome(records)) {
                let request = settings.request(dialog_id, StartKind::Retry);
                if let Ok(Some(_)) = crate::agent_run::start_run(core, request).await {
                    return Ok(Started::Run);
                }
            }
            deliver(core, dialog_id, records, round, Some(id)).await?;
            Started::Recorded
        }
        Stage::PlanReview | Stage::Review | Stage::Checks | Stage::Break | Stage::Acceptance => {
            return Err(format!("the {} stage has no run of its own", stage.id()));
        }
    })
}

/// Why a stage's run didn't start (`start_run`'s answer), if it didn't.
pub(in crate::agent_run) fn not_started_why(
    started: Result<Option<String>, String>,
) -> Option<String> {
    match started {
        Ok(Some(_)) => None,
        Ok(None) => Some("It didn't start".into()),
        Err(e) => Some(format!("It couldn't start: {e}")),
    }
}

/// Starts a stage the IDE does itself (`Stage::is_ide_work`): its record,
/// then its run, which does the stage's work and ends its record. True:
/// the run started, and its end goes on with the task; false: it couldn't,
/// and its record, skipped, says why (these stages never end a task).
pub(super) async fn start_harness_stage(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
    round: u32,
    settings: &Settings,
) -> Result<bool, String> {
    let first = &records[0].record;
    let record = StageRecord::running(&first.task, stage, round, first.effort);
    let id = add_stage(core, dialog_id, &record).await?;
    // A Retry starts it: the run takes up the task's running stage.
    let started =
        crate::agent_run::start_run(core, settings.request(dialog_id, StartKind::Retry)).await;
    let Some(why) = not_started_why(started) else {
        return Ok(true);
    };
    end_stage(core, dialog_id, &id, |r| {
        r.status = Status::Skipped;
        r.summary = Some(why);
    })
    .await?;
    Ok(false)
}

/// Starts the implementation: its record (or `existing`, one recorded
/// before a crash), then a run whose message, the IDE's, is the plan the
/// task saved, as "Implement this plan" sends it. True: the run started;
/// false: it couldn't, and its record says why.
pub(super) async fn start_implement(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
    settings: &Settings,
    existing: Option<String>,
) -> Result<bool, String> {
    let first = &records[0].record;
    let (task, effort) = (first.task.clone(), first.effort);
    let record = match existing {
        Some(id) => id,
        None => {
            let record = StageRecord::running(&task, Stage::Implement, round, effort);
            add_stage(core, dialog_id, &record).await?
        }
    };
    #[cfg(test)]
    if crashes(dialog_id, Crash::BeforeRun) {
        return Ok(true);
    }
    // T05: a candidate's chat holds none of its task's artifacts: its
    // implementation is built from the task chat's records, as the task's
    // run first built it (candidates.rs).
    let chat = task_chat(core, dialog_id).await?;
    let theirs = match chat == dialog_id {
        true => None,
        false => current_task(core, &chat).await?.map(|(_, records)| records),
    };
    let built = implementation_message(
        core,
        &chat,
        theirs.as_deref().unwrap_or(records),
        &record,
        settings.root.as_deref(),
        first.candidate.as_deref(),
    )
    .await?;
    let message = match built {
        Ok(message) => message,
        Err(why) => return not_started(core, dialog_id, &record, &why).await,
    };
    let request = settings.request(dialog_id, StartKind::Send { message });
    match not_started_why(crate::agent_run::start_run(core, request).await) {
        None => Ok(true),
        Some(why) => not_started(core, dialog_id, &record, &why).await,
    }
}

/// The implementation's record when its run couldn't start: failed, and
/// why. False: no run started.
pub(super) async fn not_started(
    core: &Core,
    dialog_id: &str,
    record: &str,
    why: &str,
) -> Result<bool, String> {
    end_stage(core, dialog_id, record, |r| {
        r.status = Status::Failed;
        r.outcome = Some("error".into());
        r.summary = Some(why.to_string());
    })
    .await?;
    Ok(false)
}

/// Goes on with the chat's task where a crash, a restart or a closed app
/// cut it, as a Resume start asks (the app's, once a chat with an
/// unfinished task is opened). None: the chat has no unfinished task, and
/// the start goes on as any Resume; else what it started, if a run.
pub async fn resume(core: &Core, request: &StartRequest) -> Result<Option<Option<String>>, String> {
    // T05: a candidate's chat goes on from its task's chat, which runs it.
    if crate::agent_run::candidates::main_of(core, &request.dialog_id)
        .await?
        .is_some()
    {
        return Ok(Some(None));
    }
    go_on(core, request).await
}

/// `go_on`'s future, boxed.
pub(in crate::agent_run) type GoingOn<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Option<String>>, String>> + Send + 'a>>;

/// `go_on`, boxed: the task chat's run that runs its candidates calls it
/// (candidates.rs), and a run can't name its own future's type.
pub(in crate::agent_run) fn go_on_boxed<'a>(
    core: &'a Core,
    request: &'a StartRequest,
) -> GoingOn<'a> {
    Box::pin(go_on(core, request))
}

/// `resume`'s work, in a candidate's chat too (candidates.rs).
pub(in crate::agent_run) async fn go_on(
    core: &Core,
    request: &StartRequest,
) -> Result<Option<Option<String>>, String> {
    let dialog_id = request.dialog_id.as_str();
    let Some((task, records)) = current_task(core, dialog_id).await? else {
        return Ok(None);
    };
    let next = next_after(&records);
    if next == Next::Over {
        return Ok(None);
    }
    // Someone is on it already: the driver between two stages, a run, or a
    // resume a moment ago.
    let Some(_driving) = try_drive_lock(dialog_id) else {
        return Ok(Some(None));
    };
    if core.agents.is_running(dialog_id) {
        return Ok(Some(None));
    }
    let settings = Settings::of(request);
    let last = records.last().expect("a task has a record");
    // H07: held up by the provider's rate limit, the stage goes on once its
    // wait is over: a resume meanwhile starts nothing, and after a restart
    // it arms the wait again.
    if let Some(until) = last.record.waits_until() {
        let (chat, record) = (dialog_id.to_string(), last.id.clone());
        retry_later(core.clone(), chat, record, settings, until);
        return Ok(Some(None));
    }
    let stage = match next {
        Next::Running(stage) => stage,
        _ => {
            advance(core, dialog_id, &task, &settings).await?;
            return Ok(Some(None));
        }
    };
    if stage == Stage::Deliver {
        // Cut halfway: it's only records, so it's done again.
        deliver(
            core,
            dialog_id,
            &records,
            last.record.round,
            Some(last.id.clone()),
        )
        .await?;
        return Ok(Some(None));
    }
    if !stage.has_run() {
        // The driver records its other stages as they end: one left
        // running is a later version's, passed.
        end_stage(core, dialog_id, &last.id, |r| {
            r.status = Status::Skipped;
            r.summary = Some("It didn't finish".into());
        })
        .await?;
        advance(core, dialog_id, &task, &settings).await?;
        return Ok(Some(None));
    }
    let recent = crate::agent_run::run::recent_messages(&core.db, dialog_id).await?;
    let waits_for_checks = recent.last().is_some_and(|m| {
        m.meta
            .as_ref()
            .and_then(|meta| meta.pointer("/verification/status"))
            .and_then(Value::as_str)
            == Some("needs-approval")
    });
    if waits_for_checks {
        return Ok(Some(None));
    }
    let start = match recovery::after_crash(core, dialog_id).await? {
        Recovery::Start(kind) => kind,
        // It paused for the user's Continue, as it would have without a crash.
        Recovery::Paused => return Ok(Some(None)),
        Recovery::Interrupted => {
            let started = run_started(core, dialog_id, last).await?;
            // An implementation by candidates is the IDE's run, which a
            // Retry takes up, as the driver started it (T05).
            if stage == Stage::Implement && !started && last.record.candidates.is_empty() {
                // Recorded, but its run's message never stored: started
                // now, as the driver would have.
                start_implement(
                    core,
                    dialog_id,
                    &records,
                    last.record.round,
                    &settings,
                    Some(last.id.clone()),
                )
                .await?;
                return Ok(Some(None));
            }
            StartKind::Retry
        }
    };
    crate::agent_run::start_run(core, settings.request(dialog_id, start))
        .await
        .map(Some)
}

/// Tests: where the driver "crashes" in a chat, once: right after ending a
/// run's stage, or after recording the implementation (or a candidate's)
/// before its run.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::agent_run) enum Crash {
    AfterEnd,
    BeforeRun,
    /// After recording a candidate's implementation, before its run; set
    /// on the task's chat (candidates.rs).
    BeforeCandidate,
}

/// Tests: the crash each chat has coming (tests run side by side).
#[cfg(test)]
pub(super) static CRASHES: Mutex<Vec<(String, Crash)>> = Mutex::new(Vec::new());

/// Tests: the driver "crashes" in the chat at `at`, once.
#[cfg(test)]
pub(in crate::agent_run) fn crash_at(dialog_id: &str, at: Crash) {
    let mut crashes = CRASHES.lock().unwrap_or_else(|e| e.into_inner());
    crashes.push((dialog_id.to_string(), at));
}

#[cfg(test)]
pub(in crate::agent_run) fn crashes(dialog_id: &str, at: Crash) -> bool {
    let mut crashes = CRASHES.lock().unwrap_or_else(|e| e.into_inner());
    let due = crashes.iter().position(|(d, c)| d == dialog_id && *c == at);
    due.map(|i| crashes.remove(i)).is_some()
}

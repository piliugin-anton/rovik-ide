//! What a run's outcome means for its stage and its task: which outcomes
//! fail a stage, which a Retry reopens, which deliver as partial, the
//! task's outcome from its records, and the strip's line for each.

use super::stage::{Recorded, Stage, Status};
use crate::agent_run::landing::pending_reason_in;
use crate::agent_run::recovery;

/// The outcomes a stage's run fails its stage with, and the task's outcome
/// it ends the stage with then. The rest are a run that finished, however
/// its checks went: the stages after it say how.
pub(super) const RUN_FAILURES: &[(&str, &str)] = &[
    ("stopped", "stopped"),
    ("verify-stopped", "stopped"),
    ("error", "error"),
    ("repetition", "repetition"),
    ("unfinished-checklist", "unfinished-checklist"),
    // Paused for a person, which nobody is (U03): the task delivers.
    ("paused-loop", "paused-loop"),
    ("paused-checkpoint", "paused-checkpoint"),
    // Past the task's budget (U04).
    ("budget", "budget"),
];

/// The task's outcome when a stage's run ended with `outcome` and so
/// failed its stage; None: the run finished.
pub(super) fn failure_of(outcome: &str) -> Option<&'static str> {
    RUN_FAILURES
        .iter()
        .find(|(run, _)| *run == outcome)
        .map(|(_, task)| *task)
}

/// Whether a run that ended with `outcome` finished its work (`failure_of`):
/// what T06 scores a candidate's implementation by.
pub fn finished(outcome: &str) -> bool {
    failure_of(outcome).is_none()
}

/// Whether a run that ended with `outcome` ended clean: with a result
/// Autopilot lands on its own, checked, its criteria met and no high
/// finding left open (D7). What T06 ranks a finished candidate by next
/// (R03).
pub fn clean(outcome: &str) -> bool {
    pending_reason_in(outcome, true).is_none()
}

/// Whether a stage its run failed with the task's `outcome` opens again on
/// a Retry or a Continue: the user is there, and asks for another try (or
/// for the checklist to be finished, or for a paused run to go on).
pub(super) fn reopens(outcome: &str) -> bool {
    RUN_FAILURES.iter().any(|(_, task)| *task == outcome)
}

/// Whether a task that failed with `outcome` ends where it is, without
/// delivering it: the user stopped it, and is there to say what happens
/// next.
pub(super) fn undelivered(outcome: &str) -> bool {
    outcome == "stopped"
}

/// Outcomes after which the stage's run goes on later, from where it is,
/// with nobody to ask: a move to or from a host, an upgrade of the
/// server's Rovik (the daemon's next start). Its stage stays under way.
/// Anything that would wait for a person ends the stage (U03).
pub(super) fn run_continues(outcome: &str) -> bool {
    outcome == format!("paused-{}", crate::agent_run::MOVE)
        || outcome == format!("paused-{}", recovery::DRAIN)
}

/// The outcomes delivered as partial: not landed, but the task's result,
/// which its report says is incomplete (D7, S09).
pub(super) const PARTIAL: &[&str] = &["unmet", "budget", "verify-flagged"];

/// The strip's line for a task's outcome when a stage failed with it.
pub(super) fn failure_summary(outcome: &str) -> String {
    match outcome {
        "plan-failed" => "No plan was saved".into(),
        "plan-blocked" => "A finding is still open after the plan's last review".into(),
        "review-open" => "A finding is still open after the change's last review".into(),
        "unmet" => "A criterion is still unmet after the last acceptance check".into(),
        "stopped" => "Stopped".into(),
        "unfinished-checklist" => "The checklist isn't finished".into(),
        "paused-loop" => "The run kept looping after it was told to change approach".into(),
        "paused-checkpoint" => "The run paused at its tool-call checkpoint".into(),
        "budget" => "Over the task's budget".into(),
        // "Not landed: the checks still fail." → "The checks still fail"
        _ => match pending_reason_in(outcome, true) {
            Some(reason) => upper_first(
                reason
                    .trim_start_matches("Not landed: ")
                    .trim_end_matches('.'),
            ),
            None => outcome.to_string(),
        },
    }
}

/// "the checks still fail" as it starts a line.
pub(super) fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// "The checks still fail" as it reads after a colon.
pub(super) fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

/// "s" after a count other than one.
pub(in crate::agent_run) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The task's outcome: the one its failed stage ended it with, else how its
/// implementation's run ended. A failed stage a Retry ran again (a later
/// round of it) is the later round's to say.
pub(super) fn task_outcome(records: &[Recorded]) -> String {
    let failed = records.iter().enumerate().find_map(|(i, r)| {
        let again = records[i + 1..]
            .iter()
            .any(|l| l.record.name == r.record.name);
        (r.record.status == Status::Failed && r.record.name != Stage::Deliver && !again)
            .then_some(r)
    });
    let implemented = || {
        records
            .iter()
            .rev()
            .find(|r| r.record.name == Stage::Implement)
    };
    failed
        .or_else(implemented)
        .and_then(|r| r.record.outcome.clone())
        .unwrap_or_else(|| "done".into())
}

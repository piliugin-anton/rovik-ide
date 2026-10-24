//! The records of the stages done inside another stage's run
//! (`Carrier::Inside`): the plan's review, the change review, the checks,
//! the adversarial tester and the acceptance check, from the rows the run
//! left in the chat.

use super::config::run_config;
use super::outcome::{failure_summary, plural};
use super::stage::{next_round, Recorded, Stage, StageRecord, Status};
use crate::chat_db::store::{self, MessageRow};
use crate::core::Core;
use serde_json::Value;

/// The record of a stage done inside the latest run of stage `run`
/// (`Carrier::Inside`). T05: inside an implementation by candidates, how
/// each candidate's went (candidates.rs); else from the rows that run left
/// in the chat with `key` in their meta (`done_in_run`).
pub(super) async fn inside_record(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
    run: Stage,
    key: &'static str,
) -> Result<StageRecord, String> {
    if run == Stage::Implement {
        let by_candidates =
            crate::agent_run::candidates::stage_record(core, dialog_id, records, stage).await?;
        if let Some(record) = by_candidates {
            return Ok(record);
        }
    }
    done_in_run(core, dialog_id, records, stage, run, key).await
}

/// The record of a stage done inside the latest run of stage `within`, from
/// the rows it left with `key` in their meta: the plan's review inside the
/// plan's run (in its submits' results); the change review, the checks,
/// the adversarial tester's and the acceptance's at the end of the
/// implementation's.
pub(super) async fn done_in_run(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
    within: Stage,
    key: &'static str,
) -> Result<StageRecord, String> {
    let run = records
        .iter()
        .rev()
        .find(|r| r.record.name == within)
        .ok_or_else(|| format!("the task has no {} stage", within.id()))?;
    let (id, since) = (dialog_id.to_string(), run.seq);
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, key, Some(since)))
        .await?;
    // The configuration the run had: Autopilot's.
    let config = run_config(core.agents.config_for(dialog_id), run.record.effort);
    // The stage's round in the task, as a stage with a run of its own has
    // it: the rounds of its review or its checks inside the run are its
    // record's (`reference`) and its line's.
    let round = next_round(records.iter().map(|r| &r.record), stage);
    let base = StageRecord::ended(
        &run.record.task,
        stage,
        round,
        run.record.effort,
        Status::Skipped,
    );
    match stage {
        Stage::PlanReview | Stage::Review => Ok(review_record(base, &rows, &config)),
        Stage::Checks => Ok(checks_record(
            base,
            run.record.outcome.as_deref(),
            rows.last(),
            &config,
        )),
        Stage::Break => Ok(break_record(base, rows.last())),
        Stage::Acceptance => Ok(acceptance_record(base, rows.last())),
        Stage::Plan
        | Stage::Prepare
        | Stage::Tests
        | Stage::Implement
        | Stage::Pick
        | Stage::Deliver => Err(format!("the {} stage has a run of its own", stage.id())),
    }
}

/// T04: the Break stage's record, from the adversarial tester's record in
/// the implementing run (`meta.breaker`).
pub(super) fn break_record(base: StageRecord, last: Option<&MessageRow>) -> StageRecord {
    let Some((row, tester)) = last.and_then(|m| Some((m, m.meta.as_ref()?.get("breaker")?))) else {
        return base.with_summary("Not run: the checks didn't pass");
    };
    let count = |key: &str| tester[key].as_array().map_or(0, Vec::len);
    let files = |n: usize| format!("{n} test file{}", plural(n));
    let (kept, removed) = (count("files"), count("removed"));
    let mut summary = match (kept, tester["found"].as_bool()) {
        (0, _) => match tester["reason"].as_str() {
            Some(why) => format!("Skipped: {why}"),
            None => "It wrote no test that stays".into(),
        },
        (n, Some(true)) => format!("{} found a fault; a repair round followed", files(n)),
        (n, Some(false)) => format!("{} pass, kept as regression tests", files(n)),
        // No check round ran its tests (`found`): the run ended first.
        (n, None) => format!("{} written; the run ended before they ran", files(n)),
    };
    if removed > 0 {
        summary.push_str(&format!(
            "; {removed} speculative test{} that passed removed",
            plural(removed)
        ));
    }
    let unrun = kept > 0 && tester["found"].as_bool().is_none();
    let status = match (kept, removed, tester["status"].as_str()) {
        (0, 0, _) | (_, _, Some("running")) => Status::Skipped,
        _ if unrun => Status::Skipped,
        _ => Status::Done,
    };
    let summary = match tester["status"].as_str() {
        Some("running") => "It didn't finish".to_string(),
        _ => summary,
    };
    StageRecord {
        status,
        reference: Some(row.id.clone()),
        ..base.with_summary(summary)
    }
}

/// S08: the acceptance's record, from the implementing run's last check
/// (`meta.acceptance`).
pub(super) fn acceptance_record(base: StageRecord, last: Option<&MessageRow>) -> StageRecord {
    let Some((row, check)) = last.and_then(|m| Some((m, m.meta.as_ref()?.get("acceptance")?)))
    else {
        return base.with_summary("Not checked");
    };
    let round = check["round"].as_u64().unwrap_or(1) as u32;
    let criteria = check["criteria"].as_array().map_or(0, Vec::len);
    let unmet = check["verdicts"]
        .as_array()
        .map_or(0, |v| v.iter().filter(|v| v["satisfied"] == false).count());
    let (status, outcome, summary) = match check["status"].as_str().unwrap_or("") {
        "satisfied" => (
            Status::Done,
            None,
            match criteria {
                1 => "The criterion is met".to_string(),
                n => format!("All {n} criteria met"),
            },
        ),
        "unsatisfied" if check["unmet"] == true => (
            Status::Failed,
            Some("unmet".to_string()),
            format!(
                "{unmet} of {criteria} criteria unmet after {round} check{}",
                plural(round as usize)
            ),
        ),
        "unsatisfied" => (
            Status::Skipped,
            None,
            format!(
                "{unmet} of {criteria} criteria unmet, and the run ended before the last check"
            ),
        ),
        // An earlier check found criteria unmet, and this one couldn't
        // judge their fix: they stand (S08, R03).
        "skipped" if check["unmet"] == true => (
            Status::Failed,
            Some("unmet".to_string()),
            format!(
                "{} of {criteria} criteria still unmet: check {round} was skipped ({})",
                check["earlierUnmet"].as_u64().unwrap_or(1),
                check["reason"].as_str().unwrap_or("it couldn't judge"),
            ),
        ),
        "skipped" => (
            Status::Skipped,
            None,
            match check["reason"].as_str() {
                Some(why) => format!("Skipped: {why}"),
                None => "Skipped".into(),
            },
        ),
        _ => (Status::Skipped, None, "It didn't finish".into()),
    };
    StageRecord {
        status,
        outcome,
        reference: Some(row.id.clone()),
        ..base.with_summary(summary)
    }
}

/// The checks' record, from how the implementing run ended (`outcome`)
/// and its last round's record.
pub(super) fn checks_record(
    base: StageRecord,
    outcome: Option<&str>,
    last: Option<&MessageRow>,
    config: &crate::agent_run::AgentConfig,
) -> StageRecord {
    let field = |key: &str| last.and_then(|m| m.meta.as_ref()?.get("verification")?.get(key));
    let ended = |status: Status, summary: String| StageRecord {
        status,
        reference: last.map(|m| m.id.clone()),
        ..base.clone().with_summary(summary)
    };
    match outcome.unwrap_or("done") {
        // S08: the checks passed; the acceptance check is what fell short.
        "verified" | "unmet" => ended(Status::Done, checks_passed(last)),
        "unverified" | "verify-unverified" => {
            let why = field("reason")
                .and_then(Value::as_str)
                .unwrap_or("none could run");
            ended(Status::Skipped, format!("No checks ran: {why}"))
        }
        "done" if !config.permissions.verify_changes => {
            ended(Status::Skipped, "The checks are off".into())
        }
        "done" => ended(Status::Skipped, "Nothing to check".into()),
        failed => StageRecord {
            outcome: Some(failed.to_string()),
            ..ended(Status::Failed, failure_summary(failed))
        },
    }
}

/// A plan stage's stop reason (`review["stop"]`), as the strip says it.
pub(super) fn stop_reason(stop: &str) -> &'static str {
    match stop {
        "plan-time" => "time limit",
        "plan-budget" => "budget share",
        "rounds" => "review limit",
        "stalled" => "no progress",
        "reviewer-unavailable" => "reviewer unavailable",
        _ => "limit",
    }
}

/// A review's record, from the last review of its kind among `rows`: the
/// plan's (`stage` planReview) or the change's.
pub(super) fn review_record(
    base: StageRecord,
    rows: &[MessageRow],
    config: &crate::agent_run::AgentConfig,
) -> StageRecord {
    use crate::agent_run::subagent::{Kind, SubagentSettings};
    let (wanted, kind) = match base.name {
        Stage::PlanReview => ("plan", Kind::PlanReviewer),
        _ => ("change", Kind::Reviewer),
    };
    let last = rows.iter().rev().find_map(|m| {
        let review = m.meta.as_ref()?.get("review")?;
        (review["kind"] == wanted).then_some((m, review))
    });
    let Some((row, review)) = last else {
        let why = match config.subagents.kind(kind) {
            None => format!(
                "Not reviewed: the {} is off",
                kind.spec().label.to_lowercase()
            ),
            Some(_) => "Not reviewed".to_string(),
        };
        return base.with_summary(why);
    };
    // S02, S06: blocked at the last round, with a finding open: the
    // plan isn't saved, or the change goes no further.
    if review["blocked"] == true {
        let round = review["round"].as_u64().unwrap_or(1) as u32;
        let open = review["findings"].as_array().map_or(0, Vec::len);
        let outcome = match wanted {
            "plan" => "plan-blocked",
            _ => "review-open",
        };
        // Balanced Autopilot's plan: which limit ended its reviews.
        let why = match review["stop"].as_str() {
            Some(stop) => format!(" ({})", stop_reason(stop)),
            None => String::new(),
        };
        let left = match open {
            0 => ": no review left".to_string(),
            n => format!(": {n} finding{} open", plural(n)),
        };
        return StageRecord {
            status: Status::Failed,
            reference: Some(row.id.clone()),
            outcome: Some(outcome.into()),
            ..base.with_summary(format!(
                "Blocked after {round} review{}{why}{left}",
                plural(round as usize)
            ))
        };
    }
    let (status, summary) = match review["status"].as_str().unwrap_or("") {
        "clean" => (Status::Done, "Clean".to_string()),
        "findings" => (Status::Done, findings_summary(review)),
        "skipped" => (
            Status::Skipped,
            match review["reason"].as_str() {
                Some(why) => format!("Skipped: {why}"),
                None => "Skipped".into(),
            },
        ),
        _ => (Status::Skipped, "It didn't finish".into()),
    };
    StageRecord {
        status,
        reference: Some(row.id.clone()),
        ..base.with_summary(summary)
    }
}

/// "3 findings, 1 high", and how many were left open at the last round.
pub(super) fn findings_summary(review: &Value) -> String {
    let findings = review["findings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let high = findings.iter().filter(|f| f["severity"] == "high").count();
    let n = findings.len();
    let mut text = format!("{n} finding{}", plural(n));
    if high > 0 {
        text.push_str(&format!(", {high} high"));
    }
    if let Some(open) = review["open"].as_u64().filter(|o| *o > 0) {
        text.push_str(&format!(" ({open} left open)"));
    }
    text
}

/// "2 checks passed", from the last round's record.
pub(super) fn checks_passed(last: Option<&MessageRow>) -> String {
    let passed = last
        .and_then(|m| {
            m.meta
                .as_ref()?
                .pointer("/verification/checks")?
                .as_array()
                .cloned()
        })
        .map(|checks| checks.iter().filter(|c| c["status"] == "passed").count())
        .unwrap_or(0);
    match passed {
        0 => "Passed".into(),
        1 => "1 check passed".into(),
        n => format!("{n} checks passed"),
    }
}

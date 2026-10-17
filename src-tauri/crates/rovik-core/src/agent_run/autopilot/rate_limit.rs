//! H07: a stage's run the provider rate limited goes on by itself once
//! the limit resets, with nobody to press Retry; the waits are kept in the
//! stage's record, so a restart keeps them.

use super::driver::{drive_lock, stopped_by, Settings};
use super::rows::{current_task, end_stage, record_in, task_spent};
use super::stage::StageRun;
use crate::agent_run::budget;
use crate::agent_run::StartKind;
use crate::chat_db::store;
use crate::core::Core;
use serde_json::json;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// H07: how long a stage waits for the provider's rate limit before
/// going on, the first time it waits without being told; doubled each
/// time after.
pub(super) const RATE_LIMIT_FIRST_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a stage waits for rate limits in all, at most.
pub(super) const RATE_LIMIT_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3600);

/// H07 (3.6): nobody is there to press Retry, on a server or on this
/// computer (Autopilot never asks, D5): a stage's run that ended `error`
/// because the provider rate limited it goes on once the limit resets: at
/// the time its failure named, else after a minute, then two, four…,
/// while the task is within its budget (U04) and the stage's waits add up
/// to an hour at most (its record keeps them, so a restart does too).
/// None: the stage ends as before.
pub(super) async fn rate_limit_wait(
    core: &Core,
    dialog_id: &str,
    run: &StageRun,
    outcome: &str,
) -> Result<Option<std::time::Duration>, String> {
    if outcome != "error" {
        return Ok(None);
    }
    let recent = crate::agent_run::run::recent_messages(&core.db, dialog_id).await?;
    let failure = recent
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| m.meta.as_ref()?.get("failure").cloned());
    let Some(failure) = failure.filter(|f| f["kind"] == "rate_limited") else {
        return Ok(None);
    };
    let task = task_spent(core, dialog_id).await?;
    let budget = core.agents.config_for(dialog_id).autopilot.budget;
    if budget::over(&budget, &task).is_some() {
        return Ok(None);
    }
    let kept = record_in(core, dialog_id, &run.record)
        .await?
        .rate_limit
        .unwrap_or_default();
    let (waits, waited) = (kept.waits, std::time::Duration::from_millis(kept.waited_ms));
    let reset = failure["resetAt"]
        .as_i64()
        .map(|at| (at - store::now_ms()).max(0) as u64);
    let wait = match reset {
        Some(ms) => std::time::Duration::from_millis(ms.max(1000)),
        None => RATE_LIMIT_FIRST_WAIT * 2u32.saturating_pow(waits.min(10)),
    };
    if waited + wait > RATE_LIMIT_PATIENCE {
        return Ok(None);
    }
    crate::agent_run::log::write(
        core,
        json!({
            "event": "rateLimitWait",
            "dialogId": dialog_id,
            "record": run.record,
            "waitMs": wait.as_millis() as u64,
        }),
    );
    Ok(Some(wait))
}

/// H07: the stage's run again once its wait is over (`until`, in ms since
/// the epoch), as a Retry, while the stage is still the task's latest,
/// under way and waiting for this very wait, and no run of the chat goes:
/// a Stop, a new message, a Retry or a resume meanwhile leaves it be. One
/// timer a wait: arming one that's armed does nothing.
pub(super) fn retry_later(
    core: Core,
    dialog_id: String,
    record: String,
    settings: Settings,
    until: i64,
) {
    let key = (record.clone(), until);
    if !armed_waits().insert(key.clone()) {
        return;
    }
    crate::rt::spawn(async move {
        let wait = (until - store::now_ms()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        armed_waits().remove(&key);
        let driving = drive_lock(&dialog_id).await;
        if core.agents.is_running(&dialog_id) {
            return;
        }
        let latest = match current_task(&core, &dialog_id).await {
            Ok(Some((_, records))) => records.last().cloned(),
            _ => None,
        };
        let still = latest.is_some_and(|r| r.id == record && r.record.waits_until() == Some(until));
        if !still {
            return;
        }
        // A chat that moved meanwhile (to a host, or back from this one)
        // goes on where it is now, never here.
        let chat = dialog_id.clone();
        let here = core
            .db
            .read(move |c| crate::chat_db::location::of_chat(c, &chat))
            .await
            .is_ok_and(|at| at == crate::chat_db::location::Location::Here);
        if !here {
            return;
        }
        // It waits no more: its run goes on.
        let gone_on = end_stage(&core, &dialog_id, &record, |r| {
            if let Some(kept) = r.rate_limit.as_mut() {
                kept.until = None;
            }
        })
        .await;
        if let Err(e) = gone_on {
            return stopped_by(&core, &dialog_id, &e);
        }
        // Started with the driver's turn still taken, as a resume starts
        // one: a resume meanwhile finds the run, never a stage to start.
        let request = settings.request(&dialog_id, StartKind::Retry);
        if let Err(e) = crate::agent_run::start_run(&core, request).await {
            stopped_by(&core, &dialog_id, &e);
        }
        drop(driving);
    });
}

/// The waits a timer of this process is armed for (`retry_later`), by
/// record and end. A restart has none: a resume arms them again from the
/// records (`go_on`).
pub(super) fn armed_waits() -> std::sync::MutexGuard<'static, HashSet<(String, i64)>> {
    static ARMED: LazyLock<Mutex<HashSet<(String, i64)>>> = LazyLock::new(Default::default);
    ARMED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether this process goes on with stage record `record` once its wait
/// for the provider's rate limit, until `until`, is over (H07).
pub(in crate::agent_run) fn rate_limit_armed(record: &str, until: i64) -> bool {
    armed_waits().contains(&(record.to_string(), until))
}

/// A restart, as far as a wait's timer goes: this process forgets it.
#[cfg(test)]
pub(in crate::agent_run) fn forget_rate_limit_wait(record: &str, until: i64) {
    armed_waits().remove(&(record.to_string(), until));
}

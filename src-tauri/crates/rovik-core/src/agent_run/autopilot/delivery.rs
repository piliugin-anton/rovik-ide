//! The last stage (4.11): what became of a task's result (landed, kept on
//! its server, delivered as partial, nothing to deliver), its push to a
//! branch on a server (D15, H04), and the facts its report is written from
//! (S09).

use super::artifacts::task_plan_file;
use super::config::MODE;
use super::outcome::{failure_summary, lower_first, task_outcome, PARTIAL};
use super::rows::{add_stage, current_task, end_stage};
use super::stage::{Recorded, Stage, StageRecord, Status};
use crate::agent_run::budget::Spent;
use crate::agent_run::landing::pending_reason_in;
use crate::agent_run::plan;
use crate::chat_db::store::{self, MessageRow};
use crate::core::Core;
use serde::Serialize;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

/// Whether an implementation's run started in the task: the IDE's message
/// that starts one is in the chat. An implement stage that never started
/// (it couldn't, or the budget was spent) changed nothing.
pub(super) async fn implementation_ran(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<bool, String> {
    // T05: the candidates' implementations, each in its own chat.
    if crate::agent_run::candidates::ran(records) {
        return Ok(true);
    }
    let (id, since) = (dialog_id.to_string(), records[0].seq);
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, MODE, Some(since)))
        .await?;
    let task = &records[0].record.task;
    Ok(rows.iter().any(|m| {
        let started = m.meta.as_ref().and_then(|x| x.get(MODE));
        m.role == "user"
            && started.is_some_and(|a| a["stage"] == Stage::Implement.id() && a["task"] == *task)
    }))
}

/// What became of a task's result (S09, D7): landed, or waiting on its
/// server; delivered as partial (`PARTIAL`), not landed; or nothing
/// delivered. The record's status, outcome and line.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub status: Status,
    pub outcome: String,
    pub delivered: bool,
    pub partial: bool,
    pub summary: String,
}

/// The delivery of a task by its records. The implementing run's end
/// landed its result already where it could (landing.rs), or kept it on
/// its server for its computer; this says which.
pub(super) async fn delivery_of(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<Delivery, String> {
    let outcome = task_outcome(records);
    let implemented = implementation_ran(core, dialog_id, records).await?;
    let on_server = core.agents.on_server();
    // An isolated chat's changes wait in its overlay, with why; a chat
    // working in the project folder made its changes there.
    let waiting = crate::overlay::chats::pending(dialog_id).map(|p| p.reason);
    let in_project = !on_server && !crate::overlay::chats::exists(dialog_id);
    let partial = implemented && PARTIAL.contains(&outcome.as_str());
    let (delivered, summary) = match (waiting, pending_reason_in(&outcome, true)) {
        // It ended before anything was changed.
        _ if !implemented => (false, "Nothing to deliver".to_string()),
        (Some(reason), _) if partial => (
            false,
            format!(
                "Delivered as partial, not landed: {}",
                reason.trim_start_matches("Not landed: ")
            ),
        ),
        (Some(reason), _) => (false, reason),
        (None, Some(_)) if partial && in_project => (
            false,
            format!(
                "Delivered as partial: {}; the changes are in the project as they are",
                lower_first(&failure_summary(&outcome))
            ),
        ),
        (None, Some(_)) if in_project => (
            false,
            format!(
                "{}; the changes are in the project as they are",
                failure_summary(&outcome)
            ),
        ),
        (None, Some(reason)) if partial => (
            false,
            format!(
                "Delivered as partial, not landed: {}",
                reason.trim_start_matches("Not landed: ")
            ),
        ),
        (None, Some(reason)) => (false, reason),
        (None, None) if on_server => (
            true,
            "Delivered: the result waits on the server for its computer".into(),
        ),
        (None, None) => (true, "Delivered: the changes are in the project".into()),
    };
    Ok(Delivery {
        status: if delivered || partial {
            Status::Done
        } else {
            Status::Failed
        },
        outcome,
        delivered,
        partial,
        summary: summary.trim_end_matches('.').to_string(),
    })
}

/// The last stage, without a report (its run couldn't start, a task the
/// user stopped, a resume after a crash): what became of the result.
/// `existing`: its record, filled in now.
pub(super) async fn deliver(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
    existing: Option<String>,
) -> Result<(), String> {
    let first = &records[0].record;
    let id = match existing {
        Some(id) => id,
        None => {
            let record = StageRecord::running(&first.task, Stage::Deliver, round, first.effort);
            add_stage(core, dialog_id, &record).await?
        }
    };
    let d = match report_facts(core, dialog_id).await? {
        Some(mut facts) => {
            push_result(core, dialog_id, &mut facts).await;
            Delivery {
                status: facts.status,
                outcome: facts.outcome,
                delivered: facts.delivered,
                partial: facts.partial,
                summary: facts.delivery,
            }
        }
        None => delivery_of(core, dialog_id, records).await?,
    };
    end_stage(core, dialog_id, &id, |r| {
        r.status = d.status;
        r.outcome = Some(d.outcome);
        r.summary = Some(d.summary);
    })
    .await?;
    Ok(())
}

/// What the IDE asks of a server that pushes its results: the chat's
/// result, named as its task is.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliverAsk {
    pub dialog_id: String,
    /// The task: the id of the message that started it.
    pub task: String,
    /// The task's name: the branch's and the pull request's title.
    pub title: String,
    /// The pull request's description until the report is written.
    pub body: String,
}

/// What became of a push: the branch it went to, the pull request it
/// opened, and why it didn't go (no branch) or only partly (no pull
/// request).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pushed {
    pub branch: Option<String>,
    pub pr: Option<String>,
    pub problem: Option<String>,
}

pub type Delivering<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A server's delivery of its tasks' results (the daemon's `Results`),
/// set with `AgentState::set_deliverer`.
pub trait Deliverer: Send + Sync {
    /// Pushes the chat's result, and opens its pull request when asked.
    /// None: the server keeps its results for their computer.
    fn push(&self, ask: DeliverAsk) -> Delivering<Option<Pushed>>;
    /// The pull request's description, once the report is written.
    fn describe(&self, dialog_id: String, pr: String, report: String) -> Delivering<()>;
}

/// The first `# ` heading of a plan: its title.
pub fn plan_title(plan: &str) -> Option<String> {
    let line = plan.lines().map(str::trim).find(|l| l.starts_with("# "))?;
    let title = line.trim_start_matches('#').trim();
    (!title.is_empty()).then(|| shortened(title, 72))
}

/// `text` cut to `max` characters, with "…" when it was cut.
pub(super) fn shortened(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_string(),
    }
}

/// D15: on a server that pushes its results, a landable one goes to its
/// branch, and its pull request opens, before the report is written: the
/// delivery's line says where, or why it didn't go. A push that fails
/// leaves the result waiting on the server for its computer, as before,
/// and the delivery failed.
pub(in crate::agent_run) async fn push_result(
    core: &Core,
    dialog_id: &str,
    facts: &mut ReportFacts,
) {
    let Some(deliverer) = core.agents.deliverer() else {
        return;
    };
    if !facts.delivered || facts.partial {
        return;
    }
    let ask = DeliverAsk {
        dialog_id: dialog_id.to_string(),
        task: facts.task.clone(),
        title: facts.title.clone(),
        body: crate::agent_run::report::pull_request_body(facts),
    };
    let Some(pushed) = deliverer.push(ask).await else {
        return;
    };
    crate::agent_run::log::write(
        core,
        json!({
            "event": "delivery",
            "dialogId": dialog_id,
            "branch": pushed.branch,
            "pr": pushed.pr,
            "problem": pushed.problem,
        }),
    );
    match (&pushed.branch, &pushed.problem) {
        (Some(branch), problem) => {
            let mut line = format!("Delivered: pushed to `{branch}`");
            if let Some(pr) = &pushed.pr {
                line.push_str(&format!(", pull request {pr}"));
            }
            if let Some(problem) = problem {
                line.push_str(&format!("; {}", lower_first(problem.trim_end_matches('.'))));
            }
            facts.delivery = line;
        }
        (None, Some(problem)) => {
            facts.delivered = false;
            facts.status = Status::Failed;
            facts.delivery = format!(
                "Not delivered: {}; the result waits on the server for its computer",
                lower_first(problem.trim_end_matches('.'))
            );
        }
        (None, None) => {}
    }
    facts.branch = pushed.branch;
    facts.pr = pushed.pr;
}

/// One check of the task's last round, for its report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckLine {
    pub command: String,
    pub status: String,
}

/// S09 (4.11): what a task's report is written from, by the IDE's
/// records: how it ended and what became of it, the plan's assumptions
/// and criteria, the acceptance's verdicts, the tests written for it, the
/// last checks, the review, and what it spent. `meta.report`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportFacts {
    /// The task: the id of the message that started it. Kept in the report
    /// too: a server's notifier goes by it (R03).
    pub task: String,
    /// The task's name: its plan's title, else its message's first line.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub outcome: String,
    /// How it ended, in a line: "Its checks pass", or why not.
    pub ending: String,
    pub delivered: bool,
    pub partial: bool,
    /// What became of the result: the delivery's line.
    pub delivery: String,
    /// On a server that pushes its results (D15, H04): the branch it went
    /// to on the project's remote, and its pull request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub assumptions: Vec<String>,
    pub criteria: Vec<String>,
    pub verdicts: Vec<crate::agent_run::acceptance::Verdict>,
    pub tests: Vec<String>,
    pub checks: Vec<CheckLine>,
    pub review: Option<String>,
    pub spent: Spent,
    #[serde(skip)]
    pub status: Status,
}

/// S09: the current task's delivery and the facts its report is written
/// from. None: the chat has no task.
pub async fn report_facts(core: &Core, dialog_id: &str) -> Result<Option<ReportFacts>, String> {
    let Some((_, records)) = current_task(core, dialog_id).await? else {
        return Ok(None);
    };
    let d = delivery_of(core, dialog_id, &records).await?;
    let ending = match d.outcome.as_str() {
        "verified" => "Its checks pass".to_string(),
        "done" => "It's done; no checks ran".to_string(),
        other => failure_summary(other),
    };
    // T06: a task implemented by candidates has its winner's work in the
    // winner's chat: its checks, its acceptance, the tests it wrote.
    let winner = crate::agent_run::candidates::winner(core, dialog_id, &records).await?;
    let mut sources = vec![(dialog_id.to_string(), Some(records[0].seq))];
    if let Some(w) = &winner {
        sources.push((w.chat.clone(), None));
    }
    let rows = |key: &'static str| task_rows(core, sources.clone(), key);
    let plan = match task_plan_file(core, dialog_id).await? {
        Some(file) => std::fs::read_to_string(file).ok(),
        None => None,
    };
    let criteria = plan.as_deref().and_then(plan::criteria);
    let task = records[0].record.task.clone();
    let title = match plan.as_deref().and_then(plan_title) {
        Some(title) => title,
        None => {
            let id = task.clone();
            let body = core.db.read(move |c| store::get_message_body(c, &id)).await;
            let body = body.unwrap_or_default();
            body.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| shortened(l, 72))
                .unwrap_or_default()
        }
    };
    let verdicts: Vec<crate::agent_run::acceptance::Verdict> = rows("acceptance")
        .await?
        .iter()
        .rev()
        .find_map(|m| {
            let v = m
                .meta
                .as_ref()?
                .pointer("/acceptance/verdicts")?
                .as_array()?;
            Some(
                v.iter()
                    .filter_map(|v| {
                        Some(crate::agent_run::acceptance::Verdict {
                            criterion: v["criterion"].as_u64()? as usize,
                            satisfied: v["satisfied"].as_bool()?,
                            note: v["note"].as_str()?.to_string(),
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let mut tests: Vec<String> = Vec::new();
    for m in rows("tests").await? {
        let files = m
            .meta
            .as_ref()
            .and_then(|x| x.pointer("/tests/files")?.as_array().cloned())
            .unwrap_or_default();
        for f in files.iter().filter_map(Value::as_str) {
            if !tests.iter().any(|t| t == f) {
                tests.push(f.to_string());
            }
        }
    }
    let checks: Vec<CheckLine> = rows("verification")
        .await?
        .last()
        .and_then(|m| {
            m.meta
                .as_ref()?
                .pointer("/verification/checks")?
                .as_array()
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            Some(CheckLine {
                command: c["command"].as_str()?.to_string(),
                status: c["status"].as_str()?.to_string(),
            })
        })
        .collect();
    let reviewed = match &winner {
        Some(w) => w.records.as_slice(),
        None => records.as_slice(),
    };
    let review = reviewed
        .iter()
        .rev()
        .find(|r| r.record.name == Stage::Review)
        .and_then(|r| r.record.summary.clone());
    let spent = records
        .iter()
        .filter_map(|r| r.record.spent_so_far())
        .fold(Spent::default(), Spent::plus);
    Ok(Some(ReportFacts {
        task,
        title,
        outcome: d.outcome,
        ending,
        delivered: d.delivered,
        partial: d.partial,
        delivery: d.summary,
        branch: None,
        pr: None,
        assumptions: criteria
            .as_ref()
            .map(|c| c.assumptions.clone())
            .unwrap_or_default(),
        criteria: criteria.map(|c| c.items).unwrap_or_default(),
        verdicts,
        tests,
        checks,
        review,
        spent,
        status: d.status,
    }))
}

/// The rows of each chat in `sources` (from its `since`, when given) that
/// have `key` in their meta, oldest first.
pub(super) async fn task_rows(
    core: &Core,
    sources: Vec<(String, Option<i64>)>,
    key: &'static str,
) -> Result<Vec<MessageRow>, String> {
    let mut all = Vec::new();
    for (chat, since) in sources {
        let rows = core
            .db
            .read(move |c| store::messages_with_meta(c, &chat, key, since))
            .await?;
        all.extend(rows);
    }
    all.sort_by_key(|m| m.created_at);
    Ok(all)
}

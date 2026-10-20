//! The notice of a task's end (docs/design/autopilot-plan.md D18, H06):
//! when an Autopilot task ends (its delivery's record is over) or one of
//! its stages fails, and its computer named a URL for it, the daemon
//! POSTs a small JSON body there:
//! `{ chat, task, stage, outcome, summary, branch, pr }`. The records come
//! by as the events every client gets (`Tap::agent`); the branch and the
//! pull request are the task's report's (H04), seen go by before its
//! delivery's end. A call is tried again three times, a second, two, then
//! four after the one before; one that never goes through is logged, the
//! URL by its host alone (it may carry a token), and changes nothing else.
//! A call goes the way this server's requests do (`rovik_core::net`, as
//! the computers' Settings → Network say).

use crate::store::Store;
use rovik_core::agent_run::events::AgentEvent;
use rovik_protocol::autopilot::url_for_logs;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long one call may take.
const CALL_WAIT: Duration = Duration::from_secs(10);
/// The wait before the first retry; each after doubles it.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const RETRIES: u32 = 3;
/// Records told, and reports kept, at most: past that, forgotten whole.
const REMEMBERED: usize = 10_000;

/// What the URL is told.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Notice {
    pub chat: String,
    pub task: String,
    pub stage: String,
    pub outcome: String,
    pub summary: String,
    pub branch: Option<String>,
    pub pr: Option<String>,
}

/// A task report's branch and pull request, where it went.
type BranchAndPr = (Option<String>, Option<String>);

/// A chat's latest task report: its task, and its branch and pull request.
type LatestReport = (String, BranchAndPr);

pub struct Notifier {
    store: Arc<Store>,
    /// The records told already, by message and status: an event that
    /// brings one again (its spend updated, a resume) tells nobody twice.
    told: Mutex<HashSet<String>>,
    /// Each chat's latest task report, as it went by: taken when that
    /// task's delivery is over, told or not (R03), so it's never another
    /// task's, and none outlives its delivery.
    reports: Mutex<HashMap<String, LatestReport>>,
}

impl Notifier {
    pub fn new(store: Arc<Store>) -> Arc<Notifier> {
        Arc::new(Notifier {
            store,
            told: Mutex::new(HashSet::new()),
            reports: Mutex::new(HashMap::new()),
        })
    }

    /// The stage records and the report an event brings: a failed stage,
    /// or a delivery that's over, is told where its computer asked.
    pub fn agent(self: &Arc<Self>, event: &AgentEvent) {
        let (dialog_id, rows): (&str, Vec<(&str, &Value)>) = match event {
            AgentEvent::MessagesAdded {
                dialog_id,
                messages,
            } => (
                dialog_id,
                messages
                    .iter()
                    .filter_map(|m| Some((m.id.as_str(), m.meta.as_ref()?)))
                    .collect(),
            ),
            AgentEvent::MessagePatched {
                dialog_id,
                message_id,
                patch,
            } => match &patch.meta {
                Some(meta) => (dialog_id, vec![(message_id.as_str(), meta)]),
                None => return,
            },
            _ => return,
        };
        for (message_id, meta) in rows {
            if let Some(report) = meta.get("report") {
                let text = |key: &str| report.get(key).and_then(Value::as_str).map(str::to_string);
                let latest = (
                    text("task").unwrap_or_default(),
                    (text("branch"), text("pr")),
                );
                let mut reports = self.reports.lock().unwrap_or_else(|e| e.into_inner());
                if reports.len() >= REMEMBERED {
                    reports.clear();
                }
                reports.insert(dialog_id.to_string(), latest);
            }
            if let Some(record) = meta.get("stage") {
                if let Some(notice) = self.notice_of(dialog_id, message_id, record) {
                    self.send(notice);
                }
            }
        }
    }

    /// The notice a stage record calls for, once: none when it's neither
    /// failed nor a delivery that's over, or its computer named no URL.
    fn notice_of(&self, dialog_id: &str, message_id: &str, record: &Value) -> Option<Notice> {
        let field = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or("");
        let (stage, status) = (field("name"), field("status"));
        let over = stage == "deliver" && status != "running";
        if !(over || status == "failed") {
            return None;
        }
        // Its report's branch and pull request, taken told or not.
        let (branch, pr) = match over {
            true => self.report_of(dialog_id, field("task")),
            false => (None, None),
        };
        self.store.autonomy_of(dialog_id).notify_url.as_ref()?;
        let key = format!("{message_id}:{status}");
        let mut told = self.told.lock().unwrap_or_else(|e| e.into_inner());
        // Bounded: what was told long ago needn't be remembered for good.
        if told.len() >= REMEMBERED {
            told.clear();
        }
        if !told.insert(key) {
            return None;
        }
        drop(told);
        let outcome = match field("outcome") {
            "" => status,
            outcome => outcome,
        };
        Some(Notice {
            chat: dialog_id.to_string(),
            task: field("task").to_string(),
            stage: stage.to_string(),
            outcome: outcome.to_string(),
            summary: field("summary").to_string(),
            branch,
            pr,
        })
    }

    /// The branch and pull request of `task`'s report in the chat, taken:
    /// none when the chat's latest report is another task's.
    fn report_of(&self, dialog_id: &str, task: &str) -> BranchAndPr {
        let mut reports = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        match reports.remove(dialog_id) {
            Some((of, pushed)) if of == task => pushed,
            _ => (None, None),
        }
    }

    /// POSTs `notice` to its chat's computer's URL, on its own: tried again
    /// three times, never in the way of the task.
    fn send(self: &Arc<Self>, notice: Notice) {
        let Some(url) = self.store.autonomy_of(&notice.chat).notify_url else {
            return;
        };
        rovik_core::rt::spawn(async move {
            // The way this server's requests go, or not at all: a client
            // made some other way would leave the proxy out.
            let client = match rovik_core::net::builder().await.timeout(CALL_WAIT).build() {
                Ok(client) => client,
                Err(e) => {
                    log::warn!(
                        "notify: {}: no client to call it with: {e}",
                        url_for_logs(&url)
                    );
                    return;
                }
            };
            let mut wait = FIRST_RETRY;
            let mut last = String::new();
            for attempt in 0..=RETRIES {
                if attempt > 0 {
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                match client.post(&url).json(&notice).send().await {
                    Ok(reply) if reply.status().is_success() => return,
                    Ok(reply) => last = format!("it answered {}", reply.status()),
                    // Its message names the URL: never logged whole.
                    Err(e) => last = e.without_url().to_string(),
                }
            }
            log::warn!(
                "chat {}: the notice of its {} to {} didn't go: {last}",
                notice.chat,
                notice.stage,
                url_for_logs(&url)
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_protocol::events::{OutMessage, Patch};
    use serde_json::json;

    fn notifier(dir: &std::path::Path) -> Arc<Notifier> {
        Notifier::new(Arc::new(Store::open(dir).unwrap()))
    }

    /// A task's report as the chat stores it.
    fn report(id: &str, task: &str, branch: &str) -> AgentEvent {
        AgentEvent::MessagesAdded {
            dialog_id: "chat".into(),
            messages: vec![OutMessage {
                id: id.into(),
                dialog_id: "chat".into(),
                seq: 1,
                role: "assistant".into(),
                content: "The report.".into(),
                body_external: false,
                body_bytes: 0,
                tool_calls: None,
                tool_call_id: None,
                meta: Some(json!({ "report": { "task": task, "branch": branch } })),
                error: None,
                failed: false,
                finalized: true,
                attachments: Vec::new(),
                created_at: 0,
                pending: false,
            }],
        }
    }

    /// A task's delivery, over.
    fn delivered(id: &str, task: &str) -> AgentEvent {
        AgentEvent::MessagePatched {
            dialog_id: "chat".into(),
            message_id: id.into(),
            patch: Patch {
                meta: Some(json!({ "stage": {
                    "task": task, "name": "deliver", "status": "done", "outcome": "verified",
                } })),
                ..Patch::default()
            },
        }
    }

    /// R03: a delivery takes its own task's report, whether a notice goes
    /// or not (none here: no URL), so none is kept past it, and a report
    /// of an earlier task's, whose delivery never ended, isn't a later
    /// one's.
    #[test]
    fn a_delivery_takes_its_own_tasks_report_and_no_other() {
        let dir = rovik_core::testing::temp_dir();
        let notifier = notifier(&dir.0);
        notifier.agent(&report("r1", "t1", "rovik/one-t1"));
        notifier.agent(&delivered("d1", "t1"));
        assert!(notifier.reports.lock().unwrap().is_empty(), "taken");
        notifier.agent(&report("r2", "t2", "rovik/two-t2"));
        assert_eq!(notifier.report_of("chat", "t3"), (None, None));
        notifier.agent(&report("r3", "t3", "rovik/three-t3"));
        assert_eq!(
            notifier.report_of("chat", "t3"),
            (Some("rovik/three-t3".into()), None)
        );
    }
}

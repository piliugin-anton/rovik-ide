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

pub struct Notifier {
    store: Arc<Store>,
    client: reqwest::Client,
    /// The records told already, by message and status: an event that
    /// brings one again (its spend updated, a resume) tells nobody twice.
    told: Mutex<HashSet<String>>,
    /// Each chat's task report's branch and pull request, as it went by.
    pushed: Mutex<HashMap<String, (Option<String>, Option<String>)>>,
}

impl Notifier {
    pub fn new(store: Arc<Store>) -> Arc<Notifier> {
        let client = reqwest::Client::builder()
            .timeout(CALL_WAIT)
            .build()
            .unwrap_or_default();
        Arc::new(Notifier {
            store,
            client,
            told: Mutex::new(HashSet::new()),
            pushed: Mutex::new(HashMap::new()),
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
                let mut pushed = self.pushed.lock().unwrap_or_else(|e| e.into_inner());
                pushed.insert(dialog_id.to_string(), (text("branch"), text("pr")));
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
        self.store.autonomy_of(dialog_id).notify_url.as_ref()?;
        let key = format!("{message_id}:{status}");
        if !self
            .told
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key)
        {
            return None;
        }
        let (branch, pr) = match stage {
            "deliver" => {
                let mut pushed = self.pushed.lock().unwrap_or_else(|e| e.into_inner());
                pushed.remove(dialog_id).unwrap_or_default()
            }
            _ => (None, None),
        };
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

    /// POSTs `notice` to its chat's computer's URL, on its own: tried again
    /// three times, never in the way of the task.
    fn send(self: &Arc<Self>, notice: Notice) {
        let Some(url) = self.store.autonomy_of(&notice.chat).notify_url else {
            return;
        };
        let me = self.clone();
        rovik_core::rt::spawn(async move {
            let mut wait = FIRST_RETRY;
            let mut last = String::new();
            for attempt in 0..=RETRIES {
                if attempt > 0 {
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                match me.client.post(&url).json(&notice).send().await {
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

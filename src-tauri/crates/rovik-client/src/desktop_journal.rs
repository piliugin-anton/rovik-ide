//! Bounded replay of shared work. Reading never owns or cancels that work.
use crate::{desktop_backend::DesktopBackend, CloudEvent};
use rovik_core::agent_run::events::AgentEvent;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
    time::Duration,
};

/// What this UI is displaying, including chats retained from a previous project.
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    /// None selects chats without a project, not every project.
    pub project_id: Option<String>,
    /// Explicitly followed chats, such as retained running chats.
    #[serde(default)]
    pub chats: Vec<String>,
}
/// Replay position includes scope and backend incarnation to detect resets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cursor {
    epoch: String,
    sequence: u64,
    scope: Scope,
}
/// An existing frontend event with its channel preserved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// `agent` or `cloud`.
    pub channel: String,
    /// Existing event shape, unchanged.
    pub payload: Value,
}
/// A read checkpoint. Reset requires authoritative refresh before advancing it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Batch {
    /// Checkpoint to use only after all events or the refresh succeeds.
    pub cursor: Cursor,
    /// Initial attach, scope change, backend restart, or history eviction.
    pub reset: bool,
    /// Ordered events for this scope.
    pub events: Vec<Event>,
}
struct Entry {
    sequence: u64,
    dialog: Option<String>,
    event: Event,
    bytes: usize,
}
struct State {
    sequence: u64,
    floor: u64,
    bytes: usize,
    entries: VecDeque<Entry>,
}
/// Shared event history, independent of UI lifetimes and private callbacks.
pub struct Journal {
    epoch: String,
    state: Mutex<State>,
    changed: tokio::sync::watch::Sender<u64>,
    capacity: usize,
}
impl Default for Journal {
    fn default() -> Self {
        Self::new(8 << 20)
    }
}
impl Journal {
    fn new(capacity: usize) -> Self {
        Self {
            epoch: uuid::Uuid::new_v4().to_string(),
            state: Mutex::new(State {
                sequence: 0,
                floor: 0,
                bytes: 0,
                entries: VecDeque::new(),
            }),
            changed: tokio::sync::watch::channel(0).0,
            capacity,
        }
    }
    fn publish(&self, channel: &str, dialog: Option<&str>, payload: impl Serialize) {
        let payload = match serde_json::to_value(payload) {
            Ok(value) => value,
            Err(error) => {
                log::error!("Could not encode shared desktop event: {error}");
                self.invalidate();
                return;
            }
        };
        let bytes = payload.to_string().len();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sequence += 1;
        let sequence = state.sequence;
        state.entries.push_back(Entry {
            sequence,
            dialog: dialog.map(str::to_string),
            event: Event {
                channel: channel.into(),
                payload,
            },
            bytes,
        });
        state.bytes += bytes;
        while state.bytes > self.capacity || state.entries.len() > 4096 {
            if let Some(old) = state.entries.pop_front() {
                state.bytes -= old.bytes;
                state.floor = old.sequence;
            }
        }
        self.changed.send_replace(sequence);
    }
    fn invalidate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sequence += 1;
        state.floor = state.sequence;
        state.entries.clear();
        state.bytes = 0;
        self.changed.send_replace(state.sequence);
    }
    /// Records a Core event before it is handed to any optional observer.
    pub fn agent(&self, event: &AgentEvent) {
        self.publish("agent", event.dialog_id(), event);
    }
    /// Records a cloud event in the same total order as Core events.
    pub fn cloud(&self, event: &CloudEvent) {
        let dialog = match event {
            CloudEvent::Uploading { dialog_id, .. }
            | CloudEvent::Pushing { dialog_id, .. }
            | CloudEvent::Changed { dialog_id }
            | CloudEvent::Moving { dialog_id, .. }
            | CloudEvent::MovingBack { dialog_id, .. }
            | CloudEvent::Ports { dialog_id, .. } => Some(dialog_id.as_str()),
            CloudEvent::Host { .. } | CloudEvent::CaughtUp { .. } => None,
        };
        self.publish("cloud", dialog, event);
    }
    fn read(&self, scope: &Scope, cursor: Option<&Cursor>) -> (Batch, Vec<Option<String>>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let valid = cursor.filter(|c| {
            c.epoch == self.epoch
                && c.scope == *scope
                && c.sequence >= state.floor
                && c.sequence <= state.sequence
        });
        let mut batch = Batch {
            cursor: Cursor {
                epoch: self.epoch.clone(),
                sequence: state.sequence,
                scope: scope.clone(),
            },
            reset: valid.is_none(),
            events: Vec::new(),
        };
        let mut dialogs = Vec::new();
        if let Some(cursor) = valid {
            for entry in state
                .entries
                .iter()
                .filter(|e| e.sequence > cursor.sequence)
                .take(64)
            {
                batch.cursor.sequence = entry.sequence;
                batch.events.push(entry.event.clone());
                dialogs.push(entry.dialog.clone());
            }
        }
        (batch, dialogs)
    }
    /// Long-polls on a separate read connection; a failed read can safely reconnect.
    pub async fn poll(
        &self,
        backend: &DesktopBackend,
        scope: Scope,
        cursor: Option<Cursor>,
    ) -> Result<Batch, String> {
        if scope.chats.len() > 256 {
            return Err("Too many chats in desktop event scope".into());
        }
        let mut changed = self.changed.subscribe();
        let (mut batch, mut dialogs) = self.read(&scope, cursor.as_ref());
        if !batch.reset && batch.events.is_empty() {
            let _ = tokio::time::timeout(Duration::from_secs(1), changed.changed()).await;
            (batch, dialogs) = self.read(&scope, cursor.as_ref());
        }
        let ids: HashSet<String> = dialogs.iter().flatten().cloned().collect();
        let allowed = allowed(backend, scope, ids).await?;
        batch.events = batch
            .events
            .into_iter()
            .zip(dialogs)
            .filter_map(|(event, dialog)| {
                (dialog.is_none() || dialog.is_some_and(|id| allowed.contains(&id)))
                    .then_some(event)
            })
            .collect();
        Ok(batch)
    }
}

async fn allowed(
    backend: &DesktopBackend,
    scope: Scope,
    ids: HashSet<String>,
) -> Result<HashSet<String>, String> {
    backend
        .core
        .db
        .read(move |db| {
            let mut allowed: HashSet<String> = scope.chats.into_iter().collect();
            for id in ids {
                if allowed.contains(&id) {
                    continue;
                }
                let mut at = id.clone();
                for _ in 0..32 {
                    let row: Option<(Option<String>, Option<String>)> = db
                        .query_row(
                            "SELECT project_id, parent_id FROM dialogs WHERE id = ?1",
                            [&at],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()
                        .map_err(|e| e.to_string())?;
                    let Some((project, parent)) = row else {
                        break;
                    };
                    if let Some(parent) = parent {
                        at = parent;
                        continue;
                    }
                    if project == scope.project_id {
                        allowed.insert(id.clone());
                    }
                    break;
                }
            }
            Ok(allowed)
        })
        .await
}
/// Authoritative run state restricted to the same scope as replayed events.
pub async fn status(
    backend: &DesktopBackend,
    scope: Scope,
) -> Result<rovik_protocol::agent::AgentStatus, String> {
    let mut status = backend.remote.status().await.map_err(|e| e.message)?;
    let ids = status
        .runs
        .iter()
        .map(|x| x.dialog_id.clone())
        .chain(status.subagents.iter().map(|x| x.dialog_id.clone()))
        .chain(status.approvals.iter().map(|x| x.dialog_id.clone()))
        .chain(status.questions.iter().map(|x| x.dialog_id.clone()))
        .chain(status.driving.iter().cloned())
        .chain(status.steering.iter().map(|x| x.dialog_id.clone()))
        .collect();
    let allowed = allowed(backend, scope, ids).await?;
    status.runs.retain(|x| allowed.contains(&x.dialog_id));
    status.subagents.retain(|x| allowed.contains(&x.dialog_id));
    status.approvals.retain(|x| allowed.contains(&x.dialog_id));
    status.questions.retain(|x| allowed.contains(&x.dialog_id));
    status.driving.retain(|x| allowed.contains(x));
    status.steering.retain(|x| allowed.contains(&x.dialog_id));
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readers_replay_in_order_without_consuming_each_others_history() {
        let journal = Journal::default();
        let scope = Scope::default();
        let initial = journal.read(&scope, None).0;
        assert!(initial.reset);
        journal.publish("agent", Some("a"), serde_json::json!({"n":1}));
        journal.cloud(&CloudEvent::CaughtUp {
            host: "host".into(),
        });
        let (a, ids) = journal.read(&scope, Some(&initial.cursor));
        let b = journal.read(&scope, Some(&initial.cursor)).0;
        assert!(!a.reset);
        assert_eq!(a.events.len(), 2);
        assert_eq!(a.events[0].payload, b.events[0].payload);
        assert_eq!(a.events[1].channel, "cloud");
        assert_eq!(ids, vec![Some("a".into()), None]);
        assert!(journal.read(&scope, Some(&a.cursor)).0.events.is_empty());
    }
    #[test]
    fn gaps_scope_changes_and_backend_restarts_require_refresh() {
        let journal = Journal::new(20);
        let scope = Scope::default();
        let cursor = journal.read(&scope, None).0.cursor;
        journal.publish("agent", None, "a payload longer than the whole journal");
        let reset = journal.read(&scope, Some(&cursor)).0;
        assert!(reset.reset);
        assert!(reset.events.is_empty());
        assert!(!journal.read(&scope, Some(&reset.cursor)).0.reset);
        let changed = Scope {
            project_id: Some("other".into()),
            chats: vec![],
        };
        assert!(journal.read(&changed, Some(&reset.cursor)).0.reset);
        assert!(Journal::default().read(&scope, Some(&reset.cursor)).0.reset);
    }
    #[test]
    fn pagination_advances_only_over_the_returned_prefix() {
        let journal = Journal::default();
        let scope = Scope::default();
        let cursor = journal.read(&scope, None).0.cursor;
        for n in 0..100 {
            journal.publish("agent", None, n);
        }
        let first = journal.read(&scope, Some(&cursor)).0;
        let second = journal.read(&scope, Some(&first.cursor)).0;
        assert_eq!(first.events.len(), 64);
        assert_eq!(second.events.len(), 36);
        assert_eq!(second.events[0].payload, 64);
    }
}

//! Messages the user sends a run while it works (steering-plan.md). Each
//! waits in the run's queue until the run's next step reads it, as part of
//! the request it works on: stored then, at the end of the chat, as a user
//! row marked `meta.steer`. A run that ends without reading what waits
//! hands it back to the input (`SteerReturned`).
//!
//! The queue is closed, under its lock, where the run decides to end, and
//! whatever ends it: a message pushed after that is answered `Ended`, for
//! the chat's next run. So a message is read, handed back, or refused,
//! never stored with nobody reading it, nor lost.

use super::{log, UserMessage};
use crate::chat_db::store::{self, NewMessage};
use crate::core::Core;
use rovik_protocol::agent::{QueuedSteer, SteerReply};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Instant;

/// Messages that may wait at once for a run's next step.
pub const MAX_WAITING: usize = 10;

/// What the agent reads before a message (3.5): the same request, not a
/// new task.
pub const FRAME: &str = "The user sent this message while you were working. It's part of \
     the same request: take it into account from here on. Keep what's done unless it says \
     otherwise, change course where it asks, and update your checklist if the steps change.";

/// What the frame adds in CLM mode, where the task card holds the request.
pub const CLM_FRAME: &str = " Update your task card (@context/TASK.md) too.";

/// Why a run takes no messages, when nothing more is known.
const TAKES_NONE: &str = "This run takes no messages while it works.";

/// What the run is doing, for the `queued` line (D18). An approval or
/// subagents waited on are read from `AgentState` instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Doing {
    /// A model reply.
    #[default]
    Reply,
    /// The reply's tool calls.
    Calls,
    /// The end-of-run checks: the change review, the checks, the merge of
    /// the project's newer changes.
    Checks,
}

impl Doing {
    pub fn id(self) -> &'static str {
        match self {
            Doing::Reply => "reply",
            Doing::Calls => "calls",
            Doing::Checks => "checks",
        }
    }
}

/// A message waiting for the run's next step.
#[derive(Clone, Debug)]
pub struct Queued {
    pub message: UserMessage,
    /// When it was sent, in milliseconds since the epoch: its row's
    /// `meta.steer.queuedAt`.
    pub queued_at: i64,
    pub at: Instant,
    /// The run's tool calls when it was sent.
    pub calls: u32,
}

impl Queued {
    pub fn waited_ms(&self) -> u64 {
        self.at.elapsed().as_millis() as u64
    }
}

#[derive(Default)]
struct State {
    /// The run takes messages: an Agent-mode lead's (D1), once prepared.
    takes: bool,
    /// Why not, when it doesn't.
    refusal: Option<&'static str>,
    /// The run decided to end, or ended: no message is taken any more (D9).
    closed: bool,
    doing: Doing,
    /// The run's tool calls so far.
    calls: u32,
    queued: Vec<Queued>,
}

/// One run's messages (`RunControl::steer`, the run's `Run::steer`).
#[derive(Default)]
pub struct SteerQueue(Mutex<State>);

impl SteerQueue {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The run takes messages from now on.
    pub fn open(&self) {
        self.lock().takes = true;
    }

    /// The run takes none, for this reason (Plan mode, an Autopilot task).
    pub fn refuse(&self, why: &'static str) {
        let mut state = self.lock();
        state.takes = false;
        state.refusal = Some(why);
    }

    /// Whether a message sent now would wait for the run's next step.
    pub fn takes(&self) -> bool {
        let state = self.lock();
        state.takes && !state.closed
    }

    /// Whether the run takes messages at all (`RunStarted::steerable`).
    pub fn steerable(&self) -> bool {
        self.lock().takes
    }

    /// What the run does now, and its tool calls so far.
    pub fn note(&self, doing: Doing, calls: u32) {
        let mut state = self.lock();
        state.doing = doing;
        state.calls = calls;
    }

    pub fn doing(&self) -> Doing {
        self.lock().doing
    }

    /// Queues a message: `Queued`, or `Ended` once the queue is closed, or
    /// `Refused` when the run takes none or `MAX_WAITING` wait already.
    pub fn push(&self, message: UserMessage) -> SteerReply {
        let mut state = self.lock();
        if state.closed {
            return SteerReply::Ended;
        }
        if !state.takes {
            let reason = state.refusal.unwrap_or(TAKES_NONE).to_string();
            return SteerReply::Refused { reason };
        }
        if state.queued.len() >= MAX_WAITING {
            return SteerReply::Refused {
                reason: format!(
                    "{MAX_WAITING} messages already wait for the agent's next step: send this one once it has read them."
                ),
            };
        }
        hold(&message.attachments);
        let calls = state.calls;
        state.queued.push(Queued {
            message,
            queued_at: store::now_ms(),
            at: Instant::now(),
            calls,
        });
        SteerReply::Queued
    }

    /// Takes a waiting message back out: None when it was read already, or
    /// never waited here.
    pub fn withdraw(&self, message_id: &str) -> Option<Queued> {
        let mut state = self.lock();
        let at = state
            .queued
            .iter()
            .position(|q| q.message.id == message_id)?;
        let queued = state.queued.remove(at);
        release(&queued.message.attachments);
        Some(queued)
    }

    /// What's waiting, in order. The queue stays open.
    pub fn take(&self) -> Vec<Queued> {
        std::mem::take(&mut self.lock().queued)
    }

    /// What's waiting; when nothing is, the queue closes, in the same step:
    /// where the run would end, it goes on with what's waiting or ends with
    /// nothing left behind.
    pub fn take_or_close(&self) -> Vec<Queued> {
        let mut state = self.lock();
        if state.queued.is_empty() {
            state.closed = true;
        }
        std::mem::take(&mut state.queued)
    }

    /// Closes the queue and returns what's left in it.
    pub fn close_and_take(&self) -> Vec<Queued> {
        let mut state = self.lock();
        state.closed = true;
        std::mem::take(&mut state.queued)
    }

    /// Copies of what's waiting, oldest first.
    pub fn waiting(&self) -> Vec<Queued> {
        self.lock().queued.clone()
    }

    /// The waiting messages as `agent_status` lists them.
    pub fn status(&self, dialog_id: &str) -> Vec<QueuedSteer> {
        self.lock()
            .queued
            .iter()
            .map(|q| QueuedSteer {
                dialog_id: dialog_id.to_string(),
                message: q.message.clone(),
                queued_at: q.queued_at,
            })
            .collect()
    }
}

/*
 * ---------------------------------------------------------------------------
 * Files held while their message waits
 * ---------------------------------------------------------------------------
 */

/// The attachments of messages that wait, by how many name each: the
/// cleanup keeps them (`attachments::collect_garbage`), as a message can
/// wait behind an approval for longer than its grace period.
static HELD: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

fn held_files() -> MutexGuard<'static, HashMap<String, usize>> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

fn hold(ids: &[String]) {
    let mut held = held_files();
    for id in ids {
        *held.entry(id.clone()).or_default() += 1;
    }
}

/// Lets a message's files go: stored, its row names them; handed back,
/// the input does.
pub fn release(ids: &[String]) {
    let mut held = held_files();
    for id in ids {
        if let Some(n) = held.get_mut(id) {
            *n -= 1;
            if *n == 0 {
                held.remove(id);
            }
        }
    }
}

/// Whether a waiting message names the attachment.
pub fn is_held(id: &str) -> bool {
    held_files().contains_key(id)
}

/*
 * ---------------------------------------------------------------------------
 * What's stored, and the log
 * ---------------------------------------------------------------------------
 */

/// The row a read message is stored as (3.5): what the user typed, their
/// attachments, and in its meta what the agent reads, the frame and then
/// what a send would have sent (`meta.sentContent`, which every later
/// request sends the same way), and when it was sent. No `mode`: it's no
/// request of its own.
pub fn stored_row(queued: &Queued, clm: bool) -> NewMessage {
    let mut row = super::stored_message(&queued.message);
    let mut meta = match row.meta.take() {
        Some(serde_json::Value::Object(meta)) => meta,
        _ => serde_json::Map::new(),
    };
    meta.remove("mode");
    let wire = super::wire_content(&queued.message);
    let clm = if clm { CLM_FRAME } else { "" };
    meta.insert(
        "sentContent".into(),
        json!(format!("[{FRAME}{clm}]\n\n{wire}")),
    );
    meta.insert("steer".into(), json!({ "queuedAt": queued.queued_at }));
    row.meta = Some(serde_json::Value::Object(meta));
    row.created_at = store::now_ms();
    row
}

/// A message's line in the agent log (D18).
pub fn log_line(
    core: &Core,
    dialog_id: &str,
    message_id: &str,
    phase: &str,
    fields: serde_json::Value,
) {
    let mut entry = json!({
        "event": "steer",
        "dialogId": dialog_id,
        "messageId": message_id,
        "phase": phase,
    });
    if let (Some(entry), serde_json::Value::Object(fields)) = (entry.as_object_mut(), fields) {
        entry.extend(fields);
    }
    log::write(core, entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str) -> UserMessage {
        UserMessage {
            id: id.into(),
            content: format!("message {id}"),
            meta: None,
            attachments: Vec::new(),
        }
    }

    fn ids(queued: &[Queued]) -> Vec<String> {
        queued.iter().map(|q| q.message.id.clone()).collect()
    }

    #[test]
    fn a_run_that_takes_none_refuses_with_its_reason() {
        let queue = SteerQueue::default();
        assert!(matches!(
            queue.push(message("a")),
            SteerReply::Refused { .. }
        ));
        queue.refuse("Plan mode takes no messages while it works.");
        assert_eq!(
            queue.push(message("a")),
            SteerReply::Refused {
                reason: "Plan mode takes no messages while it works.".into()
            }
        );
        assert!(queue.waiting().is_empty());
    }

    #[test]
    fn messages_wait_in_order_until_taken() {
        let queue = SteerQueue::default();
        queue.open();
        assert!(queue.takes());
        assert_eq!(queue.push(message("a")), SteerReply::Queued);
        assert_eq!(queue.push(message("b")), SteerReply::Queued);
        assert_eq!(ids(&queue.waiting()), ["a", "b"]);
        assert_eq!(ids(&queue.take()), ["a", "b"]);
        assert!(queue.take().is_empty());
        // Taken, the queue stays open.
        assert_eq!(queue.push(message("c")), SteerReply::Queued);
    }

    #[test]
    fn a_message_taken_back_is_gone_once() {
        let queue = SteerQueue::default();
        queue.open();
        queue.push(message("a"));
        queue.push(message("b"));
        assert_eq!(queue.withdraw("a").map(|q| q.message.id), Some("a".into()));
        assert!(queue.withdraw("a").is_none(), "taken back already");
        assert_eq!(ids(&queue.take()), ["b"]);
        assert!(queue.withdraw("b").is_none(), "read already");
        assert!(queue.withdraw("never").is_none());
    }

    #[test]
    fn the_end_takes_whats_waiting_or_closes() {
        let queue = SteerQueue::default();
        queue.open();
        queue.push(message("a"));
        // Something waits: the run goes on with it, and still takes more.
        assert_eq!(ids(&queue.take_or_close()), ["a"]);
        assert_eq!(queue.push(message("b")), SteerReply::Queued);
        assert_eq!(ids(&queue.take_or_close()), ["b"]);
        // Nothing waits: closed, in the same step.
        assert!(queue.take_or_close().is_empty());
        assert!(!queue.takes());
        assert_eq!(queue.push(message("c")), SteerReply::Ended);
        assert!(queue.steerable(), "it did take messages");
    }

    #[test]
    fn the_runs_end_closes_and_hands_back_whats_left() {
        let queue = SteerQueue::default();
        queue.open();
        queue.push(message("a"));
        queue.push(message("b"));
        assert_eq!(ids(&queue.close_and_take()), ["a", "b"]);
        assert_eq!(queue.push(message("c")), SteerReply::Ended);
        assert!(queue.close_and_take().is_empty());
    }

    #[test]
    fn at_most_ten_wait() {
        let queue = SteerQueue::default();
        queue.open();
        for i in 0..MAX_WAITING {
            assert_eq!(queue.push(message(&i.to_string())), SteerReply::Queued);
        }
        assert!(matches!(
            queue.push(message("x")),
            SteerReply::Refused { .. }
        ));
        queue.take();
        assert_eq!(queue.push(message("x")), SteerReply::Queued);
    }

    #[test]
    fn a_waiting_messages_files_are_held_until_its_end() {
        let queue = SteerQueue::default();
        queue.open();
        let file = format!("held-{}", uuid::Uuid::new_v4());
        let mut a = message("a");
        a.attachments = vec![file.clone()];
        queue.push(a.clone());
        let mut b = a.clone();
        b.id = "b".into();
        queue.push(b);
        assert!(is_held(&file));
        queue.withdraw("a");
        assert!(is_held(&file), "the other still names it");
        for q in queue.take() {
            release(&q.message.attachments);
        }
        assert!(!is_held(&file));
    }

    #[test]
    fn the_stored_row_frames_what_a_send_would_send() {
        let queued = Queued {
            message: UserMessage {
                id: "s".into(),
                content: "use the client".into(),
                meta: Some(
                    json!({ "mode": "agent", "sentContent": "[ctx]\n\nuse the client", "pdfs": [] }),
                ),
                attachments: vec!["img".into()],
            },
            queued_at: 5,
            at: Instant::now(),
            calls: 0,
        };
        let row = stored_row(&queued, false);
        assert_eq!(row.role, "user");
        assert_eq!(row.content, "use the client");
        assert_eq!(row.attachments, ["img"]);
        let meta = row.meta.unwrap();
        assert_eq!(meta["steer"]["queuedAt"], 5);
        assert!(meta.get("mode").is_none(), "{meta}");
        assert_eq!(meta["pdfs"], json!([]));
        let sent = meta["sentContent"].as_str().unwrap();
        assert!(
            sent.starts_with("[The user sent this message while you were working."),
            "{sent}"
        );
        assert!(sent.ends_with("]\n\n[ctx]\n\nuse the client"), "{sent}");
        assert!(!sent.contains("TASK.md"));
        let clm = stored_row(&queued, true);
        let sent = clm.meta.unwrap()["sentContent"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            sent.contains("Update your task card (@context/TASK.md) too.]"),
            "{sent}"
        );
    }
}

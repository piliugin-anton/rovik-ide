//! The daemon's events (4.3): numbered once per process, kept in a ring for
//! the clients that come back, and sent to the connections of the computer
//! whose chat they're about (D16). A subagent's chat goes with its lead's.
//!
//! - A reply's patches are paced: at most four a second per message, the
//!   ones in between merged, and one held back goes before its chat's next
//!   event (K28).
//! - An event over a frame's 1 MB goes as `Resync` for its chat, and
//!   `FileWritten`, which carries whole files the app would drop anyway,
//!   doesn't go at all (4.1).
//! - After a chat's events, and after a request that changed it, its
//!   computer gets `ChatChanged` with the database's clock, at most twice a
//!   second, and its mirror pulls the rows (6.3).
//! - The ring keeps the last 2,000 events or 8 MB, one patch per message
//!   and one `ChatChanged` per chat: a client back with a `since` it still
//!   covers gets what it missed, any other catches up from the database
//!   (`complete: false`).

use rovik_core::agent_run::events::{AgentEvent, Patch};
use rovik_core::chat_db::{sync, DbState};
use rovik_core::EventSink;
use rovik_protocol::{Event, Frame, MAX_FRAME};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::Instant;

/// How many events a connection may fall behind by before it's dropped
/// (it subscribes again, from its `since`).
const LIVE: usize = 1_024;

/// A chat's `ChatChanged` goes at most this often (6.3).
const CHANGED_EVERY: Duration = Duration::from_millis(500);

/// The computer a chat belongs to: the one that started it here (the
/// daemon's store keeps it).
pub type Owner = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// An event as it went out.
pub struct Published {
    pub seq: u64,
    /// The computer whose chat it's about; none: every computer's.
    pub computer: Option<String>,
    /// The `Frame::Event`, as sent.
    pub text: Arc<str>,
}

impl Published {
    pub fn reaches(&self, computer: &str) -> bool {
        self.computer.as_deref().is_none_or(|c| c == computer)
    }
}

/// What a client gets when it subscribes.
pub struct Subscription {
    /// The events after its `since` that are its computer's.
    pub replay: Vec<Arc<Published>>,
    /// The ring still had everything after `since`.
    pub complete: bool,
    /// Every computer's events from now on: the reader takes its own.
    pub live: broadcast::Receiver<Arc<Published>>,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub events: usize,
    pub bytes: usize,
    /// A message's patches go out at most this often.
    pub patch_every: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            events: 2_000,
            bytes: 8 << 20,
            patch_every: Duration::from_millis(250),
        }
    }
}

pub struct Events {
    /// New with each daemon process: a `since` from another boot is stale.
    pub boot: String,
    owner: Owner,
    me: Weak<Events>,
    live: broadcast::Sender<Arc<Published>>,
    limits: Limits,
    state: Mutex<State>,
    /// The chats' database, whose clock `ChatChanged` names: set once the
    /// core is open.
    db: OnceLock<DbState>,
}

#[derive(Default)]
struct State {
    seq: u64,
    ring: Ring,
    /// Subagents' chats, to their lead's.
    leads: HashMap<String, String>,
    /// Each message being patched, by id.
    paces: HashMap<String, Pace>,
    /// Each chat's `ChatChanged`, by its lead's id.
    changes: HashMap<String, Changed>,
}

/// When a chat's last `ChatChanged` went, and whether one is coming.
#[derive(Default)]
struct Changed {
    sent: Option<Instant>,
    coming: bool,
}

/// Who an event goes to: the computer of the chat it's about (its lead's,
/// for a subagent's), or every computer.
#[derive(Clone)]
struct Route {
    chat: Option<String>,
    computer: Option<String>,
}

struct Pace {
    /// When its last patch went out.
    sent: Instant,
    /// The patches since, merged, waiting for their time.
    held: Option<HeldPatch>,
}

struct HeldPatch {
    route: Route,
    dialog_id: String,
    patch: Patch,
}

impl Events {
    pub fn new(boot: String, owner: Owner) -> Arc<Events> {
        Events::with_limits(boot, owner, Limits::default())
    }

    pub fn with_limits(boot: String, owner: Owner, limits: Limits) -> Arc<Events> {
        Arc::new_cyclic(|me| Events {
            boot,
            owner,
            me: me.clone(),
            live: broadcast::channel(LIVE).0,
            limits,
            state: Mutex::default(),
            db: OnceLock::new(),
        })
    }

    /// The database whose clock `ChatChanged` names, from now on.
    pub fn set_db(&self, db: DbState) {
        let _ = self.db.set(db);
    }

    /// Rows of the chat may have changed: its computer hears of it (6.3).
    pub fn changed(&self, dialog_id: &str) {
        let mut state = self.lock();
        if let Some(chat) = self.route(&state, dialog_id).and_then(|r| r.chat) {
            self.change(&mut state, &chat);
        }
    }

    /// `ChatChanged` for the lead's chat `chat`, at most twice a second:
    /// one is coming already, or goes once the last is old enough. It names
    /// the clock as it is when it goes, so every change before is in it.
    fn change(&self, state: &mut State, chat: &str) {
        if self.db.get().is_none() {
            return;
        }
        let entry = state.changes.entry(chat.to_string()).or_default();
        if entry.coming {
            return;
        }
        entry.coming = true;
        let due = entry
            .sent
            .map_or_else(Instant::now, |sent| sent + CHANGED_EVERY);
        let (me, chat) = (self.me.clone(), chat.to_string());
        rovik_core::rt::handle().spawn(async move {
            tokio::time::sleep_until(due).await;
            let Some(events) = me.upgrade() else { return };
            {
                // Done before the clock is read: a change after it is in
                // the next one.
                let mut state = events.lock();
                if let Some(entry) = state.changes.get_mut(&chat) {
                    entry.coming = false;
                    entry.sent = Some(Instant::now());
                }
            }
            let Some(db) = events.db.get().cloned() else {
                return;
            };
            let rev = match db.read(sync::clock).await {
                Ok(rev) => rev,
                Err(e) => return log::warn!("the clock for {chat}'s changes: {e}"),
            };
            let mut state = events.lock();
            if let Some(route) = events.route(&state, &chat) {
                events.flush_chat(&mut state, &chat);
                let ev = Event::ChatChanged {
                    dialog_id: chat.clone(),
                    rev,
                };
                events.publish(&mut state, ev, route, None);
            }
        });
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The events after `since` that the ring still has, and the live ones.
    pub fn subscribe(&self, computer: &str, since: Option<(String, u64)>) -> Subscription {
        // Under the lock: nothing is published between the replay and `live`.
        let state = self.lock();
        let live = self.live.subscribe();
        match since {
            Some((boot, seq)) if boot == self.boot && state.ring.evicted <= seq => Subscription {
                replay: state.ring.after(seq, computer),
                complete: true,
                live,
            },
            _ => Subscription {
                replay: Vec::new(),
                complete: false,
                live,
            },
        }
    }

    /// Paces a message's patches: one goes now if its last went long
    /// enough ago; otherwise it waits, merged with any already waiting.
    fn pace(&self, state: &mut State, route: Route, dialog_id: &str, message: &str, patch: Patch) {
        let now = Instant::now();
        let every = self.limits.patch_every;
        let pace = state.paces.entry(message.to_string()).or_insert(Pace {
            sent: now - every,
            held: None,
        });
        if pace.held.is_none() && now.duration_since(pace.sent) >= every {
            pace.sent = now;
            return self.publish_patch(state, route, dialog_id, message, patch);
        }
        let due = pace.sent + every;
        pace.held = Some(match pace.held.take() {
            Some(held) => HeldPatch {
                patch: held.patch.then(patch),
                ..held
            },
            None => {
                let (me, message) = (self.me.clone(), message.to_string());
                rovik_core::rt::handle().spawn(async move {
                    tokio::time::sleep_until(due).await;
                    if let Some(events) = me.upgrade() {
                        let mut state = events.lock();
                        events.flush(&mut state, &message);
                    }
                });
                HeldPatch {
                    route,
                    dialog_id: dialog_id.to_string(),
                    patch,
                }
            }
        });
    }

    /// Sends the patch a message holds back, if it holds one.
    fn flush(&self, state: &mut State, message: &str) {
        let Some(pace) = state.paces.get_mut(message) else {
            return;
        };
        if let Some(held) = pace.held.take() {
            pace.sent = Instant::now();
            self.publish_patch(state, held.route, &held.dialog_id, message, held.patch);
        }
    }

    /// Sends what a chat's messages hold back, ahead of its next event.
    fn flush_chat(&self, state: &mut State, chat: &str) {
        let waiting: Vec<String> = state
            .paces
            .iter()
            .filter(|(_, p)| {
                p.held
                    .as_ref()
                    .is_some_and(|h| h.route.chat.as_deref() == Some(chat))
            })
            .map(|(message, _)| message.clone())
            .collect();
        for message in waiting {
            self.flush(state, &message);
        }
    }

    fn publish_patch(
        &self,
        state: &mut State,
        route: Route,
        dialog_id: &str,
        message: &str,
        patch: Patch,
    ) {
        // A finished message gets no more patches.
        if patch.finalized == Some(true) {
            state.paces.remove(message);
        }
        let kept = RingPatch {
            message: message.to_string(),
            dialog_id: dialog_id.to_string(),
            patch: patch.clone(),
        };
        let event = AgentEvent::MessagePatched {
            dialog_id: dialog_id.to_string(),
            message_id: message.to_string(),
            patch,
        };
        self.publish(state, Event::Agent(event), route, Some(kept));
    }

    /// A chat's event that isn't a run's, such as its result (6.5): to the
    /// chat's computer, after the run's events before it.
    pub fn chat(&self, dialog_id: &str, ev: Event) {
        let mut state = self.lock();
        let Some(route) = self.route(&state, dialog_id) else {
            return;
        };
        if let Some(chat) = &route.chat {
            self.flush_chat(&mut state, chat);
        }
        self.publish(&mut state, ev, route, None);
    }

    /// Where a chat's events go; none when no computer started it here.
    fn route(&self, state: &State, dialog_id: &str) -> Option<Route> {
        let mut chat = dialog_id;
        while let Some(lead) = state.leads.get(chat) {
            chat = lead;
        }
        let computer = (self.owner)(chat)?;
        Some(Route {
            chat: Some(chat.to_string()),
            computer: Some(computer),
        })
    }

    fn publish(&self, state: &mut State, ev: Event, route: Route, patch: Option<RingPatch>) {
        state.seq += 1;
        let seq = state.seq;
        let changed = match &ev {
            Event::ChatChanged { dialog_id, .. } => Some(dialog_id.clone()),
            _ => None,
        };
        let Some(text) = frame_text(seq, ev, route.chat.as_deref()) else {
            log::warn!("an event over 1 MB about no chat wasn't sent");
            return;
        };
        let published = Arc::new(Published {
            seq,
            computer: route.computer.clone(),
            text,
        });
        state
            .ring
            .push(published.clone(), patch, changed, &route, &self.limits);
        let _ = self.live.send(published);
    }
}

impl EventSink for Events {
    fn agent(&self, event: &AgentEvent) {
        if matches!(event, AgentEvent::FileWritten { .. }) {
            return;
        }
        let mut state = self.lock();
        if let AgentEvent::SubagentStarted {
            dialog_id,
            child_dialog_id,
            ..
        } = event
        {
            state
                .leads
                .insert(child_dialog_id.clone(), dialog_id.clone());
        }
        let route = match event.dialog_id() {
            Some(dialog_id) => match self.route(&state, dialog_id) {
                Some(route) => route,
                None => return,
            },
            None => Route {
                chat: None,
                computer: None,
            },
        };
        let chat = route.chat.clone();
        match event {
            AgentEvent::MessagePatched {
                dialog_id,
                message_id,
                patch,
            } => self.pace(&mut state, route, dialog_id, message_id, patch.clone()),
            _ => {
                if let Some(chat) = &route.chat {
                    self.flush_chat(&mut state, chat);
                }
                self.publish(&mut state, Event::Agent(event.clone()), route, None);
            }
        }
        if let Some(chat) = chat {
            self.change(&mut state, &chat);
        }
    }
}

/// A `Frame::Event`, or a `Resync` for its chat in place of one too big
/// for a frame; none for one too big about no chat.
fn frame_text(seq: u64, ev: Event, chat: Option<&str>) -> Option<Arc<str>> {
    let text = serde_json::to_string(&Frame::Event { seq, ev }).ok()?;
    if text.len() <= MAX_FRAME {
        return Some(text.into());
    }
    let resync = Event::Resync {
        dialog_id: chat?.to_string(),
    };
    serde_json::to_string(&Frame::Event { seq, ev: resync })
        .ok()
        .map(Into::into)
}

/// The events kept for clients that come back.
#[derive(Default)]
struct Ring {
    entries: VecDeque<Entry>,
    bytes: usize,
    /// The newest seq let go to stay within the limits.
    evicted: u64,
}

struct Entry {
    published: Arc<Published>,
    /// A patch, kept to merge with its message's next one.
    patch: Option<RingPatch>,
    /// A `ChatChanged`'s chat: its next one replaces it.
    changed: Option<String>,
}

struct RingPatch {
    message: String,
    dialog_id: String,
    patch: Patch,
}

impl Ring {
    fn push(
        &mut self,
        mut published: Arc<Published>,
        mut patch: Option<RingPatch>,
        changed: Option<String>,
        route: &Route,
        limits: &Limits,
    ) {
        // A chat's newer `ChatChanged` says all its older one did.
        if let Some(chat) = &changed {
            let older = self
                .entries
                .iter()
                .position(|e| e.changed.as_ref() == Some(chat));
            if let Some(older) = older.and_then(|i| self.entries.remove(i)) {
                self.bytes -= older.published.text.len();
            }
        }
        // A message's patches are one entry: the merge of them all, under
        // the newest's seq. They change fields of a row that is already
        // there, so moving them later changes nothing else.
        if let Some(newer) = patch.as_mut() {
            let older = self
                .entries
                .iter()
                .position(|e| e.patch.as_ref().is_some_and(|p| p.message == newer.message));
            if let Some(i) = older.and_then(|i| self.entries.remove(i)) {
                self.bytes -= i.published.text.len();
                if let Some(older) = i.patch {
                    newer.patch = older.patch.then(newer.patch.clone());
                    let merged = AgentEvent::MessagePatched {
                        dialog_id: newer.dialog_id.clone(),
                        message_id: newer.message.clone(),
                        patch: newer.patch.clone(),
                    };
                    if let Some(text) =
                        frame_text(published.seq, Event::Agent(merged), route.chat.as_deref())
                    {
                        published = Arc::new(Published {
                            seq: published.seq,
                            computer: published.computer.clone(),
                            text,
                        });
                    }
                }
            }
        }
        self.bytes += published.text.len();
        self.entries.push_back(Entry {
            published,
            patch,
            changed,
        });
        while self.entries.len() > limits.events || self.bytes > limits.bytes {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= oldest.published.text.len();
            self.evicted = self.evicted.max(oldest.published.seq);
        }
    }

    fn after(&self, seq: u64, computer: &str) -> Vec<Arc<Published>> {
        self.entries
            .iter()
            .map(|e| &e.published)
            .filter(|p| p.seq > seq && p.reaches(computer))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests;

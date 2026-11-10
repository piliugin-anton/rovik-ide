//! Client-owned resources and bounded, private event streams. The backend
//! survives a window; a window's subscriptions and terminals do not leak into
//! another client's session.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::{mpsc, watch};

const EVENT_COUNT: usize = 128;
const EVENT_BYTES: usize = 4 << 20;
type Cleanup = Box<dyn FnOnce() + Send>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// An event for one callback registered by one window.
#[derive(Debug, Serialize, Deserialize)]
pub struct DesktopEvent {
    /// Opaque callback identity, chosen before starting its operation.
    pub sink: String,
    /// A payload, or the end of this callback's lifetime.
    pub body: EventBody,
}

/// The callback's data uses the original command's serialized event shape.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum EventBody {
    /// One event, in source order.
    Data(Value),
    /// No more events will be sent to this callback.
    End,
}

struct Queued {
    event: DesktopEvent,
    bytes: usize,
}

struct Resource {
    sink: String,
    cleanup: Cleanup,
}

/// A client's registered resources and event queue, owned by its RPC lease.
pub struct Session {
    id: String,
    pub(crate) chat_requests: Arc<rovik_core::chat_request::ChatRequests>,
    pub(crate) filesystem: Arc<crate::filesystem::WatcherState>,
    pub(crate) git_history: Arc<std::sync::atomic::AtomicU64>,
    project_cleanup: Mutex<Option<Cleanup>>,
    closed: AtomicBool,
    failure: watch::Sender<Option<String>>,
    queued_bytes: Mutex<usize>,
    sender: mpsc::Sender<Queued>,
    receiver: Mutex<Option<mpsc::Receiver<Queued>>>,
    resources: Mutex<HashMap<String, Resource>>,
    streams: Mutex<HashSet<String>>,
}

impl Session {
    fn new(id: String) -> Self {
        let (sender, receiver) = mpsc::channel(EVENT_COUNT);
        let (failure, _) = watch::channel(None);
        Self {
            id,
            filesystem: Default::default(),
            chat_requests: Default::default(),
            git_history: Default::default(),
            project_cleanup: Default::default(),
            closed: AtomicBool::new(false),
            failure,
            queued_bytes: Mutex::new(0),
            sender,
            receiver: Mutex::new(Some(receiver)),
            resources: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashSet::new()),
        }
    }

    /// The session identity advertised during the authenticated handshake.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the client must reconnect before doing more work.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Completes when the client or its event connection fails.
    pub async fn closed(&self) {
        let mut failure = self.failure.subscribe();
        while failure.borrow().is_none() {
            if failure.changed().await.is_err() {
                return;
            }
        }
    }

    fn fail(&self, reason: &str) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.failure.send_replace(Some(reason.to_string()));
        }
    }

    /// Stops only this client's resources. Safe to call more than once.
    pub fn close(&self, reason: &str) {
        self.fail(reason);
        if let Some(cleanup) = lock(&self.project_cleanup).take() {
            cleanup();
        }
        lock(&self.streams).clear();
        let resources = std::mem::take(&mut *lock(&self.resources));
        for (_, resource) in resources {
            (resource.cleanup)();
        }
    }

    /// Changes this client's open folder atomically with disconnect cleanup.
    pub(crate) fn set_project(
        &self,
        remote: Arc<crate::Remote>,
        project: Option<(String, std::path::PathBuf)>,
    ) -> Result<(), String> {
        let mut cleanup = lock(&self.project_cleanup);
        if self.is_closed() {
            return Err("Desktop session ended; reopen the project after reconnecting".into());
        }
        remote.set_client_project(self.id(), project);
        let id = self.id.clone();
        *cleanup = Some(Box::new(move || remote.set_client_project(&id, None)));
        Ok(())
    }

    /// Reserves a callback before starting its producer, including early output.
    pub fn begin(&self, sink: &str) -> Result<(), String> {
        let mut streams = lock(&self.streams);
        if self.is_closed()
            || sink.is_empty()
            || sink.len() > 128
            || streams.len() >= EVENT_COUNT
            || !streams.insert(sink.to_string())
        {
            return Err("Desktop event callback is unavailable".into());
        }
        Ok(())
    }

    /// Stops the resources feeding a callback whose frontend has gone away.
    pub fn cancel_sink(&self, sink: &str) {
        self.end(sink);
        // Remove the matching registrations while holding the lock: a new
        // watch may reuse a key before cleanup of the old callback completes.
        let removed: Vec<_> = {
            let mut resources = lock(&self.resources);
            let keys: Vec<_> = resources
                .iter()
                .filter(|(_, r)| r.sink == sink)
                .map(|(key, _)| key.clone())
                .collect();
            keys.into_iter()
                .filter_map(|key| resources.remove(&key))
                .collect()
        };
        for resource in removed {
            (resource.cleanup)();
        }
    }

    /// Registers an owned resource. Failure also cleans up the new resource.
    pub fn register(
        &self,
        key: String,
        sink: String,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        let streams = lock(&self.streams);
        let mut resources = lock(&self.resources);
        if self.is_closed() || !streams.contains(&sink) || resources.contains_key(&key) {
            drop(resources);
            drop(streams);
            cleanup();
            return Err("The desktop session ended or already owns this resource".into());
        }
        resources.insert(
            key,
            Resource {
                sink,
                cleanup: Box::new(cleanup),
            },
        );
        Ok(())
    }

    /// Refuses controls for resources belonging to another client.
    pub fn owns(&self, key: &str) -> bool {
        !self.is_closed() && lock(&self.resources).contains_key(key)
    }

    /// Closes one owned resource; an unknown or another client's ID is a no-op.
    pub fn remove(&self, key: &str) -> bool {
        let resource = lock(&self.resources).remove(key);
        if let Some(resource) = resource {
            (resource.cleanup)();
            self.end(&resource.sink);
            true
        } else {
            false
        }
    }

    /// Releases a resource that exited by itself, without stopping its children.
    pub fn finished(&self, key: &str) {
        let resource = lock(&self.resources).remove(key);
        if let Some(resource) = resource {
            self.end(&resource.sink);
        }
    }

    /// Serializes an event. A slow or disconnected consumer never blocks a producer.
    pub fn emit(&self, sink: &str, payload: impl Serialize) -> bool {
        let streams = lock(&self.streams);
        if !streams.contains(sink) {
            return false;
        }
        match serde_json::to_value(payload) {
            Ok(value) => self.enqueue(DesktopEvent {
                sink: sink.into(),
                body: EventBody::Data(value),
            }),
            Err(_) => {
                self.fail("Could not encode a desktop event");
                false
            }
        }
    }

    /// Releases the window's callback after its final payload.
    pub fn end(&self, sink: &str) -> bool {
        let mut streams = lock(&self.streams);
        if !streams.remove(sink) {
            return false;
        }
        self.enqueue(DesktopEvent {
            sink: sink.into(),
            body: EventBody::End,
        })
    }

    fn enqueue(&self, event: DesktopEvent) -> bool {
        if self.is_closed() {
            return false;
        }
        let bytes = match serde_json::to_vec(&event) {
            Ok(bytes) => bytes.len(),
            Err(_) => {
                self.fail("Could not encode a desktop event");
                return false;
            }
        };
        if !{
            let mut used = lock(&self.queued_bytes);
            match used.checked_add(bytes).filter(|sum| *sum <= EVENT_BYTES) {
                Some(next) => {
                    *used = next;
                    true
                }
                None => false,
            }
        } {
            self.fail("Desktop events exceeded their buffer; reconnect before continuing");
            return false;
        }
        if self.sender.try_send(Queued { event, bytes }).is_err() {
            *lock(&self.queued_bytes) -= bytes;
            self.fail("Desktop event consumer stopped keeping up; reconnect before continuing");
            return false;
        }
        true
    }

    /// Attaches the single event consumer. Events emitted before attachment are kept.
    pub fn events(self: &Arc<Self>) -> Result<SessionEvents, String> {
        if self.is_closed() {
            return Err("Desktop session has ended".into());
        }
        let receiver = lock(&self.receiver)
            .take()
            .ok_or("Desktop events already have a consumer")?;
        Ok(SessionEvents {
            session: self.clone(),
            receiver,
            failure: self.failure.subscribe(),
        })
    }
}

/// One ordered consumer of the session's bounded event queue.
pub struct SessionEvents {
    session: Arc<Session>,
    receiver: mpsc::Receiver<Queued>,
    failure: watch::Receiver<Option<String>>,
}

impl SessionEvents {
    pub(crate) fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// Receives a payload or an explicit stream failure; events are never silently dropped.
    pub async fn next(&mut self) -> Result<DesktopEvent, String> {
        if let Some(reason) = self.failure.borrow().clone() {
            return Err(reason);
        }
        tokio::select! {
            biased;
            _ = self.failure.changed() => Err(self.failure.borrow().clone().unwrap_or_else(|| "Desktop session ended".into())),
            event = self.receiver.recv() => {
                let event = event.ok_or("Desktop event stream ended")?;
                *lock(&self.session.queued_bytes) -= event.bytes;
                Ok(event.event)
            }
        }
    }
}

impl Drop for SessionEvents {
    fn drop(&mut self) {
        self.session.close("Desktop event consumer disconnected");
    }
}

/// Active same-user client sessions; the registry does not keep disconnected clients alive.
#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, Weak<Session>>>>);

impl Sessions {
    /// Registers the identity from an authenticated connection's handshake.
    pub fn register(&self, id: String) -> Result<SessionLease, String> {
        let mut sessions = lock(&self.0);
        if sessions.get(&id).and_then(Weak::upgrade).is_some() {
            return Err("Desktop session already exists".into());
        }
        let session = Arc::new(Session::new(id.clone()));
        sessions.insert(id, Arc::downgrade(&session));
        Ok(SessionLease {
            registry: self.clone(),
            session,
        })
    }

    /// Creates an embedded window's session using the same ownership rules.
    pub fn create(&self) -> SessionLease {
        loop {
            if let Ok(lease) = self.register(uuid::Uuid::new_v4().to_string()) {
                return lease;
            }
        }
    }

    /// Finds a live session for its private event connection.
    pub fn get(&self, id: &str) -> Result<Arc<Session>, String> {
        lock(&self.0)
            .get(id)
            .and_then(Weak::upgrade)
            .filter(|s| !s.is_closed())
            .ok_or_else(|| "Desktop session no longer exists".into())
    }

    /// Closes all client resources before backend shutdown.
    pub fn close_all(&self) {
        let sessions: Vec<_> = lock(&self.0).values().filter_map(Weak::upgrade).collect();
        for session in sessions {
            session.close("Desktop backend is shutting down");
        }
    }
}

/// Dropping a connection lease closes only the resources owned by that client.
pub struct SessionLease {
    registry: Sessions,
    /// The session available to this client's handlers.
    pub session: Arc<Session>,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        lock(&self.registry.0).remove(self.session.id());
        self.session.close("Desktop client disconnected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn history_searches_from_another_window_do_not_supersede_this_window() {
        let a = Session::new("a".into());
        let b = Session::new("b".into());
        let first = a.git_history.fetch_add(1, Ordering::SeqCst) + 1;
        b.git_history.fetch_add(1, Ordering::SeqCst);
        b.git_history.fetch_add(1, Ordering::SeqCst);
        assert_eq!(a.git_history.load(Ordering::SeqCst), first);
        a.git_history.fetch_add(1, Ordering::SeqCst);
        assert_ne!(a.git_history.load(Ordering::SeqCst), first);
    }

    #[test]
    fn disconnecting_one_client_cleans_up_only_its_resources() {
        let registry = Sessions::default();
        let a = registry.create();
        let b = registry.create();
        let a_id = a.session.id().to_string();
        let a_closed = Arc::new(AtomicUsize::new(0));
        let b_closed = Arc::new(AtomicUsize::new(0));
        for (lease, counter) in [(&a, &a_closed), (&b, &b_closed)] {
            lease.session.begin("output").unwrap();
            let counter = counter.clone();
            lease
                .session
                .register("terminal:one".into(), "output".into(), move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
        }
        drop(a);
        assert!(registry.get(&a_id).is_err());
        assert_eq!(a_closed.load(Ordering::SeqCst), 1);
        assert_eq!(b_closed.load(Ordering::SeqCst), 0);
        assert!(b.session.owns("terminal:one"));
        assert!(b.session.remove("terminal:one"));
        assert!(!b.session.remove("terminal:one"));
        drop(b);
        assert_eq!(b_closed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn early_events_arrive_in_order_and_consumers_are_private() {
        let registry = Sessions::default();
        let a = registry.create();
        let b = registry.create();
        a.session.begin("a").unwrap();
        b.session.begin("b").unwrap();
        assert!(a.session.emit("a", "first"));
        assert!(a.session.emit("a", "second"));
        assert!(b.session.emit("b", "other window"));
        let mut events = a.session.events().unwrap();
        assert!(a.session.events().is_err());
        assert!(
            matches!(events.next().await.unwrap().body, EventBody::Data(value) if value == "first")
        );
        assert!(
            matches!(events.next().await.unwrap().body, EventBody::Data(value) if value == "second")
        );
        drop(events);
        assert!(a.session.is_closed());
        assert!(!b.session.is_closed());
        assert!(
            matches!(b.session.events().unwrap().next().await.unwrap().body, EventBody::Data(value) if value == "other window")
        );
    }

    #[tokio::test]
    async fn overflowing_a_client_queue_fails_explicitly_without_blocking_other_clients() {
        let registry = Sessions::default();
        let a = registry.create();
        let b = registry.create();
        a.session.begin("a").unwrap();
        b.session.begin("b").unwrap();
        let mut events = a.session.events().unwrap();
        for _ in 0..EVENT_COUNT {
            assert!(a.session.emit("a", "queued"));
        }
        assert!(!a.session.emit("a", "overflow"));
        assert!(events.next().await.unwrap_err().contains("keeping up"));
        assert!(b.session.emit("b", "still usable"));
        assert!(!b.session.is_closed());
    }

    #[test]
    fn a_single_large_event_cannot_bypass_the_byte_budget() {
        let lease = Sessions::default().create();
        lease.session.begin("output").unwrap();
        assert!(!lease.session.emit("output", "x".repeat(EVENT_BYTES)));
        assert!(lease.session.is_closed());
        assert_eq!(*lock(&lease.session.queued_bytes), 0);
    }

    #[test]
    fn a_cancelled_callback_cannot_restart_or_grow_the_session_with_unknown_ids() {
        let lease = Sessions::default().create();
        lease.session.begin("output").unwrap();
        lease.session.cancel_sink("output");
        assert!(!lease.session.emit("output", "late output"));
        for id in 0..1000 {
            lease.session.cancel_sink(&id.to_string());
        }
        assert!(lock(&lease.session.streams).is_empty());
        assert!(lock(&lease.session.resources).is_empty());
    }

    #[test]
    fn cancelling_early_output_prevents_late_resource_registration() {
        let lease = Sessions::default().create();
        lease.session.begin("output").unwrap();
        lease.session.cancel_sink("output");
        let cleaned = Arc::new(AtomicBool::new(false));
        let flag = cleaned.clone();
        assert!(lease
            .session
            .register("late".into(), "output".into(), move || {
                flag.store(true, Ordering::SeqCst);
            })
            .is_err());
        assert!(cleaned.load(Ordering::SeqCst));
        assert!(!lease.session.owns("late"));
    }

    #[test]
    fn cancellation_removes_old_registrations_before_running_reentrant_cleanup() {
        let lease = Sessions::default().create();
        lease.session.begin("old").unwrap();
        lease.session.begin("new").unwrap();
        let replaced = Arc::new(AtomicUsize::new(0));
        for (key, other) in [("one", "two"), ("two", "one")] {
            let session = lease.session.clone();
            let replaced = replaced.clone();
            lease
                .session
                .register(key.into(), "old".into(), move || {
                    if session.register(other.into(), "new".into(), || {}).is_ok() {
                        replaced.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .unwrap();
        }
        lease.session.cancel_sink("old");
        assert_eq!(replaced.load(Ordering::SeqCst), 2);
        assert!(lease.session.owns("one") && lease.session.owns("two"));
    }

    #[test]
    fn registering_after_disconnect_cleans_up_the_new_resource() {
        let lease = Sessions::default().create();
        lease.session.close("closed");
        let cleaned = Arc::new(AtomicBool::new(false));
        let flag = cleaned.clone();
        assert!(lease
            .session
            .register("resource".into(), "output".into(), move || {
                flag.store(true, Ordering::SeqCst);
            })
            .is_err());
        assert!(cleaned.load(Ordering::SeqCst));
    }
}

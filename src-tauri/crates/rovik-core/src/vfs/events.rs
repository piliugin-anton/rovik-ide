//! Callbacks for what happens to virtual files.
//!
//! [`VirtualFs::subscribe`](super::store::VirtualFs::subscribe) registers a
//! callback for the kinds of events it asks for. Events come from programs
//! using the mounted volume ([`Source::Process`]) and from the app's own
//! path API ([`Source::App`]).
//!
//! Cost is kept off the file operations:
//! - With no subscriber for an event kind, the operation pays one atomic
//!   load; the event (and its path) is never built.
//! - Callbacks run after the filesystem's lock is released, on the thread
//!   that served the operation — the calling program waits for them, so
//!   they must be quick. [`batched`] moves the work to a background thread
//!   through a bounded buffer, which never blocks and wakes the thread once
//!   per batch: when it's full, events are dropped and counted.
//! - A callback must not use the mounted volume through system calls (the
//!   request it would make can wait on the very thread running it).
//!
//! Reads and writes are reported per request the kernel sends (typically
//! 4–128 KiB), and reads served from the kernel's page cache aren't seen at
//! all; every open is. For "a program finished writing this file", use
//! [`Op::Closed`] with `modified`.

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;

use super::store::{Ino, Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// A program using the mounted volume.
    Process,
    /// The app, through the path API (`write`, `remove`…).
    App,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    /// A file, directory or symlink appeared (hard links included).
    Created { kind: Kind },
    /// A file was opened.
    Opened { write: bool },
    /// Bytes were read (`len` is what was returned).
    Read { offset: u64, len: u64 },
    /// Bytes were written.
    Written { offset: u64, len: u64 },
    /// The file's size was set (truncated or extended).
    Truncated { size: u64 },
    /// A handle to a file was closed; `modified` if it was written since
    /// the previous close.
    Closed { modified: bool },
    /// An entry was removed (a removed folder is reported once, not per
    /// file inside it).
    Removed { kind: Kind },
    /// Moved from `from` to the event's `path`, replacing anything there.
    Renamed { from: String },
}

impl Op {
    pub fn mask(&self) -> EventMask {
        match self {
            Op::Created { .. } => EventMask::CREATED,
            Op::Opened { .. } => EventMask::OPENED,
            Op::Read { .. } => EventMask::READ,
            Op::Written { .. } => EventMask::WRITTEN,
            Op::Truncated { .. } => EventMask::TRUNCATED,
            Op::Closed { .. } => EventMask::CLOSED,
            Op::Removed { .. } => EventMask::REMOVED,
            Op::Renamed { .. } => EventMask::RENAMED,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEvent {
    pub ino: Ino,
    /// Path inside the volume, `/`-separated, no leading slash. For a file
    /// that was already deleted, the path it last had.
    pub path: String,
    pub source: Source,
    #[serde(flatten)]
    pub op: Op,
}

/// Which kinds of events a subscriber wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventMask(u16);

impl EventMask {
    pub const NONE: EventMask = EventMask(0);
    pub const CREATED: EventMask = EventMask(1 << 0);
    pub const OPENED: EventMask = EventMask(1 << 1);
    pub const READ: EventMask = EventMask(1 << 2);
    pub const WRITTEN: EventMask = EventMask(1 << 3);
    pub const TRUNCATED: EventMask = EventMask(1 << 4);
    pub const CLOSED: EventMask = EventMask(1 << 5);
    pub const REMOVED: EventMask = EventMask(1 << 6);
    pub const RENAMED: EventMask = EventMask(1 << 7);
    pub const ALL: EventMask = EventMask((1 << 8) - 1);
    /// Everything except the per-request `read`/`written` and `opened`:
    /// what changed, at a rate that doesn't depend on I/O sizes.
    pub const CHANGES: EventMask = EventMask(
        Self::CREATED.0 | Self::TRUNCATED.0 | Self::CLOSED.0 | Self::REMOVED.0 | Self::RENAMED.0,
    );

    pub fn contains(self, other: EventMask) -> bool {
        self.0 & other.0 != 0
    }

    /// From names as the frontend sends them ("created", "read"…, "all",
    /// "changes").
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Result<EventMask, String> {
        names.iter().try_fold(EventMask::NONE, |mask, name| {
            let bit = match name.as_ref() {
                "created" => Self::CREATED,
                "opened" => Self::OPENED,
                "read" => Self::READ,
                "written" => Self::WRITTEN,
                "truncated" => Self::TRUNCATED,
                "closed" => Self::CLOSED,
                "removed" => Self::REMOVED,
                "renamed" => Self::RENAMED,
                "all" => Self::ALL,
                "changes" => Self::CHANGES,
                other => return Err(format!("unknown event kind {other}")),
            };
            Ok(mask | bit)
        })
    }
}

impl std::ops::BitOr for EventMask {
    type Output = EventMask;
    fn bitor(self, rhs: EventMask) -> EventMask {
        EventMask(self.0 | rhs.0)
    }
}

pub type Callback = Arc<dyn Fn(&FsEvent) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub u64);

struct Subscriber {
    id: u64,
    mask: EventMask,
    callback: Callback,
}

/// The subscriber list. Copy-on-write: emitting takes a snapshot (one
/// reference count) and calls outside any lock, so a callback may
/// subscribe or unsubscribe without deadlocking.
#[derive(Default)]
pub(crate) struct Hub {
    /// Union of all subscribers' masks: the fast "does anyone care" check.
    mask: AtomicU16,
    subscribers: RwLock<Arc<Vec<Subscriber>>>,
    next_id: AtomicU64,
}

impl Hub {
    #[inline]
    pub fn wants(&self, mask: EventMask) -> bool {
        self.mask.load(Ordering::Relaxed) & mask.0 != 0
    }

    pub fn subscribe(&self, mask: EventMask, callback: Callback) -> SubscriptionId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        let mut next: Vec<Subscriber> = subs
            .iter()
            .map(|s| Subscriber {
                id: s.id,
                mask: s.mask,
                callback: s.callback.clone(),
            })
            .collect();
        next.push(Subscriber { id, mask, callback });
        self.publish(&mut subs, next);
        SubscriptionId(id)
    }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        if !subs.iter().any(|s| s.id == id.0) {
            return false;
        }
        let next = subs
            .iter()
            .filter(|s| s.id != id.0)
            .map(|s| Subscriber {
                id: s.id,
                mask: s.mask,
                callback: s.callback.clone(),
            })
            .collect();
        self.publish(&mut subs, next);
        true
    }

    fn publish(&self, subs: &mut Arc<Vec<Subscriber>>, next: Vec<Subscriber>) {
        let mask = next.iter().fold(0, |m, s| m | s.mask.0);
        *subs = Arc::new(next);
        self.mask.store(mask, Ordering::Relaxed);
    }

    pub fn emit(&self, events: &[FsEvent]) {
        if events.is_empty() {
            return;
        }
        let subs = self
            .subscribers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for event in events {
            let bit = event.op.mask();
            for sub in subs.iter() {
                if sub.mask.contains(bit) {
                    (sub.callback)(event);
                }
            }
        }
    }
}

/// Events built during one operation, delivered after its lock is released.
/// Holds at most a couple of events; nothing is allocated when nobody
/// subscribed.
pub(crate) struct Pending<'a> {
    hub: &'a Hub,
    source: Source,
    events: Vec<FsEvent>,
}

impl<'a> Pending<'a> {
    pub fn new(hub: &'a Hub, source: Source) -> Self {
        Pending {
            hub,
            source,
            events: Vec::new(),
        }
    }

    #[inline]
    pub fn wants(&self, mask: EventMask) -> bool {
        self.hub.wants(mask)
    }

    /// Records an event if anyone wants its kind; `path` is only computed then.
    #[inline]
    pub fn add(&mut self, ino: Ino, op: Op, path: impl FnOnce() -> String) {
        if self.hub.wants(op.mask()) {
            self.events.push(FsEvent {
                ino,
                path: path(),
                source: self.source,
                op,
            });
        }
    }

    /// Delivers the events. Call after releasing the filesystem's lock.
    pub fn send(self) {
        self.hub.emit(&self.events);
    }
}

/// A batch of events, as [`batched`] delivers them.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBatch {
    pub events: Vec<FsEvent>,
    /// Events lost since the previous batch because the queue was full.
    pub dropped: u64,
}

/// Wraps `sink` so it runs on its own thread, getting events in batches of
/// up to `max_batch`. The returned callback only appends to a buffer: it
/// never blocks a file operation, and it wakes the thread once per batch
/// (not per event — a wake per event costs a context switch, which shows up
/// when a command touches thousands of files). A batch collects for
/// `interval` after its first event. At most `queue` events wait; beyond
/// that they're dropped and counted. The thread ends when the callback is
/// dropped (unsubscribed), after delivering what's left.
pub fn batched(
    queue: usize,
    max_batch: usize,
    interval: Duration,
    mut sink: impl FnMut(EventBatch) + Send + 'static,
) -> Callback {
    struct Buffer {
        events: Vec<FsEvent>,
        dropped: u64,
        closed: bool,
    }
    type Shared = Arc<(Mutex<Buffer>, Condvar)>;

    /// Owned by the callback: when the subscription goes, the thread is told.
    struct Producer(Shared);
    impl Drop for Producer {
        fn drop(&mut self) {
            let (buffer, wake) = &*self.0;
            buffer.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
            wake.notify_one();
        }
    }

    let shared: Shared = Arc::new((
        Mutex::new(Buffer {
            events: Vec::new(),
            dropped: 0,
            closed: false,
        }),
        Condvar::new(),
    ));
    let consumer = shared.clone();
    std::thread::Builder::new()
        .name("vfs-events".into())
        .spawn(move || {
            let (buffer, wake) = &*consumer;
            loop {
                {
                    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    while b.events.is_empty() && b.dropped == 0 && !b.closed {
                        b = wake.wait(b).unwrap_or_else(|e| e.into_inner());
                    }
                    if b.closed && b.events.is_empty() && b.dropped == 0 {
                        return;
                    }
                    let closing = b.closed;
                    drop(b);
                    // Let the batch fill (not when shutting down).
                    if !closing {
                        std::thread::sleep(interval);
                    }
                }
                let (events, dropped) = {
                    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        std::mem::take(&mut b.events),
                        std::mem::take(&mut b.dropped),
                    )
                };
                let mut events = events.into_iter().peekable();
                let mut dropped = Some(dropped);
                while events.peek().is_some() || dropped.is_some_and(|d| d > 0) {
                    let batch: Vec<FsEvent> = events.by_ref().take(max_batch.max(1)).collect();
                    sink(EventBatch {
                        events: batch,
                        dropped: dropped.take().unwrap_or(0),
                    });
                }
            }
        })
        .expect("spawning the vfs event thread");

    let producer = Producer(shared);
    Arc::new(move |event: &FsEvent| {
        let (buffer, wake) = &*producer.0;
        let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
        if b.events.len() >= queue {
            b.dropped += 1;
            return;
        }
        b.events.push(event.clone());
        let first = b.events.len() == 1;
        drop(b);
        if first {
            wake.notify_one();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn event(op: Op) -> FsEvent {
        FsEvent {
            ino: 2,
            path: "a".into(),
            source: Source::App,
            op,
        }
    }

    #[test]
    fn masks_filter_and_combine() {
        let mask = EventMask::from_names(&["created", "closed"]).unwrap();
        assert!(mask.contains(EventMask::CREATED) && mask.contains(EventMask::CLOSED));
        assert!(!mask.contains(EventMask::READ));
        assert!(EventMask::from_names(&["nope"]).is_err());
        assert!(
            !EventMask::CHANGES.contains(EventMask::READ | EventMask::WRITTEN | EventMask::OPENED)
        );
    }

    #[test]
    fn hub_delivers_by_mask_and_unsubscribes() {
        let hub = Hub::default();
        assert!(!hub.wants(EventMask::ALL));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let id = hub.subscribe(
            EventMask::CREATED,
            Arc::new(move |e| sink.lock().unwrap().push(e.op.clone())),
        );
        assert!(hub.wants(EventMask::CREATED) && !hub.wants(EventMask::READ));
        hub.emit(&[
            event(Op::Created { kind: Kind::File }),
            event(Op::Read { offset: 0, len: 1 }),
        ]);
        assert_eq!(*seen.lock().unwrap(), [Op::Created { kind: Kind::File }]);
        assert!(hub.unsubscribe(id));
        assert!(!hub.wants(EventMask::CREATED));
        assert!(!hub.unsubscribe(id));
    }

    #[test]
    fn callbacks_may_unsubscribe_themselves() {
        let hub = Arc::new(Hub::default());
        let id = Arc::new(Mutex::new(None));
        let (h, i) = (hub.clone(), id.clone());
        let sub = hub.subscribe(
            EventMask::ALL,
            Arc::new(move |_| {
                if let Some(id) = i.lock().unwrap().take() {
                    h.unsubscribe(id);
                }
            }),
        );
        *id.lock().unwrap() = Some(sub);
        hub.emit(&[event(Op::Closed { modified: false })]);
        assert!(!hub.wants(EventMask::ALL));
    }

    #[test]
    fn batched_delivers_in_order_and_counts_drops() {
        let (tx, rx) = mpsc::channel();
        // A sink that's stuck until released, so the buffer fills.
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let callback = batched(4, 100, Duration::from_millis(20), move |batch| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(2));
            tx.send(batch).unwrap();
        });
        for i in 0..50 {
            callback(&FsEvent {
                ino: i,
                ..event(Op::Closed { modified: true })
            });
        }
        drop(callback); // Unsubscribed: what's buffered is still delivered.
        let mut got = Vec::new();
        let mut dropped = 0;
        loop {
            let _ = release.send(());
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(batch) => {
                    dropped += batch.dropped;
                    got.extend(batch.events.into_iter().map(|e| e.ino));
                }
                Err(_) => break,
            }
        }
        assert_eq!(got, [0, 1, 2, 3], "the first events, in order");
        assert_eq!(dropped, 46, "the rest counted");
    }

    #[test]
    fn batched_groups_events() {
        let (tx, rx) = mpsc::channel();
        let callback = batched(10_000, 1000, Duration::from_millis(30), move |batch| {
            tx.send(batch.events.len()).unwrap()
        });
        for i in 0..2500 {
            callback(&FsEvent {
                ino: i,
                ..event(Op::Closed { modified: false })
            });
        }
        let mut sizes = Vec::new();
        while let Ok(n) = rx.recv_timeout(Duration::from_millis(300)) {
            sizes.push(n);
        }
        assert_eq!(sizes.iter().sum::<usize>(), 2500);
        assert!(
            sizes.len() <= 4 && sizes.iter().all(|&n| n <= 1000),
            "{sizes:?}"
        );
    }
}

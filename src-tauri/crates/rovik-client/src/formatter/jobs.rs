//! Blocking discovery/formatting workers remain owned until their cleanup finishes.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
};

#[derive(Default)]
struct Jobs {
    closing: bool,
    active: HashMap<uuid::Uuid, Arc<AtomicBool>>,
}
/// Tracks formatter workers so shutdown cancels and joins accepted work.
#[derive(Default)]
pub struct FormatterState {
    jobs: Mutex<Jobs>,
    ended: Condvar,
}
struct Worker {
    state: Arc<FormatterState>,
    id: uuid::Uuid,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.state
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
        self.state.ended.notify_all();
    }
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl FormatterState {
    pub(super) async fn work<T: Send + 'static>(
        self: Arc<Self>,
        work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if jobs.closing {
                return Err("The formatter backend is shutting down".into());
            }
            let id = uuid::Uuid::new_v4();
            jobs.active.insert(id, cancelled.clone());
            Worker {
                state: self.clone(),
                id,
            }
        };
        // Dropping an RPC future does not stop spawn_blocking. Its worker checks
        // this flag and remains registered until the process and pipes are gone.
        let _cancel = Cancel(cancelled.clone());
        crate::util::blocking(move || {
            let _worker = worker;
            if cancelled.load(Ordering::Acquire) {
                return Err("formatting cancelled".into());
            }
            work(&cancelled)
        })
        .await
    }
    /// Cancels workers and waits for cleanup. Call from a blocking thread.
    pub fn shutdown(&self) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.closing = true;
        for cancelled in jobs.active.values() {
            cancelled.store(true, Ordering::Release);
        }
        while !jobs.active.is_empty() {
            jobs = self.ended.wait(jobs).unwrap_or_else(|e| e.into_inner());
        }
    }
}

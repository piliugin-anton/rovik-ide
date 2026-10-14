//! Where the core's tasks run: one tokio runtime per process.
//!
//! The runtime is a multi-thread one, made on first use and kept for the
//! process's whole life. A task can be spawned from any thread, as
//! `tauri::async_runtime` allowed: `tokio::spawn` can't, it panics outside
//! a runtime. A process hosting the core takes the runtime from [`handle`]
//! after it's done what needs a single thread, and hands it to whatever
//! else needs one: the app gives it to Tauri, so there is one runtime.
//!
//! Tests use it too, not the runtime of a `#[tokio::test]`: on that single
//! thread a run would only move while the test waits, and the tests are
//! written for runs that go on beside them.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Handle};
use tokio::task::JoinHandle;

/// The process's runtime, made on first use.
pub fn handle() -> &'static Handle {
    static HANDLE: OnceLock<Handle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("error while building the async runtime");
        // Never dropped: a task may still be running when the process ends.
        Box::leak(Box::new(runtime)).handle().clone()
    })
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(future)
}

pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    handle().spawn_blocking(f)
}

/// Runs `future` to its end on the process's runtime, blocking the calling
/// thread, which must not be one of the runtime's own.
pub fn block_on<F: Future>(future: F) -> F::Output {
    handle().block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tasks_run_beside_a_caller_that_blocks() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn(async move { tx.send(1).unwrap() });
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(1));
        assert_eq!(block_on(spawn_blocking(|| 2)).unwrap(), 2);
    }

    #[tokio::test]
    async fn a_tokio_test_can_await_its_tasks() {
        assert_eq!(spawn(async { 3 }).await.unwrap(), 3);
    }
}

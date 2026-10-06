//! Running a turn's read-only tool calls at the same time.
//!
//! Only a run of consecutive calls that change nothing and ask nothing
//! (execute.rs decides which) runs together; anything else ends the run, so
//! a read after a write still sees the write. Results are handed back in the
//! calls' order, each as soon as it and every call before it are done:
//! whatever records them (the guard, the database, the UI) sees the same
//! order as when calls ran one by one.

use futures_util::stream::{self, BoxStream, StreamExt};
use std::future::Future;
use tokio::sync::watch;

/// The most calls running at once.
pub(super) const MAX_PARALLEL: usize = 8;

/// The tools that may run alongside each other: local reads. (The browser
/// tools share one tab, one call at a time, and what a screenshot shows
/// depends on the order pages were opened.)
pub(super) const PARALLEL_TOOLS: &[&str] = &["read_file", "list_dir", "search_code"];

/// Batches run so far, by chat: their calls' ids (for the tests to see
/// which calls ran together).
#[cfg(test)]
pub(super) static BATCHES: std::sync::Mutex<Vec<(String, Vec<String>)>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn batches_of(dialog_id: &str) -> Vec<Vec<String>> {
    BATCHES.lock().unwrap().iter().filter(|(d, _)| d == dialog_id).map(|(_, ids)| ids.clone()).collect()
}

/// How many calls from the start of `calls` can run together: the length of
/// the run of calls `eligible` accepts. Fewer than two isn't worth it: 0.
pub(super) fn batch_len<T>(calls: &[T], eligible: impl Fn(&T) -> bool) -> usize {
    let n = calls.iter().take_while(|c| eligible(c)).count();
    if n >= 2 { n } else { 0 }
}

/// Jobs run at most `limit` at a time, their results handed out in order.
pub(super) struct InOrder<T> {
    results: BoxStream<'static, T>,
    stop: watch::Receiver<bool>,
    remaining: usize,
    stopped: bool,
}

impl<T: Send + 'static> InOrder<T> {
    pub(super) fn new<F>(jobs: Vec<F>, limit: usize, stop: watch::Receiver<bool>) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        let remaining = jobs.len();
        // `buffered` starts a job only when one of the `limit` slots is free,
        // and yields results in the jobs' order.
        let results = stream::iter(jobs).buffered(limit.max(1)).boxed();
        InOrder { results, stop, remaining, stopped: false }
    }

    /// The next job's result: `Some(Some(result))`, or `Some(None)` for a
    /// job that hadn't finished when stop came (it's dropped, and with it
    /// whatever it was running). None after the last job.
    pub(super) async fn next(&mut self) -> Option<Option<T>> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.stopped || *self.stop.borrow() {
            self.stop_now();
            return Some(None);
        }
        let mut stop = self.stop.clone();
        let stopped = async move {
            // No one left to say stop: it never comes.
            if stop.wait_for(|s| *s).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            // A result that's ready is kept, even if stop came too.
            biased;
            result = self.results.next() => Some(result),
            _ = stopped => {
                self.stop_now();
                Some(None)
            }
        }
    }

    fn stop_now(&mut self) {
        if !self.stopped {
            self.stopped = true;
            // Drops the jobs still running.
            self.results = stream::empty().boxed();
        }
    }
}

/// Runs `work` as a task of its own (on another thread when one is free),
/// aborted if this future is dropped first. A panic in it is an error.
pub(super) async fn spawned<T: Send + 'static>(work: impl Future<Output = T> + Send + 'static) -> Result<T, String> {
    struct AbortOnDrop<T>(tauri::async_runtime::JoinHandle<T>);
    impl<T> Drop for AbortOnDrop<T> {
        fn drop(&mut self) {
            self.0.abort(); // nothing happens if it's done
        }
    }
    let mut task = AbortOnDrop(tauri::async_runtime::spawn(work));
    (&mut task.0).await.map_err(|e| format!("the tool failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn no_stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[test]
    fn a_batch_is_the_run_of_eligible_calls_at_the_start() {
        let even = |n: &i32| n % 2 == 0;
        assert_eq!(batch_len(&[2, 4, 6, 7, 8], even), 3);
        assert_eq!(batch_len(&[2, 4], even), 2);
        assert_eq!(batch_len(&[2, 3, 4, 6], even), 0, "one call alone runs as before");
        assert_eq!(batch_len(&[1, 2, 4], even), 0, "the run has to start at the first call");
        assert_eq!(batch_len::<i32>(&[], even), 0);
    }

    #[tokio::test]
    async fn results_come_in_order_whatever_order_the_jobs_finish_in() {
        let (senders, receivers): (Vec<_>, Vec<_>) = (0..4).map(|_| oneshot::channel::<usize>()).unzip();
        let jobs: Vec<_> = receivers.into_iter().map(|r| async move { r.await.unwrap() }).collect();
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 4, stop);
        // The last finishes first, the first last.
        for (i, s) in senders.into_iter().enumerate().rev() {
            s.send(i).unwrap();
        }
        let mut got = Vec::new();
        while let Some(r) = results.next().await {
            got.push(r.unwrap());
        }
        assert_eq!(got, [0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn a_result_is_handed_out_without_waiting_for_later_ones() {
        let (first_tx, first_rx) = oneshot::channel::<&str>();
        let (_second_tx, second_rx) = oneshot::channel::<&str>(); // never sent
        let jobs: Vec<_> = vec![first_rx, second_rx].into_iter().map(|r| async move { r.await.unwrap_or("dropped") }).collect();
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 2, stop);
        first_tx.send("first").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), results.next()).await.expect("the first result, now");
        assert_eq!(got, Some(Some("first")));
    }

    #[tokio::test]
    async fn no_more_than_the_limit_run_at_once() {
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<_> = (0..20)
            .map(|i| {
                let (running, most) = (running.clone(), most.clone());
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    i
                }
            })
            .collect();
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 3, stop);
        let mut got = Vec::new();
        while let Some(r) = results.next().await {
            got.push(r.unwrap());
        }
        assert_eq!(got, (0..20).collect::<Vec<_>>());
        assert_eq!(most.load(Ordering::SeqCst), 3, "they do run together, and never more than 3");
    }

    #[tokio::test]
    async fn a_limit_of_zero_still_runs_one_at_a_time() {
        let jobs: Vec<_> = (0..3).map(|i| async move { i }).collect();
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 0, stop);
        let mut got = Vec::new();
        while let Some(r) = results.next().await {
            got.push(r.unwrap());
        }
        assert_eq!(got, [0, 1, 2]);
    }

    #[tokio::test]
    async fn no_jobs_no_results() {
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(Vec::<std::future::Ready<u8>>::new(), 4, stop);
        assert_eq!(results.next().await, None);
    }

    #[tokio::test]
    async fn stop_keeps_finished_results_and_drops_the_rest() {
        let (done_tx, done_rx) = oneshot::channel::<&str>();
        let (_never_tx, never_rx) = oneshot::channel::<&str>();
        let (_never2_tx, never2_rx) = oneshot::channel::<&str>();
        let jobs: Vec<_> =
            vec![done_rx, never_rx, never2_rx].into_iter().map(|r| async move { r.await.unwrap_or("dropped") }).collect();
        let (stop_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 3, stop);
        done_tx.send("done").unwrap();
        assert_eq!(results.next().await, Some(Some("done")));
        stop_tx.send(true).unwrap();
        assert_eq!(results.next().await, Some(None), "unfinished: no result");
        assert_eq!(results.next().await, Some(None));
        assert_eq!(results.next().await, None, "every job accounted for, then the end");
    }

    #[tokio::test]
    async fn stop_while_waiting_ends_the_wait() {
        let (_never_tx, never_rx) = oneshot::channel::<u8>();
        let jobs = vec![async move { never_rx.await.unwrap_or(0) }];
        let (stop_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 1, stop);
        let waiting = tokio::spawn(async move { results.next().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop_tx.send(true).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("stop ends the wait").unwrap();
        assert_eq!(got, Some(None));
    }

    #[tokio::test]
    async fn a_stop_signal_nobody_can_send_never_stops() {
        let (stop_tx, stop) = no_stop();
        drop(stop_tx);
        let jobs: Vec<_> = (0..3)
            .map(|i| async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                i
            })
            .collect();
        let mut results = InOrder::new(jobs, 2, stop);
        let mut got = Vec::new();
        while let Some(r) = results.next().await {
            got.push(r);
        }
        assert_eq!(got, [Some(0), Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn stop_before_the_start_runs_nothing() {
        let ran = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<_> = (0..3)
            .map(|_| {
                let ran = ran.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                }
            })
            .collect();
        let (stop_tx, stop) = no_stop();
        stop_tx.send(true).unwrap();
        let mut results = InOrder::new(jobs, 3, stop);
        let mut got = Vec::new();
        while let Some(r) = results.next().await {
            got.push(r);
        }
        assert_eq!(got, [None, None, None]);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_dropped_task_is_aborted() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let job = spawned(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            flag.store(true, Ordering::SeqCst);
        });
        // Started, then dropped before it's done.
        let _ = tokio::time::timeout(Duration::from_millis(20), job).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!finished.load(Ordering::SeqCst), "the task kept running after it was dropped");
    }

    #[tokio::test]
    async fn a_panicking_task_is_an_error_not_a_crash() {
        let result = spawned(async { panic!("boom") }).await;
        let error: Result<(), String> = result;
        assert!(error.unwrap_err().starts_with("the tool failed"));
        assert_eq!(spawned(async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn spawned_tasks_run_at_the_same_time() {
        // Two tasks that each wait for the other: only possible if both run
        // at once.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let jobs: Vec<_> = (0..2)
            .map(|i| {
                let barrier = barrier.clone();
                spawned(async move {
                    barrier.wait().await;
                    i
                })
            })
            .collect();
        let (_tx, stop) = no_stop();
        let mut results = InOrder::new(jobs, 2, stop);
        let all = async {
            let mut got = Vec::new();
            while let Some(r) = results.next().await {
                got.push(r.unwrap().unwrap());
            }
            got
        };
        let got = tokio::time::timeout(Duration::from_secs(5), all).await.expect("they ran together");
        assert_eq!(got, [0, 1]);
    }
}

//! Accepted dependency installation outlives individual UI observers.
use futures_util::FutureExt;
use std::{future::Future, sync::Mutex};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Update<P, T> {
    pub(crate) progress: Option<P>,
    result: Option<Result<T, String>>,
}
struct State<P, T> {
    run: Option<watch::Receiver<Update<P, T>>>,
    closing: bool,
}

/// Serializes dependency installs and retains accepted work across window loss.
pub(crate) struct InstallJob<P, T>(Mutex<State<P, T>>);

impl<P, T> Default for InstallJob<P, T> {
    fn default() -> Self {
        Self(Mutex::new(State {
            run: None,
            closing: false,
        }))
    }
}
impl<P, T> Default for Update<P, T> {
    fn default() -> Self {
        Self {
            progress: None,
            result: None,
        }
    }
}

impl<P, T> InstallJob<P, T>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    /// Join accepted work or start one producer retained by the backend.
    pub(crate) fn subscribe<F, Fut>(&self, work: F) -> Result<watch::Receiver<Update<P, T>>, String>
    where
        F: FnOnce(watch::Sender<Update<P, T>>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("The dependency installer is shutting down".into());
        }
        if let Some(run) = &state.run {
            if run.borrow().result.is_none() {
                return Ok(run.clone());
            }
        }
        // Completed requests recheck the executable, including after a failed
        // download or an installation removed outside the app.
        let (sent, received) = watch::channel(Update::default());
        state.run = Some(received.clone());
        rovik_core::rt::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async { work(sent.clone()).await })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(
                        "Dependency installation failed unexpectedly; retry the installation"
                            .into(),
                    )
                });
            sent.send_modify(|update| update.result = Some(result));
        });
        Ok(received)
    }

    /// An install outlives its UI observers and prevents automatic host replacement.
    #[cfg(any(unix, windows, test))]
    pub(crate) fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .run
            .as_ref()
            .is_some_and(|run| run.borrow().result.is_none())
    }

    /// Waits for accepted installation/publication before backend shutdown.
    pub async fn shutdown(&self) {
        let run = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            state.run.clone()
        };
        if let Some(run) = run {
            if let Err(error) = follow(run, |_| true).await {
                log::warn!("Dependency installation ended before shutdown: {error}");
            }
        }
    }
}

/// Follow private progress; losing an observer does not cancel the producer.
pub(crate) async fn follow<P: Clone + PartialEq, T: Clone>(
    mut run: watch::Receiver<Update<P, T>>,
    mut progress: impl FnMut(P) -> bool,
) -> Result<T, String> {
    let mut last = None;
    loop {
        let update = run.borrow_and_update().clone();
        if let Some(next) = update.progress.filter(|next| last.as_ref() != Some(next)) {
            last = Some(next.clone());
            if !progress(next) {
                return Err("Dependency installation progress callback closed".into());
            }
        }
        if let Some(result) = update.result {
            return result;
        }
        run.changed()
            .await
            .map_err(|_| "Dependency installation ended without a result".to_string())?;
    }
}

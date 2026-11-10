//! One accepted Chrome installation belongs to the backend, not its observers.
//! A client can stop following progress without dropping the installer while a
//! blocking unpack still uses its archive and lifetime lock.
use futures_util::FutureExt;
use rovik_core::pinned_browser::{self, Build, NAME, VERSION};
use serde::Serialize;
use std::{future::Future, sync::Mutex};
use tokio::sync::watch;

/// The existing Settings/startup browser status contract.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    /// Pinned browser's display name.
    pub name: &'static str,
    /// Pinned release version.
    pub version: &'static str,
    /// Installed executable, if available.
    pub path: Option<String>,
    /// Expected download size on this platform.
    pub size: Option<u64>,
}

/// Reports the same executable the backend's web tools discover.
pub fn status() -> BrowserStatus {
    BrowserStatus {
        name: NAME,
        version: VERSION,
        path: pinned_browser::installed().map(|p| p.to_string_lossy().into_owned()),
        size: Build::here().map(|build| build.size),
    }
}

/// Progress for one client's callback, retaining the native field names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProgress {
    /// Downloaded archive bytes.
    pub downloaded: u64,
    /// Expected archive bytes (zero while waiting for another process).
    pub total: u64,
}

#[derive(Clone, Default)]
struct Update {
    progress: Option<BrowserProgress>,
    result: Option<Result<String, String>>,
}
#[derive(Default)]
struct State {
    run: Option<watch::Receiver<Update>>,
    closing: bool,
}

/// Serializes Chrome installs and retains accepted work across window loss.
#[derive(Default)]
pub struct BrowserInstaller(Mutex<State>);

impl BrowserInstaller {
    fn subscribe<F, Fut>(&self, work: F) -> Result<watch::Receiver<Update>, String>
    where
        F: FnOnce(watch::Sender<Update>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("The browser installer is shutting down".into());
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
                    Err("Browser installation failed unexpectedly; retry the installation".into())
                });
            sent.send_modify(|update| update.result = Some(result));
        });
        Ok(received)
    }

    /// Joins or starts the backend's install, forwarding only this observer's progress.
    pub async fn install(
        &self,
        progress: impl FnMut(BrowserProgress) -> bool,
    ) -> Result<String, String> {
        let run = self.subscribe(|sent| async move {
            let mut told = 0;
            let path = pinned_browser::install_for_app(|downloaded, total| {
                if downloaded.saturating_sub(told) >= 256 * 1024 || downloaded == total {
                    told = downloaded;
                    sent.send_modify(|update| {
                        update.progress = Some(BrowserProgress { downloaded, total })
                    });
                }
            })
            .await?;
            Ok(path.to_string_lossy().into_owned())
        })?;
        follow(run, progress).await
    }

    /// An install outlives its UI observers and prevents automatic host replacement.
    #[cfg(any(unix, test))]
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
                log::warn!("Browser installation ended before shutdown: {error}");
            }
        }
    }
}

async fn follow(
    mut run: watch::Receiver<Update>,
    mut progress: impl FnMut(BrowserProgress) -> bool,
) -> Result<String, String> {
    let mut last = None;
    loop {
        let update = run.borrow_and_update().clone();
        if let Some(next) = update.progress.filter(|next| last.as_ref() != Some(next)) {
            last = Some(next.clone());
            if !progress(next) {
                return Err("Browser installation progress callback closed".into());
            }
        }
        if let Some(result) = update.result {
            return result;
        }
        run.changed()
            .await
            .map_err(|_| "Browser installation ended without a result".to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn observers_share_one_job_and_a_closed_callback_does_not_cancel_it() {
        let installer = BrowserInstaller::default();
        let (release, wait) = tokio::sync::oneshot::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let a = installer
            .subscribe(move |progress| async move {
                progress.send_modify(|update| {
                    update.progress = Some(BrowserProgress {
                        downloaded: 7,
                        total: 10,
                    })
                });
                started.send(()).unwrap();
                wait.await.unwrap();
                progress.send_modify(|update| {
                    update.progress = Some(BrowserProgress {
                        downloaded: 10,
                        total: 10,
                    })
                });
                Ok("installed/chrome".into())
            })
            .unwrap();
        ready.await.unwrap();
        let b = installer
            .subscribe(|_| async { panic!("a second installation started") })
            .unwrap();
        let mut seen = Vec::new();
        assert!(follow(a, |progress| {
            seen.push(progress.downloaded);
            false
        })
        .await
        .unwrap_err()
        .contains("callback closed"));
        assert_eq!(seen, [7]);
        assert!(
            installer.active(),
            "observer loss cannot permit replacement"
        );
        release.send(()).unwrap();
        assert_eq!(follow(b, |_| true).await.unwrap(), "installed/chrome");
        installer.shutdown().await;
        assert!(!installer.active());
    }

    #[tokio::test]
    async fn shutdown_waits_for_publication_and_rejects_new_jobs() {
        let installer = Arc::new(BrowserInstaller::default());
        let (release, wait) = tokio::sync::oneshot::channel();
        let run = installer
            .subscribe(move |_| async move {
                wait.await.unwrap();
                Ok("published".into())
            })
            .unwrap();
        drop(run);
        let closing = installer.shutdown();
        tokio::pin!(closing);
        // Poll shutdown once: it must remain pending on the accepted job.
        assert!(futures_util::poll!(&mut closing).is_pending());
        assert!(installer
            .subscribe(|_| async { Ok("unexpected".into()) })
            .is_err());
        release.send(()).unwrap();
        closing.await;
    }

    #[tokio::test]
    async fn failed_or_panicked_installs_report_errors_and_can_be_retried() {
        let installer = BrowserInstaller::default();
        let run = installer
            .subscribe(|_| async { Err("checksum mismatch".into()) })
            .unwrap();
        assert_eq!(
            follow(run, |_| true).await.unwrap_err(),
            "checksum mismatch"
        );
        let run = installer
            .subscribe(|_| async { panic!("unpack failed") })
            .unwrap();
        assert!(follow(run, |_| true)
            .await
            .unwrap_err()
            .contains("unexpectedly"));
        let run = installer
            .subscribe(|_| async { Ok("retry succeeded".into()) })
            .unwrap();
        assert_eq!(follow(run, |_| true).await.unwrap(), "retry succeeded");
    }
}

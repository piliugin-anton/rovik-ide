//! Backend-owned host startup and refresh, independent of attached windows.
mod remote_chats;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
#[derive(Default)]
struct State {
    closing: bool,
    running: Option<Running>,
}
/// One startup and refresh worker for the backend's lifetime.
#[derive(Default)]
pub(crate) struct Maintenance {
    state: Mutex<State>,
    shutdown: tokio::sync::Mutex<()>,
}
impl Maintenance {
    pub(crate) fn start(
        &self,
        remote: Arc<crate::Remote>,
        hosts: Arc<crate::hosts_page::HostsPage>,
    ) -> Result<(), String> {
        let initial = hosts.clone();
        self.start_with(
            move || {
                remote_chats::from_env(&remote);
                initial.connect_all();
            },
            move || hosts.follow(),
            Duration::from_secs(5),
        )
    }
    fn start_with(
        &self,
        initialize: impl FnOnce() + Send + 'static,
        refresh: impl Fn() + Send + Sync + 'static,
        period: Duration,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err("Backend maintenance is shutting down".into());
        }
        if state.running.is_some() {
            return Ok(());
        }
        let (stop, mut stopping) = watch::channel(false);
        let task = rovik_core::rt::spawn(async move {
            if *stopping.borrow() {
                return;
            }
            // Blocking keychain/file work cannot be aborted safely. Always join
            // the accepted call, then check stop before starting another one.
            if let Err(error) = rovik_core::rt::spawn_blocking(initialize).await {
                eprintln!("Backend host startup failed: {error}");
                return;
            }
            let refresh = Arc::new(refresh);
            loop {
                if *stopping.borrow() {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = stopping.changed() => break,
                    _ = tokio::time::sleep(period) => {},
                }
                let refresh = refresh.clone();
                if let Err(error) = rovik_core::rt::spawn_blocking(move || refresh()).await {
                    eprintln!("Backend host refresh failed: {error}");
                }
            }
        });
        state.running = Some(Running { stop, task });
        Ok(())
    }
    /// Stops scheduling before joining any accepted blocking work.
    pub(crate) async fn stop(&self) {
        let _turn = self.shutdown.lock().await;
        let running = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            state.running.take()
        };
        if let Some(running) = running {
            running.stop.send_replace(true);
            if let Err(error) = running.task.await {
                eprintln!("Backend maintenance worker failed: {error}");
            }
        }
    }
}
impl Drop for Maintenance {
    fn drop(&mut self) {
        if let Some(running) = &self
            .state
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .running
        {
            running.stop.send_replace(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn startup_is_once_and_stop_joins_startup_without_scheduling_refresh() {
        let maintenance = Arc::new(Maintenance::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let (began, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        maintenance
            .start_with(
                move || {
                    called.fetch_add(1, Ordering::SeqCst);
                    began.send(()).unwrap();
                    blocked.recv().unwrap();
                },
                || panic!("refresh after shutdown"),
                Duration::from_millis(1),
            )
            .unwrap();
        maintenance
            .start_with(
                || panic!("duplicate startup"),
                || panic!("duplicate refresh"),
                Duration::from_millis(1),
            )
            .unwrap();
        started.await.unwrap();
        let closing = maintenance.clone();
        let stopped = tokio::spawn(async move { closing.stop().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !maintenance.state.lock().unwrap().closing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!stopped.is_finished());
        assert!(maintenance
            .start_with(|| {}, || {}, Duration::from_millis(1))
            .is_err());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), stopped)
            .await
            .unwrap()
            .unwrap();
        maintenance.stop().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refreshes_never_overlap_and_shutdown_waits_for_the_current_refresh() {
        let maintenance = Arc::new(Maintenance::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let (began, mut started) = tokio::sync::mpsc::unbounded_channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let blocked = Mutex::new(blocked);
        maintenance
            .start_with(
                || {},
                move || {
                    called.fetch_add(1, Ordering::SeqCst);
                    began.send(()).unwrap();
                    blocked.lock().unwrap().recv().unwrap();
                },
                Duration::from_millis(1),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), started.recv())
            .await
            .unwrap()
            .unwrap();
        let closing = maintenance.clone();
        let stopped = tokio::spawn(async move { closing.stop().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !maintenance.state.lock().unwrap().closing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!stopped.is_finished());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), stopped)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

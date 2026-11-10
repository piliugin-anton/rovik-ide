//! One local workload queue, with priority selections leased to UI sessions.
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Default)]
struct Focus {
    clients: Vec<(String, String)>,
    closing: bool,
}
/// Backend workload settings and ordered, coalesced cloud focus updates.
pub struct WorkloadState {
    focus: Mutex<Focus>,
    settings: Mutex<()>,
    publish: Box<dyn Fn(Option<String>, bool) + Send + Sync>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl WorkloadState {
    /// Starts a single sender; slow hosts cannot delay local priority changes.
    pub fn new(remote: Arc<crate::Remote>) -> Arc<Self> {
        let (sent, changes) = watch::channel((None, false));
        let worker = rovik_core::rt::spawn(forward(changes, move |dialog| {
            let remote = remote.clone();
            async move { remote.send_workload_focus(dialog).await }
        }));
        Arc::new(Self {
            focus: Mutex::new(Focus::default()),
            settings: Mutex::new(()),
            publish: Box::new(move |dialog, closing| {
                rovik_core::workload::workload_focus(dialog.clone());
                sent.send_replace((dialog, closing));
            }),
            worker: Mutex::new(Some(worker)),
        })
    }
    pub(crate) fn set_focus(&self, client: &str, dialog: Option<String>) -> Result<(), String> {
        let mut focus = self.focus.lock().unwrap_or_else(|e| e.into_inner());
        if focus.closing {
            return Err("The workload backend is shutting down".into());
        }
        let before = focus.clients.last().map(|(_, chat)| chat.clone());
        focus.clients.retain(|(id, _)| id != client);
        if let Some(dialog) = dialog {
            focus.clients.push((client.into(), dialog));
        }
        let after = focus.clients.last().map(|(_, chat)| chat.clone());
        if before != after {
            (self.publish)(after, false);
        }
        Ok(())
    }
    pub(crate) fn release(&self, client: &str) {
        // Shutdown already publishes None; disconnect after that has nothing to release.
        let _ = self.set_focus(client, None);
    }
    /// Current effective selection, shared by all windows.
    pub fn focused_dialog(&self) -> Option<String> {
        self.focus
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clients
            .last()
            .map(|(_, chat)| chat.clone())
    }
    pub(crate) async fn set_slots(self: Arc<Self>, slots: Option<usize>) -> Result<(), String> {
        crate::util::blocking(move || {
            let _turn = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            rovik_core::workload::workload_set_slots(slots)
        })
        .await
    }
    pub(crate) async fn status(self: Arc<Self>) -> Result<Status, String> {
        crate::util::blocking(move || {
            let _turn = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            Ok(Status {
                queue: rovik_core::workload::workload_status(),
                focused_dialog_id: self.focused_dialog(),
            })
        })
        .await
    }
    /// Clears local priority and waits for the final bounded cloud update.
    pub async fn shutdown(&self) {
        {
            let mut focus = self.focus.lock().unwrap_or_else(|e| e.into_inner());
            if !focus.closing {
                focus.closing = true;
                focus.clients.clear();
                (self.publish)(None, true);
            }
        }
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = worker {
            if let Err(error) = worker.await {
                log::error!("Workload focus sender failed: {error}");
            }
        }
    }
}
async fn forward<F, Fut>(mut changes: watch::Receiver<(Option<String>, bool)>, mut send: F)
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    while changes.changed().await.is_ok() {
        let (dialog, closing) = changes.borrow_and_update().clone();
        send(dialog).await;
        if closing {
            break;
        }
    }
}

/// Existing queue status plus the chat currently receiving priority.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    #[serde(flatten)]
    queue: rovik_core::workload::WorkloadStatus,
    focused_dialog_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_session::Sessions;
    #[tokio::test]
    async fn selections_are_leased_and_closing_a_window_restores_the_previous_chat() {
        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = changes.clone();
        let state = Arc::new(WorkloadState {
            focus: Default::default(),
            settings: Default::default(),
            publish: Box::new(move |chat, closing| seen.lock().unwrap().push((chat, closing))),
            worker: Default::default(),
        });
        let sessions = Sessions::default();
        let a = sessions.create();
        let b = sessions.create();
        a.session
            .set_workload(state.clone(), Some("a".into()))
            .unwrap();
        b.session
            .set_workload(state.clone(), Some("b".into()))
            .unwrap();
        a.session.set_workload(state.clone(), None).unwrap();
        assert_eq!(state.focused_dialog().as_deref(), Some("b"));
        a.session
            .set_workload(state.clone(), Some("a".into()))
            .unwrap();
        drop(a);
        assert_eq!(state.focused_dialog().as_deref(), Some("b"));
        b.session.close("disconnected");
        assert_eq!(state.focused_dialog(), None);
        assert!(b
            .session
            .set_workload(state.clone(), Some("late".into()))
            .is_err());
        state.shutdown().await;
        assert!(state.set_focus("late", Some("late".into())).is_err());
        assert_eq!(
            *changes.lock().unwrap(),
            vec![
                (Some("a".into()), false),
                (Some("b".into()), false),
                (Some("a".into()), false),
                (Some("b".into()), false),
                (None, false),
                (None, true)
            ]
        );
    }
}

#[cfg(test)]
mod sender_tests {
    use super::*;
    #[tokio::test]
    async fn slow_hosts_coalesce_pending_focus_and_finish_with_the_shutdown_clear() {
        let (sent, changes) = watch::channel((None, false));
        let (seen, mut received) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let worker = tokio::spawn(forward(changes, move |dialog| {
            let seen = seen.clone();
            let gate = gate.clone();
            async move {
                seen.send(dialog.clone()).unwrap();
                if dialog.as_deref() == Some("first") {
                    gate.notified().await;
                }
            }
        }));
        sent.send_replace((Some("first".into()), false));
        assert_eq!(received.recv().await.unwrap().as_deref(), Some("first"));
        sent.send_replace((Some("skipped".into()), false));
        sent.send_replace((Some("latest".into()), false));
        release.notify_one();
        assert_eq!(received.recv().await.unwrap().as_deref(), Some("latest"));
        sent.send_replace((None, true));
        assert_eq!(received.recv().await.unwrap(), None);
        worker.await.unwrap();
    }
}

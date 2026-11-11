//! Finite client operations run independently of the request connection so
//! progress and cancellation cannot wait behind the operation they control.
use crate::desktop_api::DesktopResult;
use crate::desktop_session::Session;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Weak,
};
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}
/// Cancelled tasks remain active until their futures have actually been dropped.
#[cfg(any(unix, windows))]
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst) != 0
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub(crate) enum TaskEvent {
    Event(Value),
    Complete(DesktopResult),
}

/// A finite operation's private progress or native-action callback.
#[derive(Clone)]
pub(crate) struct TaskOutput {
    owner: Weak<Session>,
    sink: String,
}

impl TaskOutput {
    /// Reports progress without blocking the producer on its native consumer.
    pub(crate) fn emit(&self, payload: impl Serialize) -> bool {
        let Some(owner) = self.owner.upgrade() else {
            return false;
        };
        match serde_json::to_value(payload) {
            Ok(value) => owner.emit(&self.sink, TaskEvent::Event(value)),
            Err(_) => {
                owner.close("Could not encode a desktop task event");
                false
            }
        }
    }
}

/// Registers before starting, and cancels only this task on client/callback loss.
pub(crate) fn start<F, Fut>(owner: &Arc<Session>, sink: String, work: F) -> Result<(), String>
where
    F: FnOnce(TaskOutput) -> Fut + Send + 'static,
    Fut: Future<Output = DesktopResult> + Send + 'static,
{
    owner.begin(&sink)?;
    let key = format!("task:{sink}");
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    owner.register(key.clone(), sink.clone(), move || {
        let _ = cancel.send(());
    })?;
    let output = TaskOutput {
        owner: Arc::downgrade(owner),
        sink,
    };
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    let active = Active;
    rovik_core::rt::spawn(Box::pin(async move {
        let _active = active;
        let operation =
            std::panic::AssertUnwindSafe(async { work(output.clone()).await }).catch_unwind();
        let result = tokio::select! {
            biased;
            _ = cancelled => return,
            result = operation => result.unwrap_or_else(|_| Err(Value::String(
                "Desktop operation failed unexpectedly; refresh its state before retrying".into()
            ))),
        };
        if let Some(owner) = output.owner.upgrade() {
            owner.emit(&output.sink, TaskEvent::Complete(result));
            owner.finished(&key);
        }
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_session::{EventBody, Sessions};

    #[tokio::test]
    async fn progress_precedes_completion_and_panics_finish_explicitly() {
        let lease = Sessions::default().create();
        let mut events = lease.session.events().unwrap();
        start(&lease.session, "ok".into(), |out| async move {
            assert!(out.emit("progress"));
            Ok(Value::Bool(true))
        })
        .unwrap();
        for expected in [
            serde_json::json!({"kind":"event","value":"progress"}),
            serde_json::json!({"kind":"complete","value":{"Ok":true}}),
        ] {
            let event = events.next().await.unwrap();
            assert!(matches!(event.body, EventBody::Data(value) if value == expected));
        }
        assert!(matches!(events.next().await.unwrap().body, EventBody::End));
        start(&lease.session, "panic".into(), |_| async {
            panic!("test panic")
        })
        .unwrap();
        let EventBody::Data(value) = events.next().await.unwrap().body else {
            panic!("missing completion")
        };
        assert!(value["value"]["Err"]
            .as_str()
            .unwrap()
            .contains("unexpectedly"));
        assert!(matches!(events.next().await.unwrap().body, EventBody::End));
    }

    #[tokio::test]
    async fn disconnect_cancels_only_its_own_pending_task() {
        let sessions = Sessions::default();
        let a = sessions.create();
        let b = sessions.create();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (dropped, stopped) = tokio::sync::oneshot::channel::<()>();
        start(&a.session, "pending".into(), |_| async move {
            let _guard = dropped;
            started.send(()).unwrap();
            std::future::pending().await
        })
        .unwrap();
        ready.await.unwrap();
        drop(a);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), stopped)
                .await
                .unwrap()
                .is_err()
        );
        let mut events = b.session.events().unwrap();
        start(&b.session, "other".into(), |_| async { Ok(Value::Null) }).unwrap();
        assert!(matches!(
            events.next().await.unwrap().body,
            EventBody::Data(_)
        ));
        assert!(!b.session.is_closed());
    }
}

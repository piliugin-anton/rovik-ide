//! Native callback delivery for embedded and IPC desktop clients.
use crate::desktop_session::{DesktopEvent, EventBody, SessionEvents};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::OnceCell;

type Callback = Arc<dyn Fn(Value) -> bool + Send + Sync>;
type ErrorSink = Arc<dyn Fn(String) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub(crate) enum EventSource {
    Embedded(SessionEvents),
    #[cfg(any(unix, windows))]
    Local(crate::local_backend::rpc::EventStream),
}

impl EventSource {
    async fn next(&mut self) -> Result<DesktopEvent, String> {
        match self {
            Self::Embedded(events) => events.next().await,
            #[cfg(any(unix, windows))]
            Self::Local(events) => events.next().await,
        }
    }

    async fn reject(&mut self, sink: String) -> Result<(), String> {
        match self {
            Self::Embedded(events) => {
                events.session().cancel_sink(&sink);
                Ok(())
            }
            #[cfg(any(unix, windows))]
            Self::Local(events) => events.reject(sink).await,
        }
    }
}

struct Shared {
    callbacks: Mutex<HashMap<String, Callback>>,
    failure: Mutex<Option<String>>,
    report: Mutex<ErrorSink>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            callbacks: Mutex::new(HashMap::new()),
            failure: Mutex::new(None),
            report: Mutex::new(Arc::new(|reason| {
                log::warn!("Desktop events disconnected: {reason}")
            })),
        }
    }
}

#[derive(Default)]
pub(crate) struct EventRouter {
    shared: Arc<Shared>,
    started: OnceCell<()>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl EventRouter {
    pub(crate) fn on_error(&self, report: impl Fn(String) + Send + Sync + 'static) {
        *lock(&self.shared.report) = Arc::new(report);
    }

    pub(crate) fn failure(&self) -> Option<String> {
        lock(&self.shared.failure).clone()
    }

    pub(crate) fn close(&self) {
        let mut failure = lock(&self.shared.failure);
        *failure = Some("Desktop client detached".into());
        lock(&self.shared.callbacks).clear();
        if let Some(task) = lock(&self.task).take() {
            task.abort();
        }
    }

    pub(crate) async fn start<F, Fut>(&self, open: F) -> Result<(), String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<EventSource, String>>,
    {
        if let Some(failure) = self.failure() {
            return Err(failure);
        }
        self.started
            .get_or_try_init(|| async {
                let mut source = open().await?;
                // Serialize installation with close, including a close while
                // opening the independent event connection was still pending.
                let failure = lock(&self.shared.failure);
                if let Some(reason) = &*failure {
                    return Err(reason.clone());
                }
                let shared = self.shared.clone();
                let task = rovik_core::rt::spawn(async move {
                    let reason = loop {
                        let event = match source.next().await {
                            Ok(event) => event,
                            Err(error) => break error,
                        };
                        match event.body {
                            EventBody::End => {
                                lock(&shared.callbacks).remove(&event.sink);
                            }
                            EventBody::Data(value) => {
                                let callback = lock(&shared.callbacks).get(&event.sink).cloned();
                                if !callback.is_some_and(|callback| callback(value)) {
                                    lock(&shared.callbacks).remove(&event.sink);
                                    if let Err(error) = source.reject(event.sink).await {
                                        break error;
                                    }
                                }
                            }
                        }
                    };
                    {
                        let mut failure = lock(&shared.failure);
                        if failure.is_some() {
                            return;
                        }
                        *failure = Some(reason.clone());
                    }
                    lock(&shared.callbacks).clear();
                    let report = lock(&shared.report).clone();
                    report(reason);
                });
                *lock(&self.task) = Some(task);
                Ok::<(), String>(())
            })
            .await?;
        Ok(())
    }

    pub(crate) fn register(
        &self,
        callback: impl Fn(Value) -> bool + Send + Sync + 'static,
    ) -> Result<String, String> {
        let failure = lock(&self.shared.failure);
        if let Some(reason) = &*failure {
            return Err(reason.clone());
        }
        let sink = uuid::Uuid::new_v4().to_string();
        lock(&self.shared.callbacks).insert(sink.clone(), Arc::new(callback));
        Ok(sink)
    }

    pub(crate) fn remove(&self, sink: &str) {
        lock(&self.shared.callbacks).remove(sink);
    }
}

impl Drop for EventRouter {
    fn drop(&mut self) {
        if let Some(task) = lock(&self.task).take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_session::Sessions;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn closing_during_subscription_cannot_revive_callbacks() {
        let lease = Sessions::default().create();
        let router = EventRouter::default();
        router.on_error(|reason| panic!("intentional close was reported: {reason}"));
        let (opened, opening) = oneshot::channel();
        let (resume, resumed) = oneshot::channel();
        let start = router.start(|| async {
            let source = EventSource::Embedded(lease.session.events().unwrap());
            opened.send(()).unwrap();
            resumed.await.unwrap();
            Ok(source)
        });
        tokio::pin!(start);
        tokio::select! {
            _ = opening => {},
            result = &mut start => panic!("subscription finished early: {result:?}"),
        }
        router.close();
        assert!(router.register(|_| true).is_err());
        resume.send(()).unwrap();
        assert!(start.await.unwrap_err().contains("detached"));
        assert!(lock(&router.task).is_none());
        assert!(lock(&router.shared.callbacks).is_empty());
    }

    #[tokio::test]
    async fn a_disappearing_callback_closes_its_producer_without_closing_the_session() {
        let lease = Sessions::default().create();
        let router = EventRouter::default();
        router
            .start(|| async { lease.session.events().map(EventSource::Embedded) })
            .await
            .unwrap();
        let (seen, delivered) = oneshot::channel();
        let seen = Mutex::new(Some(seen));
        let sink = router
            .register(move |value| {
                if let Some(seen) = lock(&seen).take() {
                    let _ = seen.send(value);
                }
                false
            })
            .unwrap();
        lease.session.begin(&sink).unwrap();
        let (closed, cleaned) = oneshot::channel();
        lease
            .session
            .register("producer".into(), sink.clone(), move || {
                let _ = closed.send(());
            })
            .unwrap();
        assert!(lease.session.emit(&sink, "first"));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), delivered)
                .await
                .unwrap()
                .unwrap(),
            "first"
        );
        tokio::time::timeout(Duration::from_secs(10), cleaned)
            .await
            .unwrap()
            .unwrap();
        assert!(!lease.session.owns("producer"));
        assert!(!lease.session.is_closed());
    }
}

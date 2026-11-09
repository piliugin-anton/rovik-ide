//! A separate test executable keeps backend process globals isolated from
//! the client's unit tests.
use rovik_client::desktop_backend::DesktopBackend;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::chat_db::{api, store::NewDialog};
use rovik_core::{EventSink, Paths};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl EventSink for Events {
    fn agent(&self, event: &AgentEvent) {
        if let AgentEvent::Notice { message, .. } = event {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

#[tokio::test]
async fn desktop_services_persist_chats_and_publish_without_a_window() {
    let dir = tempfile::tempdir().unwrap();
    let paths = || Paths::new(dir.path().join("data"), dir.path().join("config"));
    let events = Arc::new(Events::default());
    let backend = DesktopBackend::open(paths(), events.clone(), Arc::new(|_| {}), "test")
        .await
        .unwrap();
    api::chat_create_dialog(
        &backend.core.db,
        NewDialog {
            id: "shared-chat".into(),
            title: "Saved without a window".into(),
            created_at: 1,
            updated_at: 1,
            project_id: None,
        },
    )
    .await
    .unwrap();
    backend.core.emit(AgentEvent::Notice {
        dialog_id: Some("shared-chat".into()),
        kind: "info".into(),
        title: "Backend".into(),
        message: "ready".into(),
    });
    assert_eq!(*events.0.lock().unwrap(), vec!["ready"]);
    backend.shutdown().await;
    drop(backend);

    let reopened = DesktopBackend::open(paths(), events, Arc::new(|_| {}), "test")
        .await
        .unwrap();
    let chat = api::chat_get_dialog(&reopened.core.db, "shared-chat".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chat.title, "Saved without a window");
    reopened.shutdown().await;
}

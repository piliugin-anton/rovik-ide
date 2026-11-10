//! A separate test executable keeps backend process globals isolated from
//! the client's unit tests.
#[path = "support/desktop_browser.rs"]
mod desktop_browser;
#[path = "support/desktop_git.rs"]
mod desktop_git;
#[path = "support/desktop_journal.rs"]
mod desktop_journal;
#[cfg(unix)]
#[path = "support/desktop_lsp.rs"]
mod desktop_lsp;
#[path = "support/desktop_operations.rs"]
mod desktop_operations;
#[path = "support/desktop_pages.rs"]
mod desktop_pages;
#[path = "support/desktop_projects.rs"]
mod desktop_projects;
#[path = "support/desktop_tasks.rs"]
mod desktop_tasks;

use rovik_client::desktop_backend::DesktopBackend;
use rovik_client::desktop_connection::DesktopConnection;
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
    desktop_tasks::seed_secrets(&dir.path().join("config"));
    let paths = || Paths::new(dir.path().join("data"), dir.path().join("config"));
    let events = Arc::new(Events::default());
    let backend = DesktopBackend::open(paths(), events.clone(), Arc::new(|_| {}), "test")
        .await
        .unwrap();
    let journal_projects = desktop_journal::seed(&backend.core.db).await;
    let backend = Arc::new(backend);
    let connection = DesktopConnection::embedded(backend.clone());
    desktop_operations::check_filesystem_and_overlay_contract(&connection).await;
    desktop_operations::check_settings_and_chat_contract(&connection).await;
    desktop_operations::check_disk_contract(
        DesktopConnection::embedded(backend.clone()),
        DesktopConnection::embedded(backend.clone()),
    )
    .await;
    use rovik_client::desktop_api::vfs::VfsRequest;
    let other = DesktopConnection::embedded(backend.clone());
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let subscription = connection
        .desktop_stream(
            |sink| VfsRequest::Subscribe { kinds: None, sink }.into(),
            move |batch| sent.send(batch).is_ok(),
        )
        .await
        .unwrap()
        .as_u64()
        .unwrap();
    assert_eq!(
        other
            .desktop(VfsRequest::Unsubscribe { id: subscription }.into())
            .await
            .unwrap(),
        false
    );
    backend
        .core
        .vfs
        .write_unmounted("event-test.txt", b"private delivery")
        .unwrap();
    let batch = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(batch.to_string().contains("event-test.txt"), "{batch}");
    assert_eq!(
        connection
            .desktop(VfsRequest::Unsubscribe { id: subscription }.into())
            .await
            .unwrap(),
        true
    );
    #[cfg(unix)]
    desktop_operations::check_terminal_contract(&connection, &other).await;
    desktop_git::check(&connection, &other, &[std::process::id()]).await;
    #[cfg(unix)]
    desktop_lsp::check(
        DesktopConnection::embedded(backend.clone()),
        DesktopConnection::embedded(backend.clone()),
        &[std::process::id()],
    )
    .await;
    desktop_browser::check(&connection, &other).await;
    desktop_pages::check(&connection, &other).await;
    desktop_tasks::check(&connection, &other).await;
    desktop_journal::check(&connection, &other, journal_projects).await;
    desktop_projects::check(&connection, other).await;
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
    let status: rovik_protocol::agent::AgentStatus = connection
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    assert!(status.runs.is_empty());
    let _: () = connection
        .call(rovik_protocol::Request::ChatRename {
            dialog_id: "shared-chat".into(),
            title: "Renamed through adapter".into(),
        })
        .await
        .unwrap();
    assert!(connection
        .call_on::<()>(
            Some("missing-host"),
            rovik_protocol::Request::ChatRename {
                dialog_id: "shared-chat".into(),
                title: "Must not fall back".into(),
            }
        )
        .await
        .is_err());
    backend.core.emit(AgentEvent::Notice {
        dialog_id: Some("shared-chat".into()),
        kind: "info".into(),
        title: "Backend".into(),
        message: "ready".into(),
    });
    assert_eq!(*events.0.lock().unwrap(), vec!["ready"]);
    backend.shutdown().await;
    drop(connection);
    drop(backend);

    let reopened = DesktopBackend::open(paths(), events, Arc::new(|_| {}), "test")
        .await
        .unwrap();
    let chat = api::chat_get_dialog(&reopened.core.db, "shared-chat".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chat.title, "Renamed through adapter");
    reopened.shutdown().await;
}

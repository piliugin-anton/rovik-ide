//! Actual daemon processes arbitrate startup through the OS lifetime lock.
#![cfg(unix)]

#[path = "support/desktop_browser.rs"]
mod desktop_browser;
#[cfg(unix)]
#[path = "support/desktop_formatter.rs"]
mod desktop_formatter;
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
#[path = "support/desktop_workload.rs"]
mod desktop_workload;

use rovik_client::local_backend::{connect, directory};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn simultaneous_launchers_leave_one_owner_and_clients_share_readiness() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    desktop_tasks::seed_secrets(&config);
    let db = rovik_core::chat_db::DbState::open(&data).unwrap();
    for id in ["a", "b"] {
        rovik_core::chat_db::api::chat_create_dialog(
            &db,
            rovik_core::chat_db::store::NewDialog {
                id: id.into(),
                title: "original".into(),
                created_at: 1,
                updated_at: 1,
                project_id: None,
            },
        )
        .await
        .unwrap();
    }
    let journal_projects = desktop_journal::seed(&db).await;
    drop(db);

    let spawn = || {
        Daemon(
            Command::new(env!("CARGO_BIN_EXE_rovik-desktopd"))
                .arg("--data")
                .arg(&data)
                .arg("--config")
                .arg(&config)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut first = spawn();
    let mut second = spawn();
    let endpoint = directory(&data);
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match connect(&endpoint).await {
                Ok((_, ready)) => break ready,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(e) => panic!("backend discovery failed: {e}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("backend ready");
    // A peer that never acknowledges its protocol must not block other windows.
    let stalled = tokio::net::UnixStream::connect(endpoint.join("rpc.sock"))
        .await
        .unwrap();
    let client_a = rovik_client::local_backend::rpc::Client::connect(&endpoint)
        .await
        .unwrap();
    let client_b = rovik_client::local_backend::rpc::Client::connect(&endpoint)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        client_a.call(rovik_protocol::Request::AgentStatus),
        client_b.call(rovik_protocol::Request::AgentStatus),
    );
    assert!(matches!(
        a.unwrap().unwrap(),
        rovik_protocol::Response::AgentStatus(_)
    ));
    assert!(matches!(
        b.unwrap().unwrap(),
        rovik_protocol::Response::AgentStatus(_)
    ));
    let (a, b) = tokio::join!(
        client_a.call(rovik_protocol::Request::ChatRename {
            dialog_id: "a".into(),
            title: "A changed".into()
        }),
        client_b.call(rovik_protocol::Request::ChatRename {
            dialog_id: "b".into(),
            title: "B changed".into()
        }),
    );
    assert!(matches!(
        a.unwrap().unwrap(),
        rovik_protocol::Response::Done
    ));
    assert!(matches!(
        b.unwrap().unwrap(),
        rovik_protocol::Response::Done
    ));
    assert!(client_a
        .call_host(
            "missing-host",
            rovik_protocol::Request::ChatRename {
                dialog_id: "a".into(),
                title: "must not run locally".into(),
            }
        )
        .await
        .unwrap()
        .is_err());
    // A domain failure is a reply, not a broken connection.
    assert!(client_a
        .call(rovik_protocol::Request::HostInfo)
        .await
        .unwrap()
        .is_err());
    drop(stalled);
    let adapter = rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
        .await
        .unwrap();
    desktop_operations::check_filesystem_and_overlay_contract(&adapter).await;
    desktop_operations::check_settings_and_chat_contract(&adapter).await;
    desktop_operations::check_disk_contract(
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
    )
    .await;
    let other_adapter = rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
        .await
        .unwrap();
    desktop_operations::check_terminal_contract(&adapter, &other_adapter).await;
    desktop_git::check(&adapter, &other_adapter, &[first.0.id(), second.0.id()]).await;
    desktop_lsp::check(
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        &[first.0.id(), second.0.id()],
    )
    .await;
    #[cfg(unix)]
    desktop_formatter::check(
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        &[first.0.id(), second.0.id()],
    )
    .await;
    desktop_workload::check(
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
        rovik_client::desktop_connection::DesktopConnection::connect(&endpoint)
            .await
            .unwrap(),
    )
    .await;
    desktop_browser::check(&adapter, &other_adapter).await;
    desktop_pages::check(&adapter, &other_adapter).await;
    desktop_tasks::check(&adapter, &other_adapter).await;
    desktop_journal::check(&adapter, &other_adapter, journal_projects).await;
    desktop_projects::check(&adapter, other_adapter).await;
    // Close through the independent control while the owner's command socket
    // remains open. This proves cleanup does not depend on observing its EOF.
    let owner = rovik_client::local_backend::rpc::Client::connect(&endpoint)
        .await
        .unwrap();
    let project_root = root.path().join("control-close-project");
    std::fs::create_dir(&project_root).unwrap();
    use rovik_client::desktop_api::projects::ProjectRequest;
    let project = owner
        .desktop(
            ProjectRequest::Open {
                root: project_root.to_string_lossy().into(),
            }
            .into(),
        )
        .await
        .unwrap()
        .unwrap();
    let (mut control, _) = connect(&endpoint).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let close = serde_json::to_vec(&serde_json::json!({
        "t": "closeSession", "session": owner.ready.session,
    }))
    .unwrap();
    for _ in 0..2 {
        control.write_u32(close.len() as u32).await.unwrap();
        control.write_all(&close).await.unwrap();
        let size = control.read_u32().await.unwrap();
        assert!(size < 1024);
        let mut reply = vec![0; size as usize];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&reply).unwrap(),
            serde_json::json!({"t": "sessionClosed"})
        );
    }
    adapter
        .desktop(
            ProjectRequest::Delete {
                project_id: project["project"]["id"].as_str().unwrap().into(),
            }
            .into(),
        )
        .await
        .unwrap();
    assert!(owner
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .is_err());
    drop(control);
    drop(owner);
    let shared_volume = client_b
        .desktop(rovik_client::desktop_api::DesktopRequest::from(
            rovik_client::desktop_api::vfs::VfsRequest::Status,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared_volume["history"]["limits"]["maxBytes"], 4096);
    let status: rovik_protocol::agent::AgentStatus = adapter
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    assert!(status.runs.is_empty());
    let _: () = adapter
        .call(rovik_protocol::Request::ChatRename {
            dialog_id: "a".into(),
            title: "A through desktop adapter".into(),
        })
        .await
        .unwrap();
    // A private event connection cannot control another window's subscription.
    use rovik_client::desktop_api::vfs::VfsRequest;
    use rovik_client::desktop_session::EventBody;
    let mut events = client_a.events().await.unwrap();
    let subscription = client_a
        .desktop(
            VfsRequest::Subscribe {
                kinds: None,
                sink: "window-a".into(),
            }
            .into(),
        )
        .await
        .unwrap()
        .unwrap()
        .as_u64()
        .unwrap();
    assert_eq!(
        client_b
            .desktop(VfsRequest::Unsubscribe { id: subscription }.into())
            .await
            .unwrap()
            .unwrap(),
        false
    );
    assert_eq!(
        client_a
            .desktop(VfsRequest::Unsubscribe { id: subscription }.into())
            .await
            .unwrap()
            .unwrap(),
        true
    );
    let ended = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended.sink, "window-a");
    assert!(matches!(ended.body, EventBody::End));
    // Losing the owner connection terminates its event stream, without touching B.
    drop(adapter);
    drop(client_a);
    assert!(tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap()
        .is_err());
    assert!(matches!(
        client_b
            .call(rovik_protocol::Request::AgentStatus)
            .await
            .unwrap()
            .unwrap(),
        rovik_protocol::Response::AgentStatus(_)
    ));
    let (_, another) = connect(&endpoint).await.unwrap();
    assert_eq!(ready.boot, another.boot);
    assert_ne!(ready.session, another.session);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let a = first.0.try_wait().unwrap();
            let b = second.0.try_wait().unwrap();
            match (a, b) {
                (Some(status), None) | (None, Some(status)) => {
                    assert!(status.success(), "the losing launcher exits cleanly");
                    break;
                }
                (Some(a), Some(b)) => panic!("both backends exited: {a}, {b}"),
                (None, None) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("one launcher yields ownership");
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, ready.boot);
    client_b.close().await.unwrap();
    drop(client_b);
    let returning = rovik_client::local_backend::rpc::Client::connect(&endpoint)
        .await
        .unwrap();
    assert_eq!(
        returning.ready.boot, ready.boot,
        "detaching clients must not restart the backend"
    );
    assert!(returning
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap()
        .is_ok());
    returning.close().await.unwrap();
    drop(first);
    drop(second);
    let db = rovik_core::chat_db::DbState::open(&data).unwrap();
    for (id, title) in [("a", "A through desktop adapter"), ("b", "B changed")] {
        let chat = rovik_core::chat_db::api::chat_get_dialog(&db, id.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chat.title, title, "each request changed only its own chat");
    }
}

/// Developer socket connections are initialized by the daemon without a UI.
#[tokio::test]
async fn daemon_startup_applies_remote_chat_environment_and_shuts_down_cleanly() {
    use rovik_client::{desktop_api::hosts::HostsRequest, desktop_connection::DesktopConnection};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    desktop_tasks::seed_secrets(&config);
    let db = rovik_core::chat_db::DbState::open(&data).unwrap();
    rovik_core::chat_db::api::chat_create_dialog(
        &db,
        rovik_core::chat_db::store::NewDialog {
            id: "startup-chat".into(),
            title: "startup".into(),
            created_at: 1,
            updated_at: 1,
            project_id: None,
        },
    )
    .await
    .unwrap();
    drop(db);
    let socket = root.path().join("developer.sock");
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_rovik-desktopd"))
            .arg("--data")
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .env(
                "ROVIK_REMOTE_CHATS",
                format!("startup-chat={}", socket.display()),
            )
            .env("ROVIK_COMPUTER", "startup-test")
            .env_remove("ROVIK_REMOTE_TOKEN")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let endpoint = directory(&data);
    let client = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(client) = DesktopConnection::connect(&endpoint).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let summaries = client
                .desktop(HostsRequest::Summaries.into())
                .await
                .unwrap();
            if summaries
                .as_array()
                .unwrap()
                .iter()
                .any(|host| host["id"] == socket.to_string_lossy().as_ref())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // No daemon is listening on the developer socket: this fixture uses no
    // network service, credentials, or execution on another host.
    assert_eq!(
        unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
        0
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = daemon.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let db = rovik_core::chat_db::DbState::open(&data).unwrap();
    let host = db
        .read(|connection| rovik_core::chat_db::location::of_chat(connection, "startup-chat"))
        .await
        .unwrap();
    assert_eq!(
        host,
        rovik_core::chat_db::location::Location::Host(socket.to_string_lossy().into_owned())
    );
}

#[cfg(target_os = "linux")]
#[path = "support/desktop_attach.rs"]
mod desktop_attach;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn automatic_attachment_shares_a_host_and_does_not_relaunch_when_windows_reopen() {
    desktop_attach::check(
        std::path::Path::new(env!("CARGO_BIN_EXE_rovik-desktopd")),
        None,
    )
    .await;
}

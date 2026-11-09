//! Actual daemon processes arbitrate startup through the OS lifetime lock.
#![cfg(unix)]

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
    drop(adapter);
    drop(client_a);
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
    drop(client_b);
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

//! Attachment contract shared by the standalone and packaged executable tests.
use rovik_client::local_backend::{connect, directory};
use std::{path::Path, process::Command, time::Duration};

/// Two launchers share one host, which remains after all client sessions close.
pub async fn check(executable: &Path, flag: Option<&str>) {
    use rovik_client::{desktop_connection::DesktopConnection, local_backend::Lease};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    let command = || {
        let mut command = Command::new(executable);
        if let Some(flag) = flag {
            command.arg(flag);
        }
        command.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
        command
    };
    let (a, b) = tokio::join!(
        DesktopConnection::attach(command(), &data, &config),
        DesktopConnection::attach(command(), &data, &config)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let endpoint = directory(&data);
    let (stream, ready) = connect(&endpoint).await.unwrap();
    let pid = stream.peer_cred().unwrap().pid().unwrap();
    // This peer is the host launched for this test's unique private directory.
    struct Host(i32);
    impl Drop for Host {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGTERM);
            }
        }
    }
    let host = Host(pid);
    drop(stream);
    a.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    b.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    assert!(Lease::acquire(&endpoint).unwrap().is_none());
    use rovik_client::desktop_api::projects::ProjectRequest;
    let folder = root.path().join("project");
    std::fs::create_dir(&folder).unwrap();
    let folder = folder.to_string_lossy().into_owned();
    let project = a
        .desktop(
            ProjectRequest::Open {
                root: folder.clone(),
            }
            .into(),
        )
        .await
        .unwrap();
    let project_id = project["project"]["id"].as_str().unwrap().to_string();
    a.reconnect(
        Command::new("/missing/must-not-run"),
        &data,
        &config,
        Some((folder.clone(), project_id.clone())),
    )
    .await
    .unwrap();
    assert!(a.usable());
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, ready.boot);
    assert!(b
        .desktop(
            ProjectRequest::Delete {
                project_id: project_id.clone()
            }
            .into()
        )
        .await
        .unwrap_err()
        .contains("open in another window"));
    b.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    // A rejected snapshot leaves the connection unavailable, not half restored.
    assert!(a
        .reconnect(
            command(),
            &data,
            &config,
            Some((folder.clone(), "wrong-id".into()))
        )
        .await
        .unwrap_err()
        .contains("identity changed"));
    assert!(!a.usable());
    assert!(a
        .request(rovik_protocol::Request::AgentStatus)
        .await
        .is_err());
    a.reconnect(
        command(),
        &data,
        &config,
        Some((folder.clone(), project_id.clone())),
    )
    .await
    .unwrap();
    a.close().await.unwrap();
    b.close().await.unwrap();
    assert!(a
        .reconnect(command(), &data, &config, None)
        .await
        .unwrap_err()
        .contains("detached"));
    // A missing launch executable proves existing readiness is enough to attach.
    let c = DesktopConnection::attach(Command::new("/missing/must-not-run"), &data, &config)
        .await
        .unwrap();
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, ready.boot);
    c.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    // Abrupt loss must release the OS lease before a new process can own execution.
    assert_eq!(unsafe { libc::kill(host.0, libc::SIGKILL) }, 0);
    std::mem::forget(host);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if Lease::acquire(&endpoint).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(c
        .request(rovik_protocol::Request::AgentStatus)
        .await
        .is_err());
    assert!(!c.usable());
    c.reconnect(command(), &data, &config, Some((folder, project_id)))
        .await
        .unwrap();
    let (stream, restarted) = connect(&endpoint).await.unwrap();
    let host = Host(stream.peer_cred().unwrap().pid().unwrap());
    assert_ne!(restarted.boot, ready.boot);
    c.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    c.close().await.unwrap();
    drop(stream);
    drop(host);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if Lease::acquire(&endpoint).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

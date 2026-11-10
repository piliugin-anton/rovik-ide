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
    a.close().await.unwrap();
    b.close().await.unwrap();
    // A missing launch executable proves existing readiness is enough to attach.
    let c = DesktopConnection::attach(Command::new("/missing/must-not-run"), &data, &config)
        .await
        .unwrap();
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, ready.boot);
    c.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    c.close().await.unwrap();
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

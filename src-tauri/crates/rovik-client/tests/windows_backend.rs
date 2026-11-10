//! Native process and named-pipe coverage on hosted Windows CI; no WSL required.
#![cfg(windows)]

use rovik_client::local_backend::{directory, rpc::Client, Lease};
use std::{process::Child, time::Duration};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn competing_processes_share_one_backend_and_detaching_keeps_it_alive() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    let spawn = || {
        Daemon(
            rovik_core::process::command(env!("CARGO_BIN_EXE_rovik-desktopd"))
                .arg("--data")
                .arg(&data)
                .arg("--config")
                .arg(&config)
                .spawn()
                .unwrap(),
        )
    };
    let mut first = spawn();
    let mut second = spawn();
    let endpoint = directory(&data);
    let a = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match Client::connect(&endpoint).await {
                Ok(client) => break client,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("backend startup failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("backend ready");
    let b = Client::connect(&endpoint).await.unwrap();
    assert_eq!(a.ready.boot, b.ready.boot);
    assert_ne!(a.ready.session, b.ready.session);
    assert!(Lease::acquire(&endpoint).unwrap().is_none());
    // A losing launcher exits successfully without initializing another Core.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let exited = [first.0.try_wait().unwrap(), second.0.try_wait().unwrap()];
            if exited.iter().any(Option::is_some) {
                assert_eq!(exited.iter().flatten().count(), 1);
                assert!(exited.iter().flatten().all(|status| status.success()));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    a.close().await.unwrap();
    b.call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap()
        .unwrap();
    let boot = b.ready.boot.clone();
    b.close().await.unwrap();
    drop(a);
    drop(b);
    let reopened = Client::connect(&endpoint).await.unwrap();
    assert_eq!(reopened.ready.boot, boot);
    reopened
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap()
        .unwrap();
    reopened.close().await.unwrap();
    drop(reopened);
    drop(first);
    drop(second);
    // Process death releases ownership without stale file/socket cleanup.
    assert!(Lease::acquire(&endpoint).unwrap().is_some());
}

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
}

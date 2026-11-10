//! Native process and named-pipe coverage on hosted Windows CI; no WSL required.
#![cfg(windows)]

use rovik_client::local_backend::{directory, rpc::Client, Lease};
use std::{path::PathBuf, process::Child, time::Duration};

fn executable() -> PathBuf {
    match std::env::var_os("ROVIK_PACKAGED_BACKEND") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_absolute() && path.is_file(),
                "installed backend is missing: {}",
                path.display()
            );
            path
        }
        None => PathBuf::from(env!("CARGO_BIN_EXE_rovik-desktopd")),
    }
}

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
            rovik_core::process::command(executable())
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
    assert_eq!(a.ready.version, env!("CARGO_PKG_VERSION"));
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

#[path = "support/desktop_attach.rs"]
mod desktop_attach;

#[tokio::test]
async fn windows_recover_after_a_crash_with_stale_siblings_and_persisted_state() {
    desktop_attach::check(&executable(), None).await;
}

#[path = "support/desktop_upgrade.rs"]
mod desktop_upgrade;

#[tokio::test]
async fn windows_upgrade_preserves_a_busy_owner_and_waits_for_clean_shutdown() {
    desktop_upgrade::check(&executable()).await;
}

#[tokio::test]
async fn installing_a_new_source_preserves_the_running_backend_image() {
    use rovik_client::local_backend::ensure_backend;
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("installed.exe");
    let original = executable();
    std::fs::copy(&original, &source).unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    let endpoint = directory(&data);
    let (stream, ready) = ensure_backend(&source, &data, &config).await.unwrap();
    let host = desktop_attach::desktop_host::Host::peer(&stream);
    // Windows refuses deleting a running executable. The installation copy must
    // be replaceable while the backend still owns its sessions and resources.
    std::fs::remove_file(&source).unwrap();
    let (sibling, current) = ensure_backend(&source, &data, &config).await.unwrap();
    assert_eq!(ready.boot, current.boot);
    let client = Client::connect(&endpoint).await.unwrap();
    client
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap()
        .unwrap();
    client.close().await.unwrap();
    drop(client);
    drop(sibling);
    drop(stream);
    drop(host);
    assert!(Lease::acquire(&endpoint).unwrap().is_some());
    // A PE overlay changes the image identity while retaining a valid executable.
    std::fs::copy(&original, &source).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap()
        .write_all(b"replacement fixture")
        .unwrap();
    let (stream, next) = ensure_backend(&source, &data, &config).await.unwrap();
    let host = desktop_attach::desktop_host::Host::peer(&stream);
    assert_ne!(ready.boot, next.boot);
    assert_eq!(
        std::fs::read_dir(endpoint.join("images")).unwrap().count(),
        2
    );
    std::fs::remove_file(source).unwrap();
    drop(stream);
    drop(host);
}

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
        1
    );
    std::fs::remove_file(source).unwrap();
    drop(stream);
    drop(host);
}

#[path = "support/desktop_standalone.rs"]
mod desktop_standalone;

#[tokio::test]
async fn standalone_ui_sessions_recover_without_an_embedded_fallback() {
    desktop_standalone::check(&executable()).await;
}

#[tokio::test]
async fn an_explicit_update_replaces_only_the_approved_backend_and_keeps_saved_chats() {
    desktop_upgrade::check_restart(&executable()).await;
}

#[tokio::test]
#[ignore = "requires a built NSIS installer and disposable installation"]
async fn installer_preserves_a_busy_backend_and_saved_edits() {
    use rovik_client::{
        desktop_api::chat::ChatRequest,
        desktop_connection::DesktopConnection,
        local_backend::{
            connect,
            control::{negotiate, Reply},
            PROTOCOL,
        },
    };
    use std::{io::Write, os::windows::process::CommandExt};
    let installer =
        PathBuf::from(std::env::var_os("ROVIK_TEST_INSTALLER").expect("NSIS installer required"));
    let installed = PathBuf::from(
        std::env::var_os("ROVIK_PACKAGED_BACKEND").expect("installed backend required"),
    );
    let install_dir = installed.parent().unwrap().parent().unwrap();
    assert!(installer.is_absolute() && installer.is_file());
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    let window = DesktopConnection::attach_backend(&installed, &data, &config)
        .await
        .unwrap();
    let endpoint = directory(&data);
    let (stream, first) = connect(&endpoint).await.unwrap();
    let host = desktop_attach::desktop_host::Host::peer(&stream);
    window
        .desktop(
            ChatRequest::CreateDialog {
                id: "upgrade".into(),
                title: "Preserved".into(),
                created_at: 1,
                updated_at: 1,
                project_id: None,
            }
            .into(),
        )
        .await
        .unwrap();
    window
        .desktop(
            ChatRequest::SaveComposerDraft {
                dialog_id: "upgrade".into(),
                content: "unfinished saved edit".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    let read = || ChatRequest::GetComposerDraft {
        dialog_id: "upgrade".into(),
    };
    let saved = window.desktop(read().into()).await.unwrap();
    assert!(matches!(
        negotiate(&endpoint, env!("CARGO_PKG_VERSION"), PROTOCOL + 1)
            .await
            .unwrap()
            .unwrap()
            .1,
        Reply::Busy(_)
    ));
    // Require an actual resource replacement even when reapplying this version.
    // A valid PE overlay changes the installation copy, never the running image.
    let installed_len = std::fs::metadata(&installed).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&installed)
        .unwrap()
        .write_all(b"installer replacement fixture")
        .unwrap();
    // NSIS requires /D to be the final, unquoted argument, including spaces.
    // No shell is involved. Use the updater's /UPDATE behavior without /R.
    let mut command = rovik_core::process::command(installer);
    command
        .args(["/S", "/UPDATE"])
        .raw_arg(format!("/D={}", install_dir.display()));
    let child = command.spawn().unwrap();
    let mut installer = Daemon(child);
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(status) = installer.0.try_wait().unwrap() {
                assert!(status.success(), "NSIS update failed: {status}");
                break;
            }
            // The retained backend must remain usable throughout installation.
            window
                .request(rovik_protocol::Request::AgentStatus)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::metadata(&installed).unwrap().len(),
        installed_len,
        "NSIS must replace the modified installation resource"
    );
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, first.boot);
    assert_eq!(window.desktop(read().into()).await.unwrap(), saved);
    assert!(matches!(
        negotiate(&endpoint, env!("CARGO_PKG_VERSION"), PROTOCOL + 1)
            .await
            .unwrap()
            .unwrap()
            .1,
        Reply::Busy(_)
    ));
    window.close().await.unwrap();
    drop(stream);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match negotiate(&endpoint, env!("CARGO_PKG_VERSION"), PROTOCOL + 1)
                .await
                .unwrap()
                .unwrap()
                .1
            {
                Reply::Retiring => break,
                Reply::Busy(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                Reply::Current => panic!("upgrade was not negotiated"),
            }
        }
    })
    .await
    .unwrap();
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
    drop(host);
    let next = DesktopConnection::attach_backend(&installed, &data, &config)
        .await
        .unwrap();
    let (stream, ready) = connect(&endpoint).await.unwrap();
    let host = desktop_attach::desktop_host::Host::peer(&stream);
    assert_ne!(ready.boot, first.boot);
    assert_eq!(next.desktop(read().into()).await.unwrap(), saved);
    next.close().await.unwrap();
    drop(stream);
    drop(host);
}

#[tokio::test]
async fn closing_the_development_launchers_output_keeps_other_windows_connected() {
    desktop_upgrade::check_launcher(&executable(), None).await;
}

//! The same guarded-upgrade contract runs over Unix sockets and Windows pipes.
#[cfg(any(target_os = "linux", windows))]
use super::desktop_attach::desktop_host;
use rovik_client::local_backend::{
    connect,
    control::{negotiate, Reply},
    directory, Lease, PROTOCOL,
};
use std::{
    path::Path,
    process::{Child, Stdio},
    time::Duration,
};
#[cfg(all(unix, not(target_os = "linux")))]
#[path = "desktop_host.rs"]
mod desktop_host;
#[cfg(unix)]
use super::desktop_tasks::desktop_secrets;
#[cfg(windows)]
#[path = "desktop_secrets.rs"]
mod desktop_secrets;
struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn upgrade(dir: &Path, boot: &str) -> Reply {
    let (hello, reply) = negotiate(dir, env!("CARGO_PKG_VERSION"), PROTOCOL + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hello.boot, boot);
    reply
}
pub async fn check(executable: &Path) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    desktop_secrets::seed_secrets(&config);
    let mut daemon = Daemon(
        rovik_core::process::command(executable)
            .arg("--data")
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .env_remove("ROVIK_REMOTE_CHATS")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let dir = directory(&data);
    let (client, ready) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(connected) = connect(&dir).await {
                break connected;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    use rovik_client::{desktop_api::chat::ChatRequest, desktop_connection::DesktopConnection};
    let window =
        DesktopConnection::attach(rovik_core::process::command(executable), &data, &config)
            .await
            .unwrap();
    window
        .desktop(
            ChatRequest::CreateDialog {
                id: "upgrade-chat".into(),
                title: "Saved before upgrade".into(),
                created_at: 1,
                updated_at: 1,
                project_id: None,
            }
            .into(),
        )
        .await
        .unwrap();
    let read_chat = || ChatRequest::GetDialog {
        dialog_id: "upgrade-chat".into(),
    };
    let saved = window.desktop(read_chat().into()).await.unwrap();
    assert!(!saved.is_null());
    let refused = upgrade(&dir, &ready.boot).await;
    assert!(
        matches!(refused, Reply::Busy(ref reason) if reason.contains("connected")),
        "{refused:?}"
    );
    assert!(Lease::acquire(&dir).unwrap().is_none());
    let (other, current) = connect(&dir).await.unwrap();
    assert_eq!(ready.boot, current.boot);
    // Refusal leaves the original session usable and its saved data intact.
    assert_eq!(window.desktop(read_chat().into()).await.unwrap(), saved);
    window.close().await.unwrap();
    drop(other);
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = upgrade(&dir, &ready.boot).await;
            if matches!(reply, Reply::Retiring) {
                break;
            }
            assert!(matches!(reply, Reply::Busy(_)), "{reply:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
    let lease = Lease::acquire(&dir)
        .unwrap()
        .expect("shutdown released ownership");
    assert_eq!(
        std::fs::read_to_string(data.join("chats.db.running")).unwrap(),
        "clean"
    );
    #[cfg(unix)]
    {
        assert!(!dir.join("rpc.sock").exists());
        assert!(!dir.join("control.sock").exists());
    }
    drop(lease);
    // A replacement can start only after the old owner finished flushing.
    let (replacement, next) =
        rovik_client::local_backend::ensure_backend(executable, &data, &config)
            .await
            .unwrap();
    assert_ne!(ready.boot, next.boot);
    let host = desktop_host::Host::peer(&replacement);
    let window =
        DesktopConnection::attach(rovik_core::process::command(executable), &data, &config)
            .await
            .unwrap();
    assert_eq!(window.desktop(read_chat().into()).await.unwrap(), saved);
    window.close().await.unwrap();
    drop(replacement);
    drop(host);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if Lease::acquire(&dir).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

//! Session lifecycle through the executable-based launcher used by Windows UI.
/// Verifies detach, crash recovery, and refusal to construct an embedded fallback.
pub async fn check(executable: &std::path::Path) {
    use rovik_client::{desktop_connection::DesktopConnection, local_backend::connect};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    #[cfg(windows)]
    {
        let legacy = rovik_core::os_lock::try_hold(&data.join("slots/slot-1.lock"))
            .unwrap()
            .unwrap();
        let error = DesktopConnection::attach_backend(executable, &data, &config)
            .await
            .err()
            .expect("older UI must block shared startup");
        assert!(error.to_string().contains("Close older Rovik windows"));
        assert!(!data.join("chats.db").exists());
        drop(legacy);
    }
    let (a, b) = tokio::join!(
        DesktopConnection::attach_backend(executable, &data, &config),
        DesktopConnection::attach_backend(executable, &data, &config)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let endpoint = rovik_client::local_backend::directory(&data);
    assert!(std::fs::read_to_string(endpoint.join("backend.log"))
        .unwrap()
        .contains("Backend ready pid="));
    let (stream, first) = connect(&endpoint).await.unwrap();
    let host = super::desktop_attach::desktop_host::Host::peer(&stream);
    drop(stream);
    a.close().await.unwrap();
    b.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    host.crash();
    assert!(b
        .request(rovik_protocol::Request::AgentStatus)
        .await
        .is_err());
    let diagnostic = std::fs::read_to_string(endpoint.join("connection.log")).unwrap();
    assert!(diagnostic.contains("operation=agent_status"));
    assert!(diagnostic.contains("outcome=failed"));
    assert!(diagnostic.contains(&first.boot));
    b.reconnect_backend(executable, &data, &config, None)
        .await
        .unwrap();
    let (stream, next) = connect(&endpoint).await.unwrap();
    let host = super::desktop_attach::desktop_host::Host::peer(&stream);
    assert_ne!(first.boot, next.boot);
    let diagnostic = std::fs::read_to_string(endpoint.join("connection.log")).unwrap();
    assert!(diagnostic.contains(&first.boot));
    assert!(
        diagnostic.contains(&next.boot),
        "reconnect retains both backend identities"
    );
    b.request(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
    b.close().await.unwrap();
    drop(stream);
    drop(host);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if rovik_client::local_backend::Lease::acquire(&endpoint)
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        DesktopConnection::attach_backend(&root.path().join("missing.exe"), &data, &config)
            .await
            .is_err()
    );
}

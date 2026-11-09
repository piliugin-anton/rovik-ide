use rovik_client::desktop_api::{overlay::OverlayRequest, vfs::VfsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;

/// The same contract is exercised against embedded services and a real daemon.
pub async fn check_filesystem_and_overlay_contract(connection: &DesktopConnection) {
    let status = connection
        .desktop(DesktopRequest::Overlay(OverlayRequest::Status {
            dialog_id: "no-overlay".into(),
        }))
        .await
        .unwrap();
    assert_eq!(status["isolated"], false);
    assert_eq!(status["files"], serde_json::json!([]));
    let files = connection
        .desktop(DesktopRequest::Vfs(VfsRequest::List { path: None }))
        .await
        .unwrap();
    assert!(files.is_array());
    let error = connection
        .desktop(DesktopRequest::Vfs(VfsRequest::ReadFile {
            path: "../outside".into(),
        }))
        .await
        .unwrap_err();
    assert!(!error.is_empty());
    // An application error must not invalidate the connection.
    let status = connection
        .desktop(DesktopRequest::Vfs(VfsRequest::Status))
        .await
        .unwrap();
    assert!(status.get("mountpoint").is_some());
    connection
        .desktop(DesktopRequest::Vfs(VfsRequest::SetHistoryLimits {
            limits: rovik_core::vfs::history::HistoryLimits {
                max_bytes: 4096,
                per_file: 3,
                max_file_size: 1024,
            },
        }))
        .await
        .unwrap();
    let status = connection
        .desktop(DesktopRequest::Vfs(VfsRequest::Status))
        .await
        .unwrap();
    assert_eq!(status["history"]["limits"]["maxBytes"], 4096);
    assert_eq!(status["history"]["limits"]["perFile"], 3);
    assert!(connection
        .desktop(DesktopRequest::Overlay(OverlayRequest::File {
            dialog_id: "no-overlay".into(),
            path: "missing".into(),
        }))
        .await
        .is_err());
    let _: rovik_protocol::agent::AgentStatus = connection
        .call(rovik_protocol::Request::AgentStatus)
        .await
        .unwrap();
}

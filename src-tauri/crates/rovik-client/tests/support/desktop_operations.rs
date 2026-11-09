use rovik_client::desktop_api::{overlay::OverlayRequest, vfs::VfsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;

/// The same contract is exercised against embedded services and a real daemon.
pub async fn check_filesystem_and_overlay_contract(connection: &DesktopConnection) {
    let status = connection
        .desktop(DesktopRequest::from(OverlayRequest::Status {
            dialog_id: "no-overlay".into(),
        }))
        .await
        .unwrap();
    assert_eq!(status["isolated"], false);
    assert_eq!(status["files"], serde_json::json!([]));
    let files = connection
        .desktop(DesktopRequest::from(VfsRequest::List { path: None }))
        .await
        .unwrap();
    assert!(files.is_array());
    let error = connection
        .desktop(DesktopRequest::from(VfsRequest::ReadFile {
            path: "../outside".into(),
        }))
        .await
        .unwrap_err();
    assert!(!error.is_empty());
    // An application error must not invalidate the connection.
    let status = connection
        .desktop(DesktopRequest::from(VfsRequest::Status))
        .await
        .unwrap();
    assert!(status.get("mountpoint").is_some());
    connection
        .desktop(DesktopRequest::from(VfsRequest::SetHistoryLimits {
            limits: rovik_core::vfs::history::HistoryLimits {
                max_bytes: 4096,
                per_file: 3,
                max_file_size: 1024,
            },
        }))
        .await
        .unwrap();
    let status = connection
        .desktop(DesktopRequest::from(VfsRequest::Status))
        .await
        .unwrap();
    assert_eq!(status["history"]["limits"]["maxBytes"], 4096);
    assert_eq!(status["history"]["limits"]["perFile"], 3);
    assert!(connection
        .desktop(DesktopRequest::from(OverlayRequest::File {
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

/// Persistent mutations and structured failures must survive the IPC boundary.
pub async fn check_settings_and_chat_contract(connection: &DesktopConnection) {
    use rovik_client::desktop_api::{
        chat::ChatRequest, hosts::HostsRequest, mcp::McpRequest, memory::MemoryRequest,
        settings::SettingsRequest,
    };
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let settings = serde_json::json!({"checks": ["cargo test"]});
    connection
        .desktop_raw(DesktopRequest::from(SettingsRequest::SaveProject {
            root: root.clone(),
            settings: settings.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(
        connection
            .desktop_raw(DesktopRequest::from(SettingsRequest::LoadProject {
                root: root.clone()
            }))
            .await
            .unwrap(),
        settings
    );
    let memories = connection
        .desktop_raw(DesktopRequest::from(MemoryRequest::List {
            root,
            query: String::new(),
            before: None,
        }))
        .await
        .unwrap();
    assert_eq!(memories, serde_json::json!([]));
    connection
        .desktop_raw(DesktopRequest::from(ChatRequest::CreateDialog {
            id: "rpc-created".into(),
            title: "Created over desktop RPC".into(),
            created_at: 1,
            updated_at: 1,
            project_id: None,
        }))
        .await
        .unwrap();
    let chat = connection
        .desktop_raw(DesktopRequest::from(ChatRequest::GetDialog {
            dialog_id: "rpc-created".into(),
        }))
        .await
        .unwrap();
    assert_eq!(chat["title"], "Created over desktop RPC");
    connection
        .desktop_raw(DesktopRequest::from(ChatRequest::SaveComposerDraft {
            dialog_id: "rpc-created".into(),
            content: "draft from a client".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        connection
            .desktop_raw(DesktopRequest::from(ChatRequest::GetComposerDraft {
                dialog_id: "rpc-created".into()
            }))
            .await
            .unwrap(),
        "draft from a client"
    );
    let error = connection
        .desktop_raw(DesktopRequest::from(ChatRequest::StoreAttachment {
            bytes: b"not an image".to_vec(),
        }))
        .await
        .unwrap_err();
    assert!(error.as_str().unwrap().contains("can be attached"));
    let error = connection
        .desktop_raw(DesktopRequest::from(HostsRequest::Upgrade {
            id: "missing-host".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(error["kind"], "failed");
    assert!(error["message"].is_string());
    let servers = connection
        .desktop_raw(DesktopRequest::from(McpRequest::Import {
            text: r#"{"mcpServers":{"demo":{"command":"demo-cli"}}}"#.into(),
            taken: vec![],
        }))
        .await
        .unwrap();
    assert_eq!(servers.as_array().unwrap().len(), 1);
    assert_eq!(servers[0]["transport"]["command"], "demo-cli");
}

/// Real PTY output uses the same private callback in embedded and daemon mode.
#[cfg(unix)]
pub async fn check_terminal_contract(owner: &DesktopConnection, other: &DesktopConnection) {
    use rovik_client::desktop_api::terminal::TerminalRequest;
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let terminal = owner
        .desktop_stream(
            |sink| {
                TerminalRequest::Spawn {
                    cwd: None,
                    cols: Some(80),
                    rows: Some(24),
                    sink,
                }
                .into()
            },
            move |event| sent.send(event).is_ok(),
        )
        .await
        .unwrap();
    let id = terminal["id"].as_str().unwrap().to_string();
    assert!(other
        .desktop(
            TerminalRequest::Write {
                id: id.clone(),
                data: "must not run".into()
            }
            .into()
        )
        .await
        .is_err());
    assert!(other
        .desktop(
            TerminalRequest::Resize {
                id: id.clone(),
                cols: 90,
                rows: 30
            }
            .into()
        )
        .await
        .is_err());
    // Another window's close is harmless, and the owner can still use its shell.
    other
        .desktop(TerminalRequest::Close { id: id.clone() }.into())
        .await
        .unwrap();
    owner
        .desktop(
            TerminalRequest::Resize {
                id: id.clone(),
                cols: 100,
                rows: 40,
            }
            .into(),
        )
        .await
        .unwrap();
    // The marker never appears verbatim in echoed input: only execution can produce it.
    owner
        .desktop(
            TerminalRequest::Write {
                id: id.clone(),
                data: "printf 'rpc-%s\\n' terminal-ok\nexit 7\n".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    let (text, code) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut text = String::new();
        loop {
            let event = received
                .recv()
                .await
                .expect("terminal callback ended before its exit");
            match event["type"].as_str() {
                Some("output") => text.push_str(event["data"].as_str().unwrap()),
                Some("exit") => break (text, event["code"].as_u64()),
                _ => panic!("unexpected terminal event: {event}"),
            }
        }
    })
    .await
    .expect("terminal exited");
    assert!(text.contains("rpc-terminal-ok"), "{text}");
    assert_eq!(code, Some(7));
    assert!(owner
        .desktop(
            TerminalRequest::Write {
                id: id.clone(),
                data: "late".into()
            }
            .into()
        )
        .await
        .is_err());
    owner
        .desktop(TerminalRequest::Close { id }.into())
        .await
        .unwrap();
}

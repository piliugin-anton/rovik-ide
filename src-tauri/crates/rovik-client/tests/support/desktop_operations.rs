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
        .desktop_task(
            |sink| TerminalRequest::Write {
                id: id.clone(),
                data: "must not run".into(),
                sink,
            }
            .into(),
            |_| Ok(())
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
        .desktop_task(
            |sink| {
                TerminalRequest::Write {
                    id: id.clone(),
                    data: "printf 'rpc-%s\\n' terminal-ok\nexit 7\n".into(),
                    sink,
                }
                .into()
            },
            |_| Ok(()),
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
        .desktop_task(
            |sink| TerminalRequest::Write {
                id: id.clone(),
                data: "late".into(),
                sink,
            }
            .into(),
            |_| Ok(())
        )
        .await
        .is_err());
    owner
        .desktop(TerminalRequest::Close { id }.into())
        .await
        .unwrap();
}

/// File IDs and watch generations belong to their client, including over IPC.
pub async fn check_disk_contract(owner: DesktopConnection, other: DesktopConnection) {
    use rovik_client::desktop_api::filesystem::FilesystemRequest as Fs;
    use std::path::Path;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let replacement = root.path().join("replacement");
    for path in [&a, &b, &replacement] {
        std::fs::create_dir(path).unwrap();
    }
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let dir_id = owner
        .desktop(Fs::GetPathId { path: text(&a) }.into())
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(other
        .desktop(
            Fs::CreateFile {
                dir_id: dir_id.clone(),
                name: "wrong-window".into()
            }
            .into()
        )
        .await
        .unwrap_err()
        .contains("unknown id"));
    let (sent_a, mut events_a) = tokio::sync::mpsc::unbounded_channel();
    owner
        .desktop_stream(
            |sink| {
                Fs::StartFileWatch {
                    path_str: text(&a),
                    sink,
                }
                .into()
            },
            move |event| sent_a.send(event).is_ok(),
        )
        .await
        .unwrap();
    let (sent_b, mut events_b) = tokio::sync::mpsc::unbounded_channel();
    other
        .desktop_stream(
            |sink| {
                Fs::StartFileWatch {
                    path_str: text(&b),
                    sink,
                }
                .into()
            },
            move |event| sent_b.send(event).is_ok(),
        )
        .await
        .unwrap();
    let path = owner
        .desktop(
            Fs::CreateFile {
                dir_id: dir_id.clone(),
                name: "owned.txt".into(),
            }
            .into(),
        )
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    owner
        .desktop(
            Fs::WriteFileContent {
                path: path.clone(),
                content: "first".into(),
                expected: Some("".into()),
            }
            .into(),
        )
        .await
        .unwrap();
    assert!(owner
        .desktop(
            Fs::WriteFileContent {
                path: path.clone(),
                content: "must not replace".into(),
                expected: Some("old".into())
            }
            .into()
        )
        .await
        .unwrap_err()
        .starts_with("changed-on-disk:"));
    assert_eq!(
        owner
            .desktop(Fs::ReadFileContent { path: path.clone() }.into())
            .await
            .unwrap(),
        "first"
    );
    watch_saw(&mut events_a, &a, Path::new(&path)).await;
    let entries = owner
        .desktop(
            Fs::ReadDirEntries {
                root_path: text(&a),
                path: text(&a),
            }
            .into(),
        )
        .await
        .unwrap();
    let id = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "owned.txt")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    owner
        .desktop(
            Fs::RenamePath {
                id: id.clone(),
                new_name: "renamed.txt".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    let copy = owner
        .desktop(
            Fs::CopyPath {
                id: id.clone(),
                dest_dir_id: dir_id,
                new_name: None,
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(copy.as_str().unwrap()).unwrap(),
        "first"
    );
    owner
        .desktop(
            Fs::DeletePath {
                id,
                permanent: Some(true),
            }
            .into(),
        )
        .await
        .unwrap();
    assert!(!a.join("renamed.txt").exists());
    let image = a.join("preview.svg");
    std::fs::write(&image, b"<svg/>").unwrap();
    assert_eq!(
        owner
            .desktop_bytes(Fs::ReadImageFile { path: text(&image) }.into())
            .await
            .unwrap(),
        b"<svg/>"
    );
    let (sent_new, mut events_new) = tokio::sync::mpsc::unbounded_channel();
    owner
        .desktop_stream(
            |sink| {
                Fs::StartFileWatch {
                    path_str: text(&replacement),
                    sink,
                }
                .into()
            },
            move |event| sent_new.send(event).is_ok(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while events_a.recv().await.is_some() {}
    })
    .await
    .unwrap();
    let new_file = replacement.join("new.txt");
    owner
        .desktop(
            Fs::WriteFileContent {
                path: text(&new_file),
                content: "new".into(),
                expected: None,
            }
            .into(),
        )
        .await
        .unwrap();
    watch_saw(&mut events_new, &replacement, &new_file).await;
    // Replacing A's watch and disconnecting A must leave B's generation live.
    drop(owner);
    tokio::time::timeout(Duration::from_secs(10), async {
        while events_new.recv().await.is_some() {}
    })
    .await
    .unwrap();
    let surviving_file = b.join("still-watched.txt");
    other
        .desktop(
            Fs::WriteFileContent {
                path: text(&surviving_file),
                content: "still live".into(),
                expected: None,
            }
            .into(),
        )
        .await
        .unwrap();
    watch_saw(&mut events_b, &b, &surviving_file).await;
    drop(other);
}

async fn watch_saw(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    root: &std::path::Path,
    expected: &std::path::Path,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("watch ended before the file changed");
            if event["event"] == "fs-changes" {
                let mut found = false;
                for change in event["payload"].as_array().unwrap() {
                    let path = std::path::Path::new(change["path"].as_str().unwrap());
                    assert!(path.starts_with(root), "another client's event: {event}");
                    found |= path == expected;
                }
                if found {
                    break;
                }
            }
        }
    })
    .await
    .expect("watch delivered the file change");
}

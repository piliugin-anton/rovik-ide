use rovik_client::desktop_api::{
    agent::{AgentRequest, CompletionArgs},
    hosts::HostsRequest,
    mcp::McpRequest,
    settings::SettingsRequest,
};
use rovik_client::desktop_connection::DesktopConnection;
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};

pub fn seed_secrets(config: &Path) {
    use rovik_client::secrets::{Kdf, SecretStore};
    std::fs::create_dir_all(config).unwrap();
    SecretStore::file(
        config.join("secrets.json"),
        Kdf {
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        },
    )
    .unlock(&rovik_protocol::Secret("test-only".into()))
    .unwrap();
}

pub async fn check(a: &DesktopConnection, b: &DesktopConnection) {
    tokio::time::timeout(Duration::from_secs(30), async {
        let patch = |key: &str| {
            SettingsRequest::MergeShared {
                name: "rpc-shared".into(),
                patch: json!({key:true}).as_object().unwrap().clone(),
            }
            .into()
        };
        let (first, second) = tokio::join!(a.desktop_raw(patch("a")), b.desktop_raw(patch("b")));
        first.unwrap();
        second.unwrap();
        assert_eq!(
            a.desktop_raw(
                SettingsRequest::ReadShared {
                    name: "rpc-shared".into()
                }
                .into()
            )
            .await
            .unwrap(),
            json!({"a":true,"b":true})
        );
        assert_eq!(
            b.desktop_raw(
                SettingsRequest::StartShared {
                    name: "rpc-shared".into(),
                    entries: json!({"stale":true}).as_object().unwrap().clone()
                }
                .into()
            )
            .await
            .unwrap(),
            json!({"a":true,"b":true})
        );
        assert!(a
            .desktop_task(
                |sink| HostsRequest::Test {
                    id: "missing-host".into(),
                    sink
                }
                .into(),
                |_| Ok(())
            )
            .await
            .is_err());
        check_sign_in(a).await;
        check_chat(a, b).await;
    })
    .await
    .expect("finite tasks must finish and leave control RPC responsive");
}

async fn check_sign_in(connection: &DesktopConnection) {
    use rovik_core::{
        mcp::{McpServer, McpTransport},
        testing::{OAuthOptions, OAuthServer},
    };
    connection
        .desktop_raw(
            HostsRequest::Unlock {
                passphrase: "test-only".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    let oauth = Arc::new(OAuthServer::start(OAuthOptions::default()).await);
    let request = |sink| {
        McpRequest::SignIn {
            server: McpServer {
                id: "rpc-oauth".into(),
                name: "RPC OAuth".into(),
                enabled: true,
                disabled_tools: vec![],
                transport: McpTransport::Url {
                    url: oauth.mcp_url.clone(),
                    headers: vec![],
                    sse: false,
                    oauth: None,
                },
            },
            values: Default::default(),
            host: None,
            sink,
        }
        .into()
    };
    assert_eq!(
        connection
            .desktop_task(request, |_| Err("browser unavailable".into()))
            .await
            .unwrap_err(),
        json!("browser unavailable")
    );
    let browser = oauth.clone();
    connection
        .desktop_task(request, move |url| {
            browser.browse(url.as_str().unwrap()).map(|_| ())
        })
        .await
        .unwrap();
    let signed = connection
        .desktop_raw(
            McpRequest::SignedIn {
                servers: vec!["rpc-oauth".into()],
                hosts: vec![],
            }
            .into(),
        )
        .await
        .unwrap();
    assert!(signed["here"].to_string().contains("rpc-oauth"), "{signed}");
}

async fn check_chat(a: &DesktopConnection, b: &DesktopConnection) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (next, proceed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
        }
        let header = String::from_utf8(header).unwrap();
        assert!(
            header.starts_with("POST /v1/chat/completions "),
            "wrong mock route"
        );
        let length: usize = header
            .lines()
            .find_map(|s| {
                s.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap();
        socket.read_exact(&mut vec![0; length]).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        let data = format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"first"},"finish_reason":null}]})
        );
        socket
            .write_all(format!("{:x}\r\n{}\r\n", data.len(), data).as_bytes())
            .await
            .unwrap();
        proceed.await.unwrap();
        let data = format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"second"},"finish_reason":null}]})
        );
        socket
            .write_all(format!("{:x}\r\n{}\r\n", data.len(), data).as_bytes())
            .await
            .unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap_or(0), 0);
    });
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let request = a.desktop_task(
        |sink| {
            AgentRequest::Send {
                request_id: Some("same-id".into()),
                sink,
                params: Box::new(CompletionArgs {
                    base_url,
                    api_key: "dummy".into(),
                    model: "test".into(),
                    messages: Some(vec![serde_json::from_value(
                        json!({"role":"user","content":"hi"}),
                    )
                    .unwrap()]),
                    streaming: true,
                    first_byte_timeout_ms: Some(5000),
                    api_route: Some("chat_completions".into()),
                    ..Default::default()
                }),
            }
            .into()
        },
        move |event| sent.send(event).map_err(|e| e.to_string()),
    );
    let controls = async {
        loop {
            let event = received.recv().await.unwrap();
            if event["type"] == "Delta" {
                assert!(event.to_string().contains("first"), "{event}");
                break;
            }
            assert_ne!(event["type"], "Error", "{event}");
        }
        b.desktop_raw(
            AgentRequest::Cancel {
                request_id: "same-id".into(),
            }
            .into(),
        )
        .await
        .unwrap();
        next.send(()).unwrap();
        loop {
            let event = received.recv().await.unwrap();
            if event["type"] == "Delta" {
                assert!(event.to_string().contains("second"), "{event}");
                break;
            }
            assert_ne!(event["type"], "Error", "{event}");
        }
        a.desktop_raw(
            AgentRequest::Cancel {
                request_id: "same-id".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(request, controls);
    result.unwrap();
    server.await.unwrap();
}

//! Explicit Windows-to-WSL transport acceptance against a disposable Linux daemon.
use rovik_client::{host_shell::ProcessConnector, wsl::Wsl, Client, HostState, Identity};
use rovik_protocol::{Request, Response, Secret};
use std::{sync::Arc, time::Duration};

#[tokio::test]
#[ignore = "requires scripts/e2e/wsl-backend.ps1 and a working WSL2 FUSE distribution"]
async fn wsl_sessions_share_a_mounted_daemon_and_detach_independently() {
    let required = |key| std::env::var(key).unwrap_or_else(|_| panic!("{key} is required"));
    let wsl = Wsl::new(required("ROVIK_WSL_TEST_DISTRO"), None);
    let binary = required("ROVIK_WSL_TEST_BINARY");
    let socket = required("ROVIK_WSL_TEST_SOCKET");
    let connect = || {
        Client::connect(
            ProcessConnector::new(
                wsl.shell(),
                vec![
                    binary.clone(),
                    "attach".into(),
                    "--stdio".into(),
                    "--socket".into(),
                    socket.clone(),
                ],
            ),
            Identity {
                host: "WSL acceptance".into(),
                computer: "wsl-acceptance".into(),
                // A public test fixture, accepted only by the disposable daemon.
                token: Secret("wsl-acceptance-fixture".into()),
                client: "WSL acceptance".into(),
            },
            Arc::new(|_| {}),
        )
    };
    async fn ready(client: &Client) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while client.state() != HostState::Ready {
                assert_ne!(client.state(), HostState::Refused, "{:?}", client.refusal());
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("WSL daemon must accept an authenticated session");
    }
    let a = connect();
    let b = connect();
    tokio::join!(ready(&a), ready(&b));
    let Response::HostInfo(info) = a.call(Request::HostInfo).await.unwrap() else {
        panic!("expected WSL host information");
    };
    assert_eq!(info.os, "linux");
    assert!(
        info.virtual_files,
        "WSL FUSE is required: {:?}",
        info.virtual_files_problem
    );
    a.stop();
    drop(a);
    b.call(Request::AgentStatus).await.unwrap();
    let reopened = connect();
    ready(&reopened).await;
    reopened.call(Request::AgentStatus).await.unwrap();
    b.stop();
    reopened.stop();
}

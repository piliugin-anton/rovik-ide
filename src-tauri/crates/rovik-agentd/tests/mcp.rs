//! MCP servers on the real daemon (Q11): a command server stops with the
//! chat's worktree, and a running server starts again when a start brings
//! another secret for it.

mod common;

use common::project::{ended, start, unattended, Setup, CHAT};
use common::{start_request, until, Daemon, Session};
use rovik_core::agent_run::permissions::{ApprovalRule, Approver, Permissions};
use rovik_core::agent_run::{StartKind, UserMessage};
use rovik_core::connectors::mcp::{McpReply, McpServer as FakeServer};
use rovik_core::testing::{model, Reply};
use rovik_protocol::mcp::{McpServer, McpTransport, McpValue};
use rovik_protocol::{Request, Response, Secret};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

/// A stdio server that logs where it started, and that its input closed.
const SERVER: &str = r#"
import json, os, sys
log = sys.argv[1]
def record(value):
    with open(log, 'a') as f:
        f.write(json.dumps(value) + '\n')
record({'cwd': os.getcwd()})
while True:
    line = sys.stdin.readline()
    if not line:
        record({'bye': True})
        break
    message = json.loads(line)
    id = message.get('id')
    if id is None:
        continue
    if message.get('method') == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fake', 'version': '1'}}
    elif message.get('method') == 'tools/list':
        result = {'tools': [{'name': 'echo', 'description': 'Echo.', 'inputSchema': {'type': 'object'}}]}
    else:
        sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32601, 'message': 'no'}}) + '\n')
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': id, 'result': result}) + '\n')
    sys.stdout.flush()
"#;

fn logged(log: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A project in the repository's `src` folder, its chat run once with a
/// command MCP server: the server starts in the chat's root, `src` of the
/// worktree. Deleting the chat stops it.
#[tokio::test]
async fn deleting_a_chat_stops_the_mcp_servers_started_in_its_root() {
    needs_python3();
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure_in("src").await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);
    assert!(root.ends_with("src"), "{root:?}");
    let dir = &setup.daemon.dir.0;
    let script = dir.join("fake_mcp.py");
    std::fs::write(&script, SERVER).unwrap();
    let log = dir.join("fake.log");
    let server = McpServer {
        id: "cmd-id".into(),
        name: "cmd".into(),
        enabled: true,
        transport: McpTransport::Command {
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().into_owned(),
                log.to_string_lossy().into_owned(),
            ],
            env: Vec::new(),
        },
        disabled_tools: Vec::new(),
    };
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    let mut request = start(&setup, &url, unattended());
    request.config.as_mut().unwrap().mcp_servers = vec![server];
    setup
        .client
        .request::<Option<String>>(Request::AgentStart(Box::new(request)))
        .await
        .unwrap();
    ended(&setup.client).await;
    let started = until("the server's start", || logged(&log).first().cloned()).await;
    assert_eq!(
        Path::new(started["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        root.canonicalize().unwrap()
    );

    let delete = Request::ChatDelete {
        dialog_id: CHAT.into(),
    };
    setup.client.request::<()>(delete).await.unwrap();
    until("the server's end", || {
        logged(&log).iter().any(|m| m["bye"] == true).then_some(())
    })
    .await;
}

/// The command server here runs on python3: without it a test fails,
/// saying so, rather than passing with nothing tested.
fn needs_python3() {
    assert!(
        which("python3").is_some(),
        "The MCP tests need python3 on PATH for their fake server: install it."
    );
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// A run's second start brings another secret for the server: the server,
/// which keeps the secret it started with, starts again with the new one
/// (here a wrong token, which it refuses).
#[tokio::test(flavor = "multi_thread")]
async fn a_start_with_another_mcp_secret_restarts_the_running_server() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (fake, mut calls) = FakeServer::start(
        &[json!({ "type": "function", "function": { "name": "read", "description": "Read." } })],
        "",
        None,
    )
    .await
    .unwrap();
    let (seen, mut seen_calls) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let _ = seen.send(call.name.clone());
            let _ = call.reply.send(McpReply::text("ok"));
        }
    });
    let server = McpServer {
        id: "docs-id".into(),
        name: "docs".into(),
        enabled: true,
        transport: McpTransport::Url {
            url: fake.url(),
            headers: vec![McpValue {
                name: "Authorization".into(),
                value: String::new(),
                secret: true,
            }],
        },
        disabled_tools: Vec::new(),
    };
    let mut session = daemon.session().await;
    for (id, token) in [("u1", fake.authorization()), ("u2", "Bearer wrong".into())] {
        let (url, _) = model(vec![
            Reply::Call {
                id: "m1",
                name: "mcp",
                args: json!({ "tool": "docs.read", "arguments": {} }),
            },
            Reply::Text("Done."),
        ])
        .await;
        let mut request = start_request(&url, &project, "Read".into());
        request.start = StartKind::Send {
            message: UserMessage {
                id: id.into(),
                content: "Read".into(),
                meta: None,
                attachments: Vec::new(),
            },
        };
        let config = request.config.as_mut().unwrap();
        config.mcp_servers = vec![server.clone()];
        config.permissions = Permissions {
            mcp_approval: ApprovalRule {
                required: false,
                by: Approver::User,
            },
            ..config.permissions.clone()
        };
        let secrets = [("header/Authorization".to_string(), Secret(token))];
        request.mcp_secrets = [("docs-id".to_string(), secrets.into_iter().collect())].into();
        session
            .call(Request::AgentStart(Box::new(request)))
            .await
            .unwrap();
        ended_in(&mut session).await;
    }
    assert_eq!(seen_calls.try_recv().as_deref(), Ok("read"));
    assert!(
        seen_calls.try_recv().is_err(),
        "the second run's call reached the server with the old secret"
    );
}

/// Waits for the session's daemon to have no run; fails after 30 seconds.
async fn ended_in(session: &mut Session<UnixStream>) {
    for _ in 0..1200 {
        match session.call(Request::AgentStatus).await {
            Ok(Response::AgentStatus(s)) if s.runs.is_empty() => return,
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    panic!("the run never ended");
}

/// The host says how many MCP servers it keeps secrets for, never the
/// secrets; removing every key forgets them and stops the running servers,
/// so none keeps a removed secret in its environment.
#[tokio::test(flavor = "multi_thread")]
async fn removing_every_key_forgets_the_mcp_secrets_and_stops_the_servers() {
    needs_python3();
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let script = daemon.dir.0.join("fake_mcp.py");
    std::fs::write(&script, SERVER).unwrap();
    let log = daemon.dir.0.join("fake.log");
    let server = McpServer {
        id: "cmd-id".into(),
        name: "cmd".into(),
        enabled: true,
        transport: McpTransport::Command {
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().into_owned(),
                log.to_string_lossy().into_owned(),
            ],
            env: vec![McpValue {
                name: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
        },
        disabled_tools: Vec::new(),
    };
    let mut session = daemon.session().await;
    let kept = session.call(Request::McpSecretsKept).await;
    assert_eq!(kept, Ok(Response::Count(0)));

    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    let mut request = start_request(&url, &project, "Hi".into());
    request.config.as_mut().unwrap().mcp_servers = vec![server];
    let secrets = [("env/TOKEN".to_string(), Secret("t0p".into()))];
    request.mcp_secrets = [("cmd-id".to_string(), secrets.into_iter().collect())].into();
    session
        .call(Request::AgentStart(Box::new(request)))
        .await
        .unwrap();
    ended_in(&mut session).await;
    until("the server's start", || logged(&log).first().cloned()).await;
    let kept = session.call(Request::McpSecretsKept).await;
    assert_eq!(kept, Ok(Response::Count(1)));
    assert!(!format!("{kept:?}").contains("t0p"));

    let removed = session.call(Request::KeysRemove { provider: None }).await;
    assert_eq!(removed, Ok(Response::Count(1)), "the one server's secrets");
    let kept = session.call(Request::McpSecretsKept).await;
    assert_eq!(kept, Ok(Response::Count(0)));
    until("the server's end", || {
        logged(&log).iter().any(|m| m["bye"] == true).then_some(())
    })
    .await;
}

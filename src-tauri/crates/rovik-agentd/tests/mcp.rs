//! MCP servers on the real daemon (Q11): a command server stops with the
//! chat's worktree, and a running server starts again when a start brings
//! another secret for it; servers on the user's loopback, Streamable HTTP
//! and HTTP+SSE (Q13), reached through the app.

mod common;

use common::app::App;
use common::project::{ended, message, ready_to_run, start, unattended, Setup, CHAT};
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
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure_in("src").await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);
    assert!(root.ends_with("src"), "{root:?}");
    let (server, log) = command_server(&setup.daemon.dir.0, Vec::new());
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
            sse: false,
            oauth: None,
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
    let daemon = Daemon::start(&["--same-machine"]).await;
    let (server, log) = command_server(&daemon.dir.0, token());
    let mut session = daemon.session().await;
    let kept = session.call(Request::McpSecretsKept).await;
    assert_eq!(kept, Ok(Response::Count(0)));

    run_with(&mut session, &daemon.dir.0, &server).await;
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

/// The command server in `dir`, with these env vars, and its log. It runs
/// on python3, which the test then needs.
fn command_server(dir: &Path, env: Vec<McpValue>) -> (McpServer, PathBuf) {
    needs_python3();
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
            env,
        },
        disabled_tools: Vec::new(),
    };
    (server, log)
}

/// The env var TOKEN, a secret.
fn token() -> Vec<McpValue> {
    vec![McpValue {
        name: "TOKEN".into(),
        value: String::new(),
        secret: true,
    }]
}

/// A run in `dir`'s project with `server` on, its start bringing "t0p" for
/// each of its secrets, until it ends; its start lists the server's tools,
/// which starts the server. The configuration it ran with.
async fn run_with(
    session: &mut Session<UnixStream>,
    dir: &Path,
    server: &McpServer,
) -> rovik_protocol::agent::AgentConfig {
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    let mut request = start_request(&url, &project, "Hi".into());
    let config = request.config.as_mut().unwrap();
    config.mcp_servers = vec![server.clone()];
    let config = config.clone();
    let secrets = (server.transport.secret_keys().into_iter())
        .map(|key| (key, Secret("t0p".into())))
        .collect();
    request.mcp_secrets = [(server.id.clone(), secrets)].into();
    session
        .call(Request::AgentStart(Box::new(request)))
        .await
        .unwrap();
    ended_in(session).await;
    config
}

/// Q11: a server the user turns off on their computer stops on the host:
/// no process there goes on with what the user took back.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_turned_off_stops_on_the_host() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let (mut server, log) = command_server(&daemon.dir.0, Vec::new());
    let mut session = daemon.session().await;
    let mut config = run_with(&mut session, &daemon.dir.0, &server).await;
    until("the server's start", || logged(&log).first().cloned()).await;

    server.enabled = false;
    config.mcp_servers = vec![server];
    session
        .call(Request::AgentConfigure(Box::new(config)))
        .await
        .unwrap();
    until("the server's end", || {
        logged(&log).iter().any(|m| m["bye"] == true).then_some(())
    })
    .await;
}

/// Q11: a chat on the host calls an MCP server on the loopback of the
/// user's computer, which a host can't reach, each POST made by the app
/// there; one that isn't among the app's own servers isn't posted to.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_on_the_user_s_loopback_is_called_through_the_app() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    // On the user's computer: URL servers on its loopback, a token saved.
    let read =
        json!({ "type": "function", "function": { "name": "read", "description": "Read." } });
    let loopback = |id: &str, name: &str, url: String| McpServer {
        id: id.into(),
        name: name.into(),
        enabled: true,
        transport: McpTransport::Url {
            url,
            headers: vec![McpValue {
                name: "Authorization".into(),
                value: String::new(),
                secret: true,
            }],
            sse: false,
            oauth: None,
        },
        disabled_tools: Vec::new(),
    };
    let (docs, mut calls) = FakeServer::start(std::slice::from_ref(&read), "", None)
        .await
        .unwrap();
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let _ = call.reply.send(McpReply::text("read it"));
        }
    });
    let (other, _) = FakeServer::start(&[read], "", None).await.unwrap();
    let token = docs.authorization();
    app.test
        .core
        .mcp
        .set_secrets(std::sync::Arc::new(move |_, _| Ok(Some(token.clone()))));
    let docs = loopback("docs-id", "docs", docs.url());
    let other = loopback("other-id", "other", other.url());
    let (url, seen) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "docs.read", "arguments": {} }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = start(&setup, &url, unattended());
    let config = request.config.as_mut().unwrap();
    config.permissions.mcp_approval = ApprovalRule {
        required: false,
        by: Approver::User,
    };
    // The app's settings have docs; the chat's start has other too.
    let mut mine = config.clone();
    mine.mcp_servers = vec![docs.clone()];
    config.mcp_servers = vec![docs, other];
    rovik_core::dispatch(&app.test.core, Request::AgentConfigure(Box::new(mine)))
        .await
        .unwrap();
    app.remote.start(request).await.unwrap();
    ended(&setup.client).await;
    let asked = seen.lock().unwrap().clone();
    let listed = asked[0]["messages"].to_string();
    assert!(
        listed.contains("posts only to its own MCP servers"),
        "{listed}"
    );
    let answered = asked[1]["messages"].to_string();
    assert!(answered.contains("read it"), "{answered}");
}

/// Q13: a chat on the host calls a server of the old HTTP+SSE transport on
/// the loopback of the user's computer, set up as a plain URL: its POST
/// refused there, its event stream's GET is made by the app, and its
/// messages are posted by the app to the endpoint the stream named, the
/// settings' token on each. One that isn't among the app's own servers
/// gets no request at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_on_http_sse_on_the_user_s_loopback_is_called_through_the_app() {
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let old = rovik_core::testing::SseServer::start(None).await;
    let stranger = rovik_core::testing::SseServer::start(None).await;
    app.test
        .core
        .mcp
        .set_secrets(std::sync::Arc::new(|_, _| Ok(Some("Bearer t0p".into()))));
    let sse_server = |id: &str, name: &str, url: &str| McpServer {
        id: id.into(),
        name: name.into(),
        enabled: true,
        transport: McpTransport::Url {
            url: url.into(),
            headers: vec![McpValue {
                name: "Authorization".into(),
                value: String::new(),
                secret: true,
            }],
            sse: false,
            oauth: None,
        },
        disabled_tools: Vec::new(),
    };
    let server = sse_server("old-id", "old", &old.url);
    let other = sse_server("stranger-id", "stranger", &stranger.url);
    let (url, seen) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "old.echo", "arguments": { "text": "from the host" } }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = start(&setup, &url, unattended());
    let config = request.config.as_mut().unwrap();
    config.permissions.mcp_approval = ApprovalRule {
        required: false,
        by: Approver::User,
    };
    // The app's settings have old; the chat's start has stranger too.
    let mut mine = config.clone();
    mine.mcp_servers = vec![server.clone()];
    config.mcp_servers = vec![server, other];
    rovik_core::dispatch(&app.test.core, Request::AgentConfigure(Box::new(mine)))
        .await
        .unwrap();
    app.remote.start(request).await.unwrap();
    ended(&setup.client).await;
    let asked = seen.lock().unwrap().clone();
    let listed = asked[0]["messages"].to_string();
    assert!(
        listed.contains("posts only to its own MCP servers"),
        "{listed}"
    );
    assert!(stranger.seen().is_empty(), "{:?}", stranger.seen());
    let answered = asked[1]["messages"].to_string();
    assert!(answered.contains("echo: from the host"), "{answered}");
    let got = old.seen();
    assert!(got.iter().any(|r| r.method == "GET"), "its stream");
    assert!(
        got.iter().any(|r| r.path.starts_with("/messages")),
        "its endpoint"
    );
    assert!(got
        .iter()
        .all(|r| r.authorization.as_deref() == Some("Bearer t0p")));
}

/// Q14: a sign-in made on the user's computer for the host goes to its
/// daemon, which keeps it and uses it for the host's runs, refreshing it
/// there when the server refuses its token; once signed out there, the
/// server says to sign in. One the server refuses even fresh is forgotten
/// there.
#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_for_the_host_is_used_and_refreshed_there_until_signed_out() {
    use common::app::HOST;
    use rovik_core::mcp::oauth;
    let mut setup = Setup::new().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let auth = rovik_core::testing::OAuthServer::start(Default::default()).await;
    #[derive(Default)]
    struct Kept(std::sync::Mutex<Option<oauth::Client>>);
    impl oauth::Registrations for Kept {
        fn get(&self, _: &str) -> Option<oauth::Client> {
            self.0.lock().unwrap().clone()
        }
        fn put(&self, _: &str, client: &oauth::Client) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(client.clone());
            Ok(())
        }
    }
    let browser = |url: &str| auth.browse(url).map(drop);
    let kept: std::sync::Arc<dyn oauth::Registrations> = std::sync::Arc::new(Kept::default());
    let sign_in = || {
        oauth::sign_in_to(
            &auth.mcp_url,
            false,
            &[],
            None,
            None,
            kept.clone(),
            &browser,
        )
    };
    let grant = sign_in().await.unwrap();
    let first = grant.access_token.clone();
    app.remote
        .mcp_sign_in(HOST, "signed-id", Some(grant))
        .await
        .unwrap();
    let signed_in = || async {
        let listed = app.remote.mcp_signed_in(HOST).await.unwrap();
        listed.into_iter().map(|s| s.server).collect::<Vec<_>>()
    };
    assert_eq!(signed_in().await, ["signed-id"]);

    let server = McpServer {
        id: "signed-id".into(),
        name: "signed".into(),
        enabled: true,
        transport: McpTransport::Url {
            url: auth.mcp_url.clone(),
            headers: Vec::new(),
            sse: false,
            oauth: None,
        },
        disabled_tools: Vec::new(),
    };
    let call = Reply::Call {
        id: "m1",
        name: "mcp",
        args: json!({ "tool": "signed.whoami", "arguments": {} }),
    };
    let (url, seen) = model(vec![call, Reply::Text("Done.")]).await;
    let run = |url: &str, id: &str| {
        let mut request = message(&setup, url, id);
        let config = request.config.as_mut().unwrap();
        config.permissions.mcp_approval = ApprovalRule {
            required: false,
            by: Approver::User,
        };
        config.mcp_servers = vec![server.clone()];
        request
    };
    // The app has the server too: on its loopback, it's posted to there.
    let mine = run(&url, "u0").config.unwrap();
    rovik_core::dispatch(&app.test.core, Request::AgentConfigure(Box::new(mine)))
        .await
        .unwrap();
    app.remote.start(run(&url, "u1")).await.unwrap();
    ended(&setup.client).await;
    let answered = seen.lock().unwrap()[1]["messages"].to_string();
    assert!(
        answered.contains(&format!("token {}", first.0)),
        "{answered}"
    );

    // The server stops taking that token: the host refreshes its own.
    auth.refuse_access();
    let call = Reply::Call {
        id: "m2",
        name: "mcp",
        args: json!({ "tool": "signed.whoami", "arguments": {} }),
    };
    let (url, seen) = model(vec![call, Reply::Text("Done.")]).await;
    app.remote.start(run(&url, "u2")).await.unwrap();
    ended(&setup.client).await;
    // Its result, the last the model was given: the history has the first.
    let messages = seen.lock().unwrap()[1]["messages"].clone();
    let results: Vec<&str> = (messages.as_array().unwrap().iter())
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["content"].as_str())
        .collect();
    let refreshed_token = results.last().copied().unwrap_or_default();
    assert!(refreshed_token.starts_with("token at-"), "{results:?}");
    assert_ne!(refreshed_token, format!("token {}", first.0), "a new one");
    let refreshed = auth
        .seen()
        .into_iter()
        .filter(|r| r.path == "/auth/token")
        .count();
    assert_eq!(refreshed, 2, "the code's, then the host's refresh");

    // Signed out there: the server says to sign in.
    app.remote
        .mcp_sign_in(HOST, "signed-id", None)
        .await
        .unwrap();
    assert!(signed_in().await.is_empty());
    let call = Reply::Call {
        id: "m3",
        name: "mcp",
        args: json!({ "tool": "signed.whoami", "arguments": {} }),
    };
    let (url, seen) = model(vec![call, Reply::Text("Done.")]).await;
    app.remote.start(run(&url, "u3")).await.unwrap();
    ended(&setup.client).await;
    let messages = seen.lock().unwrap()[1]["messages"].clone();
    let answered = (messages.as_array().unwrap().iter())
        .rfind(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(answered.contains("needs you to sign in"), "{answered}");

    // Signed in again, and refused even fresh: forgotten there.
    app.remote
        .mcp_sign_in(HOST, "signed-id", Some(sign_in().await.unwrap()))
        .await
        .unwrap();
    auth.refuse_every_token();
    let call = Reply::Call {
        id: "m4",
        name: "mcp",
        args: json!({ "tool": "signed.whoami", "arguments": {} }),
    };
    let (url, seen) = model(vec![call, Reply::Text("Done.")]).await;
    app.remote.start(run(&url, "u4")).await.unwrap();
    ended(&setup.client).await;
    let messages = seen.lock().unwrap()[1]["messages"].clone();
    let answered = (messages.as_array().unwrap().iter())
        .rfind(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(answered.contains("sign in again"), "{answered}");
    assert!(signed_in().await.is_empty(), "forgotten there");
}

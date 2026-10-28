//! The client against servers of both eras: Rovik's own MCP server
//! (connectors/mcp.rs, a legacy Streamable HTTP one), a modern HTTP server
//! made here, and a fake command's server in Python (both eras), as
//! docs/design/mcp-plan.md's Q03 and Q04 say they're spoken to.

use super::*;
use crate::connectors::mcp::{McpReply, McpServer as RovikServer};
use crate::testing::temp_dir;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn server(name: &str, transport: McpTransport) -> McpServer {
    McpServer {
        id: format!("id-{name}"),
        name: name.into(),
        enabled: true,
        transport,
        disabled_tools: Vec::new(),
    }
}

fn header(name: &str, value: &str, secret: bool) -> McpValue {
    McpValue {
        name: name.into(),
        value: if secret { String::new() } else { value.into() },
        secret,
    }
}

/// Rovik's tools as its own server lists them: `read` read-only.
fn rovik_tools() -> Vec<Value> {
    vec![
        json!({ "type": "function", "function": { "name": "read", "description": "Read a file. More.", "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } } }),
        json!({ "type": "function", "function": { "name": "bash", "description": "Run a command." } }),
    ]
}

/// A legacy HTTP server (Rovik's own): its token comes from the keychain,
/// its tools are listed in name order with their marks, and a call is
/// answered on its event stream.
#[tokio::test]
async fn a_legacy_http_server_is_listed_and_called_with_its_token_from_the_keychain() {
    let (rovik, mut calls) = RovikServer::start(&rovik_tools(), "Rovik's tools.", None)
        .await
        .unwrap();
    let state = McpState::default();
    let token = rovik.authorization();
    state.set_secrets(Arc::new(move |id, key| {
        assert_eq!((id, key), ("id-rovik", "header/Authorization"));
        Ok(Some(token.clone()))
    }));
    let rovik_server = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
        },
    );
    let listed = state.list(std::slice::from_ref(&rovik_server), None).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].instructions, "Rovik's tools.");
    let tools = listed[0].tools.clone().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["bash", "read"], "in name order");
    assert!(tools[1].read_only && !tools[0].read_only);

    let answering = tokio::spawn(async move {
        let call = calls.recv().await.unwrap();
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, json!({ "path": "a.rs" }));
        call.reply.send(McpReply::text("fn a() {}")).unwrap();
    });
    let stop = StopSignal::new();
    let result = state
        .call(
            &rovik_server,
            None,
            &tools[1],
            json!({ "path": "a.rs" }),
            &stop,
        )
        .await
        .unwrap();
    answering.await.unwrap();
    assert_eq!(result["content"][0]["text"], "fn a() {}");
    assert_eq!(result["isError"], false);

    // Tested from the page, as it's being set up: the token typed there.
    let tested = McpState::default()
        .test(
            &rovik_server,
            &HashMap::from([("header/Authorization".to_string(), rovik.authorization())]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(tested.protocol, rpc::LEGACY);
    assert!(tested.server.starts_with("rovik "), "{}", tested.server);
    assert_eq!(tested.tools.len(), 2);
}

/// A wrong token is the server's refusal, said as such; a secret that
/// isn't saved names the value to enter, never a secret's value.
#[tokio::test]
async fn a_refused_or_missing_secret_says_what_to_fix() {
    let (rovik, _calls) = RovikServer::start(&rovik_tools(), "", None).await.unwrap();
    let wrong = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "Bearer wrong", false)],
        },
    );
    let listed = McpState::default().list(&[wrong], None).await;
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("refused Rovik's credentials"), "{error}");
    assert!(error.contains("401"), "{error}");

    let unsaved = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
        },
    );
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(None)));
    let error = state.list(&[unsaved], None).await[0]
        .tools
        .clone()
        .unwrap_err();
    assert!(
        error.contains("Authorization's value isn't saved"),
        "{error}"
    );
}

/// A running server keeps the secrets it started with; once the page
/// saves others (`forget`), its next use starts it again with them (Q02).
#[tokio::test]
async fn a_server_starts_again_with_its_new_secrets_once_forgotten() {
    let (rovik, _calls) = RovikServer::start(&rovik_tools(), "", None).await.unwrap();
    let token = Arc::new(Mutex::new(rovik.authorization()));
    let saved = token.clone();
    let state = McpState::default();
    state.set_secrets(Arc::new(move |_, _| {
        Ok(Some(saved.lock().unwrap().clone()))
    }));
    let rovik_server = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
        },
    );
    let servers = std::slice::from_ref(&rovik_server);
    assert!(state.list(servers, None).await[0].tools.is_ok());
    *token.lock().unwrap() = "Bearer wrong".into();
    assert!(
        state.list(servers, None).await[0].tools.is_ok(),
        "the running server keeps its token"
    );
    state.forget(&rovik_server.id);
    let error = state.list(servers, None).await[0]
        .tools
        .clone()
        .unwrap_err();
    assert!(error.contains("refused Rovik's credentials"), "{error}");
}

/// The keychain may take a moment, or ask the user: it's read on a
/// blocking thread, and the async threads go on meanwhile (Q02).
#[tokio::test]
async fn the_keychain_is_read_off_the_async_threads() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    let slow = server(
        "slow",
        McpTransport::Url {
            url,
            headers: vec![header("Authorization", "", true)],
        },
    );
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| {
        std::thread::sleep(Duration::from_millis(300));
        Ok(Some("Bearer token".into()))
    }));
    // The test's runtime has one thread: a keychain read on it would
    // hold the ticks back until it's done.
    let ticks = Arc::new(AtomicUsize::new(0));
    let counted = ticks.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let listed = state.list(&[slow], None).await;
    ticker.abort();
    assert!(listed[0].tools.is_err(), "nothing answers there");
    let ticks = ticks.load(Ordering::SeqCst);
    assert!(ticks >= 3, "{ticks} ticks while the keychain was read");
}

/// What a modern HTTP server saw of each request.
#[derive(Clone, Debug, Default)]
struct Seen {
    requests: Vec<(HashMap<String, String>, Value)>,
}

/// A modern-only server (2026-07-28) on 127.0.0.1: `initialize` is unknown
/// to it, `server/discover` lists its version, and every request must carry
/// the version in its `_meta` and its headers. Its tool mirrors `region`
/// into a header.
async fn modern_server() -> (String, Arc<Mutex<Seen>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let kept = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = kept.clone();
            tokio::spawn(async move {
                let Some((headers, body)) = read_http(&mut stream).await else {
                    return;
                };
                seen.lock()
                    .unwrap()
                    .requests
                    .push((headers.clone(), body.clone()));
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let method = body["method"].as_str().unwrap_or("");
                let version =
                    body.pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion");
                let (status, answer) = if method == "initialize" {
                    (
                        400,
                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32022, "message": "Unsupported protocol version", "data": { "supported": ["2026-07-28"] } } }),
                    )
                } else if version != Some(&json!("2026-07-28"))
                    || headers.get("mcp-protocol-version").map(String::as_str) != Some("2026-07-28")
                    || headers.get("mcp-method").map(String::as_str) != Some(method)
                {
                    (
                        400,
                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32020, "message": "Header mismatch" } }),
                    )
                } else {
                    let result = match method {
                        "server/discover" => {
                            json!({ "resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": { "tools": {} }, "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "spanner", "version": "2.0" } }, "instructions": "SQL on Spanner." })
                        }
                        "tools/list" => {
                            json!({ "resultType": "complete", "tools": [{ "name": "execute_sql", "description": "Execute SQL.", "inputSchema": { "type": "object", "properties": { "region": { "type": "string", "x-mcp-header": "Region" }, "query": { "type": "string" } } } }], "ttlMs": 1000, "cacheScope": "private" })
                        }
                        "tools/call" => {
                            json!({ "resultType": "complete", "content": [{ "type": "text", "text": format!("ran in {}", headers.get("mcp-param-region").cloned().unwrap_or_default()) }] })
                        }
                        _ => json!({}),
                    };
                    (200, json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                };
                let body = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, seen)
}

/// One HTTP request's headers (lower-case names) and JSON body.
async fn read_http(stream: &mut tokio::net::TcpStream) -> Option<(HashMap<String, String>, Value)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let headers: HashMap<String, String> = head
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers.get("content-length")?.parse().ok()?;
    let mut body = data[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some((headers, serde_json::from_slice(&body).ok()?))
}

/// Q03: a server that refuses the handshake and lists 2026-07-28 is spoken
/// to statelessly: each request with its version in `_meta` and headers,
/// and a tool's `x-mcp-header` argument mirrored into its header.
#[tokio::test]
async fn a_modern_http_server_is_spoken_to_statelessly() {
    let (url, seen) = modern_server().await;
    let modern = server(
        "spanner",
        McpTransport::Url {
            url,
            headers: Vec::new(),
        },
    );
    let state = McpState::default();
    let listed = state.list(std::slice::from_ref(&modern), None).await;
    assert_eq!(listed[0].instructions, "SQL on Spanner.");
    let tools = listed[0].tools.clone().unwrap();
    assert_eq!(tools[0].name, "execute_sql");
    let stop = StopSignal::new();
    let result = state
        .call(
            &modern,
            None,
            &tools[0],
            json!({ "region": "us-west1", "query": "SELECT 1" }),
            &stop,
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "ran in us-west1");
    let seen = seen.lock().unwrap().clone();
    let methods: Vec<&str> = seen
        .requests
        .iter()
        .map(|(_, body)| body["method"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        methods,
        ["initialize", "server/discover", "tools/list", "tools/call"]
    );
    let (call_headers, call) = &seen.requests[3];
    assert_eq!(
        call_headers.get("mcp-name").map(String::as_str),
        Some("execute_sql")
    );
    assert_eq!(
        call.pointer("/params/_meta/io.modelcontextprotocol~1clientInfo/name"),
        Some(&json!("rovik"))
    );
    let tested = state.test(&modern, &HashMap::new(), None).await.unwrap();
    assert_eq!(
        (tested.server.as_str(), tested.protocol.as_str()),
        ("spanner 2.0", "2026-07-28")
    );
}

/// A server that can't be reached is unavailable, saying why, and isn't
/// tried again by the next run right away (Q04).
#[tokio::test]
async fn a_server_that_cant_be_reached_waits_before_it_is_tried_again() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    let gone = server(
        "gone",
        McpTransport::Url {
            url,
            headers: Vec::new(),
        },
    );
    let state = McpState::default();
    let first = state.list(std::slice::from_ref(&gone), None).await[0]
        .tools
        .clone();
    assert!(first.is_err());
    let started = Instant::now();
    let again = state.list(std::slice::from_ref(&gone), None).await[0]
        .tools
        .clone();
    assert_eq!(again, first, "the same failure, not tried again");
    assert!(started.elapsed() < Duration::from_millis(100));
}

/// The fake command server (Python): `legacy` speaks the handshake and asks
/// Rovik a `ping` in the middle of a call; `modern` refuses the handshake
/// and wants `_meta` on every request; `crash` exits at once, saying why.
/// What it receives, its folder and its environment go in `log`, one JSON
/// line each.
const FAKE_SERVER: &str = r#"
import json, os, sys
mode, log = sys.argv[1], sys.argv[2]
def record(value):
    with open(log, 'a') as f:
        f.write(json.dumps(value) + '\n')
def send(value):
    sys.stdout.write(json.dumps(value) + '\n')
    sys.stdout.flush()
record({'cwd': os.getcwd(), 'token': os.environ.get('FAKE_TOKEN'), 'plain': os.environ.get('FAKE_PLAIN')})
sys.stderr.write('fake server starting\n')
sys.stderr.flush()
if mode == 'crash':
    sys.stderr.write('boom: no token\n')
    sys.exit(3)
tools = [
    {'name': 'echo', 'description': 'Echo the text. Then more.', 'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}, 'required': ['text']}, 'annotations': {'readOnlyHint': True}},
    {'name': 'slow', 'description': 'Never answers.', 'inputSchema': {'type': 'object'}},
    {'name': 'note', 'description': 'Write a note.', 'inputSchema': {'type': 'object', 'properties': {'note': {'type': 'string'}}, 'required': ['note']}},
]
print('not json, a stray log line', flush=True)
while True:
    line = sys.stdin.readline()
    if not line:
        record({'bye': True})
        break
    message = json.loads(line)
    record(message)
    method, id = message.get('method'), message.get('id')
    if id is None:
        continue
    params = message.get('params') or {}
    if method == 'initialize':
        if mode == 'modern':
            send({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32601, 'message': 'Method not found'}})
        else:
            send({'jsonrpc': '2.0', 'id': id, 'result': {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fake', 'version': '1.2'}, 'instructions': 'Fake tools.'}})
        continue
    if method == 'server/discover':
        send({'jsonrpc': '2.0', 'id': id, 'result': {'resultType': 'complete', 'supportedVersions': ['2026-07-28'], 'capabilities': {'tools': {}}, '_meta': {'io.modelcontextprotocol/serverInfo': {'name': 'fake', 'version': '2.0'}}}})
        continue
    if mode == 'modern' and (params.get('_meta') or {}).get('io.modelcontextprotocol/protocolVersion') != '2026-07-28':
        send({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32602, 'message': 'missing _meta'}})
        continue
    if method == 'tools/list':
        if params.get('cursor') is None:
            send({'jsonrpc': '2.0', 'id': id, 'result': {'tools': tools[:2], 'nextCursor': 'page-2'}})
        else:
            send({'jsonrpc': '2.0', 'id': id, 'result': {'tools': tools[2:]}})
    elif method == 'tools/call':
        name, arguments = params.get('name'), params.get('arguments') or {}
        if name == 'slow':
            continue
        if name == 'note' and not arguments.get('note'):
            send({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32602, 'message': 'note is required'}})
            continue
        if mode == 'legacy':
            send({'jsonrpc': '2.0', 'id': 'server-1', 'method': 'ping'})
            record(json.loads(sys.stdin.readline()))
        send({'jsonrpc': '2.0', 'id': id, 'result': {'content': [{'type': 'text', 'text': 'echo: ' + str(arguments.get('text'))}], 'isError': False}})
"#;

/// The fake server's command in `dir`, in `mode`, and its log; none
/// without python3.
#[cfg(unix)]
fn fake(dir: &Path, mode: &str) -> Option<(McpTransport, PathBuf)> {
    if !crate::process::on_path("python3") {
        return None;
    }
    let script = dir.join("fake_mcp.py");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    let log = dir.join(format!("{mode}.log"));
    let transport = McpTransport::Command {
        command: "python3".into(),
        args: vec![
            script.to_string_lossy().into_owned(),
            mode.into(),
            log.to_string_lossy().into_owned(),
        ],
        env: vec![
            McpValue {
                name: "FAKE_TOKEN".into(),
                value: String::new(),
                secret: true,
            },
            McpValue {
                name: "FAKE_PLAIN".into(),
                value: "plain".into(),
                secret: false,
            },
        ],
    };
    Some((transport, log))
}

/// What the fake server's log has, line by line.
#[cfg(unix)]
fn logged(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(unix)]
fn with_token(state: &McpState) {
    state.set_secrets(Arc::new(|_, key| {
        assert_eq!(key, "env/FAKE_TOKEN");
        Ok(Some("t0ken".into()))
    }));
}

/// A legacy command server: started in the project's folder with its
/// secret env var from the keychain; its stray output line is passed over,
/// its tools read page by page, its `ping` answered in the middle of a
/// call; refused arguments say so; and it's stopped, its input closed
/// first, when the settings no longer have it.
#[cfg(unix)]
#[tokio::test]
async fn a_legacy_command_server_starts_in_the_project_and_ends_with_its_settings() {
    let dir = temp_dir();
    let Some((transport, log)) = fake(&dir.0, "legacy") else {
        return;
    };
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let listed = state
        .list(std::slice::from_ref(&fake_server), Some(&project))
        .await;
    assert_eq!(listed[0].instructions, "Fake tools.");
    let tools = listed[0].tools.clone().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "note", "slow"], "both pages, in name order");
    let first = &logged(&log)[0];
    assert_eq!(
        std::fs::canonicalize(first["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&project).unwrap()
    );
    assert_eq!(
        (first["token"].as_str(), first["plain"].as_str()),
        (Some("t0ken"), Some("plain"))
    );

    let stop = StopSignal::new();
    let echoed = state
        .call(
            &fake_server,
            Some(&project),
            &tools[0],
            json!({ "text": "hi" }),
            &stop,
        )
        .await
        .unwrap();
    assert_eq!(echoed["content"][0]["text"], "echo: hi");
    assert!(
        logged(&log)
            .iter()
            .any(|m| m["id"] == "server-1" && m["result"] == json!({})),
        "its ping answered"
    );
    let refused = state
        .call(&fake_server, Some(&project), &tools[1], json!({}), &stop)
        .await
        .unwrap_err();
    assert!(refused.bad_arguments);
    assert!(
        refused.text.contains("note is required"),
        "{}",
        refused.text
    );

    // Another run lists it again: the same process.
    state
        .list(std::slice::from_ref(&fake_server), Some(&project))
        .await;
    let starts = logged(&log)
        .iter()
        .filter(|m| m.get("cwd").is_some())
        .count();
    assert_eq!(starts, 1);

    state.configure(&[]);
    for _ in 0..200 {
        if logged(&log).iter().any(|m| m["bye"] == true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server's input wasn't closed: {:?}", logged(&log));
}

/// A call that's taking long ends when Stop is pressed, and the server is
/// told (`notifications/cancelled`).
#[cfg(unix)]
#[tokio::test]
async fn a_stopped_call_is_cancelled_at_the_server() {
    let dir = temp_dir();
    let Some((transport, log)) = fake(&dir.0, "legacy") else {
        return;
    };
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let tools = state.list(std::slice::from_ref(&fake_server), None).await[0]
        .tools
        .clone()
        .unwrap();
    let slow = tools.iter().find(|t| t.name == "slow").unwrap();
    let stop = Arc::new(StopSignal::new());
    let pressing = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        pressing.set();
    });
    let error = state
        .call(&fake_server, None, slow, json!({}), &stop)
        .await
        .unwrap_err();
    assert_eq!(error.text, "stopped");
    for _ in 0..100 {
        if logged(&log)
            .iter()
            .any(|m| m["method"] == "notifications/cancelled")
        {
            state.shutdown().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no cancellation: {:?}", logged(&log));
}

/// A modern command server: the handshake refused, `server/discover`
/// answered, and every request after it carries its `_meta`.
#[cfg(unix)]
#[tokio::test]
async fn a_modern_command_server_gets_its_version_on_every_request() {
    let dir = temp_dir();
    let Some((transport, log)) = fake(&dir.0, "modern") else {
        return;
    };
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let tools = state.list(std::slice::from_ref(&fake_server), None).await[0]
        .tools
        .clone()
        .unwrap();
    assert_eq!(tools.len(), 3);
    let stop = StopSignal::new();
    let echoed = state
        .call(
            &fake_server,
            None,
            &tools[0],
            json!({ "text": "hi" }),
            &stop,
        )
        .await
        .unwrap();
    assert_eq!(echoed["content"][0]["text"], "echo: hi");
    let methods: Vec<String> = logged(&log)
        .iter()
        .filter_map(|m| m["method"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "server/discover",
            "tools/list",
            "tools/list",
            "tools/call"
        ]
    );
    state.shutdown().await;
}

/// A server whose process exits as it starts is unavailable with its exit
/// and its last words.
#[cfg(unix)]
#[tokio::test]
async fn a_server_that_exits_says_why() {
    let dir = temp_dir();
    let Some((transport, _log)) = fake(&dir.0, "crash") else {
        return;
    };
    let state = McpState::default();
    with_token(&state);
    let error = state.list(&[server("fake", transport)], None).await[0]
        .tools
        .clone()
        .unwrap_err();
    assert!(error.contains("exited with code 3"), "{error}");
    assert!(error.contains("boom: no token"), "{error}");
}

/// A command that isn't installed says so, and the servers turned off, or
/// whose names can't be used, aren't started at all.
#[tokio::test]
async fn a_missing_command_says_so_and_servers_turned_off_are_left_out() {
    let missing = server(
        "missing",
        McpTransport::Command {
            command: "rovik-no-such-mcp-server".into(),
            args: Vec::new(),
            env: Vec::new(),
        },
    );
    let mut off = missing.clone();
    off.name = "off".into();
    off.enabled = false;
    let mut bad = missing.clone();
    bad.name = "Bad.Name".into();
    let listed = McpState::default().list(&[missing, off, bad], None).await;
    assert_eq!(listed.len(), 1);
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("isn't installed"), "{error}");
}

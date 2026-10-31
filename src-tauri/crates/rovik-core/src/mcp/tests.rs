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
            sse: false,
            oauth: None,
        },
    );
    let listed = state
        .list(std::slice::from_ref(&rovik_server), None, None)
        .await;
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
            None,
        )
        .await
        .unwrap();
    assert_eq!(tested.protocol, rpc::LEGACY);
    assert!(tested.server.starts_with("rovik "), "{}", tested.server);
    assert_eq!(tested.tools.len(), 2);
}

/// A server on the old HTTP+SSE transport at `url`, its token from the
/// keychain; on it from the start when `sse`.
fn old_server(url: &str, sse: bool) -> McpServer {
    server(
        "old",
        McpTransport::Url {
            url: url.into(),
            headers: vec![header("Authorization", "", true)],
            sse,
            oauth: None,
        },
    )
}

/// Q13 (mcp-plan-2.md): a server on the old HTTP+SSE transport, as its
/// entry says: its stream's GET and each message carry the settings'
/// headers, it speaks the version it answered, its tools are listed, and a
/// call is answered on its stream once Rovik has answered the server's
/// own `ping` there.
#[tokio::test]
async fn an_sse_server_is_spoken_to_on_its_event_stream() {
    let old = crate::testing::SseServer::start(None).await;
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(Some("Bearer t0p".into()))));
    let sse = old_server(&old.url, true);
    let listed = state.list(std::slice::from_ref(&sse), None, None).await;
    assert_eq!(listed[0].instructions, "Old tools.");
    let tools = listed[0].tools.clone().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "slow"]);
    let stop = StopSignal::new();
    let echo = &tools[0];
    let result = state.call(&sse, None, None, echo, json!({ "text": "hi" }), &stop);
    assert_eq!(result.await.unwrap()["content"][0]["text"], "echo: hi");

    let seen = old.seen();
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/sse")
    );
    assert!(seen
        .iter()
        .all(|r| r.authorization.as_deref() == Some("Bearer t0p")));
    assert!(
        !seen.iter().any(|r| r.method == "POST" && r.path == "/sse"),
        "no Streamable HTTP tried first"
    );
    let ping = seen.iter().find(|r| {
        r.body["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("server-ping"))
    });
    assert_eq!(
        ping.map(|r| r.body["result"].clone()),
        Some(json!({})),
        "its ping answered"
    );
    let tested = state.test(&sse, &HashMap::new(), None, None).await.unwrap();
    assert_eq!(tested.protocol, "2024-11-05");
    assert_eq!(tested.server, "old-sse 0.9");
}

/// Q13: a URL set up as Streamable HTTP whose POST is refused 405 is tried
/// as HTTP+SSE, and spoken to so for the session: its URL isn't posted to
/// again.
#[tokio::test]
async fn a_url_that_refuses_streamable_http_is_spoken_to_on_http_sse() {
    let old = crate::testing::SseServer::start(None).await;
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(Some("Bearer t0p".into()))));
    let plain = old_server(&old.url, false);
    let listed = state.list(std::slice::from_ref(&plain), None, None).await;
    let tools = listed[0].tools.clone().unwrap();
    let stop = StopSignal::new();
    let result = state.call(
        &plain,
        None,
        None,
        &tools[0],
        json!({ "text": "again" }),
        &stop,
    );
    assert_eq!(result.await.unwrap()["content"][0]["text"], "echo: again");
    let seen = old.seen();
    let posts_to_url = seen
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/sse");
    assert_eq!(posts_to_url.count(), 1, "the first try alone");
    assert_eq!(
        seen.iter().filter(|r| r.method == "GET").count(),
        1,
        "one stream"
    );
}

/// Q13: a stream that names another origin to post to isn't posted to,
/// and its server is unavailable, saying so.
#[tokio::test]
async fn an_sse_server_naming_another_origin_isn_t_posted_to() {
    let old = crate::testing::SseServer::start(Some("http://evil.example/messages")).await;
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(Some("Bearer t0p".into()))));
    let listed = state
        .list(
            std::slice::from_ref(&old_server(&old.url, true)),
            None,
            None,
        )
        .await;
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("Rovik doesn't post there"), "{error}");
    assert!(
        old.seen().iter().all(|r| r.method == "GET"),
        "nothing posted"
    );
}

/// Q13: a stream that ends fails the call waiting on it at once, saying
/// so, and the next use opens a stream again.
#[tokio::test]
async fn an_sse_stream_that_ends_fails_its_call_and_opens_again() {
    let old = crate::testing::SseServer::start(None).await;
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(Some("Bearer t0p".into()))));
    let sse = old_server(&old.url, true);
    let listed = state.list(std::slice::from_ref(&sse), None, None).await;
    let tools = listed[0].tools.clone().unwrap();
    let slow = tools.iter().find(|t| t.name == "slow").unwrap().clone();
    let stop = StopSignal::new();
    let state_ = state.clone();
    let sse_ = sse.clone();
    let waiting = tokio::spawn(async move {
        state_
            .call(&sse_, None, None, &slow, json!({}), &stop)
            .await
    });
    // The call is waiting on the stream; then the server goes away.
    for _ in 0..200 {
        if old
            .seen()
            .iter()
            .any(|r| r.body["params"]["name"] == "slow")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let started = std::time::Instant::now();
    old.end_streams();
    let error = waiting.await.unwrap().unwrap_err();
    assert!(error.text.contains("event stream ended"), "{}", error.text);
    assert!(started.elapsed() < Duration::from_secs(5), "at once");
    let again = state.list(std::slice::from_ref(&sse), None, None).await;
    assert!(again[0].tools.is_ok(), "{:?}", again[0].tools);
    assert_eq!(old.seen().iter().filter(|r| r.method == "GET").count(), 2);
}

/// Sign-ins kept in memory, as the keychain keeps them (Q14).
#[derive(Default)]
struct Grants(Mutex<HashMap<String, oauth::McpGrant>>);

impl oauth::GrantStore for Grants {
    fn get(&self, server: &str) -> Result<Option<oauth::McpGrant>, String> {
        Ok(self.0.lock().unwrap().get(server).cloned())
    }
    fn put(&self, server: &str, grant: &oauth::McpGrant) -> Result<(), String> {
        self.0.lock().unwrap().insert(server.into(), grant.clone());
        Ok(())
    }
    fn forget(&self, server: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(server);
        Ok(())
    }
}

/// Registrations kept in memory, as the keychain keeps them (Q14).
struct Registered(Mutex<HashMap<String, oauth::Client>>);

impl oauth::Registrations for Registered {
    fn get(&self, issuer: &str) -> Option<oauth::Client> {
        self.0.lock().unwrap().get(issuer).cloned()
    }
    fn put(&self, issuer: &str, client: &oauth::Client) -> Result<(), String> {
        self.0.lock().unwrap().insert(issuer.into(), client.clone());
        Ok(())
    }
}

/// A sign-in to `oauth`'s MCP server, asking for the scopes `wants` too,
/// its browser `oauth`'s own.
async fn sign_in(oauth: &crate::testing::OAuthServer, wants: Option<&str>) -> oauth::McpGrant {
    let kept: Arc<dyn oauth::Registrations> = Arc::new(Registered(Mutex::default()));
    let browser = |url: &str| oauth.browse(url).map(drop);
    oauth::sign_in_to(&oauth.mcp_url, false, &[], None, wants, kept, &browser)
        .await
        .unwrap()
}

/// `oauth`'s MCP server, signed in to (its grant kept for `id-signed`),
/// and the state that keeps it.
async fn signed_in(
    options: crate::testing::OAuthOptions,
) -> (
    crate::testing::OAuthServer,
    McpServer,
    McpState,
    Arc<Grants>,
) {
    let oauth = crate::testing::OAuthServer::start(options).await;
    let grant = sign_in(&oauth, None).await;
    let grants = Arc::new(Grants::default());
    grants.0.lock().unwrap().insert("id-signed".into(), grant);
    let state = McpState::default();
    state.set_grants(grants.clone());
    let server = server(
        "signed",
        McpTransport::Url {
            url: oauth.mcp_url.clone(),
            headers: Vec::new(),
            sse: false,
            oauth: None,
        },
    );
    (oauth, server, state, grants)
}

/// What the server's tool says of the token it got.
async fn whoami(state: &McpState, server: &McpServer) -> Result<String, String> {
    let listed = state.list(std::slice::from_ref(server), None, None).await;
    let tools = listed[0].tools.clone()?;
    let stop = StopSignal::new();
    let result = state
        .call(server, None, None, &tools[0], json!({}), &stop)
        .await;
    let result = result.map_err(|e| e.text)?;
    Ok(result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// Q14: a sign-in's access token goes on each request; one about to
/// expire is refreshed first, and the rotated grant kept.
#[tokio::test]
async fn a_sign_in_s_token_goes_on_each_request_refreshed_before_it_expires() {
    let options = crate::testing::OAuthOptions {
        expires_in: 30,
        ..Default::default()
    };
    let (_oauth, signed, state, grants) = signed_in(options).await;
    let first = grants.0.lock().unwrap()["id-signed"].clone();
    let said = whoami(&state, &signed).await.unwrap();
    let kept = grants.0.lock().unwrap()["id-signed"].clone();
    assert_ne!(
        kept.access_token, first.access_token,
        "refreshed: it expired within a minute"
    );
    assert_ne!(kept.refresh_token, first.refresh_token, "rotated, and kept");
    assert_eq!(said, format!("token {}", kept.access_token.0));
}

/// Q14: a token the server refuses (401) is refreshed, and the request
/// sent again with the new one, the caller none the wiser.
#[tokio::test]
async fn a_refused_token_is_refreshed_and_the_request_sent_again() {
    let (oauth, signed, state, grants) = signed_in(Default::default()).await;
    assert!(whoami(&state, &signed).await.is_ok());
    oauth.refuse_access();
    let said = whoami(&state, &signed).await.unwrap();
    let kept = grants.0.lock().unwrap()["id-signed"].clone();
    assert_eq!(said, format!("token {}", kept.access_token.0));
}

/// Q14: a sign-in its server no longer refreshes leaves the server
/// unavailable, saying to sign in again, and is forgotten where it was
/// kept: the place lists it signed out. One with none says to sign in.
#[tokio::test]
async fn a_refused_or_missing_sign_in_says_to_sign_in() {
    let (oauth, signed, state, grants) = signed_in(Default::default()).await;
    oauth.refuse_access();
    oauth.refuse_refresh();
    let listed = state.list(std::slice::from_ref(&signed), None, None).await;
    let refused = listed[0].tools.clone().unwrap_err();
    assert!(
        refused.contains("sign in again from Settings → MCP servers"),
        "{refused}"
    );
    assert!(grants.0.lock().unwrap().is_empty(), "forgotten");

    let never = McpState::default();
    let listed = never.list(std::slice::from_ref(&signed), None, None).await;
    let missing = listed[0].tools.clone().unwrap_err();
    assert_eq!(missing, oauth::NEEDS_SIGN_IN);
}

/// Q14: a server that refuses even a fresh token has its sign-in
/// forgotten where it was kept, the place listing it signed out, after
/// one refresh.
#[tokio::test]
async fn a_sign_in_refused_even_fresh_is_forgotten() {
    let (oauth, signed, state, grants) = signed_in(Default::default()).await;
    oauth.refuse_every_token();
    let refused = whoami(&state, &signed).await.unwrap_err();
    assert!(
        refused.contains("refused its sign-in's token: sign in again"),
        "{refused}"
    );
    assert!(grants.0.lock().unwrap().is_empty(), "forgotten");
    let tokens = (oauth.seen().into_iter())
        .filter(|r| r.path == "/auth/token")
        .count();
    assert_eq!(tokens, 2, "the sign-in's, then one refresh");
}

/// Q14: a call that needs more than the sign-in gave (a 403
/// `insufficient_scope`) says which scopes, and to sign in again; they're
/// kept with the sign-in, and the next sign-in asks for them, after which
/// the call goes through.
#[tokio::test]
async fn a_call_needing_more_scopes_says_so_and_the_next_sign_in_asks_for_them() {
    let options = crate::testing::OAuthOptions {
        call_needs: Some("mcp:write".into()),
        ..Default::default()
    };
    let (oauth, signed, state, grants) = signed_in(options).await;
    let refused = whoami(&state, &signed).await.unwrap_err();
    assert!(
        refused.contains("more permissions (mcp:read mcp:write)"),
        "{refused}"
    );
    assert!(refused.contains("sign in again"), "{refused}");
    let kept = grants.0.lock().unwrap()["id-signed"].clone();
    assert_eq!(kept.wants.as_deref(), Some("mcp:read mcp:write"));

    let wider = sign_in(&oauth, kept.wants.as_deref()).await;
    assert_eq!(wider.scope, "mcp:read mcp:write");
    assert_eq!(wider.wants, None, "it has them");
    grants.0.lock().unwrap().insert("id-signed".into(), wider);
    state.forget(&signed.id);
    let said = whoami(&state, &signed).await.unwrap();
    assert!(said.starts_with("token at-"), "{said}");
}

/// A server reached with its settings' own token, which lacks a scope
/// (a 403 `insufficient_scope`), says what the server answered: it's
/// never offered a sign-in, so it isn't told to sign in.
#[tokio::test]
async fn a_settings_token_without_a_scope_isnt_told_to_sign_in() {
    let options = crate::testing::OAuthOptions {
        call_needs: Some("mcp:write".into()),
        ..Default::default()
    };
    let (oauth, _signed, _state, grants) = signed_in(options).await;
    let token = grants.0.lock().unwrap()["id-signed"].access_token.0.clone();
    let keyed = server(
        "keyed",
        McpTransport::Url {
            url: oauth.mcp_url.clone(),
            headers: vec![header("Authorization", &format!("Bearer {token}"), false)],
            sse: false,
            oauth: None,
        },
    );
    let refused = whoami(&McpState::default(), &keyed).await.unwrap_err();
    assert!(refused.contains("403"), "{refused}");
    assert!(!refused.contains("sign in"), "{refused}");
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
            sse: false,
            oauth: None,
        },
    );
    let listed = McpState::default().list(&[wrong], None, None).await;
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("refused Rovik's credentials"), "{error}");
    assert!(error.contains("401"), "{error}");

    let unsaved = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    state.set_secrets(Arc::new(|_, _| Ok(None)));
    let error = state.list(&[unsaved], None, None).await[0]
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
            sse: false,
            oauth: None,
        },
    );
    let servers = std::slice::from_ref(&rovik_server);
    assert!(state.list(servers, None, None).await[0].tools.is_ok());
    *token.lock().unwrap() = "Bearer wrong".into();
    assert!(
        state.list(servers, None, None).await[0].tools.is_ok(),
        "the running server keeps its token"
    );
    state.forget(&rovik_server.id);
    let error = state.list(servers, None, None).await[0]
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
            sse: false,
            oauth: None,
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
    let listed = state.list(&[slow], None, None).await;
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

/// A web API at a URL, not an MCP server: every request, whatever its
/// method, is answered `status` with the JSON `body`, as Context7's REST
/// API (`https://context7.com/api/v2`) answers a handshake sent to it.
async fn web_api(status: &'static str, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/api/v2", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Its head, and the body its length says, before the answer.
                let (mut data, mut buf) = (Vec::new(), [0u8; 4096]);
                while let Ok(n @ 1..) = stream.read(&mut buf).await {
                    data.extend_from_slice(&buf[..n]);
                    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                    let length = (head.lines())
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes()).await;
            });
        }
    });
    url
}

/// A URL that's a web API's, not an MCP server's, answers the handshake
/// with its own JSON: Test says what it answered and what to check, never
/// an MCP error "without a message (error 0)".
#[tokio::test]
async fn a_web_api_s_answer_is_said_as_it_is_with_what_to_check() {
    let at = |url: String, headers: Vec<McpValue>| {
        server(
            "api",
            McpTransport::Url {
                url,
                headers,
                sse: false,
                oauth: None,
            },
        )
    };
    let tested = |server: McpServer| async move {
        let state = McpState::default();
        state
            .test(&server, &HashMap::new(), None, None)
            .await
            .unwrap_err()
    };
    let missing = web_api("404 Not Found", r#"{"error":"Not Found"}"#).await;
    let error = tested(at(missing, Vec::new())).await;
    assert!(
        error.contains(r#"HTTP 404: {"error":"Not Found"}"#),
        "{error}"
    );
    assert!(error.contains("check its URL"), "{error}");
    assert!(!error.contains("error 0"), "{error}");

    let keyed = web_api("401 Unauthorized", r#"{"error":"Unauthorized"}"#).await;
    let wrong = vec![header("Authorization", "Bearer wrong", false)];
    let error = tested(at(keyed, wrong)).await;
    assert!(error.contains("refused Rovik's credentials"), "{error}");
    assert!(error.contains("check its headers"), "{error}");

    let fine = web_api("200 OK", r#"{"status":"ok"}"#).await;
    let error = tested(at(fine, Vec::new())).await;
    assert!(
        error.contains(r#"isn't an MCP message (check its URL): {"status":"ok"}"#),
        "{error}"
    );
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
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    let listed = state.list(std::slice::from_ref(&modern), None, None).await;
    assert_eq!(listed[0].instructions, "SQL on Spanner.");
    let tools = listed[0].tools.clone().unwrap();
    assert_eq!(tools[0].name, "execute_sql");
    let stop = StopSignal::new();
    let result = state
        .call(
            &modern,
            None,
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
    let tested = state
        .test(&modern, &HashMap::new(), None, None)
        .await
        .unwrap();
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
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    let first = state.list(std::slice::from_ref(&gone), None, None).await[0]
        .tools
        .clone();
    assert!(first.is_err());
    let started = Instant::now();
    let again = state.list(std::slice::from_ref(&gone), None, None).await[0]
        .tools
        .clone();
    assert_eq!(again, first, "the same failure, not tried again");
    assert!(started.elapsed() < Duration::from_millis(100));
}

/// A run's listing doesn't wait for a server that hasn't answered: it goes
/// without its tools, saying so, and the user's message isn't held back
/// while the server takes its time.
#[tokio::test]
async fn a_run_doesnt_wait_for_a_server_that_says_nothing() {
    // A server that accepts the connection and then never answers: what a
    // command hanging on a tunnel, or a slow host, looks like.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let silent = server(
        "silent",
        McpTransport::Url {
            url,
            headers: Vec::new(),
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    let started = Instant::now();
    let listed = state
        .list_at_start(std::slice::from_ref(&silent), None, None)
        .await;
    assert_eq!(listed.len(), 1);
    let error = listed[0].tools.as_ref().unwrap_err();
    assert_eq!(error.as_str(), STILL_STARTING);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the run's listing waited {:?} for it",
        started.elapsed()
    );
    drop(listener);
}

/// A send whose server is still being started from an earlier one goes on
/// at once: it doesn't wait for that start, nor start the server again.
#[tokio::test]
async fn a_send_while_a_server_is_starting_isnt_waited_for_again() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let silent = server(
        "silent",
        McpTransport::Url {
            url,
            headers: Vec::new(),
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    let servers = std::slice::from_ref(&silent);
    let first = state.list_at_start(servers, None, None).await;
    assert_eq!(
        first[0].tools.as_ref().unwrap_err().as_str(),
        STILL_STARTING
    );
    // It is being started now; the next send doesn't wait for it.
    let started = Instant::now();
    let again = state.list_at_start(servers, None, None).await;
    assert_eq!(
        again[0].tools.as_ref().unwrap_err().as_str(),
        STILL_STARTING
    );
    assert!(
        started.elapsed() < LISTED_AT_START / 2,
        "the second send waited {:?} for the start",
        started.elapsed()
    );
    drop(listener);
}

/// Quitting doesn't wait for a start nobody waited for: the start stops
/// with the app, rather than holding its server's state for a handshake
/// that never answers (`START_WITHIN`).
#[tokio::test]
async fn shutdown_doesnt_wait_for_a_start_nobody_waits_for() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let silent = server(
        "silent",
        McpTransport::Url {
            url,
            headers: Vec::new(),
            sse: false,
            oauth: None,
        },
    );
    let state = McpState::default();
    let listed = state
        .list_at_start(std::slice::from_ref(&silent), None, None)
        .await;
    assert_eq!(
        listed[0].tools.as_ref().unwrap_err().as_str(),
        STILL_STARTING
    );
    // Its start is in flight, and nobody waits for it.
    let started = Instant::now();
    state.shutdown().await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutting down waited {:?} for the start",
        started.elapsed()
    );
    drop(listener);
}

/// A server that answers at once is listed for the run as it always was:
/// the short wait takes nothing from it.
#[tokio::test]
async fn a_run_lists_a_server_that_answers_at_once() {
    let (rovik, _calls) = RovikServer::start(&rovik_tools(), "", None).await.unwrap();
    let state = McpState::default();
    let token = rovik.authorization();
    state.set_secrets(Arc::new(move |_, _| Ok(Some(token.clone()))));
    let rovik_server = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
            sse: false,
            oauth: None,
        },
    );
    let listed = state
        .list_at_start(std::slice::from_ref(&rovik_server), None, None)
        .await;
    let tools = listed[0].tools.clone().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["bash", "read"]);
}

/// The fake command server (Python): `legacy` speaks the handshake and asks
/// Rovik a `ping` in the middle of a call; `modern` refuses the handshake
/// and wants `_meta` on every request; `wait` holds a call until the file
/// `<log>.release` exists; `crash` exits at once, saying why; `late` answers
/// the handshake half a second late, and notes that it did.
/// What it receives, its folder and its environment go in `log`, one JSON
/// line each.
const FAKE_SERVER: &str = r#"
import json, os, sys, time
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
        if mode == 'late':
            time.sleep(0.5)
        if mode == 'modern':
            send({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32601, 'message': 'Method not found'}})
        else:
            send({'jsonrpc': '2.0', 'id': id, 'result': {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fake', 'version': '1.2'}, 'instructions': 'Fake tools.'}})
            if mode == 'late':
                record({'answered': 'initialize'})
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
        if mode == 'wait':
            while not os.path.exists(log + '.release'):
                time.sleep(0.02)
        if name == 'note' and not arguments.get('note'):
            send({'jsonrpc': '2.0', 'id': id, 'error': {'code': -32602, 'message': 'note is required'}})
            continue
        if mode == 'legacy':
            send({'jsonrpc': '2.0', 'id': 'server-1', 'method': 'ping'})
            record(json.loads(sys.stdin.readline()))
        send({'jsonrpc': '2.0', 'id': id, 'result': {'content': [{'type': 'text', 'text': 'echo: ' + str(arguments.get('text'))}], 'isError': False}})
"#;

/// The fake server's command in `dir`, in `mode`, and its log. It runs on
/// python3: without it the test fails, saying so, rather than passing
/// with nothing tested.
#[cfg(unix)]
fn fake(dir: &Path, mode: &str) -> (McpTransport, PathBuf) {
    assert!(
        crate::process::on_path("python3"),
        "The MCP tests need python3 on PATH for their fake servers: install it."
    );
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
    (transport, log)
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

/// A server a run stopped waiting for keeps starting on its own: the run
/// after it lists the tools the first one went without.
#[cfg(unix)]
#[tokio::test]
async fn a_server_a_run_didnt_wait_for_is_started_anyway() {
    let dir = temp_dir();
    // It answers the handshake half a second late: past the run's wait.
    let (transport, log) = fake(&dir.0, "late");
    let state = McpState::default();
    with_token(&state);
    let late = server("late", transport);
    let listed = state
        .list_at_start(std::slice::from_ref(&late), None, None)
        .await;
    assert_eq!(
        listed[0].tools.as_ref().unwrap_err().as_str(),
        STILL_STARTING
    );
    // Nobody waits now, and it answers anyway: the start went on.
    let answered = || {
        logged(&log)
            .iter()
            .any(|line| line.get("answered") == Some(&json!("initialize")))
    };
    let started = Instant::now();
    while !answered() && started.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(answered(), "the start went on without a waiter");
    // And its session is the run's to list: nothing to start again.
    let next = state.list(std::slice::from_ref(&late), None, None).await;
    assert!(next[0].tools.is_ok(), "{:?}", next[0].tools);
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
    let (transport, log) = fake(&dir.0, "legacy");
    let project = dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let listed = state
        .list(std::slice::from_ref(&fake_server), Some(&project), None)
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
            None,
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
        .call(
            &fake_server,
            Some(&project),
            None,
            &tools[1],
            json!({}),
            &stop,
        )
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
        .list(std::slice::from_ref(&fake_server), Some(&project), None)
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
    let (transport, log) = fake(&dir.0, "legacy");
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let tools = state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await[0]
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
        .call(&fake_server, None, None, slow, json!({}), &stop)
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
    let (transport, log) = fake(&dir.0, "modern");
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let tools = state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await[0]
        .tools
        .clone()
        .unwrap();
    assert_eq!(tools.len(), 3);
    let stop = StopSignal::new();
    let echoed = state
        .call(
            &fake_server,
            None,
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
    let (transport, _log) = fake(&dir.0, "crash");
    let state = McpState::default();
    with_token(&state);
    let error = state.list(&[server("fake", transport)], None, None).await[0]
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
    let listed = McpState::default()
        .list(&[missing, off, bad], None, None)
        .await;
    assert_eq!(listed.len(), 1);
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("isn't installed"), "{error}");
}

/// The servers' saved secrets come out, by id and key, whether a server is
/// on or off (a start passes those on, Test the one it tries); a server
/// with a secret field and none saved has an empty entry, so a secret
/// cleared here clears on the host.
#[tokio::test]
async fn saved_secrets_are_the_servers_values_the_keychain_has_on_or_off() {
    let state = McpState::default();
    state.set_secrets(Arc::new(|id, key| match (id, key) {
        ("id-s1", "header/Authorization") => Ok(Some("Bearer x".into())),
        ("id-s2", _) => Ok(Some("other".into())),
        _ => Ok(None),
    }));
    let url = |headers| McpTransport::Url {
        url: "http://127.0.0.1:1/mcp".into(),
        headers,
        sse: false,
        oauth: None,
    };
    let s1 = server(
        "s1",
        url(vec![
            header("Authorization", "", true),
            header("X-Missing", "", true),
            header("X-Plain", "plain", false),
        ]),
    );
    let mut s2 = server("s2", url(vec![header("Authorization", "", true)]));
    s2.enabled = false;
    let secrets = state.saved_secrets(&[s1, s2]).await;
    assert_eq!(secrets.len(), 2);
    assert_eq!(secrets["id-s1"]["header/Authorization"].0, "Bearer x");
    assert_eq!(secrets["id-s1"].len(), 1, "{secrets:?}");
    assert_eq!(secrets["id-s2"]["header/Authorization"].0, "other", "off");

    let s3 = server("s3", url(vec![header("Authorization", "", true)]));
    let plain = server("s4", url(vec![header("X-Plain", "plain", false)]));
    let secrets = state.saved_secrets(&[s3, plain]).await;
    assert!(secrets["id-s3"].is_empty(), "{secrets:?}");
    assert!(!secrets.contains_key("id-s4"), "no secret field, no entry");
}

/// A server with a secret the keychain can't give is left out whole, so
/// the host keeps what it had; the other servers still come.
#[tokio::test]
async fn a_server_with_an_unreadable_secret_is_left_out_not_an_error() {
    let state = McpState::default();
    state.set_secrets(Arc::new(|id, key| match (id, key) {
        ("id-s1", "header/Locked") => Err("locked".into()),
        _ => Ok(Some("v".into())),
    }));
    let url = |headers| McpTransport::Url {
        url: "http://127.0.0.1:1/mcp".into(),
        headers,
        sse: false,
        oauth: None,
    };
    let s1 = server(
        "s1",
        url(vec![header("Locked", "", true), header("Open", "", true)]),
    );
    let s2 = server("s2", url(vec![header("Open", "", true)]));
    let secrets = state.saved_secrets(&[s1, s2]).await;
    assert!(!secrets.contains_key("id-s1"), "{secrets:?}");
    assert_eq!(secrets["id-s2"]["header/Open"].0, "v");
}

/// A project's values changed (allowed again, revoked, or a host's keys
/// removed): each of its servers starts again at its next use, and no
/// other project's (Q12, mcp-plan-2.md).
#[cfg(unix)]
#[tokio::test]
async fn forgetting_a_project_s_values_starts_its_servers_again_only() {
    let (one, other) = (temp_dir(), temp_dir());
    let state = McpState::default();
    let mut started = Vec::new();
    for (dir, id) in [(&one, "project/p1/h1"), (&other, "project/p2/h1")] {
        let (transport, log) = fake(&dir.0, "legacy");
        let mut fake = server("fake", transport);
        fake.id = id.into();
        let token = BTreeMap::from([("env/FAKE_TOKEN".to_string(), "t0ken".to_string())]);
        state.set_project_values(id, token);
        started.push((fake, log));
    }
    let starts = |log: &Path| logged(log).iter().filter(|m| m["token"] == "t0ken").count();
    for _ in 0..2 {
        for (fake, _) in &started {
            let listed = state.list(std::slice::from_ref(fake), None, None).await;
            assert!(listed[0].tools.is_ok(), "{:?}", listed[0].tools);
        }
        state.forget("project/p1");
    }
    assert!(exited(&started[0].1).await, "p1's stopped");
    assert_eq!(starts(&started[0].1), 2, "and started again");
    assert_eq!(starts(&started[1].1), 1, "p2's ran on");
}

/// Whether the fake server of `log` has seen its input close.
#[cfg(unix)]
async fn exited(log: &Path) -> bool {
    soon_logged(log, |m| m["bye"] == true).await
}

/// Whether the fake server of `log` logs a line that `check` picks within
/// four seconds.
#[cfg(unix)]
async fn soon_logged(log: &Path, check: impl Fn(&Value) -> bool) -> bool {
    for _ in 0..200 {
        if logged(log).iter().any(&check) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A daemon's sweep: a server unused for a while is stopped, unless a call
/// is running on it; the next list starts it again.
#[cfg(unix)]
#[tokio::test]
async fn an_idle_server_is_stopped_and_one_in_use_is_not() {
    let dir = temp_dir();
    let (transport, log) = fake(&dir.0, "wait");
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    let tools = state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await[0]
        .tools
        .clone()
        .unwrap();
    let echo = tools.iter().find(|t| t.name == "echo").unwrap().clone();
    let calling = {
        let (state, fake_server) = (state.clone(), fake_server.clone());
        tokio::spawn(async move {
            state
                .call(
                    &fake_server,
                    None,
                    None,
                    &echo,
                    json!({ "text": "hi" }),
                    &StopSignal::new(),
                )
                .await
        })
    };
    assert!(
        soon_logged(&log, |m| m["method"] == "tools/call").await,
        "the call never reached the server: {:?}",
        logged(&log)
    );
    state.stop_idle(Duration::ZERO);
    std::fs::write(format!("{}.release", log.display()), "").unwrap();
    let result = calling.await.unwrap().unwrap();
    assert_eq!(result["content"][0]["text"], "echo: hi");
    let starts = || {
        logged(&log)
            .iter()
            .filter(|m| m.get("cwd").is_some())
            .count()
    };
    // The sweep left the instance in the map: listing it starts nothing.
    state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await;
    assert_eq!(starts(), 1, "the server in use was not stopped");

    state.stop_idle(Duration::ZERO);
    assert!(exited(&log).await, "{:?}", logged(&log));
    state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await;
    assert_eq!(starts(), 2, "started again by the next list");
    state.shutdown().await;
}

/// A server idle for less than the time given is left running.
#[cfg(unix)]
#[tokio::test]
async fn a_recently_used_server_is_not_stopped_as_idle() {
    let dir = temp_dir();
    let (transport, log) = fake(&dir.0, "legacy");
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await;
    state.stop_idle(Duration::from_secs(3600));
    state
        .list(std::slice::from_ref(&fake_server), None, None)
        .await;
    let starts = logged(&log)
        .iter()
        .filter(|m| m.get("cwd").is_some())
        .count();
    assert_eq!(starts, 1);
    state.shutdown().await;
}

/// A chat's worktree that goes takes its servers with it, not another
/// folder's.
#[cfg(unix)]
#[tokio::test]
async fn a_worktree_s_servers_stop_when_it_goes() {
    let dir = temp_dir();
    let (transport, log) = fake(&dir.0, "legacy");
    let (a, b) = (dir.0.join("a"), dir.0.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let state = McpState::default();
    with_token(&state);
    let fake_server = server("fake", transport);
    state
        .list(std::slice::from_ref(&fake_server), Some(&a), None)
        .await;
    state
        .list(std::slice::from_ref(&fake_server), Some(&b), None)
        .await;
    state.stop_in(&a);
    assert!(exited(&log).await, "{:?}", logged(&log));
    let byes = || logged(&log).iter().filter(|m| m["bye"] == true).count();
    assert_eq!(byes(), 1, "only a's process exited");
    let tools = state
        .list(std::slice::from_ref(&fake_server), Some(&b), None)
        .await[0]
        .tools
        .clone();
    assert!(tools.is_ok(), "b's still answers");
    assert_eq!(byes(), 1);
    state.shutdown().await;
}

/// A relay that posts as the app on the user's computer would, and keeps
/// the computers it posted on; `away` refuses every post, as a relay to a
/// computer that isn't connected does (Q11).
#[derive(Default)]
struct Posting {
    computers: Mutex<Vec<String>>,
    away: bool,
}

impl Relay for Posting {
    fn computer_of(&self, chat: &str) -> Option<String> {
        (chat == "chat").then(|| "laptop".into())
    }

    fn post(
        &self,
        computer: &str,
        post: RelayedPost,
    ) -> BoxFuture<'static, Result<reqwest::Response, String>> {
        lock(&self.computers).push(computer.to_string());
        let away = self.away;
        Box::pin(async move {
            if away {
                return Err("It runs on the user's computer, which is away.".into());
            }
            let client = crate::net::client().await;
            let mut request = client.post(&post.url).body(post.body);
            for (name, value) in &post.headers {
                request = request.header(name.as_str(), value.0.as_str());
            }
            request.send().await.map_err(|e| e.to_string())
        })
    }
}

/// Rovik's own server, on this machine's loopback, with its token saved.
async fn loopback_server(state: &McpState) -> (RovikServer, McpServer) {
    let (rovik, mut calls) = RovikServer::start(&rovik_tools(), "", None).await.unwrap();
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let _ = call.reply.send(McpReply::text("read it"));
        }
    });
    let token = rovik.authorization();
    state.set_secrets(Arc::new(move |_, _| Ok(Some(token.clone()))));
    let server = server(
        "rovik",
        McpTransport::Url {
            url: rovik.url(),
            headers: vec![header("Authorization", "", true)],
            sse: false,
            oauth: None,
        },
    );
    (rovik, server)
}

/// Q11 (mcp-plan-2.md): on a daemon, a server on the loopback is on the
/// user's computer: its every POST goes through the relay, on the computer
/// the chat belongs to, with the server's secret header, and its tools are
/// listed and called as any server's.
#[tokio::test]
async fn on_a_daemon_a_loopback_server_is_reached_through_the_chat_s_computer() {
    let state = McpState::default();
    let (_rovik, server) = loopback_server(&state).await;
    let relay = Arc::new(Posting::default());
    state.set_relay(relay.clone());
    let via = state.via("chat");
    assert_eq!(via.as_deref(), Some("laptop"));
    let listed = state
        .list(std::slice::from_ref(&server), None, via.as_deref())
        .await;
    let tools = listed[0].tools.clone().unwrap();
    let read = tools.iter().find(|t| t.name == "read").unwrap();
    let stop = StopSignal::new();
    let result = state
        .call(
            &server,
            None,
            via.as_deref(),
            read,
            json!({ "path": "a" }),
            &stop,
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "read it");
    let computers = lock(&relay.computers).clone();
    assert!(computers.len() >= 3, "{computers:?}");
    assert!(computers.iter().all(|c| c == "laptop"), "{computers:?}");
}

/// A computer that's away has its loopback servers unavailable, saying
/// so; and with no computer to go through, a loopback server can't be
/// reached from the daemon at all.
#[tokio::test]
async fn a_loopback_server_is_unavailable_while_its_computer_is_away() {
    let state = McpState::default();
    let (_rovik, server) = loopback_server(&state).await;
    state.set_relay(Arc::new(Posting {
        away: true,
        ..Posting::default()
    }));
    let listed = state
        .list(std::slice::from_ref(&server), None, Some("laptop"))
        .await;
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("which is away"), "{error}");
    let listed = state.list(std::slice::from_ref(&server), None, None).await;
    let error = listed[0].tools.clone().unwrap_err();
    assert!(error.contains("this chat belongs to none"), "{error}");
}

/// Each computer's loopback is its own: a server listed for two computers'
/// chats runs twice, once through each.
#[tokio::test]
async fn each_computer_s_loopback_server_is_an_instance_of_its_own() {
    let state = McpState::default();
    let (_rovik, server) = loopback_server(&state).await;
    let relay = Arc::new(Posting::default());
    state.set_relay(relay.clone());
    for computer in ["laptop", "desk"] {
        let listed = state
            .list(std::slice::from_ref(&server), None, Some(computer))
            .await;
        assert!(listed[0].tools.is_ok(), "{computer}");
    }
    let computers = lock(&relay.computers).clone();
    assert!(computers.contains(&"laptop".to_string()) && computers.contains(&"desk".to_string()));
    assert_eq!(lock(&state.0.servers).len(), 2);
}

/// Without a relay, as in the app, a loopback server is posted to straight;
/// and only localhost, 127.0.0.0/8 and [::1] are the loopback.
#[tokio::test]
async fn only_a_daemon_relays_and_only_the_loopback() {
    let state = McpState::default();
    let (_rovik, server) = loopback_server(&state).await;
    assert_eq!(state.via("chat"), None);
    let listed = state
        .list(std::slice::from_ref(&server), None, Some("laptop"))
        .await;
    assert!(listed[0].tools.is_ok(), "straight, with no relay");
    for url in [
        "http://localhost:3000/mcp",
        "http://LOCALHOST/mcp",
        "http://127.0.0.1:8080/mcp",
        "http://127.4.5.6/mcp",
        "http://[::1]:3000/mcp",
    ] {
        assert!(on_loopback(url), "{url}");
    }
    for url in [
        "https://mcp.example.com/mcp",
        "http://10.0.0.5:3000/mcp",
        "http://[::2]/mcp",
        "http://localhost.example.com/mcp",
        "not a url",
    ] {
        assert!(!on_loopback(url), "{url}");
    }
}

/// A relayed answer reads as the server's: its status, its headers, its
/// body's parts in order; an error among them ends it as a broken
/// connection would.
#[tokio::test]
async fn a_relayed_answer_reads_as_the_server_s_own() {
    let parts = vec![Ok("data: ".to_string()), Ok("{}\n\n".to_string())];
    let headers = [
        ("content-type".to_string(), "text/event-stream".to_string()),
        ("bad header".to_string(), "x".to_string()),
    ];
    let response = relayed_response(200, &headers, futures_util::stream::iter(parts)).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.text().await.unwrap(), "data: {}\n\n");
    let broken = vec![
        Ok("half".to_string()),
        Err("the computer went away".to_string()),
    ];
    let response = relayed_response(200, &[], futures_util::stream::iter(broken)).unwrap();
    assert!(response.text().await.is_err());
}

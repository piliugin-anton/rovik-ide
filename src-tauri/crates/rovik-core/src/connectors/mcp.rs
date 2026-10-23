//! Rovik's tools over MCP, for a harness that runs them through Rovik
//! instead of its own (connectors/mod.rs, "Rovik's tools" mode): a small
//! MCP server on the loopback interface, one per harness turn, that lists
//! the tools of the run's request and hands each call to the connector's
//! code, which answers it with the result Rovik's run stored.
//!
//! It speaks MCP's Streamable HTTP transport, as much as a harness needs:
//!
//! - `POST /mcp` takes one JSON-RPC message. `initialize`, `tools/list`
//!   and `ping` are answered at once, as JSON; a notification gets 202;
//!   anything else is "method not found" (a client's probe for a newer
//!   protocol falls back to `initialize` on it).
//! - `tools/call` is answered as an event stream: the call goes to the
//!   connector ([`McpCall`]) and the stream stays open, a comment every
//!   few seconds, until its result comes, which can take as long as the
//!   user takes to approve it. Claude Code sends the tool use id the call
//!   is for (`_meta["claudecode/toolUseId"]`), which matches it to the
//!   call in its stream.
//! - `GET` (a stream of server messages) isn't offered: 405.
//!
//! Only the harness may call: the server listens on 127.0.0.1 alone, and
//! every request carries a bearer token made for it, which the harness
//! reads from a file only this user can read. On a shared server, other
//! users reach the port but not the token.
//!
//! Each tool's entry asks Claude Code to load it up front rather than
//! behind its tool search (`anthropic/alwaysLoad`), to keep results up to
//! 500,000 characters in the conversation rather than in a file it would
//! need its own tools to read (`anthropic/maxResultSizeChars`), and marks
//! the read-only ones, which it then calls in parallel (`readOnlyHint`).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// The server's name in the harness's configuration: its tools are
/// `mcp__rovik__<name>` there.
pub const SERVER_NAME: &str = "rovik";
/// What a harness puts before a tool's name.
pub const TOOL_PREFIX: &str = "mcp__rovik__";
/// The protocol version answered when the client names none.
const PROTOCOL: &str = "2025-06-18";
/// The most a tool's text result may be before Claude Code saves it to a
/// file: its ceiling.
const MAX_RESULT_CHARS: u64 = 500_000;
/// A comment on a call's open stream this often, so nothing between the
/// two closes it as idle while the user decides on an approval.
const KEEPALIVE: Duration = Duration::from_secs(15);
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 16 << 20;
/// Rovik's tools that change nothing, which the harness may call in parallel.
const READ_ONLY: &[&str] = &[
    "read",
    "list_dir",
    "search_code",
    "outline",
    "callers",
    "neighbours",
    "diagnostics",
];

/// A call the harness made: the connector answers it through `reply`.
#[derive(Debug)]
pub struct McpCall {
    /// The tool use it's for, when the harness says (Claude Code does).
    pub tool_use_id: Option<String>,
    /// The tool's name as Rovik names it, without the prefix.
    pub name: String,
    pub arguments: Value,
    pub reply: oneshot::Sender<McpReply>,
}

/// A call's result.
#[derive(Debug, Clone, PartialEq)]
pub struct McpReply {
    pub text: String,
    pub is_error: bool,
}

impl McpReply {
    pub fn text(text: impl Into<String>) -> McpReply {
        McpReply {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> McpReply {
        McpReply {
            text: text.into(),
            is_error: true,
        }
    }
}

/// What every connection shares.
struct Shared {
    token: String,
    session: String,
    tools: Vec<Value>,
    instructions: String,
    calls: mpsc::UnboundedSender<McpCall>,
}

/// The server, listening until it's dropped.
pub struct McpServer {
    pub port: u16,
    token: String,
    accept: JoinHandle<()>,
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl McpServer {
    /// Listens on 127.0.0.1, on a port of the system's choosing, offering
    /// `tools` (OpenAI function schemas, as the run's request has them).
    /// The calls come on the receiver.
    pub async fn start(
        tools: &[Value],
        instructions: &str,
    ) -> std::io::Result<(McpServer, mpsc::UnboundedReceiver<McpCall>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (calls, received) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            token: token.clone(),
            session: uuid::Uuid::new_v4().to_string(),
            tools: mcp_tools(tools),
            instructions: instructions.to_string(),
            calls,
        });
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, shared).await {
                        log::debug!("rovik mcp: {e}");
                    }
                });
            }
        });
        Ok((
            McpServer {
                port,
                token,
                accept,
            },
            received,
        ))
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// The harness's MCP configuration for it, with the token: for a file
    /// only this user can read, never a command line.
    pub fn config_json(&self) -> String {
        json!({
            "mcpServers": {
                SERVER_NAME: {
                    "type": "http",
                    "url": self.url(),
                    "headers": { "Authorization": format!("Bearer {}", self.token) },
                }
            }
        })
        .to_string()
    }
}

/// Rovik's tools (OpenAI function schemas) as MCP lists them.
pub fn mcp_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?;
            let schema = function
                .get("parameters")
                .filter(|p| p.is_object())
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            let mut entry = json!({
                "name": name,
                "description": function.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                "inputSchema": schema,
                "_meta": {
                    "anthropic/alwaysLoad": true,
                    "anthropic/maxResultSizeChars": MAX_RESULT_CHARS,
                },
            });
            if READ_ONLY.contains(&name) {
                entry["annotations"] = json!({ "readOnlyHint": true });
            }
            Some(entry)
        })
        .collect()
}

/// One HTTP request.
struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// One connection: its requests, one after another, until either side
/// closes it (a call's answer closes it).
async fn serve(stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let Some(request) = read_request(&mut reader).await? else {
            return Ok(());
        };
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|a| a.strip_prefix("Bearer "))
            .map(str::trim);
        if !bearer.is_some_and(|t| same(t, &shared.token)) {
            return respond(&mut write, 401, &[], b"", true).await;
        }
        if request.path.split('?').next() != Some("/mcp") {
            return respond(&mut write, 404, &[], b"", true).await;
        }
        match request.method.as_str() {
            "POST" => {}
            "DELETE" => {
                respond(&mut write, 200, &[], b"", false).await?;
                continue;
            }
            _ => {
                respond(&mut write, 405, &[("Allow", "POST")], b"", false).await?;
                continue;
            }
        }
        let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
            let body = rpc_error(Value::Null, -32700, "Parse error");
            return respond_json(&mut write, &body, &[], true).await;
        };
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        let method = message.get("method").and_then(|m| m.as_str());
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (id, method) {
            // A notification, or the client's answer to something.
            (None, _) | (Some(_), None) => respond(&mut write, 202, &[], b"", false).await?,
            (Some(id), Some("tools/call")) => return call(&mut write, &shared, id, &params).await,
            (Some(id), Some(method)) => {
                let (body, session) = answer(&shared, id, method, &params);
                let headers: Vec<(&str, &str)> = match &session {
                    Some(session) => vec![("Mcp-Session-Id", session.as_str())],
                    None => Vec::new(),
                };
                respond_json(&mut write, &body, &headers, false).await?;
            }
        }
    }
}

/// A token compared without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// What a request other than a call gets, and the session id with
/// `initialize`'s.
fn answer(shared: &Shared, id: Value, method: &str, params: &Value) -> (Value, Option<String>) {
    let result = match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL);
            let result = json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "title": "Rovik IDE",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": shared.instructions,
            });
            return (rpc_result(id, result), Some(shared.session.clone()));
        }
        "tools/list" => json!({ "tools": shared.tools }),
        "ping" => json!({}),
        _ => return (rpc_error(id, -32601, "Method not found"), None),
    };
    (rpc_result(id, result), None)
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A call: handed to the connector, its stream kept open until the result
/// comes, then answered and closed.
async fn call(
    write: &mut OwnedWriteHalf,
    shared: &Shared,
    id: Value,
    params: &Value,
) -> std::io::Result<()> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let tool_use_id = params
        .get("_meta")
        .and_then(|m| m.get("claudecode/toolUseId"))
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let arguments = params
        .get("arguments")
        .cloned()
        .filter(|a| !a.is_null())
        .unwrap_or_else(|| json!({}));
    let (reply, mut replied) = oneshot::channel();
    let sent = shared.calls.send(McpCall {
        tool_use_id,
        name,
        arguments,
        reply,
    });
    if sent.is_err() {
        let gone = McpReply::error("Rovik's run has ended: this call wasn't run.");
        return respond_json(write, &call_result(id, &gone), &[], true).await;
    }
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    write.write_all(head.as_bytes()).await?;
    write.flush().await?;
    let result = loop {
        tokio::select! {
            got = &mut replied => {
                break got.unwrap_or_else(|_| McpReply::error("Rovik's run ended before this call was answered."));
            }
            _ = tokio::time::sleep(KEEPALIVE) => {
                // The harness is gone: nothing to answer.
                write.write_all(b": keepalive\n\n").await?;
                write.flush().await?;
            }
        }
    };
    let event = format!("event: message\ndata: {}\n\n", call_result(id, &result));
    write.write_all(event.as_bytes()).await?;
    write.flush().await?;
    write.shutdown().await
}

fn call_result(id: Value, reply: &McpReply) -> Value {
    rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": reply.text }],
            "isError": reply.is_error,
        }),
    )
}

async fn respond_json(
    write: &mut OwnedWriteHalf,
    body: &Value,
    headers: &[(&str, &str)],
    close: bool,
) -> std::io::Result<()> {
    let mut all = vec![("Content-Type", "application/json")];
    all.extend_from_slice(headers);
    respond(write, 200, &all, body.to_string().as_bytes(), close).await
}

async fn respond(
    write: &mut OwnedWriteHalf,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
    close: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(if close {
        "Connection: close\r\n\r\n"
    } else {
        "Connection: keep-alive\r\n\r\n"
    });
    write.write_all(head.as_bytes()).await?;
    write.write_all(body).await?;
    write.flush().await?;
    if close {
        write.shutdown().await?;
    }
    Ok(())
}

/// The next request on the connection; none once the client closed it.
async fn read_request<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Request>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut line = String::new();
    // Blank lines between requests are allowed.
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(bad("malformed request line"));
    };
    let (method, path) = (method.to_string(), path.to_string());
    let mut headers = HashMap::new();
    let mut head = line.len();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Err(bad("the headers didn't end"));
        }
        head += line.len();
        if head > MAX_HEAD {
            return Err(bad("headers too long"));
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        read_chunked(reader).await?
    } else {
        let length: usize = match headers.get("content-length") {
            Some(n) => n.parse().map_err(|_| bad("bad content-length"))?,
            None => 0,
        };
        if length > MAX_BODY {
            return Err(bad("body too large"));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        body
    };
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

async fn read_chunked<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Vec<u8>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| bad("bad chunk size"))?;
        if size == 0 {
            // Trailers, to the blank line.
            loop {
                line.clear();
                if reader.read_line(&mut line).await? == 0 || line.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(bad("body too large"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).await?;
        line.clear();
        reader.read_line(&mut line).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![
            json!({ "type": "function", "function": { "name": "read", "description": "Read a file.", "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } } }),
            json!({ "type": "function", "function": { "name": "bash", "description": "Run a command." } }),
        ]
    }

    /// One request on a fresh connection; the response's status, headers
    /// and body, read to the connection's end or the body's length.
    async fn send(port: u16, token: &str, method: &str, body: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!(
            "{method} /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).await.unwrap();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let mut head = String::new();
        let mut length = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = Some(n.trim().parse::<usize>().unwrap());
            }
            head.push_str(&line);
        }
        let mut body = String::new();
        match length {
            Some(n) => {
                let mut bytes = vec![0; n];
                reader.read_exact(&mut bytes).await.unwrap();
                body = String::from_utf8(bytes).unwrap();
            }
            None => {
                reader.read_to_string(&mut body).await.unwrap();
            }
        }
        (status, head, body)
    }

    fn token_of(server: &McpServer) -> String {
        let config: Value = serde_json::from_str(&server.config_json()).unwrap();
        let header = config["mcpServers"]["rovik"]["headers"]["Authorization"]
            .as_str()
            .unwrap();
        header.strip_prefix("Bearer ").unwrap().to_string()
    }

    #[test]
    fn rovik_tools_are_listed_as_mcp_tools() {
        let listed = mcp_tools(&tools());
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0]["name"], "read");
        assert_eq!(listed[0]["inputSchema"]["required"][0], "path");
        assert_eq!(listed[0]["annotations"]["readOnlyHint"], true);
        assert_eq!(listed[0]["_meta"]["anthropic/alwaysLoad"], true);
        assert_eq!(listed[0]["_meta"]["anthropic/maxResultSizeChars"], 500_000);
        // No parameters: an empty object schema; not read-only.
        assert_eq!(
            listed[1]["inputSchema"],
            json!({ "type": "object", "properties": {} })
        );
        assert!(listed[1].get("annotations").is_none());
    }

    #[tokio::test]
    async fn the_server_answers_the_handshake_and_lists_the_tools() {
        let (server, _calls) = McpServer::start(&tools(), "Rovik's tools.").await.unwrap();
        let token = token_of(&server);
        let config: Value = serde_json::from_str(&server.config_json()).unwrap();
        assert_eq!(config["mcpServers"]["rovik"]["type"], "http");
        assert_eq!(config["mcpServers"]["rovik"]["url"], server.url());

        let (status, head, body) = send(server.port, &token, "POST", r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#).await;
        assert_eq!(status, 200);
        assert!(head.contains("Mcp-Session-Id"), "{head}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(body["result"]["serverInfo"]["name"], "rovik");
        assert_eq!(body["result"]["instructions"], "Rovik's tools.");

        let (status, _, body) = send(
            server.port,
            &token,
            "POST",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;
        assert_eq!(status, 200);
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 2);

        // A notification; a probe for a newer protocol; a stream of server messages.
        assert_eq!(
            send(
                server.port,
                &token,
                "POST",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .0,
            202
        );
        let (_, _, body) = send(
            server.port,
            &token,
            "POST",
            r#"{"jsonrpc":"2.0","id":"p","method":"server/discover"}"#,
        )
        .await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
            -32601
        );
        assert_eq!(send(server.port, &token, "GET", "").await.0, 405);
    }

    #[tokio::test]
    async fn only_the_token_is_let_in() {
        let (server, _calls) = McpServer::start(&tools(), "").await.unwrap();
        assert_eq!(
            send(
                server.port,
                "wrong",
                "POST",
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#
            )
            .await
            .0,
            401
        );
        let token = token_of(&server);
        let shorter = &token[..token.len() - 1];
        assert_eq!(
            send(
                server.port,
                shorter,
                "POST",
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#
            )
            .await
            .0,
            401
        );
    }

    #[tokio::test]
    async fn a_call_waits_for_its_result_and_is_answered_as_an_event() {
        let (server, mut calls) = McpServer::start(&tools(), "").await.unwrap();
        let token = token_of(&server);
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read","arguments":{"path":"a.rs"},"_meta":{"claudecode/toolUseId":"toolu_1"}}}"#;
        let port = server.port;
        let request = tokio::spawn(async move { send(port, &token, "POST", body).await });
        let call = calls.recv().await.unwrap();
        assert_eq!(call.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, json!({ "path": "a.rs" }));
        call.reply.send(McpReply::text("fn a() {}")).unwrap();
        let (status, head, body) = request.await.unwrap();
        assert_eq!(status, 200);
        assert!(head.contains("text/event-stream"), "{head}");
        let data = body.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
        let data: Value = serde_json::from_str(data).unwrap();
        assert_eq!(data["id"], 7);
        assert_eq!(data["result"]["content"][0]["text"], "fn a() {}");
        assert_eq!(data["result"]["isError"], false);
    }

    #[tokio::test]
    async fn a_call_whose_run_ended_is_answered_as_an_error() {
        let (server, calls) = McpServer::start(&tools(), "").await.unwrap();
        let token = token_of(&server);
        drop(calls);
        let (_, _, body) = send(server.port, &token, "POST", r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bash","arguments":{}}}"#).await;
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["result"]["isError"], true);
    }

    #[tokio::test]
    async fn chunked_bodies_are_read() {
        let mut input: &[u8] = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut reader = BufReader::new(&mut input);
        assert_eq!(read_chunked(&mut reader).await.unwrap(), b"hello world");
    }
}

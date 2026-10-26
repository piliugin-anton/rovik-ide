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
//!   user takes to approve it. The harness sends the tool use id the call
//!   is for, which matches it to the call in its stream: Claude Code as
//!   `_meta["claudecode/toolUseId"]`, Codex as `_meta.callId`.
//! - `GET` (a stream of server messages) isn't offered: 405.
//!
//! It also takes a harness's hooks, as Claude Code's `http` hooks post
//! them: `POST /hooks` with the hook's event as its body. The event goes
//! to the connector as a call named [`HOOK`], and the answer is the JSON
//! the connector replies, in a chunked body that a space keeps open every
//! few seconds until then: what the hook waits for (a toolchain's
//! install) may take minutes.
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

use crate::overlay::confine::network::Network;
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
/// The name a hook's event comes with to the connector: never one of
/// Rovik's tools.
pub const HOOK: &str = "hook";
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
    /// The tool use it's for, when the harness says (Claude Code and Codex
    /// do).
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
    /// `tools` (OpenAI function schemas, as the run's request has them):
    /// inside `network` when the harness runs there (a team server's
    /// private network, overlay/confine/network.rs), where other users
    /// can't reach the port at all. The calls come on the receiver.
    pub async fn start(
        tools: &[Value],
        instructions: &str,
        network: Option<&Arc<Network>>,
    ) -> std::io::Result<(McpServer, mpsc::UnboundedReceiver<McpCall>)> {
        let (calls, received) = mpsc::unbounded_channel();
        let server = Self::start_sending(tools, instructions, network, calls).await?;
        Ok((server, received))
    }

    /// `start`, its calls sent on `calls`: two servers of one run (Rovik's
    /// tools, and the hooks the harness calls as tools) share a receiver.
    pub async fn start_sending(
        tools: &[Value],
        instructions: &str,
        network: Option<&Arc<Network>>,
        calls: mpsc::UnboundedSender<McpCall>,
    ) -> std::io::Result<McpServer> {
        let listener = match network {
            Some(network) => {
                let network = network.clone();
                let listener = crate::rt::spawn_blocking(move || network.listen())
                    .await
                    .map_err(std::io::Error::other)??;
                listener.set_nonblocking(true)?;
                TcpListener::from_std(listener)?
            }
            None => TcpListener::bind(("127.0.0.1", 0)).await?,
        };
        let port = listener.local_addr()?.port();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
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
        Ok(McpServer {
            port,
            token,
            accept,
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// Where a harness posts its hooks' events.
    pub fn hooks_url(&self) -> String {
        format!("http://127.0.0.1:{}/hooks", self.port)
    }

    /// The `Authorization` header every request carries, with the token:
    /// for a file only this user can read, never a command line.
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// The token alone, for a harness that reads it from a variable of its
    /// environment (Codex's `bearer_token_env_var`): never a command line.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The harness's MCP configuration for it, with the token: for a file
    /// only this user can read, never a command line.
    pub fn config_json(&self) -> String {
        json!({
            "mcpServers": {
                SERVER_NAME: {
                    "type": "http",
                    "url": self.url(),
                    "headers": { "Authorization": self.authorization() },
                }
            }
        })
        .to_string()
    }
}

/// What a harness's environment gets so it reaches Rovik's servers on
/// the loopback interface directly, never through a proxy it's given:
/// `127.0.0.1` and `localhost` added to `NO_PROXY` (both spellings).
pub fn loopback_env() -> Vec<(String, String)> {
    ["NO_PROXY", "no_proxy"]
        .iter()
        .map(|key| {
            let now = crate::process::child_var(key)
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default();
            let value = if now.trim().is_empty() {
                "127.0.0.1,localhost".to_string()
            } else {
                format!("{now},127.0.0.1,localhost")
            };
            (key.to_string(), value)
        })
        .collect()
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
        match request.path.split('?').next() {
            Some("/mcp") => {}
            Some("/hooks") if request.method == "POST" => {
                return hook(&mut write, &shared, &request.body).await
            }
            _ => return respond(&mut write, 404, &[], b"", true).await,
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
    let meta = params.get("_meta");
    let tool_use_id = ["claudecode/toolUseId", "callId"]
        .iter()
        .find_map(|key| meta?.get(*key)?.as_str())
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

/// A hook's event: handed to the connector as a call named [`HOOK`], its
/// answer the JSON the connector replies, `{}` when the run is over. The
/// body is chunked, a space at a time while the connector works: the
/// harness's client sees the response start at once, and its wait for the
/// body is never idle for long.
async fn hook(write: &mut OwnedWriteHalf, shared: &Shared, body: &[u8]) -> std::io::Result<()> {
    let Ok(event) = serde_json::from_slice::<Value>(body) else {
        return respond(write, 400, &[], b"", true).await;
    };
    let tool_use_id = event
        .get("tool_use_id")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let (reply, mut replied) = oneshot::channel();
    let sent = shared.calls.send(McpCall {
        tool_use_id,
        name: HOOK.to_string(),
        arguments: event,
        reply,
    });
    if sent.is_err() {
        return respond_json(write, &json!({}), &[], true).await;
    }
    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    write.write_all(head.as_bytes()).await?;
    write.flush().await?;
    let answer = loop {
        tokio::select! {
            got = &mut replied => break got.map(|r| r.text).unwrap_or_else(|_| "{}".into()),
            _ = tokio::time::sleep(KEEPALIVE) => {
                write.write_all(b"1\r\n \r\n").await?;
                write.flush().await?;
            }
        }
    };
    let last = format!("{:x}\r\n{answer}\r\n0\r\n\r\n", answer.len());
    write.write_all(last.as_bytes()).await?;
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
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        send_on(stream, token, method, body).await
    }

    /// `send`, on `stream`.
    async fn send_on(
        mut stream: TcpStream,
        token: &str,
        method: &str,
        body: &str,
    ) -> (u16, String, String) {
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

    /// Posts `event` to the server's hooks with `token`: the status, and
    /// the body as its chunks make it.
    async fn post_hook(port: u16, token: &str, event: &Value) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let body = event.to_string();
        let request = format!(
            "POST /hooks HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut all = String::new();
        stream.read_to_string(&mut all).await.unwrap();
        let (head, mut rest) = all.split_once("\r\n\r\n").unwrap();
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        if !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            return (status, rest.to_string());
        }
        let mut body = String::new();
        loop {
            let (size, after) = rest.split_once("\r\n").unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            body.push_str(&after[..size]);
            rest = &after[size + 2..];
        }
        (status, body)
    }

    /// A hook's event reaches the connector as a call named `hook`, with
    /// the tool use it's for; its answer is the response. Without the token
    /// it's refused; with the run over, it's answered with nothing to say.
    #[tokio::test]
    async fn a_hooks_event_is_answered_by_the_connector() {
        let (server, mut calls) = McpServer::start(&[], "test", None).await.unwrap();
        let token = token_of(&server);
        let event = json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_use_id": "toolu_1",
            "tool_input": { "command": "npm test" },
            "error": "Exit code 127\n/bin/bash: line 1: npm: command not found",
        });
        let expected = event.clone();
        let answering = tokio::spawn(async move {
            let call = calls.recv().await.unwrap();
            assert_eq!(call.name, HOOK);
            assert_eq!(call.tool_use_id.as_deref(), Some("toolu_1"));
            assert_eq!(call.arguments, expected);
            let said = json!({ "hookSpecificOutput": { "additionalContext": "Installed." } });
            call.reply.send(McpReply::text(said.to_string())).unwrap();
            calls
        });
        let (status, body) = post_hook(server.port, &token, &event).await;
        assert_eq!(status, 200);
        let body: Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(
            body["hookSpecificOutput"]["additionalContext"],
            "Installed."
        );
        let calls = answering.await.unwrap();

        let (refused, _) = post_hook(server.port, "not-the-token", &event).await;
        assert_eq!(refused, 401);
        drop(calls);
        let (status, body) = post_hook(server.port, &token, &event).await;
        assert_eq!((status, body.trim()), (200, "{}"));
        assert!(server.hooks_url().ends_with("/hooks"));
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
        let (server, _calls) = McpServer::start(&tools(), "Rovik's tools.", None)
            .await
            .unwrap();
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

    /// In a team server's private network the server listens inside it,
    /// where the harness runs: reached there, not on the host's loopback.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    async fn in_a_private_network_the_server_is_reached_only_inside_it() {
        let Some((_dir, network)) = crate::overlay::confine::network::for_test(
            std::path::Path::new("/bin/true"),
            &[],
            None,
        ) else {
            return;
        };
        let network = Arc::new(network);
        let (server, _calls) = McpServer::start(&tools(), "", Some(&network))
            .await
            .unwrap();
        let token = token_of(&server);
        let on_host = crate::listening::sockets(std::path::Path::new("/proc/net")).unwrap();
        if !on_host.values().any(|&port| port == server.port) {
            assert!(TcpStream::connect(("127.0.0.1", server.port))
                .await
                .is_err());
        }
        let inside = network.connect(server.port).unwrap();
        inside.set_nonblocking(true).unwrap();
        let inside = TcpStream::from_std(inside).unwrap();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let (status, _, answer) = send_on(inside, &token, "POST", body).await;
        assert_eq!(status, 200, "{answer}");
    }

    #[tokio::test]
    async fn only_the_token_is_let_in() {
        let (server, _calls) = McpServer::start(&tools(), "", None).await.unwrap();
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
        let (server, mut calls) = McpServer::start(&tools(), "", None).await.unwrap();
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

    /// Codex names the call it's for as `_meta.callId`; two servers of a
    /// run can share one receiver.
    #[tokio::test]
    async fn codex_s_call_id_names_the_tool_use_and_servers_share_a_receiver() {
        let (sender, mut calls) = mpsc::unbounded_channel();
        let tools_server = McpServer::start_sending(&tools(), "", None, sender.clone())
            .await
            .unwrap();
        let hooks_server = McpServer::start_sending(&[], "", None, sender)
            .await
            .unwrap();
        assert_ne!(tools_server.port, hooks_server.port);
        assert_eq!(
            hooks_server.authorization(),
            format!("Bearer {}", hooks_server.token())
        );
        let token = token_of(&hooks_server);
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"hook","arguments":{},"_meta":{"callId":"exec-1","threadId":"t"}}}"#;
        let port = hooks_server.port;
        let request = tokio::spawn(async move { send(port, &token, "POST", body).await });
        let call = calls.recv().await.unwrap();
        assert_eq!(call.tool_use_id.as_deref(), Some("exec-1"));
        assert_eq!(call.name, "hook");
        call.reply.send(McpReply::text("{}")).unwrap();
        assert_eq!(request.await.unwrap().0, 200);
    }

    #[tokio::test]
    async fn a_call_whose_run_ended_is_answered_as_an_error() {
        let (server, calls) = McpServer::start(&tools(), "", None).await.unwrap();
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

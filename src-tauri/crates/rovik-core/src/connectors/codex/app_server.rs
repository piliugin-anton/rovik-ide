//! Codex's app server (`codex app-server`), as the connector speaks to it
//! (developers.openai.com/codex/app-server): JSON-RPC messages, one per
//! line on its stdio, without the `"jsonrpc"` field. A request has an id
//! and is answered with its result or an error; a notification has none.
//! The server's own requests (an approval, a question) are answered the
//! same way.
//!
//! What one run is set to goes on its command line as `-c key=value`, the
//! value in TOML as Codex's config.toml has it ([`config`]): a run's own
//! settings never touch the user's files.

use super::super::harness::{Input, Process, Stop};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::WeakUnboundedSender;
use tokio::sync::watch;

/// One message from the server.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Message {
    /// The answer to one of our requests, by its id.
    Response {
        id: u64,
        result: Result<Value, RpcError>,
    },
    /// Something that happened: an event of a thread, a status.
    Notification { method: String, params: Value },
    /// The server asks, and waits for the answer ([`decline`]).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

/// An error the server answered a request with.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RpcError {
    pub(super) code: i64,
    pub(super) message: String,
}

/// A line of the server's as a message; None for one that isn't (a stray
/// print, an answer to an id we never sent).
pub(super) fn parse(line: &str) -> Option<Message> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match (value.get("id").filter(|id| !id.is_null()), method) {
        (Some(id), Some(method)) => Some(Message::Request {
            id: id.clone(),
            method,
            params,
        }),
        (None, Some(method)) => Some(Message::Notification { method, params }),
        (Some(id), None) => {
            let id = id.as_u64()?;
            let result = match value.get("error") {
                Some(error) => Err(RpcError {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Message::Response { id, result })
        }
        (None, None) => None,
    }
}

/// What the run writes to the server: its requests, numbered, and its
/// answers. Clones share the numbering, so the parser's requests (a turn
/// interrupted) never take an id of the run's. It never keeps the
/// server's input open: the run closes it (`Process::finish`), which ends
/// the server.
#[derive(Clone)]
pub(super) struct Rpc {
    input: WeakUnboundedSender<String>,
    next: Arc<AtomicU64>,
}

impl Rpc {
    pub(super) fn new(input: Input) -> Rpc {
        Rpc {
            input: input.downgrade(),
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    fn send(&self, message: Value) {
        // Once the run is over its input is gone: there's no one to tell.
        if let Some(input) = self.input.upgrade() {
            let _ = input.send(format!("{message}\n"));
        }
    }

    /// Sends a request; its id, which its answer carries.
    pub(super) fn request(&self, method: &str, params: Value) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.send(json!({ "id": id, "method": method, "params": params }));
        id
    }

    /// Sends a notification without parameters (`initialized`).
    pub(super) fn notify(&self, method: &str) {
        self.send(json!({ "method": method }));
    }

    /// Answers a request of the server's.
    pub(super) fn respond(&self, id: &Value, result: Value) {
        self.send(json!({ "id": id, "result": result }));
    }

    /// Answers a request of the server's with an error.
    pub(super) fn refuse(&self, id: &Value, code: i64, message: &str) {
        self.send(json!({ "id": id, "error": { "code": code, "message": message } }));
    }
}

/// JSON-RPC's "method not found".
const METHOD_NOT_FOUND: i64 = -32601;

/// Answers what the server asks that nobody is there to give: a run is
/// unattended. Rovik's permissions decide through its hooks before Codex
/// would ask (codex/hooks.rs), so what still comes here is refused, never
/// let through: an approval declined, extra permissions not given, a
/// question unanswered. Anything else isn't a request Rovik takes.
pub(super) fn decline(rpc: &Rpc, id: &Value, method: &str) {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            rpc.respond(id, json!({ "decision": "decline" }))
        }
        "item/permissions/requestApproval" => {
            rpc.respond(id, json!({ "permissions": {}, "scope": "turn" }))
        }
        "item/tool/requestUserInput" => rpc.respond(id, json!({ "answers": {} })),
        "mcpServer/elicitation/request" => rpc.respond(
            id,
            json!({ "action": "decline", "content": null, "_meta": null }),
        ),
        _ => rpc.refuse(id, METHOD_NOT_FOUND, "Rovik IDE doesn't take this request."),
    }
}

/// Why a request got no result.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Unanswered {
    /// The server answered it with an error.
    Refused(RpcError),
    /// The server ended, didn't answer in time, or the run was stopped.
    Gone(Stop),
}

/// Sends `method` and reads the server's lines until its answer comes,
/// within `within`: what comes before it goes to `other`.
pub(super) async fn call(
    process: &mut Process,
    rpc: &Rpc,
    method: &str,
    params: Value,
    within: Duration,
    cancel: &mut watch::Receiver<bool>,
    other: &mut (dyn FnMut(Message) + Send),
) -> Result<Value, Unanswered> {
    let id = rpc.request(method, params);
    let deadline = tokio::time::sleep(within);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            line = process.next_line() => {
                let Some(line) = line else {
                    return Err(Unanswered::Gone(Stop::Ended));
                };
                match parse(&line) {
                    Some(Message::Response { id: answered, result }) if answered == id => {
                        return result.map_err(Unanswered::Refused);
                    }
                    Some(message) => other(message),
                    None => {}
                }
            }
            _ = &mut deadline => return Err(Unanswered::Gone(Stop::Stalled)),
            _ = crate::chat_request::cancelled(cancel) => {
                return Err(Unanswered::Gone(Stop::Stopped));
            }
        }
    }
}

/// `-c key=value`: one setting of this run, its value in TOML.
pub(super) fn config(key: &str, value: &Value) -> [String; 2] {
    ["-c".to_string(), format!("{key}={}", toml(value))]
}

/// A JSON value in TOML: strings in TOML's escapes, arrays, inline tables
/// (whose null fields are left out: TOML has no null).
fn toml(value: &Value) -> String {
    match value {
        Value::Null => "\"\"".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quoted(s),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(toml).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(fields) => {
            let fields: Vec<String> = fields
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| format!("{}={}", key(k), toml(v)))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
    }
}

/// A key as TOML takes it: bare when it can be, else quoted.
fn key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_string()
    } else {
        quoted(key)
    }
}

/// `text` as a TOML basic string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_go_as_toml_values_codex_reads() {
        let [flag, setting] = config(
            "mcp_servers.rovik",
            &json!({ "omit_tools_from": ["code_mode", "direct"], "unset": null }),
        );
        assert_eq!(flag, "-c");
        assert_eq!(
            setting,
            r#"mcp_servers.rovik={omit_tools_from=["code_mode","direct"]}"#
        );
        assert_eq!(toml(&json!(86400.0)), "86400.0");
        assert_eq!(toml(&json!(true)), "true");
        // Text keeps every character through TOML's escapes; a key that
        // isn't bare is quoted.
        assert_eq!(
            toml(&json!({ "a b": "say \"hi\"\\\n\tthere\u{1}" })),
            r#"{"a b"="say \"hi\"\\\n\tthere\u0001"}"#
        );
        assert_eq!(toml(&json!("${tool_input}")), r#""${tool_input}""#);
    }

    #[test]
    fn the_servers_lines_are_answers_events_and_requests() {
        assert_eq!(
            parse(r#"{"id":3,"result":{"thread":{"id":"t"}}}"#),
            Some(Message::Response {
                id: 3,
                result: Ok(json!({ "thread": { "id": "t" } }))
            })
        );
        assert_eq!(
            parse(r#"{"error":{"code":-32600,"message":"no rollout found"},"id":4}"#),
            Some(Message::Response {
                id: 4,
                result: Err(RpcError {
                    code: -32600,
                    message: "no rollout found".into()
                })
            })
        );
        assert_eq!(
            parse(r#"{"method":"turn/started","params":{"threadId":"t"}}"#),
            Some(Message::Notification {
                method: "turn/started".into(),
                params: json!({ "threadId": "t" })
            })
        );
        assert_eq!(
            parse(r#"{"id":"a1","method":"item/tool/requestUserInput","params":{}}"#),
            Some(Message::Request {
                id: json!("a1"),
                method: "item/tool/requestUserInput".into(),
                params: json!({})
            })
        );
        assert_eq!(parse("warning: not json"), None);
    }

    #[test]
    fn what_codex_asks_is_refused_never_let_through() {
        let (input, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let rpc = Rpc::new(input.clone());
        let mut answer = |method: &str| {
            decline(&rpc, &json!(7), method);
            serde_json::from_str::<Value>(sent.try_recv().unwrap().trim()).unwrap()
        };
        assert_eq!(
            answer("item/commandExecution/requestApproval")["result"]["decision"],
            "decline"
        );
        assert_eq!(
            answer("item/fileChange/requestApproval")["result"]["decision"],
            "decline"
        );
        assert_eq!(
            answer("item/permissions/requestApproval")["result"]["permissions"],
            json!({})
        );
        assert_eq!(
            answer("mcpServer/elicitation/request")["result"]["action"],
            "decline"
        );
        let unknown = answer("something/new");
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(unknown["id"], 7);
    }

    #[test]
    fn requests_are_numbered_once_across_clones() {
        let (input, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let rpc = Rpc::new(input.clone());
        let other = rpc.clone();
        assert_eq!(rpc.request("initialize", json!({})), 1);
        assert_eq!(other.request("turn/interrupt", json!({})), 2);
        let first: Value = serde_json::from_str(sent.try_recv().unwrap().trim()).unwrap();
        assert_eq!(
            first,
            json!({ "id": 1, "method": "initialize", "params": {} })
        );
    }

    #[test]
    fn the_writer_never_keeps_the_servers_input_open() {
        let (input, mut sent) = tokio::sync::mpsc::unbounded_channel::<String>();
        let rpc = Rpc::new(input.clone());
        drop(input);
        // The run closed it: nothing is written, and the input is closed.
        rpc.notify("initialized");
        assert!(matches!(
            sent.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
    }
}

//! JSON-RPC as MCP speaks it (docs/design/mcp-plan.md, Q03): the messages
//! Rovik sends, what it reads back, and the two protocol eras.

use serde_json::{json, Map, Value};

/// The newest version with the `initialize` handshake.
pub(super) const LEGACY: &str = "2025-11-25";
/// The stateless version: no handshake, the version on every request.
pub(super) const MODERN: &str = "2026-07-28";

/// The `_meta` keys of a modern request.
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const CLIENT_KEY: &str = "io.modelcontextprotocol/clientInfo";

/// JSON-RPC's "invalid params": a tool's arguments the server refused.
pub(super) const INVALID_PARAMS: i64 = -32602;
const METHOD_NOT_FOUND: i64 = -32601;

/// How a server is spoken to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Era {
    /// After `initialize`: the version the server answered with.
    Legacy(String),
    /// Stateless (`MODERN`): every request says its version.
    Modern,
}

impl Era {
    /// The version its requests go with.
    pub(super) fn version(&self) -> &str {
        match self {
            Era::Legacy(version) => version,
            Era::Modern => MODERN,
        }
    }
}

/// A request that didn't get its result.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Failure {
    /// The server answered with an error.
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// An HTTP status other than success, without a JSON-RPC error in its
    /// body: the status, and the start of the body.
    Http { status: u16, body: String },
    /// No answer, and why: the server's process ended, its connection
    /// failed, what it sent couldn't be read.
    Gone(String),
    /// A modern result asking for input the user would give
    /// (`input_required`): what it asked.
    InputRequired(String),
    /// No answer in time.
    TimedOut,
    /// The user's Stop.
    Stopped,
}

impl Failure {
    /// For the agent or the settings page.
    pub(super) fn text(&self) -> String {
        match self {
            Failure::Rpc { code, message, .. } => format!("{message} (error {code})"),
            Failure::Http { status, body } if body.is_empty() => format!("HTTP {status}"),
            Failure::Http { status, body } => format!("HTTP {status}: {body}"),
            Failure::Gone(why) => why.clone(),
            Failure::InputRequired(what) => format!(
                "the server asked for input Rovik can't give it ({what}); \
                 the tool can't be used from Rovik"
            ),
            Failure::TimedOut => "it didn't answer in time".into(),
            Failure::Stopped => "stopped".into(),
        }
    }
}

/// Rovik as a client names itself.
pub(super) fn client_info() -> Value {
    json!({ "name": "rovik", "title": "Rovik IDE", "version": env!("CARGO_PKG_VERSION") })
}

/// `params` as `era` sends them: a modern request's carry its version,
/// Rovik's (empty) capabilities and its name in `_meta`, beside any of
/// their own.
pub(super) fn params_for(era: &Era, params: Value) -> Value {
    let Era::Modern = era else {
        return params;
    };
    let mut params = match params {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let meta = params
        .entry("_meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(meta) = meta {
        meta.insert(VERSION_KEY.into(), json!(MODERN));
        meta.insert(CAPABILITIES_KEY.into(), json!({}));
        meta.insert(CLIENT_KEY.into(), client_info());
    }
    Value::Object(params)
}

pub(super) fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub(super) fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The answer to a server's request (a legacy server's): `ping` has an
/// empty result, anything else isn't Rovik's to do.
pub(super) fn answer(id: &Value, method: &str) -> Value {
    if method == "ping" {
        return json!({ "jsonrpc": "2.0", "id": id, "result": {} });
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": METHOD_NOT_FOUND, "message": format!("Rovik doesn't serve {method}") },
    })
}

/// A message from the server.
#[derive(Debug, PartialEq)]
pub(super) enum Incoming {
    /// The answer to Rovik's request `id`.
    Response {
        id: u64,
        outcome: Result<Value, Failure>,
    },
    /// The server asks something (a legacy server may): answered with
    /// [`answer`].
    Request { id: Value, method: String },
    /// A notification, or anything else Rovik doesn't act on.
    Other,
}

/// What `message` is.
pub(super) fn incoming(message: &Value) -> Incoming {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => Incoming::Request {
            id: id.clone(),
            method: method.to_string(),
        },
        (Some(_), None) => Incoming::Other,
        (None, Some(id)) => {
            // A number, as Rovik sends them; a server that writes it as a
            // string is taken at its word.
            let id = id
                .as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()));
            let Some(id) = id else {
                return Incoming::Other;
            };
            Incoming::Response {
                id,
                outcome: outcome(message),
            }
        }
        (None, None) => Incoming::Other,
    }
}

/// A response's result, or its error.
fn outcome(message: &Value) -> Result<Value, Failure> {
    if let Some(error) = message.get("error") {
        return Err(Failure::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("an error without a message")
                .to_string(),
            data: error.get("data").cloned(),
        });
    }
    let result = message.get("result").cloned().unwrap_or(Value::Null);
    // A legacy server's results have no type: they're complete.
    match result.get("resultType").and_then(Value::as_str) {
        None | Some("complete") => Ok(result),
        Some("input_required") => {
            let asked: Vec<String> = result
                .get("inputRequests")
                .and_then(Value::as_object)
                .map(|requests| {
                    requests
                        .values()
                        .filter_map(|r| r.get("method").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Err(Failure::InputRequired(if asked.is_empty() {
                "input".into()
            } else {
                asked.join(", ")
            }))
        }
        Some(other) => Err(Failure::Gone(format!(
            "the server answered with a result Rovik can't read ({other})"
        ))),
    }
}

/// A JSON-RPC error in an HTTP error's body, when it has one: a JSON-RPC
/// message whose `error` is an object. A web API's own error, such as
/// `{"error": "Not Found"}` from a URL that's no MCP server's, isn't one:
/// its HTTP status says what went wrong.
pub(super) fn error_in(body: &[u8]) -> Option<Failure> {
    let message: Value = serde_json::from_slice(body).ok()?;
    let rpc = message.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    if !rpc || !message.get("error").is_some_and(Value::is_object) {
        return None;
    }
    outcome(&message).err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modern_request_says_its_version_and_keeps_its_own_meta() {
        let params = params_for(
            &Era::Modern,
            json!({ "name": "t", "_meta": { "progressToken": 1 } }),
        );
        assert_eq!(params["name"], "t");
        assert_eq!(params["_meta"]["progressToken"], 1);
        assert_eq!(params["_meta"][VERSION_KEY], MODERN);
        assert_eq!(params["_meta"][CAPABILITIES_KEY], json!({}));
        assert_eq!(params["_meta"][CLIENT_KEY]["name"], "rovik");
        let legacy = Era::Legacy("2025-06-18".into());
        assert_eq!(params_for(&legacy, json!({ "a": 1 })), json!({ "a": 1 }));
        assert_eq!(legacy.version(), "2025-06-18");
        assert_eq!(Era::Modern.version(), MODERN);
    }

    #[test]
    fn responses_requests_and_notifications_are_told_apart() {
        assert_eq!(
            incoming(&json!({ "jsonrpc": "2.0", "id": 3, "result": { "tools": [] } })),
            Incoming::Response {
                id: 3,
                outcome: Ok(json!({ "tools": [] }))
            }
        );
        assert_eq!(
            incoming(&json!({ "jsonrpc": "2.0", "id": "4", "result": {} })),
            Incoming::Response {
                id: 4,
                outcome: Ok(json!({}))
            },
            "an id the server wrote as a string"
        );
        assert_eq!(
            incoming(&json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })),
            Incoming::Request {
                id: json!("p"),
                method: "ping".into()
            }
        );
        assert_eq!(
            incoming(&json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })),
            Incoming::Other
        );
        assert_eq!(answer(&json!("p"), "ping")["result"], json!({}));
        assert_eq!(
            answer(&json!(9), "sampling/createMessage")["error"]["code"],
            METHOD_NOT_FOUND
        );
    }

    #[test]
    fn errors_and_results_asking_for_input_are_failures() {
        let error = incoming(&json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32602, "message": "Unknown tool: x" },
        }));
        let Incoming::Response {
            outcome: Err(failure),
            ..
        } = error
        else {
            panic!("{error:?}");
        };
        assert_eq!(failure.text(), "Unknown tool: x (error -32602)");
        let asking = json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {
                "resultType": "input_required",
                "inputRequests": { "login": { "method": "elicitation/create", "params": {} } },
            },
        });
        assert_eq!(
            incoming(&asking),
            Incoming::Response {
                id: 2,
                outcome: Err(Failure::InputRequired("elicitation/create".into()))
            }
        );
        let unknown = json!({ "jsonrpc": "2.0", "id": 5, "result": { "resultType": "task" } });
        assert!(matches!(
            incoming(&unknown),
            Incoming::Response {
                outcome: Err(Failure::Gone(_)),
                ..
            }
        ));
        let modern = error_in(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"supported":["2026-07-28"]}}}"#,
        )
        .unwrap();
        assert!(matches!(modern, Failure::Rpc { code: -32022, .. }));
        assert_eq!(error_in(b"Bad Request"), None);
        assert_eq!(error_in(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#), None);
        // A web API's own errors, not JSON-RPC's.
        assert_eq!(error_in(br#"{"error":"Not Found"}"#), None);
        assert_eq!(error_in(br#"{"error":{"message":"Not Found"}}"#), None);
        assert_eq!(
            error_in(br#"{"jsonrpc":"2.0","id":1,"error":"Not Found"}"#),
            None
        );
    }
}

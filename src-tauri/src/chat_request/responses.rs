//! OpenAI's Responses API (`/responses`), for models that aren't served on
//! `/chat/completions` (OpenAI's codex and *-pro models, some gateways).
//! Requests are built as chat completions everywhere else: this module
//! translates the body on the way out and the replies on the way in, so the
//! rest of the app never sees which route a model is on.
//!
//! Which route: every model starts on `/chat/completions`. A refusal that
//! points at the Responses API (`points_to_responses`) sends the same request
//! to `/responses` instead (`agent::run_with_reasoning`); when that works,
//! the model is remembered on it, until the app restarts.

use super::{classify_body_error, ChatError, ErrorKind, ReplyParser};
use crate::agent::ChatStreamEvent;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Api {
    #[default]
    ChatCompletions,
    Responses,
}

/*
 * ---------------------------------------------------------------------------
 * Which route a model is on
 * ---------------------------------------------------------------------------
 */

/// Models found to need `/responses`, by chat completions URL and model.
static ROUTES: LazyLock<Mutex<HashMap<String, Api>>> = LazyLock::new(Default::default);

fn key(chat_url: &str, model: &str) -> String {
    format!("{chat_url}|{model}")
}

/// The route to start with: chat completions unless this model was found
/// to need the Responses API.
pub fn remembered(chat_url: &str, model: &str) -> Api {
    ROUTES
        .lock()
        .ok()
        .and_then(|r| r.get(&key(chat_url, model)).copied())
        .unwrap_or_default()
}

pub fn remember(chat_url: &str, model: &str, api: Api) {
    if let Ok(mut routes) = ROUTES.lock() {
        routes.insert(key(chat_url, model), api);
    }
}

/// `{base}/responses` for `{base}/chat/completions`; None for any other URL
/// (it isn't known where that provider's Responses API would be).
pub fn responses_url(chat_url: &str) -> Option<String> {
    chat_url.strip_suffix("/chat/completions").map(|base| format!("{base}/responses"))
}

/// Whether a refused chat completions request means the model needs the
/// Responses API:
/// - OpenAI: "This model is only supported in v1/responses and not in
///   v1/chat/completions.", "This is not a chat model…";
/// - Azure: "The chatCompletion operation does not work with the specified
///   model…";
/// - a 404 or 405 that isn't a provider's JSON error: the server has no
///   chat completions route at all.
pub fn points_to_responses(error: &ChatError) -> bool {
    if error.partial || !matches!(error.kind, ErrorKind::BadRequest | ErrorKind::NotFound) {
        return false;
    }
    let text = format!("{} {}", error.message, error.details.as_deref().unwrap_or("")).to_lowercase();
    let named = [
        "v1/responses",
        "/responses endpoint",
        "responses api",
        "responses endpoint",
        "not a chat model",
        "chatcompletion operation",
        "not supported in the v1/chat/completions",
        "not supported in v1/chat/completions",
        "not in v1/chat/completions",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    let no_route = matches!(error.status, Some(404 | 405))
        && error
            .details
            .as_deref()
            .and_then(|d| serde_json::from_str::<Value>(d).ok())
            .and_then(|v| super::error_message_from(&v))
            .is_none();
    named || no_route
}

/*
 * ---------------------------------------------------------------------------
 * The request
 * ---------------------------------------------------------------------------
 */

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Chat content (a string, or text and image_url parts) as Responses input
/// content. An assistant's own text is `output_text`.
fn content_of(content: &Value, assistant: bool) -> Value {
    match content {
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .map(|part| match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => json!({
                        "type": if assistant { "output_text" } else { "input_text" },
                        "text": part.get("text").cloned().unwrap_or(json!("")),
                    }),
                    Some("image_url") => {
                        let image = part.get("image_url");
                        let url = image.and_then(|i| i.get("url")).or(image).cloned().unwrap_or(Value::Null);
                        let detail = image.and_then(|i| i.get("detail")).cloned().unwrap_or(json!("auto"));
                        json!({ "type": "input_image", "image_url": url, "detail": detail })
                    }
                    _ => part.clone(),
                })
                .collect(),
        ),
        Value::Null => json!(""),
        other => other.clone(),
    }
}

fn is_empty(content: &Value) -> bool {
    match content {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// A chat completions request body as a Responses API one. Nothing is
/// stored with the provider (`store: false`): the whole history is sent
/// each time, as with chat completions.
pub fn request_body(chat: &Value) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for message in chat.get("messages").and_then(|m| m.as_array()).into_iter().flatten() {
        let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = message.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => instructions.push(text_of(content)),
            "tool" => {
                let output = match content {
                    Value::String(_) | Value::Array(_) => content_of(content, false),
                    Value::Null => json!(""),
                    other => json!(other.to_string()),
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id").cloned().unwrap_or(json!("")),
                    "output": output,
                }));
            }
            _ => {
                let assistant = role == "assistant";
                if !is_empty(content) {
                    input.push(json!({ "role": role, "content": content_of(content, assistant) }));
                }
                for call in message.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.get("id").cloned().unwrap_or(json!("")),
                        "name": call.pointer("/function/name").cloned().unwrap_or(json!("")),
                        "arguments": call.pointer("/function/arguments").cloned().unwrap_or(json!("{}")),
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": chat.get("model").cloned().unwrap_or(Value::Null),
        "input": input,
        "stream": chat.get("stream").cloned().unwrap_or(json!(false)),
        "store": false,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    if let Some(max) = chat.get("max_tokens").or_else(|| chat.get("max_completion_tokens")) {
        body["max_output_tokens"] = max.clone();
    }
    for field in ["temperature", "top_p", "tool_choice", "parallel_tool_calls"] {
        if let Some(value) = chat.get(field) {
            body[field] = value.clone();
        }
    }
    // Chat tools nest the function (`{type, function: {name, …}}`); here
    // it's flat (`{type, name, …}`).
    if let Some(tools) = chat.get("tools").and_then(|t| t.as_array()) {
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| match tool.get("function").and_then(|f| f.as_object()) {
                Some(function) => {
                    let mut flat = function.clone();
                    flat.insert("type".into(), json!("function"));
                    Value::Object(flat)
                }
                None => tool.clone(),
            })
            .collect();
        body["tools"] = json!(tools);
    }
    // Every form of the effort (`agent::REASONING_FIELDS`) that has one is
    // `reasoning.effort` here; `chat_template_kwargs` has no equivalent.
    let effort = chat
        .get("reasoning_effort")
        .and_then(|e| e.as_str())
        .or_else(|| chat.pointer("/reasoning/effort").and_then(|e| e.as_str()))
        .or_else(|| (chat.pointer("/reasoning/enabled") == Some(&json!(false))).then_some("none"));
    if let Some(effort) = effort {
        body["reasoning"] = json!({ "effort": effort });
    }
    // `stop` and `stream_options` aren't taken: usage always comes with the
    // completed response.
    body
}

/*
 * ---------------------------------------------------------------------------
 * The reply
 * ---------------------------------------------------------------------------
 */

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn delta(delta: Value) -> Value {
    json!({ "choices": [{ "delta": delta }] })
}

impl ReplyParser {
    /// One Responses API stream event, translated into the chat completions
    /// form `feed` takes. True once the response has ended.
    pub fn feed_responses(
        &mut self,
        value: &Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<bool, ChatError> {
        match str_at(value, "type") {
            "response.output_text.delta" | "response.refusal.delta" => {
                self.feed(&delta(json!({ "content": str_at(value, "delta") })), emit)?;
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.feed(&delta(json!({ "reasoning_content": str_at(value, "delta") })), emit)?;
            }
            // Separate summary parts read as paragraphs.
            "response.reasoning_summary_part.added" => {
                if value.get("summary_index").and_then(|i| i.as_u64()).is_some_and(|i| i > 0) {
                    self.feed(&delta(json!({ "reasoning_content": "\n\n" })), emit)?;
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                if str_at(item, "type") == "function_call" {
                    let index = value.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0);
                    // `call_id` is what the result is sent back with.
                    let id = item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(json!(""));
                    // Whole arguments: only what hasn't been streamed yet.
                    let streamed = self
                        .calls
                        .iter()
                        .find(|(i, _)| *i == index)
                        .map(|(_, c)| c.arguments.clone())
                        .unwrap_or_default();
                    let rest = str_at(item, "arguments").strip_prefix(streamed.as_str()).unwrap_or("");
                    self.feed(
                        &delta(json!({ "tool_calls": [{
                            "index": index,
                            "id": id,
                            "function": { "name": str_at(item, "name"), "arguments": rest },
                        }] })),
                        emit,
                    )?;
                }
            }
            "response.function_call_arguments.delta" => {
                let index = value.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0);
                self.feed(
                    &delta(json!({ "tool_calls": [{
                        "index": index,
                        "function": { "arguments": str_at(value, "delta") },
                    }] })),
                    emit,
                )?;
            }
            kind @ ("response.completed" | "response.incomplete") => {
                let response = value.get("response").unwrap_or(&Value::Null);
                if let Some(usage) = response.get("usage").filter(|u| u.is_object()) {
                    self.usage = Some(usage.clone());
                }
                self.finish_reason = Some(if kind == "response.incomplete" {
                    match response.pointer("/incomplete_details/reason").and_then(|r| r.as_str()) {
                        Some("max_output_tokens") | None => "length".to_string(),
                        Some(other) => other.to_string(),
                    }
                } else if self.emitted_call || !self.calls.is_empty() {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                });
                return Ok(true);
            }
            "response.failed" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                return Err(classify_body_error(response).unwrap_or_else(|| {
                    ChatError::new(ErrorKind::Provider, "The provider ended the reply with an error.")
                        .with_details(value.to_string())
                }));
            }
            "error" => {
                let error = json!({ "error": {
                    "message": value.get("message").cloned().unwrap_or(Value::Null),
                    "code": value.get("code").cloned().unwrap_or(Value::Null),
                } });
                return Err(classify_body_error(&error)
                    .unwrap_or_else(|| ChatError::new(ErrorKind::Provider, "The provider reported an error.")));
            }
            _ => {}
        }
        Ok(false)
    }

    /// A complete (non-streaming) Responses API response.
    pub fn feed_complete_responses(
        &mut self,
        body: &Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        if let Some(e) = classify_body_error(body) {
            return Err(e);
        }
        for (index, item) in body.get("output").and_then(|o| o.as_array()).into_iter().flatten().enumerate() {
            match str_at(item, "type") {
                "message" => {
                    for part in item.get("content").and_then(|c| c.as_array()).into_iter().flatten() {
                        let text = part.get("text").or_else(|| part.get("refusal")).and_then(|t| t.as_str());
                        if let Some(text) = text {
                            self.feed_responses(&json!({ "type": "response.output_text.delta", "delta": text }), emit)?;
                        }
                    }
                }
                "reasoning" => {
                    let parts = ["summary", "content"]
                        .iter()
                        .flat_map(|k| item.get(*k).and_then(|s| s.as_array()).into_iter().flatten())
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>();
                    if !parts.is_empty() {
                        self.feed_responses(
                            &json!({ "type": "response.reasoning_text.delta", "delta": parts.join("\n\n") }),
                            emit,
                        )?;
                    }
                }
                "function_call" => {
                    self.feed_responses(
                        &json!({ "type": "response.output_item.done", "output_index": index, "item": item }),
                        emit,
                    )?;
                }
                _ => {}
            }
        }
        let ended = match str_at(body, "status") {
            "incomplete" => "response.incomplete",
            "failed" => "response.failed",
            _ => "response.completed",
        };
        self.feed_responses(&json!({ "type": ended, "response": body }), emit)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_request::ErrorKind;

    #[test]
    fn translates_a_chat_request() {
        let chat = json!({
            "model": "gpt-5-codex",
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": 1000,
            "reasoning_effort": "high",
            "chat_template_kwargs": { "enable_thinking": true },
            "tool_choice": "auto",
            "tools": [{ "type": "function", "function": { "name": "read_file", "description": "d", "parameters": { "type": "object" } } }],
            "messages": [
                { "role": "system", "content": "Be useful." },
                { "role": "user", "content": [
                    { "type": "text", "text": "Look" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } }
                ] },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "file text" },
                { "role": "assistant", "content": "Done." }
            ]
        });
        let body = request_body(&chat);
        assert_eq!(body["instructions"], "Be useful.");
        assert_eq!(body["max_output_tokens"], 1000);
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert_eq!(body["store"], false);
        for gone in ["messages", "stream_options", "chat_template_kwargs", "reasoning_effort", "max_tokens"] {
            assert!(body.get(gone).is_none(), "{gone}");
        }
        assert_eq!(
            body["tools"][0],
            json!({ "type": "function", "name": "read_file", "description": "d", "parameters": { "type": "object" } })
        );
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 4, "{input:?}");
        assert_eq!(input[0]["content"][0], json!({ "type": "input_text", "text": "Look" }));
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(input[0]["content"][1]["image_url"], "data:image/png;base64,AA");
        assert_eq!(
            input[1],
            json!({ "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":\"a\"}" })
        );
        assert_eq!(input[2], json!({ "type": "function_call_output", "call_id": "call_1", "output": "file text" }));
        assert_eq!(input[3], json!({ "role": "assistant", "content": "Done." }));
    }

    #[test]
    fn reasoning_off_is_effort_none() {
        let body = request_body(&json!({ "model": "m", "messages": [], "reasoning": { "enabled": false } }));
        assert_eq!(body["reasoning"], json!({ "effort": "none" }));
    }

    fn feed_all(events: &[Value]) -> (Result<bool, ChatError>, Vec<ChatStreamEvent>, ReplyParser) {
        let mut parser = ReplyParser::default();
        let mut out = Vec::new();
        let mut ended = Ok(false);
        for event in events {
            ended = parser.feed_responses(event, &mut |e| out.push(e));
            if !matches!(ended, Ok(false)) {
                break;
            }
        }
        (ended, out, parser)
    }

    #[test]
    fn parses_a_streamed_reply_with_a_tool_call() {
        let (ended, mut events, mut parser) = feed_all(&[
            json!({ "type": "response.created", "response": { "status": "in_progress" } }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Thinking" }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "Reading it." }),
            json!({ "type": "response.output_item.added", "output_index": 2,
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read_file", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"path\":" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "delta": "\"a\"}" }),
            json!({ "type": "response.output_item.done", "output_index": 2,
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read_file", "arguments": "{\"path\":\"a\"}" } }),
            json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 10, "output_tokens": 5 } } }),
        ]);
        assert!(ended.unwrap());
        parser.finish(&mut |e| events.push(e)).unwrap();
        let complete = events.iter().find_map(|e| match e {
            ChatStreamEvent::ToolCallComplete { id, name, arguments, .. } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        });
        assert_eq!(complete, Some(("call_9".into(), "read_file".into(), "{\"path\":\"a\"}".into())));
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::Delta { content } if content == "Reading it.")));
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::Reasoning { content } if content == "Thinking")));
        assert!(events.iter().any(|e| matches!(e,
            ChatStreamEvent::Finished { finish_reason: Some(r), usage: Some(_) } if r == "tool_calls")));
    }

    #[test]
    fn arguments_sent_only_when_done_are_kept() {
        let (_, mut events, mut parser) = feed_all(&[
            json!({ "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "function_call", "call_id": "c", "name": "f", "arguments": "{}" } }),
            json!({ "type": "response.completed", "response": {} }),
        ]);
        parser.finish(&mut |e| events.push(e)).unwrap();
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::ToolCallComplete { arguments, .. } if arguments == "{}")));
    }

    #[test]
    fn failures_and_limits() {
        let (ended, _, _) = feed_all(&[json!({ "type": "response.failed",
            "response": { "status": "failed", "error": { "code": "server_error", "message": "boom" } } })]);
        assert_eq!(ended.unwrap_err().kind, ErrorKind::Provider);
        let (ended, _, _) = feed_all(&[json!({ "type": "error", "code": "rate_limit_exceeded", "message": "slow down" })]);
        assert!(ended.unwrap_err().message.contains("slow down"));
        let (ended, _, mut parser) = feed_all(&[json!({ "type": "response.incomplete",
            "response": { "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } } })]);
        assert!(ended.unwrap());
        assert_eq!(parser.finish(&mut |_| {}).unwrap_err().kind, ErrorKind::Length);
    }

    #[test]
    fn parses_a_complete_response() {
        let body = json!({
            "status": "completed",
            "output": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "Hmm" }] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "A title" }] }
            ],
            "usage": { "input_tokens": 3, "output_tokens": 2 }
        });
        let mut parser = ReplyParser::default();
        let mut events = Vec::new();
        parser.feed_complete_responses(&body, &mut |e| events.push(e)).unwrap();
        parser.finish(&mut |e| events.push(e)).unwrap();
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::Delta { content } if content == "A title")));
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::Finished { finish_reason: Some(r), .. } if r == "stop")));
    }

    #[test]
    fn recognizes_refusals_that_point_to_responses() {
        let headers = reqwest::header::HeaderMap::new();
        let classify = |status, body: &str| super::super::classify_status(status, &headers, body);
        let openai = r#"{"error":{"message":"This model is only supported in v1/responses and not in v1/chat/completions.","type":"invalid_request_error"}}"#;
        assert!(points_to_responses(&classify(404, openai)));
        assert!(points_to_responses(&classify(400, openai)));
        let azure = r#"{"error":{"code":"OperationNotSupported","message":"The chatCompletion operation does not work with the specified model, codex-mini."}}"#;
        assert!(points_to_responses(&classify(400, azure)));
        // A server with no chat completions route.
        assert!(points_to_responses(&classify(404, "404 page not found")));
        assert!(points_to_responses(&classify(405, "")));
        // A missing model, a bad field, an auth failure: not the route.
        let missing = r#"{"error":{"message":"The model `nope` does not exist or you do not have access to it."}}"#;
        assert!(!points_to_responses(&classify(404, missing)));
        assert!(!points_to_responses(&classify(400, r#"{"error":{"message":"Unrecognized request argument supplied: reasoning"}}"#)));
        assert!(!points_to_responses(&classify(401, openai)));
    }

    #[test]
    fn urls_and_memory() {
        assert_eq!(responses_url("https://api.openai.com/v1/chat/completions").as_deref(), Some("https://api.openai.com/v1/responses"));
        assert_eq!(responses_url("https://example.test/custom"), None);
        let url = "https://memory.test/v1/chat/completions";
        assert_eq!(remembered(url, "m"), Api::ChatCompletions);
        remember(url, "m", Api::Responses);
        assert_eq!(remembered(url, "m"), Api::Responses);
        assert_eq!(remembered(url, "other"), Api::ChatCompletions);
    }
}

//! Anthropic's Messages API (`/v1/messages`, the native API of
//! api.anthropic.com; crate::anthropic builds the requests): its replies,
//! read into the chat completions form `ReplyParser::feed` takes, as
//! `responses` does for OpenAI's Responses API, so the rest of the app never
//! sees which API a model is on.
//!
//! - A text block is the reply. A thinking block is reasoning: shown apart,
//!   and kept whole with its signature as a `reasoning_details` entry in
//!   OpenRouter's form for Anthropic (`reasoning_echo::ANTHROPIC_FORMAT`), to
//!   send back with the next step; a `redacted_thinking` block is kept the
//!   same way (`reasoning.encrypted`). An entry notes the tool call it came
//!   right before (`before_call`), so it goes back in its place.
//! - A tool call is keyed by its content block's index; one whose input
//!   streamed nothing gets `{}`.
//! - `usage` keeps Anthropic's fields and adds the chat completions ones the
//!   rest of the app reads: `prompt_tokens` is everything the request read,
//!   cache reads and writes included (Anthropic's `input_tokens` is only
//!   what came after the last cache breakpoint), `completion_tokens`, and
//!   `prompt_tokens_details.cached_tokens` (the cache reads).
//! - A refusal (`stop_reason: "refusal"`, a safety classifier declining the
//!   request) ends the reply with an error that isn't retried: asked again,
//!   the same request is declined again.

use super::{classify_body_error, ChatError, ErrorKind, ReplyParser};
use crate::agent::ChatStreamEvent;
use crate::reasoning_echo::ANTHROPIC_FORMAT;
use serde_json::{json, Map, Value};

/// What is known of the reply's content blocks and usage so far.
#[derive(Default)]
pub(super) struct Blocks {
    /// Each content block's index and type, in the order they came.
    kinds: Vec<(u64, String)>,
    /// Usage in Anthropic's form: `message_start`'s, updated by each
    /// `message_delta` (its counts are cumulative).
    usage: Map<String, Value>,
    /// The thinking block whose text was shown last: the next one's text
    /// starts a new paragraph.
    thinking: Option<u64>,
}

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn delta(delta: Value) -> Value {
    json!({ "choices": [{ "delta": delta }] })
}

fn is_reasoning(kind: &str) -> bool {
    kind == "thinking" || kind == "redacted_thinking"
}

/// The usage in Anthropic's form, with the chat completions fields added.
fn normalized(raw: &Map<String, Value>) -> Value {
    let count = |key: &str| raw.get(key).and_then(Value::as_u64).unwrap_or(0);
    let read = count("cache_read_input_tokens");
    let prompt = count("input_tokens") + count("cache_creation_input_tokens") + read;
    let output = count("output_tokens");
    let mut usage = Value::Object(raw.clone());
    usage["prompt_tokens"] = json!(prompt);
    usage["completion_tokens"] = json!(output);
    usage["total_tokens"] = json!(prompt + output);
    usage["prompt_tokens_details"] = json!({ "cached_tokens": read });
    usage
}

/// An `error` event (`{"type":"error","error":{"type":…,"message":…}}`),
/// classified by its type as the HTTP status it stands for. The type says
/// whose fault it is, as a bare status in another API's stream doesn't: an
/// `invalid_request_error` is the request's, which the caller can change
/// and send again (anthropic.rs `adjust`), not one to wait out and retry.
fn stream_error(value: &Value) -> ChatError {
    let error = value
        .get("error")
        .filter(|e| e.is_object())
        .unwrap_or(value);
    let code = match str_at(error, "type") {
        "invalid_request_error" => Some(400),
        "authentication_error" => Some(401),
        "billing_error" => Some(402),
        "permission_error" => Some(403),
        "not_found_error" => Some(404),
        "request_too_large" => Some(413),
        "rate_limit_error" => Some(429),
        "api_error" => Some(500),
        "timeout_error" => Some(504),
        "overloaded_error" => Some(529),
        _ => None,
    };
    let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("The provider reported an error.");
    if code == Some(400) {
        return ChatError::new(
            ErrorKind::BadRequest,
            format!("The request was rejected: {message}"),
        )
        .with_details(value.to_string());
    }
    let mut body = json!({ "error": { "message": message } });
    if let Some(code) = code {
        body["error"]["code"] = json!(code);
    }
    classify_body_error(&body)
        .unwrap_or_else(|| ChatError::new(ErrorKind::Provider, message.to_string()))
        .with_details(value.to_string())
}

/// The model's safety classifiers declined the request (`stop_details`:
/// its `category` and `explanation`, either of which may be null).
fn refusal(details: Option<&Value>) -> ChatError {
    let field = |key: &str| {
        details
            .and_then(|d| d.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let mut message = String::from("The model declined to answer");
    if let Some(category) = field("category") {
        message.push_str(&format!(" ({category})"));
    }
    message.push('.');
    if let Some(explanation) = field("explanation") {
        message.push(' ');
        message.push_str(explanation);
    }
    let error = ChatError::new(ErrorKind::Refused, message);
    match details {
        Some(details) => error.with_details(details.to_string()),
        None => error,
    }
}

impl ReplyParser {
    fn kind_of(&self, index: u64) -> Option<&str> {
        self.messages
            .kinds
            .iter()
            .rev()
            .find(|(i, _)| *i == index)
            .map(|(_, k)| k.as_str())
    }

    fn note_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage.and_then(Value::as_object) else {
            return;
        };
        for (key, value) in usage.iter().filter(|(_, v)| !v.is_null()) {
            self.messages.usage.insert(key.clone(), value.clone());
        }
        self.usage = Some(normalized(&self.messages.usage));
    }

    /// Thinking text: shown, and kept to send back.
    fn thinking(
        &mut self,
        index: u64,
        text: &str,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        // Each block's text is a paragraph of its own.
        if self.reasoning_len > 0 && self.messages.thinking != Some(index) {
            self.feed(&delta(json!({ "reasoning_content": "\n\n" })), emit)?;
        }
        self.messages.thinking = Some(index);
        let entry = json!({ "type": "reasoning.text", "text": text, "format": ANTHROPIC_FORMAT, "index": index });
        self.feed(
            &delta(json!({ "reasoning_content": text, "reasoning_details": [entry] })),
            emit,
        )
    }

    fn signature(
        &mut self,
        index: u64,
        signature: &str,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        let entry = json!({ "type": "reasoning.text", "signature": signature, "format": ANTHROPIC_FORMAT, "index": index });
        self.feed(&delta(json!({ "reasoning_details": [entry] })), emit)
    }

    /// Notes on each kept reasoning entry the tool call that came right
    /// after its block, if one did: it goes back right before that call
    /// (crate::anthropic), where it was.
    fn place_reasoning(&mut self) {
        let kinds = self.messages.kinds.clone();
        for (at, (index, kind)) in kinds.iter().enumerate() {
            if !is_reasoning(kind) {
                continue;
            }
            let next = kinds[at + 1..].iter().find(|(_, k)| !is_reasoning(k));
            let call = match next {
                Some((call_index, k)) if k == "tool_use" => self
                    .calls
                    .iter()
                    .find(|(i, _)| i == call_index)
                    .map(|(_, c)| c.id.clone())
                    .filter(|id| !id.is_empty()),
                _ => None,
            };
            let Some(call) = call else {
                continue;
            };
            for entry in self.reasoning_details.iter_mut().filter(|e| {
                e.get("index").and_then(Value::as_u64) == Some(*index)
                    && e.get("format").and_then(|f| f.as_str()) == Some(ANTHROPIC_FORMAT)
            }) {
                entry["before_call"] = json!(call);
            }
        }
    }

    /// One Messages API stream event (its `data`), translated into the
    /// chat completions form `feed` takes. True once the message has ended.
    pub fn feed_messages(
        &mut self,
        value: &Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<bool, ChatError> {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
        match str_at(value, "type") {
            "message_start" => {
                let message = value.get("message").unwrap_or(&Value::Null);
                self.note_usage(message.get("usage"));
                // Thinking blocks the API didn't pass on to the model (the
                // `thinking-binding-controls` beta lists them): for the log.
                if let Some(changes) = message
                    .get("input_transformations")
                    .and_then(|t| t.as_array())
                    .filter(|t| !t.is_empty())
                {
                    eprintln!(
                        "anthropic: {} thinking block(s) not passed to the model: {}",
                        changes.len(),
                        Value::Array(changes.clone())
                    );
                }
            }
            "content_block_start" => {
                // Streamed, a block starts empty and its deltas fill it; a
                // complete reply's blocks (`feed_complete_messages`) come
                // whole, here.
                let block = value.get("content_block").unwrap_or(&Value::Null);
                let kind = str_at(block, "type");
                self.messages.kinds.push((index, kind.to_string()));
                match kind {
                    "text" => {
                        let text = str_at(block, "text");
                        if !text.is_empty() {
                            self.feed(&delta(json!({ "content": text })), emit)?;
                        }
                    }
                    "thinking" => {
                        let text = str_at(block, "thinking");
                        if !text.is_empty() {
                            self.thinking(index, text, emit)?;
                        }
                        let signature = str_at(block, "signature");
                        if !signature.is_empty() {
                            self.signature(index, signature, emit)?;
                        }
                    }
                    "redacted_thinking" => {
                        let data = str_at(block, "data");
                        if !data.is_empty() {
                            let entry = json!({ "type": "reasoning.encrypted", "data": data, "format": ANTHROPIC_FORMAT, "index": index });
                            self.feed(&delta(json!({ "reasoning_details": [entry] })), emit)?;
                        }
                    }
                    "tool_use" => {
                        let arguments = block
                            .get("input")
                            .filter(|i| i.as_object().is_some_and(|o| !o.is_empty()))
                            .map(|i| i.to_string())
                            .unwrap_or_default();
                        self.feed(
                            &delta(json!({ "tool_calls": [{
                                "index": index,
                                "id": str_at(block, "id"),
                                "function": { "name": str_at(block, "name"), "arguments": arguments },
                            }] })),
                            emit,
                        )?;
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let d = value.get("delta").unwrap_or(&Value::Null);
                match str_at(d, "type") {
                    "text_delta" => {
                        let text = str_at(d, "text");
                        if !text.is_empty() {
                            self.feed(&delta(json!({ "content": text })), emit)?;
                        }
                    }
                    "thinking_delta" => {
                        let text = str_at(d, "thinking");
                        if !text.is_empty() {
                            self.thinking(index, text, emit)?;
                        }
                    }
                    "signature_delta" => {
                        let signature = str_at(d, "signature");
                        if !signature.is_empty() {
                            self.signature(index, signature, emit)?;
                        }
                    }
                    "input_json_delta" => {
                        let part = str_at(d, "partial_json");
                        if !part.is_empty() {
                            self.feed(
                                &delta(json!({ "tool_calls": [{
                                    "index": index,
                                    "function": { "arguments": part },
                                }] })),
                                emit,
                            )?;
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                // A call without parameters streams no input: its arguments
                // are an empty object, not nothing.
                let unfilled = self.kind_of(index) == Some("tool_use")
                    && self
                        .calls
                        .iter()
                        .find(|(i, _)| *i == index)
                        .is_some_and(|(_, c)| c.arguments.trim().is_empty());
                if unfilled {
                    self.feed(
                        &delta(json!({ "tool_calls": [{
                            "index": index,
                            "function": { "arguments": "{}" },
                        }] })),
                        emit,
                    )?;
                }
            }
            "message_delta" => {
                self.note_usage(value.get("usage"));
                let d = value.get("delta").unwrap_or(&Value::Null);
                if let Some(reason) = d.get("stop_reason").and_then(|r| r.as_str()) {
                    if reason == "refusal" {
                        return Err(refusal(
                            d.get("stop_details")
                                .or_else(|| value.get("stop_details"))
                                .filter(|s| s.is_object()),
                        ));
                    }
                    // The context window filled before anything was said:
                    // the conversation is too long, not the reply.
                    if reason == "model_context_window_exceeded" && !self.visible() {
                        return Err(ChatError::new(
                            ErrorKind::ContextLength,
                            "The conversation is too long for this model: the context window filled before the reply began.",
                        ));
                    }
                    self.finish_reason = Some(
                        match reason {
                            "end_turn" | "stop_sequence" | "pause_turn" => "stop",
                            "tool_use" => "tool_calls",
                            "max_tokens" | "model_context_window_exceeded" => "length",
                            other => other,
                        }
                        .to_string(),
                    );
                }
            }
            "message_stop" => {
                self.place_reasoning();
                return Ok(true);
            }
            "error" => return Err(stream_error(value)),
            // `ping`, and what later API versions add.
            _ => {}
        }
        Ok(false)
    }

    /// A complete (non-streaming) Messages API response: its blocks, as if
    /// each had streamed whole.
    pub fn feed_complete_messages(
        &mut self,
        body: &Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<(), ChatError> {
        if str_at(body, "type") == "error" {
            return Err(stream_error(body));
        }
        let usage = body.get("usage").cloned().unwrap_or(Value::Null);
        self.feed_messages(
            &json!({ "type": "message_start", "message": { "usage": usage } }),
            emit,
        )?;
        for (index, block) in body
            .get("content")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            for event in [
                json!({ "type": "content_block_start", "index": index, "content_block": block }),
                json!({ "type": "content_block_stop", "index": index }),
            ] {
                self.feed_messages(&event, emit)?;
            }
        }
        let stop = json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": body.get("stop_reason").cloned().unwrap_or(Value::Null),
                "stop_details": body.get("stop_details").cloned().unwrap_or(Value::Null),
            },
            "usage": usage,
        });
        self.feed_messages(&stop, emit)?;
        self.feed_messages(&json!({ "type": "message_stop" }), emit)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds the events, then finishes the reply.
    fn parse(events: &[Value]) -> (Result<(), ChatError>, Vec<ChatStreamEvent>) {
        let mut parser = ReplyParser::default();
        let mut out = Vec::new();
        let mut emit = |e: ChatStreamEvent| out.push(e);
        let mut result = Ok(());
        for event in events {
            match parser.feed_messages(event, &mut emit) {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if result.is_ok() {
            result = parser.finish(&mut emit);
        }
        (result, out)
    }

    fn text_of(events: &[ChatStreamEvent]) -> (String, String) {
        let mut reply = String::new();
        let mut reasoning = String::new();
        for e in events {
            match e {
                ChatStreamEvent::Delta { content } => reply.push_str(content),
                ChatStreamEvent::Reasoning { content } => reasoning.push_str(content),
                _ => {}
            }
        }
        (reply, reasoning)
    }

    fn start(index: u64, block: Value) -> Value {
        json!({ "type": "content_block_start", "index": index, "content_block": block })
    }

    fn block_delta(index: u64, delta: Value) -> Value {
        json!({ "type": "content_block_delta", "index": index, "delta": delta })
    }

    fn stop(index: u64) -> Value {
        json!({ "type": "content_block_stop", "index": index })
    }

    fn message_delta(reason: &str, usage: Value) -> Value {
        json!({ "type": "message_delta", "delta": { "stop_reason": reason, "stop_sequence": null }, "usage": usage })
    }

    #[test]
    fn thinking_then_a_tool_call_streams_as_reasoning_and_a_call() {
        let (result, events) = parse(&[
            json!({ "type": "message_start", "message": { "id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "claude-opus-5-5",
                "usage": { "input_tokens": 12, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 3000, "output_tokens": 1 } } }),
            start(
                0,
                json!({ "type": "thinking", "thinking": "", "signature": "" }),
            ),
            json!({ "type": "ping" }),
            block_delta(
                0,
                json!({ "type": "thinking_delta", "thinking": "Read the " }),
            ),
            block_delta(
                0,
                json!({ "type": "thinking_delta", "thinking": "file first." }),
            ),
            block_delta(
                0,
                json!({ "type": "signature_delta", "signature": "sig-0" }),
            ),
            stop(0),
            start(1, json!({ "type": "text", "text": "" })),
            block_delta(1, json!({ "type": "text_delta", "text": "Reading it." })),
            stop(1),
            start(
                2,
                json!({ "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} }),
            ),
            block_delta(2, json!({ "type": "input_json_delta", "partial_json": "" })),
            block_delta(
                2,
                json!({ "type": "input_json_delta", "partial_json": "{\"path\":" }),
            ),
            block_delta(
                2,
                json!({ "type": "input_json_delta", "partial_json": " \"a.ts\"}" }),
            ),
            stop(2),
            message_delta("tool_use", json!({ "output_tokens": 40 })),
            json!({ "type": "message_stop" }),
        ]);
        result.unwrap();
        assert_eq!(
            text_of(&events),
            (
                "Reading it.".to_string(),
                "Read the file first.".to_string()
            )
        );
        let call = events.iter().find_map(|e| match e {
            ChatStreamEvent::ToolCallComplete {
                id,
                name,
                arguments,
                extra_content,
            } => Some((
                id.clone(),
                name.clone(),
                arguments.clone(),
                extra_content.clone(),
            )),
            _ => None,
        });
        assert_eq!(
            call,
            Some((
                "toolu_1".into(),
                "read".into(),
                "{\"path\": \"a.ts\"}".into(),
                None
            ))
        );
        let details = events.iter().find_map(|e| match e {
            ChatStreamEvent::ReasoningDetails { details } => Some(details.clone()),
            _ => None,
        });
        // Text came between the thinking and the call: it goes back first.
        assert_eq!(
            details,
            Some(vec![
                json!({ "type": "reasoning.text", "text": "Read the file first.", "signature": "sig-0", "format": ANTHROPIC_FORMAT, "index": 0 })
            ])
        );
        let finished = events.iter().find_map(|e| match e {
            ChatStreamEvent::Finished {
                finish_reason,
                usage,
            } => Some((finish_reason.clone(), usage.clone())),
            _ => None,
        });
        let (reason, usage) = finished.unwrap();
        assert_eq!(reason.as_deref(), Some("tool_calls"));
        let usage = usage.unwrap();
        // Everything read, the cache included; Anthropic's own fields kept.
        assert_eq!(usage["prompt_tokens"], 3112);
        assert_eq!(usage["completion_tokens"], 40);
        assert_eq!(usage["total_tokens"], 3152);
        assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 3000);
        assert_eq!(usage["input_tokens"], 12);
        assert_eq!(crate::agent::calibration::prompt_tokens(&usage), Some(3112));
        assert_eq!(crate::prompt_cache::cached_tokens(&usage), Some(3000));
    }

    #[test]
    fn reasoning_right_before_a_call_is_placed_there() {
        // Opus 5.5 style: reasoning, then progress notes before each call.
        let (result, events) = parse(&[
            start(
                0,
                json!({ "type": "thinking", "thinking": "", "signature": "" }),
            ),
            block_delta(0, json!({ "type": "thinking_delta", "thinking": "" })),
            block_delta(0, json!({ "type": "signature_delta", "signature": "s0" })),
            stop(0),
            start(
                1,
                json!({ "type": "tool_use", "id": "toolu_a", "name": "read", "input": {} }),
            ),
            block_delta(
                1,
                json!({ "type": "input_json_delta", "partial_json": "{}" }),
            ),
            stop(1),
            start(
                2,
                json!({ "type": "thinking", "thinking": "", "signature": "" }),
            ),
            block_delta(
                2,
                json!({ "type": "thinking_delta", "thinking": "Now the test." }),
            ),
            block_delta(2, json!({ "type": "signature_delta", "signature": "s2" })),
            stop(2),
            start(3, json!({ "type": "redacted_thinking", "data": "blob" })),
            stop(3),
            start(
                4,
                json!({ "type": "tool_use", "id": "toolu_b", "name": "read", "input": {} }),
            ),
            stop(4),
            message_delta("tool_use", json!({ "output_tokens": 9 })),
            json!({ "type": "message_stop" }),
        ]);
        result.unwrap();
        let details = events
            .iter()
            .find_map(|e| match e {
                ChatStreamEvent::ReasoningDetails { details } => Some(details.clone()),
                _ => None,
            })
            .unwrap();
        let placed: Vec<(Value, Value)> = details
            .iter()
            .map(|d| (d["index"].clone(), d["before_call"].clone()))
            .collect();
        assert_eq!(
            placed,
            [
                (json!(0), json!("toolu_a")),
                (json!(2), json!("toolu_b")),
                (json!(3), json!("toolu_b")),
            ]
        );
        // Omitted thinking: kept with its signature, no text shown for it.
        assert_eq!(details[0]["signature"], "s0");
        assert_eq!(text_of(&events).1, "Now the test.");
        // A call without parameters gets an empty object.
        let arguments: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::ToolCallComplete { arguments, .. } => Some(arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(arguments, ["{}", "{}"]);
    }

    #[test]
    fn separate_thinking_blocks_read_as_paragraphs() {
        let (_, events) = parse(&[
            start(0, json!({ "type": "thinking", "thinking": "" })),
            block_delta(0, json!({ "type": "thinking_delta", "thinking": "One." })),
            block_delta(0, json!({ "type": "signature_delta", "signature": "s" })),
            stop(0),
            start(1, json!({ "type": "thinking", "thinking": "" })),
            block_delta(1, json!({ "type": "thinking_delta", "thinking": "Two." })),
            block_delta(1, json!({ "type": "signature_delta", "signature": "t" })),
            stop(1),
            start(2, json!({ "type": "text", "text": "" })),
            block_delta(2, json!({ "type": "text_delta", "text": "Done." })),
            stop(2),
            message_delta("end_turn", json!({ "output_tokens": 3 })),
            json!({ "type": "message_stop" }),
        ]);
        assert_eq!(
            text_of(&events),
            ("Done.".to_string(), "One.\n\nTwo.".to_string())
        );
    }

    #[test]
    fn stop_reasons_become_finish_reasons() {
        for (reason, expected) in [
            ("end_turn", "stop"),
            ("stop_sequence", "stop"),
            ("max_tokens", "length"),
            ("model_context_window_exceeded", "length"),
            ("pause_turn", "stop"),
        ] {
            let (result, events) = parse(&[
                start(0, json!({ "type": "text", "text": "" })),
                block_delta(0, json!({ "type": "text_delta", "text": "Hi" })),
                stop(0),
                message_delta(reason, json!({ "output_tokens": 1 })),
                json!({ "type": "message_stop" }),
            ]);
            result.unwrap();
            let got = events.iter().find_map(|e| match e {
                ChatStreamEvent::Finished { finish_reason, .. } => finish_reason.clone(),
                _ => None,
            });
            assert_eq!(got.as_deref(), Some(expected), "{reason}");
        }
        // Out of room before anything was said: the conversation is too long.
        let (result, _) = parse(&[
            message_delta(
                "model_context_window_exceeded",
                json!({ "output_tokens": 0 }),
            ),
            json!({ "type": "message_stop" }),
        ]);
        assert_eq!(result.unwrap_err().kind, ErrorKind::ContextLength);
    }

    #[test]
    fn a_refusal_is_an_error_not_retried() {
        let (result, _) = parse(&[
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 400, "output_tokens": 0 } } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "refusal", "stop_details": {
                "type": "refusal", "category": "cyber", "explanation": "This request was declined." } },
                "usage": { "output_tokens": 0 } }),
            json!({ "type": "message_stop" }),
        ]);
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Refused);
        assert!(!error.retryable);
        // What the chat shows: the reason, as the provider gave it.
        assert_eq!(
            crate::agent_run::failure::describe(&error),
            "The model declined to answer (cyber). This request was declined."
        );
        assert_eq!(
            error.message,
            "The model declined to answer (cyber). This request was declined."
        );
        // Without a category or explanation.
        let (result, _) = parse(&[json!({ "type": "message_delta",
            "delta": { "stop_reason": "refusal", "stop_details": { "type": "refusal", "category": null, "explanation": null } } })]);
        assert_eq!(result.unwrap_err().message, "The model declined to answer.");
    }

    #[test]
    fn stream_errors_are_classified_by_type() {
        let error = |kind: &str| {
            parse(&[json!({ "type": "error", "error": { "type": kind, "message": "Overloaded" } })])
                .0
                .unwrap_err()
        };
        let overloaded = error("overloaded_error");
        assert_eq!(overloaded.kind, ErrorKind::ServerError);
        assert!(overloaded.retryable);
        assert_eq!(error("rate_limit_error").kind, ErrorKind::RateLimited);
        assert_eq!(error("api_error").kind, ErrorKind::ServerError);
        // A refused request reported in the stream, by its type: the
        // request's, to change and send again, not to retry as it was.
        let refused = error("invalid_request_error");
        assert_eq!(refused.kind, ErrorKind::BadRequest);
        assert!(!refused.retryable);
        assert_eq!(error("something_new").kind, ErrorKind::Provider);
    }

    #[test]
    fn a_complete_response_reads_like_a_stream() {
        let body = json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5",
            "content": [
                { "type": "thinking", "thinking": "Short.", "signature": "sig" },
                { "type": "text", "text": "Calling." },
                { "type": "tool_use", "id": "toolu_9", "name": "bash", "input": { "command": "ls" } },
            ],
            "stop_reason": "tool_use", "stop_sequence": null,
            "usage": { "input_tokens": 50, "output_tokens": 20 },
        });
        let mut parser = ReplyParser::default();
        let mut events = Vec::new();
        let mut emit = |e: ChatStreamEvent| events.push(e);
        parser.feed_complete_messages(&body, &mut emit).unwrap();
        parser.finish(&mut emit).unwrap();
        assert_eq!(
            text_of(&events),
            ("Calling.".to_string(), "Short.".to_string())
        );
        let call = events.iter().find_map(|e| match e {
            ChatStreamEvent::ToolCallComplete { id, arguments, .. } => {
                Some((id.clone(), arguments.clone()))
            }
            _ => None,
        });
        assert_eq!(
            call,
            Some(("toolu_9".into(), "{\"command\":\"ls\"}".into()))
        );
        let details = events.iter().find_map(|e| match e {
            ChatStreamEvent::ReasoningDetails { details } => Some(details.clone()),
            _ => None,
        });
        assert_eq!(details.unwrap()[0]["signature"], "sig");
        // A refusal in a complete response.
        let refused = json!({ "type": "message", "content": [], "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "bio", "explanation": null },
            "usage": { "input_tokens": 5, "output_tokens": 0 } });
        let mut parser = ReplyParser::default();
        let error = parser
            .feed_complete_messages(&refused, &mut |_| {})
            .unwrap_err();
        assert_eq!(error.message, "The model declined to answer (bio).");
        // An error body.
        let mut parser = ReplyParser::default();
        let error = parser
            .feed_complete_messages(
                &json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
                &mut |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ServerError);
    }

    #[test]
    fn nothing_said_is_an_empty_response() {
        let (result, _) = parse(&[
            start(0, json!({ "type": "text", "text": "" })),
            stop(0),
            message_delta("end_turn", json!({ "output_tokens": 0 })),
            json!({ "type": "message_stop" }),
        ]);
        assert_eq!(result.unwrap_err().kind, ErrorKind::EmptyResponse);
    }
}

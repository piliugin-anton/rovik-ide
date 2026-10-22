//! OpenAI's Responses API (`/responses`): where a model is tried first, and
//! the only route for models that aren't served on `/chat/completions`
//! (OpenAI's codex and *-pro models, some gateways). Requests are built as
//! chat completions everywhere else: this module translates the body on the
//! way out and the replies on the way in, so the rest of the app never sees
//! which route a model is on.
//!
//! Which route: a model starts on the route it was last found to work on
//! (remembered here, and saved with the model in the settings), else on
//! `/responses`. A refusal that suggests the other route (`wrong_route`; a
//! provider without the Responses API, without the model on it, or whose
//! Responses API can't read the request) sends the same request there once
//! (`agent::run_with_reasoning`); when that works, the model is remembered
//! on it and the frontend is told, so it's saved (`ChatStreamEvent::ApiRoute`).

use super::{classify_body_error, ChatError, ErrorKind, ReplyParser};
use crate::agent::ChatStreamEvent;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Api {
    ChatCompletions,
    /// Where a model starts while no route is known for it.
    #[default]
    Responses,
    /// Anthropic's Messages API (`messages`, crate::anthropic): the route
    /// for api.anthropic.com, told by the host, so it's neither tried nor
    /// saved with the model.
    Messages,
}

impl Api {
    /// As saved with the model: "chat_completions" or "responses".
    pub fn as_str(self) -> &'static str {
        match self {
            Api::ChatCompletions => "chat_completions",
            Api::Responses => "responses",
            Api::Messages => "messages",
        }
    }

    pub fn parse(text: &str) -> Option<Api> {
        match text {
            "chat_completions" => Some(Api::ChatCompletions),
            "responses" => Some(Api::Responses),
            _ => None,
        }
    }

    /// The route to try when this one refuses the model. The Messages API
    /// has none: it's the only one Anthropic's models are on here.
    pub fn other(self) -> Api {
        match self {
            Api::ChatCompletions => Api::Responses,
            Api::Responses => Api::ChatCompletions,
            Api::Messages => Api::Messages,
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * Which route a model is on
 * ---------------------------------------------------------------------------
 */

/// The route each model was last found to work on, by chat completions URL
/// and model.
static ROUTES: LazyLock<Mutex<HashMap<String, Api>>> = LazyLock::new(Default::default);

fn key(chat_url: &str, model: &str) -> String {
    format!("{chat_url}|{model}")
}

/// Models seen labelling their messages with a `phase`, by Responses URL
/// and model: their earlier replies are sent back labelled too.
static PHASES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn phase_key(responses_url: &str, model: &str) -> String {
    key(responses_url, model)
}

pub fn note_phases(phase_key: &str) {
    if let Ok(mut phases) = PHASES.lock() {
        phases.insert(phase_key.to_string());
    }
}

pub fn uses_phases(phase_key: &str) -> bool {
    PHASES.lock().is_ok_and(|p| p.contains(phase_key))
}

/// The route this model was found to work on since the app started.
pub fn remembered(chat_url: &str, model: &str) -> Option<Api> {
    ROUTES
        .lock()
        .ok()
        .and_then(|r| r.get(&key(chat_url, model)).copied())
}

pub fn remember(chat_url: &str, model: &str, api: Api) {
    if let Ok(mut routes) = ROUTES.lock() {
        routes.insert(key(chat_url, model), api);
    }
}

/// `{base}/responses` for `{base}/chat/completions`; None for any other URL
/// (it isn't known where that provider's Responses API would be).
pub fn responses_url(chat_url: &str) -> Option<String> {
    chat_url
        .strip_suffix("/chat/completions")
        .map(|base| format!("{base}/responses"))
}

/// Whether a request refused on `sent_to` may have gone to the wrong route,
/// so the other one is worth a try:
/// - the provider says so. On `/chat/completions`, OpenAI: "This model is
///   only supported in v1/responses and not in v1/chat/completions.", "This
///   is not a chat model…"; Azure: "The chatCompletion operation does not
///   work with the specified model…". On `/responses`: "only supported in
///   v1/chat/completions", "the Responses API is not supported", and so on;
/// - OpenAI's "Invalid URL (POST /v1/…)": the server has no such route;
/// - a 404 or 405 that isn't a provider's JSON error: the same, from a
///   server that answers unknown paths with a bare page;
/// - on `/responses`, any 404 or 405: providers without the Responses API
///   also say "Not Found" in JSON of their own, and some serve only part of
///   their models there ("The model … does not exist"). `/chat/completions`,
///   which every OpenAI-compatible provider has, is worth the one try;
/// - "ModelProtocolUnsupported" / "Model does not support this protocol.",
///   which doesn't say which API the model wants: the other one, either way
///   (`super::looks_like_unsupported_protocol`);
/// - on `/responses`, a server that can't read the request's `input`
///   (`input_unreadable`).
///
/// A rejected field, an auth failure, a model missing from
/// `/chat/completions`: not the route.
pub fn wrong_route(error: &ChatError, sent_to: Api) -> bool {
    if error.partial || !matches!(error.kind, ErrorKind::BadRequest | ErrorKind::NotFound) {
        return false;
    }
    let text = format!(
        "{} {}",
        error.message,
        error.details.as_deref().unwrap_or("")
    )
    .to_lowercase();
    let (needles, path): (&[&str], &str) = match sent_to {
        Api::ChatCompletions => (
            &[
                "v1/responses",
                "/responses endpoint",
                "responses api",
                "responses endpoint",
                "not a chat model",
                "chatcompletion operation",
                "not supported in the v1/chat/completions",
                "not supported in v1/chat/completions",
                "not in v1/chat/completions",
            ],
            "/chat/completions",
        ),
        Api::Responses => (
            &[
                "v1/chat/completions",
                "chat completions api",
                "chat completions endpoint",
                "use chat/completions",
                "responses api is not",
                "responses is not supported",
                "responses are not supported",
                "does not support the responses",
                "not supported in the v1/responses",
                "not supported in v1/responses",
                "not in v1/responses",
            ],
            "/responses",
        ),
        Api::Messages => return false,
    };
    let named = needles.iter().any(|needle| text.contains(needle));
    let invalid_url = text.contains("invalid url") && text.contains(path);
    let no_route = matches!(error.status, Some(404 | 405))
        && (sent_to == Api::Responses
            || error
                .details
                .as_deref()
                .and_then(|d| serde_json::from_str::<Value>(d).ok())
                .and_then(|v| super::error_message_from(&v))
                .is_none());
    let protocol = super::looks_like_unsupported_protocol(&text);
    let unreadable = sent_to == Api::Responses && input_unreadable(&text);
    named || invalid_url || no_route || protocol || unreadable
}

/// A Responses API whose own types don't take the request's `input`, the
/// items `request_body` sends, as serde words it (a Rust server's: OpenAI
/// words its refusals otherwise). SGLang's router wants an `id` on every
/// `function_call` item, which OpenAI doesn't, so a run's first request
/// works there and the next, with the run's tool calls in it, doesn't:
/// "Failed to deserialize the JSON body into the target type: input: data
/// did not match any variant of untagged enum ResponseInput". Chat
/// completions takes the same history. `text` is lowercase.
pub(super) fn input_unreadable(text: &str) -> bool {
    // Serde's own phrasing: OpenAI's "Invalid type for 'input[2]…'" is
    // a refusal of what was sent, not of how.
    let unreadable = [
        "deserialize",
        "did not match any variant",
        "unknown variant `",
        "missing field `",
        "invalid type:",
    ]
    .iter()
    .any(|words| text.contains(words));
    unreadable && (crate::agent::mentions_field(text, "input") || text.contains("responseinput"))
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

/// Chat content (a string, or text, image_url and file parts) as Responses
/// input content. An assistant's own text is `output_text`.
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
                        let url = image
                            .and_then(|i| i.get("url"))
                            .or(image)
                            .cloned()
                            .unwrap_or(Value::Null);
                        let detail = image
                            .and_then(|i| i.get("detail"))
                            .cloned()
                            .unwrap_or(json!("auto"));
                        json!({ "type": "input_image", "image_url": url, "detail": detail })
                    }
                    Some("file") => {
                        let mut file = part.get("file").cloned().unwrap_or_else(|| json!({}));
                        file["type"] = json!("input_file");
                        file
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
///
/// A step's output goes back as it came, in its order: OpenAI's guide says
/// to replay the returned output items, reasoning included, with the tool
/// calls and their results. The step's reasoning and commentary are kept
/// as its `reasoning_details` entries (reasoning_echo.rs), each noting the
/// call it came right before: it goes back there, else before the step's
/// text.
///
/// `phases`: the model labels its messages (GPT-5.3 Codex and later), and
/// expects its earlier ones back labelled: text sent with tool calls is
/// `commentary`, any other reply its `final_answer`. Its commentary isn't
/// part of the reply (it's shown with the reasoning): it goes back as items
/// of its own, labelled, where it was. A history carrying commentary is
/// from such a model, whatever this run of the app has seen.
/// `echo`: reasoning may go back to the server (reasoning_echo.rs): its
/// items, and the request for encrypted reasoning to send back later.
pub fn request_body(chat: &Value, phases: bool, echo: bool) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    let messages: &[Value] = chat
        .get("messages")
        .and_then(|m| m.as_array())
        .map_or(&[], Vec::as_slice);
    let phases = phases
        || messages.iter().any(|m| {
            m.get("reasoning_details")
                .and_then(|d| d.as_array())
                .is_some_and(|d| d.iter().any(crate::reasoning_echo::is_commentary))
        });
    for message in messages {
        let role = message
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user");
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
                let calls: &[Value] = message
                    .get("tool_calls")
                    .and_then(|c| c.as_array())
                    .map_or(&[], Vec::as_slice);
                let ids: Vec<&str> = calls
                    .iter()
                    .map(|c| c.get("id").and_then(|i| i.as_str()).unwrap_or(""))
                    .collect();
                // The step's reasoning and commentary, each where it was.
                let items = match message.get("reasoning_details").and_then(|d| d.as_array()) {
                    Some(details) if assistant => {
                        crate::reasoning_echo::to_responses_items(details)
                    }
                    _ => Vec::new(),
                };
                let mut sent = vec![false; items.len()];
                // Before the text: what came before it, or before no call
                // of this step.
                for (k, (at, item)) in items.iter().enumerate() {
                    if !at.as_deref().is_some_and(|id| ids.contains(&id)) {
                        input.push(item.clone());
                        sent[k] = true;
                    }
                }
                if !is_empty(content) {
                    let mut item =
                        json!({ "role": role, "content": content_of(content, assistant) });
                    if assistant && phases {
                        item["phase"] = json!(if calls.is_empty() {
                            "final_answer"
                        } else {
                            "commentary"
                        });
                    }
                    input.push(item);
                }
                for (call, id) in calls.iter().zip(&ids) {
                    // What came right before this call.
                    for (k, (at, item)) in items.iter().enumerate() {
                        if !sent[k] && at.as_deref() == Some(*id) {
                            input.push(item.clone());
                            sent[k] = true;
                        }
                    }
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
    if let Some(max) = chat
        .get("max_tokens")
        .or_else(|| chat.get("max_completion_tokens"))
    {
        body["max_output_tokens"] = max.clone();
    }
    // `prompt_cache_key` is the same on both APIs (prompt_cache.rs).
    for field in [
        "temperature",
        "top_p",
        "parallel_tool_calls",
        "prompt_cache_key",
    ] {
        if let Some(value) = chat.get(field) {
            body[field] = value.clone();
        }
    }
    // `tool_choice`: strings pass through; a named chat-completions choice
    // (`{type: function, function: {name}}`) becomes the Responses form
    // (`{type: function, name}`).
    if let Some(choice) = chat.get("tool_choice") {
        body["tool_choice"] = match choice {
            Value::String(_) => choice.clone(),
            other => other
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .map(|name| json!({ "type": "function", "name": name }))
                .unwrap_or_else(|| other.clone()),
        };
    }
    // Chat tools nest the function (`{type, function: {name, …}}`); here
    // it's flat (`{type, name, …}`).
    if let Some(tools) = chat.get("tools").and_then(|t| t.as_array()) {
        let tools: Vec<Value> = tools
            .iter()
            .map(
                |tool| match tool.get("function").and_then(|f| f.as_object()) {
                    Some(function) => {
                        let mut flat = function.clone();
                        flat.insert("type".into(), json!("function"));
                        Value::Object(flat)
                    }
                    None => tool.clone(),
                },
            )
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
    // Stateless (`store: false`): the reasoning comes back encrypted only
    // when asked for, to send with the next step. Asked whatever the effort:
    // at the provider's default effort the model reasons too, and in a tool
    // calling loop it needs its reasoning back with the calls.
    if echo && effort != Some("none") {
        body["include"] = json!(["reasoning.encrypted_content"]);
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
    /// The entries kept to send back since the reply's text or the last
    /// call came right before the call `id`: they go back right before it
    /// (`request_body`), where they were.
    fn place_before_call(&mut self, id: &str) {
        for entry in self
            .reasoning_details
            .iter_mut()
            .skip(self.responses_placed)
        {
            let ours = entry.get("format").and_then(|f| f.as_str())
                == Some(crate::reasoning_echo::RESPONSES_FORMAT);
            if let Some(entry) = entry.as_object_mut().filter(|_| ours) {
                entry
                    .entry("before_call")
                    .or_insert_with(|| Value::String(id.to_string()));
            }
        }
        self.responses_placed = self.reasoning_details.len();
    }

    /// One Responses API stream event, translated into the chat completions
    /// form `feed` takes. True once the response has ended.
    pub fn feed_responses(
        &mut self,
        value: &Value,
        emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<bool, ChatError> {
        let kind = str_at(value, "type");
        match kind {
            // Commentary (`phase: "commentary"`: "Checking how the modes
            // differ.", written between tool calls) isn't the reply: it's
            // shown with the reasoning.
            "response.output_text.delta" | "response.refusal.delta" => {
                let index = value.get("output_index").and_then(|i| i.as_u64());
                let text = str_at(value, "delta");
                if index.is_some_and(|i| self.commentary.contains(&i)) {
                    self.commentary_text.push_str(text);
                    self.feed(&delta(json!({ "reasoning_content": text })), emit)?;
                } else {
                    self.feed(&delta(json!({ "content": text })), emit)?;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.feed(
                    &delta(json!({ "reasoning_content": str_at(value, "delta") })),
                    emit,
                )?;
            }
            // Separate summary parts read as paragraphs.
            "response.reasoning_summary_part.added" => {
                if value
                    .get("summary_index")
                    .and_then(|i| i.as_u64())
                    .is_some_and(|i| i > 0)
                {
                    self.feed(&delta(json!({ "reasoning_content": "\n\n" })), emit)?;
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                let index = value
                    .get("output_index")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0);
                if kind == "response.output_item.added" && str_at(item, "type") == "message" {
                    if let Some(phase) = item.get("phase").and_then(|p| p.as_str()) {
                        if let Some(key) = &self.phase_key {
                            note_phases(key);
                        }
                        if phase == "commentary" && !self.commentary.contains(&index) {
                            self.commentary.push(index);
                            // Apart from the reasoning before it.
                            if self.reasoning_len > 0 {
                                self.feed(&delta(json!({ "reasoning_content": "\n\n" })), emit)?;
                            }
                        }
                    }
                    // The reply's text: what came before it goes back
                    // before it, not before a call after it.
                    if !self.commentary.contains(&index) {
                        self.responses_placed = self.reasoning_details.len();
                    }
                }
                // Reasoning to send back with the next request: the whole item.
                if kind == "response.output_item.done" && str_at(item, "type") == "reasoning" {
                    let entries = crate::reasoning_echo::from_responses_item(item);
                    if !entries.is_empty() {
                        self.feed(&delta(json!({ "reasoning_details": entries })), emit)?;
                    }
                }
                // Commentary goes back too, where it was: it's the model's
                // own account of what it's doing between its calls.
                if kind == "response.output_item.done"
                    && str_at(item, "type") == "message"
                    && self.commentary.contains(&index)
                {
                    if let Some(entry) = crate::reasoning_echo::commentary_entry(item, index) {
                        self.feed(&delta(json!({ "reasoning_details": [entry] })), emit)?;
                    }
                }
                if str_at(item, "type") == "function_call" {
                    // `call_id` is what the result is sent back with.
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .cloned()
                        .unwrap_or(json!(""));
                    if let Some(id) = id.as_str().filter(|id| !id.is_empty()) {
                        self.place_before_call(id);
                    }
                    // Whole arguments: only what hasn't been streamed yet.
                    let streamed = self
                        .calls
                        .iter()
                        .find(|(i, _)| *i == index)
                        .map(|(_, c)| c.arguments.clone())
                        .unwrap_or_default();
                    let rest = str_at(item, "arguments")
                        .strip_prefix(streamed.as_str())
                        .unwrap_or("");
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
                let index = value
                    .get("output_index")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0);
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
                    match response
                        .pointer("/incomplete_details/reason")
                        .and_then(|r| r.as_str())
                    {
                        Some("max_output_tokens") | None => "length".to_string(),
                        Some(other) => other.to_string(),
                    }
                } else if self.emitted_call || !self.calls.is_empty() {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                });
                // Nothing but commentary: that's the reply, then (and not
                // sent back a second time as commentary).
                if !self.visible() && !self.commentary_text.is_empty() {
                    let text = std::mem::take(&mut self.commentary_text);
                    self.reasoning_details
                        .retain(|e| !crate::reasoning_echo::is_commentary(e));
                    self.feed(&delta(json!({ "content": text })), emit)?;
                }
                return Ok(true);
            }
            "response.failed" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                return Err(classify_body_error(response).unwrap_or_else(|| {
                    ChatError::new(
                        ErrorKind::Provider,
                        "The provider ended the reply with an error.",
                    )
                    .with_details(value.to_string())
                }));
            }
            // `{"type":"error","error":{…}}`, or the fields at the top level.
            "error" if value.get("error").is_some_and(|e| e.is_object()) => {
                return Err(classify_body_error(value).unwrap_or_else(|| {
                    ChatError::new(ErrorKind::Provider, "The provider reported an error.")
                }));
            }
            "error" => {
                let error = json!({ "error": {
                    "message": value.get("message").cloned().unwrap_or(Value::Null),
                    "code": value.get("code").cloned().unwrap_or(Value::Null),
                } });
                return Err(classify_body_error(&error).unwrap_or_else(|| {
                    ChatError::new(ErrorKind::Provider, "The provider reported an error.")
                }));
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
        for (index, item) in body
            .get("output")
            .and_then(|o| o.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            // Each item as a stream ends it (`output_item.done`), so what's
            // sent back is kept here too.
            let done =
                json!({ "type": "response.output_item.done", "output_index": index, "item": item });
            match str_at(item, "type") {
                "message" => {
                    self.feed_responses(
                        &json!({ "type": "response.output_item.added", "output_index": index, "item": item }),
                        emit,
                    )?;
                    for part in item
                        .get("content")
                        .and_then(|c| c.as_array())
                        .into_iter()
                        .flatten()
                    {
                        let text = part
                            .get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(|t| t.as_str());
                        if let Some(text) = text {
                            self.feed_responses(
                                &json!({ "type": "response.output_text.delta", "output_index": index, "delta": text }),
                                emit,
                            )?;
                        }
                    }
                    self.feed_responses(&done, emit)?;
                }
                "reasoning" => {
                    let parts = ["summary", "content"]
                        .iter()
                        .flat_map(|k| {
                            item.get(*k)
                                .and_then(|s| s.as_array())
                                .into_iter()
                                .flatten()
                        })
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>();
                    if !parts.is_empty() {
                        self.feed_responses(
                            &json!({ "type": "response.reasoning_text.delta", "delta": parts.join("\n\n") }),
                            emit,
                        )?;
                    }
                    self.feed_responses(&done, emit)?;
                }
                "function_call" => {
                    self.feed_responses(&done, emit)?;
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

    #[test]
    fn reasoning_goes_back_as_items_before_the_step() {
        let item = json!({
            "type": "reasoning", "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "Read first." }],
            "encrypted_content": "enc",
        });
        let details = crate::reasoning_echo::from_responses_item(&item);
        let chat = json!({
            "model": "m",
            "reasoning_effort": "high",
            "messages": [
                { "role": "user", "content": "Read a.txt" },
                { "role": "assistant", "content": "", "reasoning_details": details,
                  "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{}" } }] },
                { "role": "tool", "tool_call_id": "c1", "content": "one" },
            ],
        });
        let body = request_body(&chat, false, true);
        let input = body["input"].as_array().unwrap();
        let kinds: Vec<&str> = input
            .iter()
            .map(|i| i["type"].as_str().unwrap_or("message"))
            .collect();
        assert_eq!(
            kinds,
            [
                "message",
                "reasoning",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(input[1], item);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        // Refused by the server: not asked for (the items are stripped before).
        assert!(request_body(&chat, false, false).get("include").is_none());
        let off = json!({ "model": "m", "reasoning_effort": "none", "messages": [] });
        assert!(request_body(&off, false, true).get("include").is_none());
        // No effort picked: the model reasons at its default, and its
        // reasoning still has to come back to go with the next step.
        let default = request_body(&json!({ "model": "m", "messages": [] }), false, true);
        assert_eq!(default["include"], json!(["reasoning.encrypted_content"]));
        assert!(default.get("reasoning").is_none(), "no effort is sent");
    }

    /// A step's input items, in order, named for comparing.
    fn order_of(input: &[Value]) -> Vec<String> {
        input
            .iter()
            .map(|i| match i["type"].as_str() {
                Some("reasoning") => format!("reasoning {}", i["id"].as_str().unwrap_or("")),
                Some("function_call") => format!("call {}", i["call_id"].as_str().unwrap_or("")),
                Some("function_call_output") => {
                    format!("result {}", i["call_id"].as_str().unwrap_or(""))
                }
                _ => format!(
                    "{} {}",
                    i["role"].as_str().unwrap_or(""),
                    i.get("phase").and_then(|p| p.as_str()).unwrap_or("-")
                ),
            })
            .collect()
    }

    fn details_of(events: &[ChatStreamEvent]) -> Vec<Value> {
        events
            .iter()
            .find_map(|e| match e {
                ChatStreamEvent::ReasoningDetails { details } => Some(details.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_streamed_step_goes_back_in_the_order_it_came() {
        let reasoning = |at: u64, id: &str| {
            json!({ "type": "response.output_item.done", "output_index": at,
                    "item": { "type": "reasoning", "id": id, "summary": [], "encrypted_content": format!("enc-{id}") } })
        };
        let call = |at: u64, id: &str| {
            json!({ "type": "response.output_item.done", "output_index": at,
                    "item": { "type": "function_call", "call_id": id, "name": "read", "arguments": "{}" } })
        };
        // Reasoning, commentary and a call; more reasoning and a second call.
        let (ended, mut events, mut parser) = feed_all(&[
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1", "summary": [] } }),
            reasoning(0, "rs_1"),
            json!({ "type": "response.output_item.added", "output_index": 1,
                    "item": { "type": "message", "role": "assistant", "phase": "commentary", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "Reading a, then b." }),
            json!({ "type": "response.output_item.done", "output_index": 1,
                    "item": { "type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary",
                              "content": [{ "type": "output_text", "text": "Reading a, then b." }] } }),
            json!({ "type": "response.output_item.added", "output_index": 2,
                    "item": { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "" } }),
            call(2, "c1"),
            reasoning(3, "rs_2"),
            call(4, "c2"),
            json!({ "type": "response.completed", "response": {} }),
        ]);
        assert!(ended.unwrap());
        parser.finish(&mut |e| events.push(e)).unwrap();
        let (reply, shown) = text_of_events(&events);
        assert_eq!(
            (reply.as_str(), shown.as_str()),
            ("", "Reading a, then b."),
            "commentary is shown with the reasoning, not as the reply"
        );

        // The step as the history sends it back (chat_db::history::to_wire).
        let chat = json!({ "model": "m", "messages": [
            { "role": "user", "content": "Read a and b" },
            { "role": "assistant", "content": "", "reasoning_details": details_of(&events), "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{}" } },
                { "id": "c2", "type": "function", "function": { "name": "read", "arguments": "{}" } }
            ] },
            { "role": "tool", "tool_call_id": "c1", "content": "a" },
            { "role": "tool", "tool_call_id": "c2", "content": "b" },
        ] });
        let body = request_body(&chat, false, true);
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            order_of(input),
            [
                "user -",
                "reasoning rs_1",
                "assistant commentary",
                "call c1",
                "reasoning rs_2",
                "call c2",
                "result c1",
                "result c2"
            ]
        );
        assert_eq!(input[1]["encrypted_content"], "enc-rs_1");
        assert_eq!(input[2]["content"], "Reading a, then b.");
        // Echo refused: neither reasoning nor commentary goes back.
        let mut stripped = chat.clone();
        crate::reasoning_echo::strip(&mut stripped);
        let input = request_body(&stripped, false, false)["input"].clone();
        assert_eq!(
            order_of(input.as_array().unwrap()),
            ["user -", "call c1", "call c2", "result c1", "result c2"]
        );
    }

    #[test]
    fn a_complete_response_keeps_what_goes_back_in_its_place() {
        // Not streamed: reasoning before the text stays before it, and
        // reasoning after it goes right before the call it came before.
        let body = json!({ "status": "completed", "output": [
            { "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "enc-1" },
            { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Plan: read it." }] },
            { "type": "reasoning", "id": "rs_2", "summary": [], "encrypted_content": "enc-2" },
            { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}" }
        ] });
        let mut parser = ReplyParser::default();
        let mut events = Vec::new();
        parser
            .feed_complete_responses(&body, &mut |e| events.push(e))
            .unwrap();
        parser.finish(&mut |e| events.push(e)).unwrap();
        let details = details_of(&events);
        let placed: Vec<(Value, Value)> = details
            .iter()
            .map(|d| (d["id"].clone(), d["before_call"].clone()))
            .collect();
        assert_eq!(
            placed,
            [(json!("rs_1"), Value::Null), (json!("rs_2"), json!("c1"))]
        );
        let chat = json!({ "model": "m", "messages": [
            { "role": "assistant", "content": "Plan: read it.", "reasoning_details": details, "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{}" } }
            ] },
        ] });
        assert_eq!(
            order_of(
                request_body(&chat, false, true)["input"]
                    .as_array()
                    .unwrap()
            ),
            ["reasoning rs_1", "assistant -", "reasoning rs_2", "call c1"]
        );
    }

    #[test]
    fn a_finished_reasoning_item_is_kept_to_send_back() {
        let mut parser = ReplyParser::default();
        let mut events = Vec::new();
        let mut emit = |e: ChatStreamEvent| events.push(e);
        let item = json!({ "type": "reasoning", "id": "rs_1", "summary": [{ "type": "summary_text", "text": "Hm." }], "encrypted_content": "enc" });
        for event in [
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Hm." }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "Done." }),
        ] {
            parser.feed_responses(&event, &mut emit).unwrap();
        }
        parser.finish(&mut emit).unwrap();
        let details = events.iter().find_map(|e| match e {
            ChatStreamEvent::ReasoningDetails { details } => Some(details.clone()),
            _ => None,
        });
        assert_eq!(
            details.map(|d| crate::reasoning_echo::to_responses_items(&d)),
            Some(vec![(None, item)])
        );
    }
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
            "prompt_cache_key": "chat-1",
            "tools": [{ "type": "function", "function": { "name": "read", "description": "d", "parameters": { "type": "object" } } }],
            "messages": [
                { "role": "system", "content": "Be useful." },
                { "role": "user", "content": [
                    { "type": "text", "text": "Look" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } },
                    { "type": "file", "file": { "filename": "a.pdf", "file_data": "data:application/pdf;base64,JVBE" } }
                ] },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "file text" },
                { "role": "assistant", "content": "Done." }
            ]
        });
        let body = request_body(&chat, false, true);
        assert_eq!(body["instructions"], "Be useful.");
        assert_eq!(body["max_output_tokens"], 1000);
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert_eq!(body["store"], false);
        assert_eq!(
            body["prompt_cache_key"], "chat-1",
            "the cache key is the same on both APIs"
        );
        for gone in [
            "messages",
            "stream_options",
            "chat_template_kwargs",
            "reasoning_effort",
            "max_tokens",
        ] {
            assert!(body.get(gone).is_none(), "{gone}");
        }
        assert_eq!(
            body["tools"][0],
            json!({ "type": "function", "name": "read", "description": "d", "parameters": { "type": "object" } })
        );
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 4, "{input:?}");
        assert_eq!(
            input[0]["content"][0],
            json!({ "type": "input_text", "text": "Look" })
        );
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(
            input[0]["content"][1]["image_url"],
            "data:image/png;base64,AA"
        );
        assert_eq!(
            input[0]["content"][2],
            json!({ "type": "input_file", "filename": "a.pdf", "file_data": "data:application/pdf;base64,JVBE" })
        );
        assert_eq!(
            input[1],
            json!({ "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a\"}" })
        );
        assert_eq!(
            input[2],
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "file text" })
        );
        assert_eq!(input[3], json!({ "role": "assistant", "content": "Done." }));
    }

    #[test]
    fn reasoning_off_is_effort_none() {
        let body = request_body(
            &json!({ "model": "m", "messages": [], "reasoning": { "enabled": false } }),
            false,
            true,
        );
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
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"path\":" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "delta": "\"a\"}" }),
            json!({ "type": "response.output_item.done", "output_index": 2,
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "read", "arguments": "{\"path\":\"a\"}" } }),
            json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 10, "output_tokens": 5 } } }),
        ]);
        assert!(ended.unwrap());
        parser.finish(&mut |e| events.push(e)).unwrap();
        let complete = events.iter().find_map(|e| match e {
            ChatStreamEvent::ToolCallComplete {
                id,
                name,
                arguments,
                ..
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        });
        assert_eq!(
            complete,
            Some(("call_9".into(), "read".into(), "{\"path\":\"a\"}".into()))
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatStreamEvent::Delta { content } if content == "Reading it.")));
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatStreamEvent::Reasoning { content } if content == "Thinking")));
        assert!(events.iter().any(|e| matches!(e,
            ChatStreamEvent::Finished { finish_reason: Some(r), usage: Some(_) } if r == "tool_calls")));
    }

    fn text_of_events(events: &[ChatStreamEvent]) -> (String, String) {
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

    #[test]
    fn commentary_goes_with_the_reasoning_not_the_reply() {
        let key = phase_key("https://phases.test/v1/responses", "gpt-5.5");
        let mut parser = ReplyParser {
            phase_key: Some(key.clone()),
            ..Default::default()
        };
        let mut events = Vec::new();
        for event in [
            json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "Plan" }),
            json!({ "type": "response.output_item.added", "output_index": 1,
                    "item": { "type": "message", "role": "assistant", "phase": "commentary", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "delta": "Checking how modes differ." }),
            json!({ "type": "response.output_item.added", "output_index": 2,
                    "item": { "type": "function_call", "call_id": "c", "name": "search_code", "arguments": "{}" } }),
            json!({ "type": "response.completed", "response": {} }),
        ] {
            parser
                .feed_responses(&event, &mut |e| events.push(e))
                .unwrap();
        }
        parser.finish(&mut |e| events.push(e)).unwrap();
        let (reply, reasoning) = text_of_events(&events);
        assert_eq!(reply, "");
        assert_eq!(reasoning, "Plan\n\nChecking how modes differ.");
        assert!(uses_phases(&key));

        // The final answer is the reply.
        let (_, events, _) = feed_all(&[
            json!({ "type": "response.output_item.added", "output_index": 0,
                    "item": { "type": "message", "role": "assistant", "phase": "final_answer", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "No: Trimming doesn't use TASK.md." }),
        ]);
        assert_eq!(
            text_of_events(&events).0,
            "No: Trimming doesn't use TASK.md."
        );
    }

    #[test]
    fn a_reply_of_only_commentary_is_kept_as_the_reply() {
        let (ended, mut events, mut parser) = feed_all(&[
            json!({ "type": "response.output_item.added", "output_index": 0,
                    "item": { "type": "message", "phase": "commentary", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "Done." }),
            json!({ "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "message", "phase": "commentary", "content": [{ "type": "output_text", "text": "Done." }] } }),
            json!({ "type": "response.completed", "response": {} }),
        ]);
        assert!(ended.unwrap());
        parser.finish(&mut |e| events.push(e)).unwrap();
        assert_eq!(text_of_events(&events).0, "Done.");
        assert!(
            details_of(&events).is_empty(),
            "the reply now: not sent back again as commentary"
        );
    }

    #[test]
    fn complete_responses_split_commentary_too() {
        let body = json!({ "status": "completed", "output": [
            { "type": "message", "phase": "commentary", "content": [{ "type": "output_text", "text": "Looking." }] },
            { "type": "message", "phase": "final_answer", "content": [{ "type": "output_text", "text": "Answer." }] }
        ] });
        let mut parser = ReplyParser::default();
        let mut events = Vec::new();
        parser
            .feed_complete_responses(&body, &mut |e| events.push(e))
            .unwrap();
        assert_eq!(
            text_of_events(&events),
            ("Answer.".to_string(), "Looking.".to_string())
        );
    }

    #[test]
    fn earlier_replies_are_sent_back_with_their_phase() {
        let chat = json!({ "model": "m", "messages": [
            { "role": "user", "content": "Q" },
            { "role": "assistant", "content": "Let me look.", "tool_calls": [
                { "id": "c", "type": "function", "function": { "name": "f", "arguments": "{}" } }
            ] },
            { "role": "tool", "tool_call_id": "c", "content": "r" },
            { "role": "assistant", "content": "A." }
        ] });
        let input = request_body(&chat, true, true)["input"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(input[1]["phase"], "commentary");
        assert_eq!(input[4]["phase"], "final_answer");
        assert!(input[0].get("phase").is_none(), "never on user messages");
        let plain = request_body(&chat, false, true)["input"]
            .as_array()
            .unwrap()
            .clone();
        assert!(plain.iter().all(|i| i.get("phase").is_none()));
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
        let (ended, _, _) = feed_all(&[
            json!({ "type": "error", "code": "rate_limit_exceeded", "message": "slow down" }),
        ]);
        assert!(ended.unwrap_err().message.contains("slow down"));
        let (ended, _, _) = feed_all(&[json!({ "type": "error",
            "error": { "type": "ModelProtocolUnsupported", "message": "Model does not support this protocol." } })]);
        let error = ended.unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(error
            .message
            .contains("Model does not support this protocol."));
        let (ended, _, mut parser) = feed_all(&[json!({ "type": "response.incomplete",
            "response": { "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } } })]);
        assert!(ended.unwrap());
        assert_eq!(
            parser.finish(&mut |_| {}).unwrap_err().kind,
            ErrorKind::Length
        );
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
        parser
            .feed_complete_responses(&body, &mut |e| events.push(e))
            .unwrap();
        parser.finish(&mut |e| events.push(e)).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatStreamEvent::Delta { content } if content == "A title")));
        assert!(events.iter().any(
            |e| matches!(e, ChatStreamEvent::Finished { finish_reason: Some(r), .. } if r == "stop")
        ));
    }

    #[test]
    fn recognizes_refusals_that_point_to_the_other_route() {
        let headers = reqwest::header::HeaderMap::new();
        let classify = |status, body: &str| super::super::classify_status(status, &headers, body);
        let chat = Api::ChatCompletions;
        let openai = r#"{"error":{"message":"This model is only supported in v1/responses and not in v1/chat/completions.","type":"invalid_request_error"}}"#;
        assert!(wrong_route(&classify(404, openai), chat));
        assert!(wrong_route(&classify(400, openai), chat));
        let azure = r#"{"error":{"code":"OperationNotSupported","message":"The chatCompletion operation does not work with the specified model, codex-mini."}}"#;
        assert!(wrong_route(&classify(400, azure), chat));
        // A server with no such route.
        assert!(wrong_route(&classify(404, "404 page not found"), chat));
        assert!(wrong_route(&classify(405, ""), Api::Responses));
        let invalid = r#"{"error":{"message":"Invalid URL (POST /v1/responses)","type":"invalid_request_error"}}"#;
        assert!(wrong_route(&classify(404, invalid), Api::Responses));
        let back =
            r#"{"error":{"message":"This model is only supported in v1/chat/completions."}}"#;
        assert!(wrong_route(&classify(400, back), Api::Responses));
        // A missing model, a bad field, an auth failure: not the route.
        let missing = r#"{"error":{"message":"The model `nope` does not exist or you do not have access to it."}}"#;
        assert!(!wrong_route(&classify(404, missing), chat));
        // On `/responses`, any 404: the model may be on chat completions
        // only, and a provider without the Responses API may say "Not
        // Found" in JSON of its own.
        assert!(wrong_route(&classify(404, missing), Api::Responses));
        let not_found =
            r#"{"type":"error","error":{"type":"not_found_error","message":"Not Found"}}"#;
        assert!(wrong_route(&classify(404, not_found), Api::Responses));
        assert!(!wrong_route(&classify(404, not_found), chat));
        // Says nothing about which API: the other one, from either side.
        let protocol = r#"{"type":"error","error":{"type":"ModelProtocolUnsupported","message":"Model does not support this protocol."}}"#;
        assert!(wrong_route(&classify(400, protocol), chat));
        assert!(wrong_route(&classify(400, protocol), Api::Responses));
        assert!(
            wrong_route(&classify(500, protocol), chat),
            "not a server error to retry"
        );
        let in_body = classify_body_error(&serde_json::from_str(protocol).unwrap()).unwrap();
        assert_eq!(in_body.kind, ErrorKind::BadRequest);
        assert!(!in_body.retryable);
        assert!(in_body
            .message
            .contains("Model does not support this protocol."));
        assert!(wrong_route(&in_body, Api::Responses));
        let field = r#"{"error":{"message":"Unrecognized request argument supplied: reasoning"}}"#;
        assert!(!wrong_route(&classify(400, field), chat));
        assert!(!wrong_route(&classify(400, field), Api::Responses));
        assert!(!wrong_route(&classify(401, openai), chat));
        // A server whose Responses API can't read the input (SGLang's
        // router, with a tool call in it): chat completions takes it.
        let unreadable = r#"{"error":{"message":"Invalid JSON data: Failed to deserialize the JSON body into the target type: input: data did not match any variant of untagged enum ResponseInput at line 1 column 1888","type":"invalid_request_error","code":"json_parse_error"}}"#;
        assert!(wrong_route(&classify(400, unreadable), Api::Responses));
        assert!(wrong_route(&classify(422, unreadable), Api::Responses));
        assert!(!wrong_route(&classify(400, unreadable), chat));
        let item = "Failed to deserialize the JSON body into the target type: input[2]: missing field `id` at line 1 column 512";
        assert!(wrong_route(&classify(422, item), Api::Responses));
        // Another field it can't read, or OpenAI's own words: not the route.
        let effort = "Failed to deserialize the JSON body into the target type: reasoning.effort: unknown variant `xhigh`, expected one of `minimal`, `low`, `medium`, `high` at line 1 column 90";
        assert!(!wrong_route(&classify(422, effort), Api::Responses));
        let openai_input = r#"{"error":{"message":"Invalid type for 'input[2].content': expected one of an array of objects or string, but got an object instead.","type":"invalid_request_error","param":"input[2].content"}}"#;
        assert!(!wrong_route(&classify(400, openai_input), Api::Responses));
        // A gateway that had started its stream reports it there, with the
        // upstream's 400: the request is at fault, not the provider.
        let gateway = r#"{"type":"error","error":{"message":"BadRequestError: Failed to deserialize the JSON body into the target type: input: data did not match any variant of untagged enum ResponseInput at line 1 column 1888","type":"BadRequestError","code":400}}"#;
        let in_stream = classify_body_error(&serde_json::from_str(gateway).unwrap()).unwrap();
        assert_eq!(in_stream.kind, ErrorKind::BadRequest);
        assert!(wrong_route(&in_stream, Api::Responses));
        let upstream = r#"{"type":"error","error":{"message":"Upstream timed out","code":408}}"#;
        let in_stream = classify_body_error(&serde_json::from_str(upstream).unwrap()).unwrap();
        assert!(!wrong_route(&in_stream, Api::Responses));
    }

    #[test]
    fn urls_and_memory() {
        assert_eq!(
            responses_url("https://api.openai.com/v1/chat/completions").as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
        assert_eq!(responses_url("https://example.test/custom"), None);
        let url = "https://memory.test/v1/chat/completions";
        assert_eq!(remembered(url, "m"), None);
        remember(url, "m", Api::Responses);
        assert_eq!(remembered(url, "m"), Some(Api::Responses));
        assert_eq!(remembered(url, "other"), None);
        assert_eq!(Api::parse(Api::Responses.as_str()), Some(Api::Responses));
    }
}

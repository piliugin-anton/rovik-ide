//! The Messages API request for the chat completions body agent.rs builds
//! for every provider, sent the way a `Plan` says.
//!
//! - System messages become the top-level `system`; a `tool` message, a
//!   `tool_result` block in a user turn; consecutive messages of one role,
//!   one turn (tool results first in theirs).
//! - Images and PDFs (`image_url` and `file` parts, as `data:` URLs) become
//!   `image` and `document` blocks.
//! - Prompt caching: the `cache_control` breakpoints prompt_cache.rs puts on
//!   text parts stay on their blocks (on a tool result, on the result).
//! - Reasoning sent back (`reasoning_details`, the model's own) becomes its
//!   thinking blocks again, each where it was: before the tool call it came
//!   right before, else first (reasoning_echo.rs). The app changes a
//!   history's tail between steps (its notes move on, old results shrink),
//!   which a block bound to its conversation no longer matches: the API is
//!   told to leave such a block out (`block_binding`), not to refuse it.
//! - `max_tokens`, which the API requires: the body's, else the model's
//!   most (it doesn't count against the rate limits); never above what the
//!   model takes.

use super::models::{ModelInfo, LEVELS};
use crate::agent::ReasoningEffort;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// `max_tokens` when the model's limit isn't known: within every Claude
/// model's since Claude 4 (32,000 on Opus 4 and 4.1).
const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// `max_tokens` at most for a request that isn't streamed: its reply comes
/// back whole, and a long one would run into the timeouts.
const UNSTREAMED_MAX_TOKENS: u32 = 16_000;
/// Thinking budgets by effort, for models from before adaptive thinking.
const BUDGETS: [(&str, u32); 3] = [("low", 2_048), ("medium", 8_192), ("high", 16_384)];
/// The smallest thinking budget the API takes.
const MIN_BUDGET: u32 = 1_024;
/// The first message has to be the user's: in front of a history that
/// starts with a reply (its start trimmed away).
const CONTINUED: &str = "[Earlier messages in this chat were omitted to fit the context window.]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thinking {
    /// Not sent: the model's own default.
    Default,
    /// `{type: "adaptive"}`: the model decides when and how much to think.
    Adaptive,
    /// `{type: "enabled", budget_tokens}`, before adaptive thinking.
    Budget(u32),
    Disabled,
    /// `{type: "between_tools"}`: off, with short progress notes between
    /// tool calls (Claude Sonnet 5.5 can't turn it off otherwise).
    BetweenTools,
}

/// What one request leaves out after the API refused it (`super::adjust`),
/// unlike what's learned about the model (`ModelInfo`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tweaks {
    /// Thinking isn't sent back.
    pub no_echo: bool,
    /// No `block_binding` (nor its beta).
    pub no_binding: bool,
    /// `tool_choice` `auto` in place of a forced tool.
    pub no_forced_tool: bool,
    /// Thinking off: the history needs thinking it no longer has.
    pub no_thinking: bool,
    /// `max_tokens` at most: what the context window has room for.
    pub room: Option<u32>,
    pub no_temperature: bool,
    pub no_top_p: bool,
    pub no_stop: bool,
}

/// How to send a request to one model.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// `max_tokens` when the body has none.
    pub max_tokens: u32,
    /// `max_tokens` at most.
    pub limit: Option<u32>,
    pub thinking: Thinking,
    /// The reasoning is shown, summarized (`display: "summarized"`).
    pub display: bool,
    pub effort: Option<&'static str>,
    /// The model thinks when `thinking` isn't sent.
    pub thinks_by_default: bool,
    pub tweaks: Tweaks,
}

/// The thinking budget for an effort level.
fn budget_for(level: &str) -> u32 {
    BUDGETS
        .iter()
        .find(|(l, _)| *l == level)
        .map_or(BUDGETS[1].1, |(_, b)| *b)
}

impl Plan {
    /// The effort the user chose (None: the provider's default, nothing
    /// sent), the way this model takes it:
    /// - a level: adaptive thinking with `output_config.effort`, or a
    ///   thinking budget on models from before adaptive thinking;
    /// - "none": thinking turned off, as the model allows (`disabled`, else
    ///   `between_tools`); a model that can't turn it off (Claude Opus 5.5,
    ///   Fable) gets its lowest effort instead;
    /// - the default: nothing, but a model that thinks anyway is asked to
    ///   show it.
    pub fn new(
        info: &ModelInfo,
        effort: Option<ReasoningEffort>,
        streaming: bool,
        tweaks: &Tweaks,
    ) -> Plan {
        let max_tokens = match (streaming, info.max_output) {
            (true, cap) => cap.unwrap_or(DEFAULT_MAX_TOKENS),
            (false, cap) => cap.map_or(UNSTREAMED_MAX_TOKENS, |c| c.min(UNSTREAMED_MAX_TOKENS)),
        };
        let limit = match (info.max_output, tweaks.room) {
            (Some(cap), Some(room)) => Some(cap.min(room)),
            (cap, room) => cap.or(room),
        };
        let mut plan = Plan {
            max_tokens,
            limit,
            thinking: Thinking::Default,
            display: false,
            effort: None,
            thinks_by_default: info.thinks_by_default,
            tweaks: tweaks.clone(),
        };
        if tweaks.no_thinking {
            if info.thinking && info.disabled {
                plan.thinking = Thinking::Disabled;
            }
            plan.thinks_by_default = false;
            return plan;
        }
        let adaptive = |plan: &mut Plan| {
            plan.thinking = Thinking::Adaptive;
            plan.display = info.display;
        };
        match effort {
            None => {
                if info.thinks_by_default && info.adaptive {
                    adaptive(&mut plan);
                }
            }
            Some(ReasoningEffort::None) => {
                if !info.thinking {
                    // Nothing to turn off.
                } else if info.disabled {
                    plan.thinking = Thinking::Disabled;
                } else if info.between_tools {
                    plan.thinking = Thinking::BetweenTools;
                } else {
                    plan.effort = info.lowest_effort();
                }
            }
            Some(level) => {
                let level = level.level();
                if info.adaptive {
                    adaptive(&mut plan);
                } else if info.budget {
                    plan.thinking = Thinking::Budget(budget_for(level));
                }
                plan.effort = info.effort_for(level);
            }
        }
        plan
    }

    /// Whether the model thinks on this request.
    fn thinks(&self, thinking: Option<&Value>) -> bool {
        match thinking
            .and_then(|t| t.get("type"))
            .and_then(|t| t.as_str())
        {
            Some("adaptive" | "enabled") => true,
            Some(_) => false,
            None => self.thinks_by_default,
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * Content
 * ---------------------------------------------------------------------------
 */

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

/// `data:<media type>;base64,<data>`: the media type and the data.
fn data_url(url: &str) -> Option<(&str, &str)> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    Some((meta.strip_suffix(";base64")?, data))
}

/// A text block; none for an empty or blank text, which the API refuses.
fn text_block(text: &str, cache: Option<&Value>) -> Option<Value> {
    if text.trim().is_empty() {
        return None;
    }
    let mut block = json!({ "type": "text", "text": text });
    if let Some(cache) = cache {
        block["cache_control"] = cache.clone();
    }
    Some(block)
}

fn image_block(image: &Value) -> Option<Value> {
    let url = str_of(image, "url").or_else(|| image.as_str())?;
    let source = match data_url(url) {
        Some((media, data)) => json!({ "type": "base64", "media_type": media, "data": data }),
        None if url.starts_with("https://") || url.starts_with("http://") => {
            json!({ "type": "url", "url": url })
        }
        None => return None,
    };
    Some(json!({ "type": "image", "source": source }))
}

/// A PDF (`file` part), titled with its name.
fn document_block(file: &Value) -> Option<Value> {
    let (media, data) = data_url(str_of(file, "file_data")?)?;
    if media != "application/pdf" {
        return None;
    }
    let mut block = json!({
        "type": "document",
        "source": { "type": "base64", "media_type": media, "data": data },
    });
    if let Some(name) = str_of(file, "filename").filter(|n| !n.trim().is_empty()) {
        block["title"] = json!(name);
    }
    Some(block)
}

/// One content part as a block: text, and in a user's message also images
/// and PDFs. Its breakpoint (prompt_cache.rs) comes along.
fn part_block(part: &Value, user: bool) -> Option<Value> {
    let cache = part.get("cache_control").filter(|c| c.is_object());
    let mut block = match str_of(part, "type")? {
        "text" => return text_block(str_of(part, "text")?, cache),
        "image_url" if user => image_block(part.get("image_url")?)?,
        "file" if user => document_block(part.get("file")?)?,
        // Already the Messages API's own.
        "image" | "document" if user => part.clone(),
        _ => return None,
    };
    if let Some(cache) = cache {
        block["cache_control"] = cache.clone();
    }
    Some(block)
}

/// A message's content (a string, or parts) as blocks.
fn blocks_of(content: &Value, user: bool) -> Vec<Value> {
    match content {
        Value::String(text) => text_block(text, None).into_iter().collect(),
        Value::Array(parts) => parts.iter().filter_map(|p| part_block(p, user)).collect(),
        _ => Vec::new(),
    }
}

/// A tool call id as the API takes them (letters, digits, `_` and `-`):
/// other providers' ids, earlier in a chat, can hold other characters. The
/// same id always comes out the same, so a call and its result still pair.
fn tool_id(id: &str) -> String {
    let id: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if id.is_empty() {
        "call".into()
    } else {
        id
    }
}

fn tool_use(call: &Value) -> Value {
    let arguments = call
        .pointer("/function/arguments")
        .and_then(|a| a.as_str())
        .unwrap_or("");
    // Arguments that aren't an object (a reply cut off mid-call) go back as
    // their text, as chat_db/history.rs sends them elsewhere.
    let input = match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(object)) => Value::Object(object),
        _ if arguments.trim().is_empty() => json!({}),
        _ => json!({ "invalid_arguments": arguments }),
    };
    json!({
        "type": "tool_use",
        "id": tool_id(str_of(call, "id").unwrap_or("")),
        "name": call.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or(""),
        "input": input,
    })
}

fn is_thinking(block: &Value) -> bool {
    matches!(
        str_of(block, "type"),
        Some("thinking" | "redacted_thinking")
    )
}

/// An assistant message as blocks: its thinking (when sent back), text and
/// tool calls, each thinking block where it was.
fn assistant_blocks(message: &Value, echo: bool) -> Vec<Value> {
    let calls: Vec<&Value> = message
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    let ids: Vec<&str> = calls
        .iter()
        .map(|c| str_of(c, "id").unwrap_or(""))
        .collect();
    let reasoning = match message.get("reasoning_details").and_then(|d| d.as_array()) {
        Some(details) if echo => crate::reasoning_echo::to_anthropic_blocks(details),
        _ => Vec::new(),
    };
    let mut blocks: Vec<Value> = reasoning
        .iter()
        .filter(|(at, _)| !at.as_deref().is_some_and(|id| ids.contains(&id)))
        .map(|(_, block)| block.clone())
        .collect();
    blocks.extend(blocks_of(
        message.get("content").unwrap_or(&Value::Null),
        false,
    ));
    for (call, id) in calls.iter().zip(&ids) {
        // The reasoning that came right before this call.
        for (at, block) in &reasoning {
            if at.as_deref() == Some(*id) {
                blocks.push(block.clone());
            }
        }
        blocks.push(tool_use(call));
    }
    blocks
}

/// A `tool` message as a `tool_result` block. Its breakpoint goes on the
/// result.
fn tool_result(message: &Value) -> Value {
    let content = message.get("content").unwrap_or(&Value::Null);
    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": tool_id(str_of(message, "tool_call_id").unwrap_or("")),
    });
    let cache = content
        .as_array()
        .and_then(|parts| {
            parts
                .iter()
                .rev()
                .find_map(|p| p.get("cache_control").filter(|c| c.is_object()))
        })
        .cloned();
    let mut parts = blocks_of(content, true);
    for part in &mut parts {
        if let Some(part) = part.as_object_mut() {
            part.remove("cache_control");
        }
    }
    if parts.iter().all(|p| str_of(p, "type") == Some("text")) {
        let text: Vec<&str> = parts.iter().filter_map(|p| str_of(p, "text")).collect();
        if !text.is_empty() {
            block["content"] = json!(text.join("\n"));
        }
    } else {
        block["content"] = json!(parts);
    }
    if let Some(cache) = cache {
        block["cache_control"] = cache;
    }
    block
}

/// Adds blocks to the conversation: to the last turn when it's the same
/// role's, else as a turn of their own. Nothing for no blocks.
fn push(turns: &mut Vec<(&'static str, Vec<Value>)>, role: &'static str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match turns.last_mut() {
        Some((last, content)) if *last == role => content.extend(blocks),
        _ => turns.push((role, blocks)),
    }
}

/*
 * ---------------------------------------------------------------------------
 * The request
 * ---------------------------------------------------------------------------
 */

fn tools_of(tools: Option<&Value>) -> Vec<Value> {
    tools
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            let name = str_of(function, "name").filter(|n| !n.is_empty())?;
            let mut schema = function
                .get("parameters")
                .or_else(|| function.get("input_schema"))
                .filter(|p| p.is_object())
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            if schema.get("type").is_none() {
                schema["type"] = json!("object");
            }
            let mut out = json!({ "name": name, "input_schema": schema });
            if let Some(description) = str_of(function, "description").filter(|d| !d.is_empty()) {
                out["description"] = json!(description);
            }
            if let Some(cache) = tool.get("cache_control").filter(|c| c.is_object()) {
                out["cache_control"] = cache.clone();
            }
            Some(out)
        })
        .collect()
}

/// `tool_choice` as the Messages API takes it: `auto`, `none`, `any`
/// (OpenAI's `required`), or a `tool` by name.
fn tool_choice(chat: &Value, plan: &Plan) -> Option<Value> {
    let mut choice = match chat.get("tool_choice") {
        Some(Value::String(choice)) => match choice.as_str() {
            "none" => json!({ "type": "none" }),
            "required" | "any" => json!({ "type": "any" }),
            _ => json!({ "type": "auto" }),
        },
        Some(Value::Object(choice)) => {
            let name = choice
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| choice.get("name"))
                .and_then(|n| n.as_str());
            match name {
                Some(name) => json!({ "type": "tool", "name": name }),
                None => json!({ "type": "auto" }),
            }
        }
        _ => json!({ "type": "auto" }),
    };
    if plan.tweaks.no_forced_tool && matches!(str_of(&choice, "type"), Some("any" | "tool")) {
        choice = json!({ "type": "auto" });
    }
    let serial = chat.get("parallel_tool_calls") == Some(&json!(false));
    if serial && str_of(&choice, "type") != Some("none") {
        choice["disable_parallel_tool_use"] = json!(true);
    }
    (chat.get("tool_choice").is_some() || serial).then_some(choice)
}

/// A tool for each name the history's tool calls use, said to be
/// unavailable: for a request that offers no tools.
fn placeholder_tools(body: &Value) -> Vec<Value> {
    let mut names: Vec<&str> = Vec::new();
    for block in body
        .get("messages")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("content").and_then(|c| c.as_array()))
        .flatten()
        .filter(|b| str_of(b, "type") == Some("tool_use"))
    {
        let name = str_of(block, "name").unwrap_or("");
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            json!({
                "name": name,
                "description": "Not available in this conversation any more; don't call it.",
                "input_schema": { "type": "object", "properties": {} },
            })
        })
        .collect()
}

/// Stop sequences: the API takes no blank one.
fn stop_sequences(stop: Option<&Value>) -> Vec<String> {
    let all: Vec<&Value> = match stop {
        Some(Value::Array(stops)) => stops.iter().collect(),
        Some(stop @ Value::String(_)) => vec![stop],
        _ => Vec::new(),
    };
    all.into_iter()
        .filter_map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The `thinking` object for the plan, if any.
fn thinking_of(plan: &Plan, max_tokens: u32) -> Option<Value> {
    match plan.thinking {
        Thinking::Default => None,
        Thinking::Adaptive => {
            let mut thinking = json!({ "type": "adaptive" });
            if plan.display {
                thinking["display"] = json!("summarized");
            }
            Some(thinking)
        }
        // Thinking counts toward `max_tokens`: at most half of it, so the
        // reply keeps room, and at least the API's minimum, or none.
        Thinking::Budget(budget) => {
            let budget = budget.min(max_tokens / 2);
            (budget >= MIN_BUDGET).then(|| json!({ "type": "enabled", "budget_tokens": budget }))
        }
        Thinking::Disabled => Some(json!({ "type": "disabled" })),
        Thinking::BetweenTools => Some(json!({ "type": "between_tools" })),
    }
}

/// The Messages API body for a chat completions body.
pub fn body(chat: &Value, plan: &Plan) -> Value {
    let wanted = chat
        .get("max_tokens")
        .or_else(|| chat.get("max_completion_tokens"))
        .and_then(|n| n.as_u64())
        .filter(|n| *n > 0)
        .map_or(plan.max_tokens, |n| u32::try_from(n).unwrap_or(u32::MAX));
    let max_tokens = plan.limit.map_or(wanted, |limit| wanted.min(limit)).max(1);
    let mut thinking = thinking_of(plan, max_tokens);
    // Thinking goes back only while the model thinks: turned off, a history
    // edited since (`block_binding` can't be set) mustn't refuse it.
    let echo = plan.thinks(thinking.as_ref()) && !plan.tweaks.no_echo;

    let mut system: Vec<Value> = Vec::new();
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // Tool call ids are unique in a request: some providers use the same
    // one in every reply ("function-call-1"), which a chat that moved on to
    // Claude still has. A repeat gets a suffix, and the results of the
    // latest reply's calls take their calls' ids, in order.
    let mut used: HashSet<String> = HashSet::new();
    let mut latest: HashMap<String, VecDeque<String>> = HashMap::new();
    for message in chat
        .get("messages")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
    {
        let content = message.get("content").unwrap_or(&Value::Null);
        match str_of(message, "role").unwrap_or("user") {
            "system" | "developer" => system.extend(blocks_of(content, false)),
            "assistant" => {
                let mut blocks = assistant_blocks(message, echo);
                latest.clear();
                for call in blocks
                    .iter_mut()
                    .filter(|b| str_of(b, "type") == Some("tool_use"))
                {
                    let id = str_of(call, "id").unwrap_or("").to_string();
                    let mut unique = id.clone();
                    let mut n = 1;
                    while used.contains(&unique) {
                        n += 1;
                        unique = format!("{id}_{n}");
                    }
                    used.insert(unique.clone());
                    call["id"] = json!(unique);
                    latest.entry(id).or_default().push_back(unique);
                }
                push(&mut turns, "assistant", blocks);
            }
            "tool" => {
                let mut result = tool_result(message);
                let id = str_of(&result, "tool_use_id").unwrap_or("").to_string();
                if let Some(unique) = latest.get_mut(&id).and_then(|ids| ids.pop_front()) {
                    result["tool_use_id"] = json!(unique);
                }
                push(&mut turns, "user", vec![result]);
            }
            _ => push(&mut turns, "user", blocks_of(content, true)),
        }
    }
    if turns.first().is_none_or(|(role, _)| *role != "user") {
        turns.insert(
            0,
            ("user", vec![json!({ "type": "text", "text": CONTINUED })]),
        );
    }
    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, mut blocks)| {
            // In a user's turn, the tool results come first.
            blocks.sort_by_key(|b| str_of(b, "type") != Some("tool_result"));
            json!({ "role": role, "content": blocks })
        })
        .collect();

    let mut body = json!({
        "model": chat.get("model").cloned().unwrap_or(Value::Null),
        "max_tokens": max_tokens,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if chat.get("stream").and_then(|s| s.as_bool()) == Some(true) {
        body["stream"] = json!(true);
    }
    let tools = tools_of(chat.get("tools"));
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        if let Some(choice) = tool_choice(chat, plan) {
            body["tool_choice"] = choice;
        }
    } else {
        // The API refuses tool calls in a history without tools (a model
        // set to work without them, in a chat that used them): the tools it
        // names are declared, and none may be called.
        let placeholders = placeholder_tools(&body);
        if !placeholders.is_empty() {
            body["tools"] = json!(placeholders);
            body["tool_choice"] = json!({ "type": "none" });
        }
    }
    for (field, dropped) in [
        ("temperature", plan.tweaks.no_temperature),
        ("top_p", plan.tweaks.no_top_p),
    ] {
        if let Some(value) = chat.get(field).filter(|v| v.is_number() && !dropped) {
            body[field] = value.clone();
        }
    }
    let stops = stop_sequences(chat.get("stop"));
    if !stops.is_empty() && !plan.tweaks.no_stop {
        body["stop_sequences"] = json!(stops);
    }
    // Thinking sent back is bound to the conversation it was written in,
    // which the app's history doesn't keep byte for byte: such a block is
    // left out of the request rather than refusing it. Said on every
    // request the model thinks on, not only those sending thinking back, so
    // the thinking settings don't change from one step to the next (a
    // change to them can cost the cached history).
    if !plan.tweaks.no_binding {
        let binding = json!({ "prefix_mismatch_behavior": "drop_block" });
        match &mut thinking {
            Some(t) if matches!(str_of(t, "type"), Some("adaptive" | "enabled")) => {
                t["block_binding"] = binding;
            }
            // Thinking by default: said, to carry the setting.
            None if plan.thinks_by_default => {
                thinking = Some(json!({ "type": "adaptive", "block_binding": binding }))
            }
            _ => {}
        }
    }
    if let Some(thinking) = thinking {
        body["thinking"] = thinking;
    }
    if let Some(effort) = plan.effort {
        body["output_config"] = json!({ "effort": effort });
    }
    body
}

/// Whether a Messages API body sends thinking back.
pub fn sends_thinking(body: &Value) -> bool {
    body.get("messages")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("content").and_then(|c| c.as_array()))
        .flatten()
        .any(is_thinking)
}

/// Whether `text` quotes the effort level `level` ('high', "high", `high`).
pub fn quotes(text: &str, level: &str) -> bool {
    ["'", "\"", "`"]
        .iter()
        .any(|q| text.contains(&format!("{q}{level}{q}")))
}

/// The effort levels a refusal quotes, other than `sent`.
pub fn offered_levels(text: &str, sent: &str) -> Vec<&'static str> {
    LEVELS
        .iter()
        .copied()
        .filter(|l| *l != sent && quotes(text, l))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(model: &str) -> ModelInfo {
        ModelInfo::guess(model)
    }

    fn plan(model: &str, effort: Option<ReasoningEffort>) -> Plan {
        Plan::new(&info(model), effort, true, &Tweaks::default())
    }

    /// A chat completions body as agent.rs builds it, after
    /// prompt_cache.rs marked its breakpoints.
    fn chat() -> Value {
        json!({
            "model": "claude-opus-5-5",
            "stream": true,
            "stream_options": { "include_usage": true },
            "reasoning_effort": "high",
            "chat_template_kwargs": { "enable_thinking": true },
            "tool_choice": "auto",
            "tools": [{ "type": "function", "function": {
                "name": "read", "description": "Read a file",
                "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } } }],
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "Be useful.", "cache_control": { "type": "ephemeral" } }] },
                { "role": "user", "content": [
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                    { "type": "file", "file": { "filename": "spec.pdf", "file_data": "data:application/pdf;base64,JVBERi0=" } },
                    { "type": "text", "text": "Read a.ts", "cache_control": { "type": "ephemeral" } }
                ] },
                { "role": "assistant", "content": "Reading it.",
                  "reasoning_details": [
                      { "type": "reasoning.text", "text": "Plan.", "signature": "s0", "format": "anthropic-claude-v1", "index": 0 },
                      { "type": "reasoning.text", "text": "", "signature": "s3", "format": "anthropic-claude-v1", "index": 3, "before_call": "toolu_b" }
                  ],
                  "tool_calls": [
                      { "id": "toolu_a", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a.ts\"}" } },
                      { "id": "toolu_b", "type": "function", "function": { "name": "read", "arguments": "" } }
                  ] },
                { "role": "tool", "tool_call_id": "toolu_a", "content": "file a" },
                { "role": "tool", "tool_call_id": "toolu_b", "content": "   " },
                { "role": "user", "content": [
                    { "type": "text", "text": "Screenshot from tool call toolu_b:" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,BBBB" } }
                ] }
            ]
        })
    }

    #[test]
    fn a_chat_becomes_a_messages_request() {
        let body = body(
            &chat(),
            &plan("claude-opus-5-5", Some(ReasoningEffort::High)),
        );
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["system"],
            json!([{ "type": "text", "text": "Be useful.", "cache_control": { "type": "ephemeral" } }])
        );
        // The OpenAI fields aren't sent.
        for field in ["stream_options", "reasoning_effort", "chat_template_kwargs"] {
            assert!(body.get(field).is_none(), "{field}");
        }
        assert_eq!(
            body["tools"],
            json!([{ "name": "read", "description": "Read a file",
                "input_schema": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } }])
        );
        assert_eq!(body["tool_choice"], json!({ "type": "auto" }));
        let messages = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        // The user's files, then the text with its breakpoint.
        assert_eq!(
            messages[0]["content"],
            json!([
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                { "type": "document", "source": { "type": "base64", "media_type": "application/pdf", "data": "JVBERi0=" }, "title": "spec.pdf" },
                { "type": "text", "text": "Read a.ts", "cache_control": { "type": "ephemeral" } }
            ])
        );
        // Thinking where it was: the first before everything, the second
        // right before the call it came before.
        assert_eq!(
            messages[1]["content"],
            json!([
                { "type": "thinking", "thinking": "Plan.", "signature": "s0" },
                { "type": "text", "text": "Reading it." },
                { "type": "tool_use", "id": "toolu_a", "name": "read", "input": { "path": "a.ts" } },
                { "type": "thinking", "thinking": "", "signature": "s3" },
                { "type": "tool_use", "id": "toolu_b", "name": "read", "input": {} }
            ])
        );
        // Both results and the screenshot after them, in one turn; a blank
        // result has no content.
        assert_eq!(
            messages[2]["content"],
            json!([
                { "type": "tool_result", "tool_use_id": "toolu_a", "content": "file a" },
                { "type": "tool_result", "tool_use_id": "toolu_b" },
                { "type": "text", "text": "Screenshot from tool call toolu_b:" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "BBBB" } }
            ])
        );
        // Adaptive thinking at the effort, shown; thinking sent back is
        // left out where the history changed, not refused.
        assert_eq!(
            body["thinking"],
            json!({ "type": "adaptive", "display": "summarized",
                "block_binding": { "prefix_mismatch_behavior": "drop_block" } })
        );
        assert_eq!(body["output_config"], json!({ "effort": "high" }));
        assert!(sends_thinking(&body));
        // Streamed, with the limit unknown: the default.
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn thinking_goes_back_only_while_the_model_thinks() {
        // Turned off: no thinking blocks, no binding.
        let mut info = info("claude-opus-4-8");
        info.disabled = true;
        let off = Plan::new(&info, Some(ReasoningEffort::None), true, &Tweaks::default());
        assert_eq!(off.thinking, Thinking::Disabled);
        let request = body(&chat(), &off);
        assert_eq!(request["thinking"], json!({ "type": "disabled" }));
        assert!(!sends_thinking(&request));
        // Refused by the API: not sent back this time.
        let tweaks = Tweaks {
            no_echo: true,
            ..Tweaks::default()
        };
        let plan = Plan::new(
            &self::info("claude-opus-5-5"),
            Some(ReasoningEffort::High),
            true,
            &tweaks,
        );
        let request = body(&chat(), &plan);
        assert!(!sends_thinking(&request));
        // The thinking settings stay as on every other step.
        assert_eq!(
            request["thinking"]["block_binding"],
            json!({ "prefix_mismatch_behavior": "drop_block" })
        );
        // The binding refused: not said.
        let unbound = Tweaks {
            no_binding: true,
            ..Tweaks::default()
        };
        let plan = Plan::new(
            &self::info("claude-opus-5-5"),
            Some(ReasoningEffort::High),
            true,
            &unbound,
        );
        let request = body(&chat(), &plan);
        assert!(sends_thinking(&request));
        assert_eq!(
            request["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        // A model that thinks by default, with nothing said: said, to carry
        // the binding.
        let mut quiet = self::plan("claude-opus-5-5", None);
        quiet.thinking = Thinking::Default;
        let request = body(&chat(), &quiet);
        assert_eq!(
            request["thinking"],
            json!({ "type": "adaptive", "block_binding": { "prefix_mismatch_behavior": "drop_block" } })
        );
        // A model that doesn't: nothing sent back, nothing said.
        let mut silent = quiet.clone();
        silent.thinks_by_default = false;
        let request = body(&chat(), &silent);
        assert!(request.get("thinking").is_none() && !sends_thinking(&request));
    }

    #[test]
    fn efforts_take_the_models_form() {
        // Adaptive models: adaptive thinking at the effort, shown.
        let p = plan("claude-sonnet-4-6", Some(ReasoningEffort::Low));
        assert_eq!(
            (p.thinking, p.display, p.effort),
            (Thinking::Adaptive, true, Some("low"))
        );
        // Before adaptive thinking: a budget.
        let p = plan("claude-sonnet-4-5", Some(ReasoningEffort::Medium));
        assert_eq!((p.thinking, p.effort), (Thinking::Budget(8_192), None));
        // No thinking: nothing to send.
        let p = plan("claude-3-5-haiku-20241022", Some(ReasoningEffort::High));
        assert_eq!((p.thinking, p.effort), (Thinking::Default, None));
        // The default: nothing, unless the model thinks anyway (shown then).
        let p = plan("claude-opus-4-7", None);
        assert_eq!((p.thinking, p.effort), (Thinking::Default, None));
        let p = plan("claude-opus-5-5", None);
        assert_eq!(
            (p.thinking, p.display, p.effort),
            (Thinking::Adaptive, true, None)
        );
        // "None": off where the model allows it...
        let mut sonnet55 = info("claude-sonnet-5-5");
        sonnet55.disabled = false;
        sonnet55.between_tools = true;
        let p = Plan::new(
            &sonnet55,
            Some(ReasoningEffort::None),
            true,
            &Tweaks::default(),
        );
        assert_eq!(p.thinking, Thinking::BetweenTools);
        // ...else as little as it takes.
        let mut opus55 = info("claude-opus-5-5");
        opus55.disabled = false;
        let p = Plan::new(
            &opus55,
            Some(ReasoningEffort::None),
            true,
            &Tweaks::default(),
        );
        assert_eq!((p.thinking, p.effort), (Thinking::Default, Some("low")));
        let p = plan("claude-opus-4-8", Some(ReasoningEffort::None));
        assert_eq!(p.thinking, Thinking::Disabled);
        // A model that took display no longer gets it.
        let mut no_display = info("claude-opus-4-6");
        no_display.display = false;
        let p = Plan::new(
            &no_display,
            Some(ReasoningEffort::High),
            true,
            &Tweaks::default(),
        );
        assert_eq!(
            body(&json!({ "messages": [] }), &p)["thinking"],
            json!({ "type": "adaptive", "block_binding": { "prefix_mismatch_behavior": "drop_block" } })
        );
    }

    #[test]
    fn max_tokens_is_always_sent_within_the_models_limit() {
        let request = |chat: Value, plan: &Plan| body(&chat, plan)["max_tokens"].clone();
        let mut opus = info("claude-opus-5-5");
        opus.max_output = Some(128_000);
        let streamed = Plan::new(&opus, None, true, &Tweaks::default());
        assert_eq!(request(json!({ "messages": [] }), &streamed), 128_000);
        // Not streamed: less, so the reply doesn't run into the timeouts.
        let whole = Plan::new(&opus, None, false, &Tweaks::default());
        assert_eq!(
            request(json!({ "messages": [] }), &whole),
            UNSTREAMED_MAX_TOKENS
        );
        // The body's own, but not above the limit.
        assert_eq!(
            request(json!({ "max_tokens": 4096, "messages": [] }), &streamed),
            4096
        );
        assert_eq!(
            request(json!({ "max_tokens": 500000, "messages": [] }), &streamed),
            128_000
        );
        // What the context window has room for.
        let tweaks = Tweaks {
            room: Some(3_000),
            ..Tweaks::default()
        };
        let tight = Plan::new(&opus, None, true, &tweaks);
        assert_eq!(request(json!({ "messages": [] }), &tight), 3_000);
        // A budget leaves the reply half of max_tokens, or isn't sent below
        // the minimum.
        let haiku = info("claude-haiku-4-5");
        let p = Plan::new(
            &haiku,
            Some(ReasoningEffort::High),
            true,
            &Tweaks::default(),
        );
        let roomy = body(&json!({ "max_tokens": 64000, "messages": [] }), &p);
        assert_eq!(roomy["thinking"]["budget_tokens"], 16_384);
        let small = body(&json!({ "max_tokens": 4000, "messages": [] }), &p);
        assert_eq!(small["thinking"]["budget_tokens"], 2_000);
        let unstreamed = Plan::new(
            &haiku,
            Some(ReasoningEffort::High),
            false,
            &Tweaks::default(),
        );
        let whole = body(&json!({ "messages": [] }), &unstreamed);
        assert_eq!(whole["thinking"]["budget_tokens"], 8_000);
        let tiny = body(&json!({ "max_tokens": 2000, "messages": [] }), &p);
        assert!(tiny.get("thinking").is_none());
    }

    #[test]
    fn tool_choices_and_ids() {
        let tools = json!([{ "type": "function", "function": { "name": "todo" } }]);
        let with = |choice: Value, tweaks: Tweaks| {
            let chat = json!({ "messages": [], "tools": tools, "tool_choice": choice });
            let plan = Plan::new(&info("claude-opus-4-8"), None, true, &tweaks);
            body(&chat, &plan)["tool_choice"].clone()
        };
        let none = Tweaks::default();
        assert_eq!(with(json!("none"), none.clone()), json!({ "type": "none" }));
        assert_eq!(
            with(json!("required"), none.clone()),
            json!({ "type": "any" })
        );
        assert_eq!(
            with(
                json!({ "type": "function", "function": { "name": "todo" } }),
                none.clone()
            ),
            json!({ "type": "tool", "name": "todo" })
        );
        // Refused: `auto`.
        let unforced = Tweaks {
            no_forced_tool: true,
            ..Tweaks::default()
        };
        assert_eq!(
            with(
                json!({ "type": "function", "function": { "name": "todo" } }),
                unforced
            ),
            json!({ "type": "auto" })
        );
        // A tool without parameters gets an empty object schema.
        let chat = json!({ "messages": [], "tools": tools });
        let request = body(&chat, &plan("claude-opus-4-8", None));
        assert_eq!(
            request["tools"][0]["input_schema"],
            json!({ "type": "object", "properties": {} })
        );
        assert!(request.get("tool_choice").is_none());
        // Ids from other providers, made the API's, still paired.
        let chat = json!({ "messages": [
            { "role": "user", "content": "Go" },
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "functions.read:0", "type": "function", "function": { "name": "read", "arguments": "{\"path\":" } }
            ] },
            { "role": "tool", "tool_call_id": "functions.read:0", "content": "error: bad arguments" }
        ] });
        let request = body(&chat, &plan("claude-opus-4-8", None));
        let call = &request["messages"][1]["content"][0];
        assert_eq!(call["id"], "functions_read_0");
        assert_eq!(call["input"], json!({ "invalid_arguments": "{\"path\":" }));
        assert_eq!(
            request["messages"][2]["content"][0]["tool_use_id"],
            "functions_read_0"
        );
        // No tools offered, calls in the history: declared, none callable.
        assert_eq!(request["tools"][0]["name"], "read");
        assert_eq!(request["tools"].as_array().unwrap().len(), 1);
        assert_eq!(request["tool_choice"], json!({ "type": "none" }));
    }

    #[test]
    fn a_call_id_used_in_every_reply_is_made_unique() {
        let call = |id: &str| {
            json!({ "role": "assistant", "content": "", "tool_calls": [
                { "id": id, "type": "function", "function": { "name": "read", "arguments": "{}" } }
            ] })
        };
        let result =
            |id: &str, text: &str| json!({ "role": "tool", "tool_call_id": id, "content": text });
        let chat = json!({ "messages": [
            { "role": "user", "content": "Go" },
            call("function-call-1"),
            result("function-call-1", "one"),
            call("function-call-1"),
            result("function-call-1", "two"),
            // The same id twice in one reply: paired in order.
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "function-call-1", "type": "function", "function": { "name": "read", "arguments": "{}" } },
                { "id": "function-call-1", "type": "function", "function": { "name": "bash", "arguments": "{}" } }
            ] },
            result("function-call-1", "three"),
            result("function-call-1", "four"),
        ] });
        let request = body(&chat, &plan("claude-opus-4-8", None));
        let pairs: Vec<(String, String)> = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap().clone())
            .filter_map(|b| match b["type"].as_str() {
                Some("tool_use") => {
                    Some(("use".to_string(), b["id"].as_str().unwrap().to_string()))
                }
                Some("tool_result") => Some((
                    b["content"].as_str().unwrap().to_string(),
                    b["tool_use_id"].as_str().unwrap().to_string(),
                )),
                _ => None,
            })
            .collect();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            pairs,
            [
                pair("use", "function-call-1"),
                pair("one", "function-call-1"),
                pair("use", "function-call-1_2"),
                pair("two", "function-call-1_2"),
                pair("use", "function-call-1_3"),
                pair("use", "function-call-1_4"),
                pair("three", "function-call-1_3"),
                pair("four", "function-call-1_4"),
            ]
        );
    }

    #[test]
    fn the_conversation_starts_with_the_user_and_skips_empty_messages() {
        let chat = json!({
            "messages": [
                { "role": "system", "content": "S" },
                { "role": "assistant", "content": "Earlier answer." },
                { "role": "user", "content": "" },
                { "role": "user", "content": "Next?" }
            ],
            "temperature": 0.2,
            "top_p": 0.9,
            "stop": ["END", "  "]
        });
        let request = body(&chat, &plan("claude-opus-4-8", None));
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"][0]["text"], CONTINUED);
        assert_eq!(
            messages[2]["content"],
            json!([{ "type": "text", "text": "Next?" }])
        );
        assert_eq!(request["system"], json!([{ "type": "text", "text": "S" }]));
        assert_eq!(request["temperature"], 0.2);
        assert_eq!(request["top_p"], 0.9);
        assert_eq!(request["stop_sequences"], json!(["END"]));
        let tweaks = Tweaks {
            no_temperature: true,
            no_top_p: true,
            no_stop: true,
            ..Tweaks::default()
        };
        let plain = body(
            &chat,
            &Plan::new(&info("claude-opus-4-8"), None, true, &tweaks),
        );
        for field in ["temperature", "top_p", "stop_sequences"] {
            assert!(plain.get(field).is_none(), "{field}");
        }
    }

    #[test]
    fn breakpoints_on_tool_results_go_on_the_result() {
        let chat = json!({ "messages": [
            { "role": "user", "content": "Go" },
            { "role": "assistant", "content": "", "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{}" } }
            ] },
            { "role": "tool", "tool_call_id": "c1", "content": [
                { "type": "text", "text": "file text", "cache_control": { "type": "ephemeral" } }
            ] }
        ] });
        let request = body(&chat, &plan("claude-opus-4-8", None));
        assert_eq!(
            request["messages"][2]["content"][0],
            json!({ "type": "tool_result", "tool_use_id": "c1", "content": "file text",
                "cache_control": { "type": "ephemeral" } })
        );
    }

    #[test]
    fn refusals_quote_the_levels_they_take() {
        let text = "output_config.effort: input should be 'low', 'medium' or 'high'";
        assert_eq!(offered_levels(text, "max"), ["low", "medium", "high"]);
        assert!(offered_levels("effort is not supported", "high").is_empty());
    }
}

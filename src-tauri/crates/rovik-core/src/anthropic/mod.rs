//! Anthropic's API, at api.anthropic.com, spoken natively. Its
//! OpenAI-compatible endpoint (`/v1/chat/completions`) leaves out what the
//! app relies on: it ignores the reasoning effort, never returns the
//! model's reasoning, drops PDFs, and caches nothing. So chats with a
//! provider on api.anthropic.com go to the Messages API (`/v1/messages`):
//!
//! - Every request there carries what Anthropic requires: the key as
//!   `x-api-key` (no bearer token), `anthropic-version`, and the
//!   `anthropic-beta` a request's features need (`headers`). The model list
//!   and System One requests get them too (`authorize`).
//! - The chat completions body the app builds is translated on the way out
//!   (`request`) and the reply on the way in (chat_request/messages.rs), so
//!   the rest of the app never sees the difference.
//! - What a model takes comes from the Models API, once per model
//!   (`models`): its thinking modes, effort levels and output limit (the
//!   model list also gives its context window, images and PDFs). Without
//!   it, it's judged by the model's name.
//! - The effort goes in the model's own form: adaptive thinking at an
//!   `output_config.effort`, or a thinking budget on models from before
//!   adaptive thinking; "none" turns thinking off as far as the model
//!   allows. The reasoning is shown (`display: "summarized"`) and sent back
//!   with the run's steps, where it was.
//! - Prompt caching: the breakpoints prompt_cache.rs places (system prompt,
//!   history), as Anthropic takes them.
//! - A 400 naming something the model doesn't take (a thinking mode, an
//!   effort level, `display`, a sampling parameter, a forced tool, more
//!   output than it writes) gets the request sent again without it, and the
//!   model is remembered that way (`adjust`).

mod models;
mod request;

pub use models::ModelInfo;

use crate::agent::{ChatStreamEvent, ReasoningEffort, ReasoningSupport};
use crate::chat_request::{Api, ChatError, Clients, ErrorKind, Limits, RequestSpec};
use regex::Regex;
use request::{Plan, Tweaks};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::RequestBuilder;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Anthropic's API host.
pub const HOST: &str = "api.anthropic.com";
/// The API version every request names (the latest).
pub const API_VERSION: &str = "2023-06-01";
/// The beta under which a request says what to do with thinking blocks
/// whose conversation has changed (`thinking.block_binding`).
pub const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// Requests sent again after refusals, at most, for one request.
const MAX_ADJUSTMENTS: usize = 8;

/// API bases that refused `thinking.block_binding` (or its beta): not sent
/// there again while the app runs.
static NO_BINDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn binding_refused(base: &str) -> bool {
    NO_BINDING.lock().is_ok_and(|n| n.contains(base))
}

/// Whether `url` is on Anthropic's API host.
pub fn is_anthropic(url: &str) -> bool {
    reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        })
        .is_some_and(|h| h == HOST)
}

/// The API's base URL for a provider's base URL: as given, or with `/v1`
/// when it names only the host ("https://api.anthropic.com").
pub fn api_base(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    match reqwest::Url::parse(base) {
        Ok(url) if url.path().trim_matches('/').is_empty() => format!("{base}/v1"),
        _ => base.to_string(),
    }
}

/// The headers of a request to Anthropic's API: the key (`x-api-key`), the
/// API version, and the betas `body` (a Messages API body) needs.
pub fn headers(api_key: &str, body: Option<&Value>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let key = api_key.trim();
    if !key.is_empty() {
        if let Ok(mut value) = HeaderValue::from_str(key) {
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static("x-api-key"), value);
        }
    }
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static(API_VERSION),
    );
    let mut betas: Vec<&str> = Vec::new();
    if body.is_some_and(|b| b.pointer("/thinking/block_binding").is_some()) {
        betas.push(BINDING_BETA);
    }
    if !betas.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&betas.join(",")) {
            headers.insert(HeaderName::from_static("anthropic-beta"), value);
        }
    }
    headers
}

/// A request's credentials: Anthropic's headers on its API host, else the
/// key as a bearer token (none without a key).
pub fn authorize(request: RequestBuilder, url: &str, api_key: &str) -> RequestBuilder {
    if is_anthropic(url) {
        return request.headers(headers(api_key, None));
    }
    match api_key.trim() {
        "" => request,
        key => request.bearer_auth(key),
    }
}

/// Keeps what the Models API said of `model` (an entry of the model list),
/// so its first chat doesn't have to ask.
pub fn remember(base_url: &str, model: &Value) -> ModelInfo {
    let info = ModelInfo::from_api(model);
    if let Some(id) = model.get("id").and_then(|i| i.as_str()) {
        models::remember(&api_base(base_url), id, info.clone());
    }
    info
}

/*
 * ---------------------------------------------------------------------------
 * Refusals
 * ---------------------------------------------------------------------------
 */

/// "max_tokens: 64000 > 32000, which is the maximum allowed number of
/// output tokens for …": the model's limit.
static OUTPUT_CAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"max_tokens`?:\s*(\d+)\s*>\s*(\d+)").expect("valid regex"));
/// "input length and `max_tokens` exceed context limit: 197471 + 21333 >
/// 200000": the room the input leaves.
static CONTEXT_ROOM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s*\+\s*(\d+)\s*>\s*(\d+)").expect("valid regex"));
/// Less room than this isn't worth a reply: the conversation is too long.
const MIN_ROOM: u64 = 1_024;

fn output_cap(text: &str) -> Option<u32> {
    let caps = OUTPUT_CAP.captures(text)?;
    caps[2].parse().ok().filter(|n| *n > 0)
}

fn context_room(text: &str) -> Option<u32> {
    if !text.contains("context") {
        return None;
    }
    let caps = CONTEXT_ROOM.captures(text)?;
    let input: u64 = caps[1].parse().ok()?;
    let window: u64 = caps[3].parse().ok()?;
    let room = window.checked_sub(input).filter(|r| *r >= MIN_ROOM)?;
    Some(u32::try_from(room).unwrap_or(u32::MAX))
}

/// What to change after the API refused `sent` (a Messages API body) with
/// `text`: something it named that the model doesn't take, from now on
/// (`info`) or for this request (`tweaks`). Says what, for the log; None
/// when there's nothing to change, and the refusal stands. A refusal for
/// the conversation's length (`context`) can only be about `max_tokens`.
fn adjust(
    text: &str,
    sent: &Value,
    context: bool,
    info: &mut ModelInfo,
    tweaks: &mut Tweaks,
) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let says = |field: &str| crate::agent::mentions_field(&lower, field);
    // More output than the context window has room for.
    if let Some(room) = context_room(&lower) {
        if tweaks.room.is_none_or(|r| room < r) {
            tweaks.room = Some(room);
            return Some("max_tokens (the room the context window leaves)");
        }
    }
    if context {
        return None;
    }
    let at = |pointer: &str| sent.pointer(pointer).and_then(|v| v.as_str());
    // A forced tool (the newest models only take `auto`).
    if !tweaks.no_forced_tool
        && matches!(at("/tool_choice/type"), Some("any" | "tool"))
        && says("tool_choice")
    {
        tweaks.no_forced_tool = true;
        return Some("a forced tool_choice");
    }
    // The setting for thinking whose conversation changed, or its beta.
    if !tweaks.no_binding
        && sent.pointer("/thinking/block_binding").is_some()
        && (says("block_binding") || lower.contains(BINDING_BETA) || says("anthropic-beta"))
    {
        tweaks.no_binding = true;
        return Some("thinking.block_binding");
    }
    // The latest reply doesn't start with its thinking, which a model
    // thinking with a budget wants (its thinking wasn't kept, or not where
    // it was): no thinking this time.
    if !tweaks.no_thinking && lower.contains("must start with a thinking block") {
        tweaks.no_thinking = true;
        return Some("thinking, for a history without its thinking");
    }
    // Thinking sent back that it won't take (a signature it can't check).
    if !tweaks.no_echo
        && request::sends_thinking(sent)
        && (says("signature")
            || says("redacted_thinking")
            || lower.contains("thinking block")
            || lower.contains("thinking` block"))
    {
        tweaks.no_echo = true;
        return Some("the thinking sent back");
    }
    // More output than the model writes.
    if let Some(cap) = output_cap(&lower).filter(|cap| info.max_output != Some(*cap)) {
        info.max_output = Some(cap);
        return Some("max_tokens");
    }
    if sent.pointer("/thinking/display").is_some() && says("display") && info.display {
        info.display = false;
        return Some("thinking.display");
    }
    match at("/thinking/type") {
        Some("adaptive") if says("adaptive") && info.adaptive => {
            info.adaptive = false;
            return Some("adaptive thinking");
        }
        Some("enabled") if (says("enabled") || says("budget_tokens")) && info.budget => {
            info.budget = false;
            return Some("a thinking budget");
        }
        Some("disabled") if says("disabled") && info.disabled => {
            info.disabled = false;
            return Some("thinking turned off");
        }
        Some("between_tools") if says("between_tools") && info.between_tools => {
            info.between_tools = false;
            return Some("thinking between tools");
        }
        _ => {}
    }
    if let Some(level) = at("/output_config/effort") {
        if says("effort") || says("output_config") {
            // The levels it takes, when the refusal lists them; else none.
            info.efforts = request::offered_levels(&lower, level)
                .into_iter()
                .filter(|l| info.efforts.is_empty() || info.efforts.contains(l))
                .collect();
            return Some("output_config.effort");
        }
    }
    let mut sampling = None;
    if !tweaks.no_temperature && sent.get("temperature").is_some() && says("temperature") {
        tweaks.no_temperature = true;
        sampling = Some("temperature");
    }
    if !tweaks.no_top_p && sent.get("top_p").is_some() && says("top_p") {
        tweaks.no_top_p = true;
        sampling = sampling.or(Some("top_p"));
    }
    if sampling.is_some() {
        return sampling;
    }
    if !tweaks.no_stop && sent.get("stop_sequences").is_some() && says("stop_sequences") {
        tweaks.no_stop = true;
        return Some("stop_sequences");
    }
    // Thinking in any form: the model takes none.
    if at("/thinking/type").is_some() && says("thinking") && info.thinking {
        info.thinking = false;
        info.adaptive = false;
        info.budget = false;
        return Some("thinking");
    }
    None
}

/*
 * ---------------------------------------------------------------------------
 * A request
 * ---------------------------------------------------------------------------
 */

/// Sends a chat request (a chat completions `body`, as agent.rs builds it
/// for every provider) to the Messages API of `base_url`, with the effort
/// the user chose in the model's own form. As `agent::run_with_reasoning`:
/// what the request showed about the effort comes back for the frontend to
/// keep with the model: taken in whichever form it asked for (`formats`;
/// none asked, the effort isn't sent), or not at all (a model that doesn't
/// think and takes no effort).
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: Clients<'_>,
    base_url: &str,
    api_key: &str,
    body: &Value,
    streaming: bool,
    limits: Limits,
    effort: Option<ReasoningEffort>,
    formats: &[String],
    chat_id: Option<&str>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> (Result<(), ChatError>, Option<ReasoningSupport>) {
    let base = api_base(base_url);
    let url = format!("{base}/messages");
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let effort = effort.filter(|_| !formats.is_empty());
    let http = client.get().await;
    let reported = tokio::select! {
        _ = crate::chat_request::cancelled(cancel) => return (Err(ChatError::stopped()), None),
        info = models::info(&http, &base, api_key, &model) => info,
    };
    let mut info = reported.unwrap_or_else(|| ModelInfo::guess(&model));
    let known = info.clone();
    let mut tweaks = Tweaks {
        no_binding: binding_refused(&base),
        ..Tweaks::default()
    };
    let mut adjustments = 0;
    let result = loop {
        let plan = Plan::new(&info, effort, streaming, &tweaks);
        let request = request::body(body, &plan);
        let spec = RequestSpec {
            client,
            url: &url,
            api_key,
            body: &request,
            streaming,
            limits,
            api: Api::Messages,
            chat_id,
        };
        let result = crate::chat_request::run(&spec, cancel, emit).await;
        let refused = match &result {
            Err(e) if !e.partial && adjustments < MAX_ADJUSTMENTS => match e.kind {
                ErrorKind::BadRequest | ErrorKind::ContextLength => Some(e),
                _ => None,
            },
            _ => None,
        };
        let Some(error) = refused else {
            break result;
        };
        let text = format!(
            "{} {}",
            error.message,
            error.details.as_deref().unwrap_or("")
        );
        let context = error.kind == ErrorKind::ContextLength;
        match adjust(&text, &request, context, &mut info, &mut tweaks) {
            Some(what) => {
                adjustments += 1;
                eprintln!(
                    "anthropic: {model} refused {what}; sending the request again without it"
                );
            }
            None => break result,
        }
    };
    // What the refusals showed holds for the model's next requests.
    if info != known {
        models::remember(&base, &model, info.clone());
    }
    if tweaks.no_binding {
        if let Ok(mut refused) = NO_BINDING.lock() {
            refused.insert(base.clone());
        }
    }
    if effort.is_none() || result.is_err() {
        return (result, None);
    }
    let plan = Plan::new(&info, effort, streaming, &tweaks);
    let taken = plan.thinking != request::Thinking::Default || plan.effort.is_some();
    let support = if taken {
        ReasoningSupport {
            accepted: formats.to_vec(),
            ..Default::default()
        }
    } else {
        ReasoningSupport {
            rejected: formats.to_vec(),
            ..Default::default()
        }
    };
    (result, Some(support))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn only_anthropics_api_host() {
        assert!(is_anthropic("https://api.anthropic.com/v1"));
        assert!(is_anthropic(" HTTPS://API.Anthropic.com./v1/messages "));
        assert!(!is_anthropic("https://api.anthropic.com.evil.com/v1"));
        assert!(!is_anthropic("https://evil.com/api.anthropic.com/v1"));
        assert!(!is_anthropic("https://openrouter.ai/api/v1"));
        assert!(!is_anthropic("not a url"));
    }

    #[test]
    fn the_base_gets_v1_when_it_names_only_the_host() {
        assert_eq!(
            api_base("https://api.anthropic.com"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            api_base("https://api.anthropic.com/"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            api_base(" https://api.anthropic.com/v1/ "),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(api_base("http://127.0.0.1:9/v1"), "http://127.0.0.1:9/v1");
    }

    #[test]
    fn requests_carry_the_key_version_and_betas() {
        let h = headers(" sk-ant-1 ", None);
        assert_eq!(h.get("x-api-key").unwrap(), "sk-ant-1");
        assert!(h.get("x-api-key").unwrap().is_sensitive());
        assert_eq!(h.get("anthropic-version").unwrap(), API_VERSION);
        assert!(h.get("anthropic-beta").is_none());
        assert!(h.get("authorization").is_none());
        let bound = json!({ "thinking": { "type": "adaptive", "block_binding": { "prefix_mismatch_behavior": "drop_block" } } });
        assert_eq!(
            headers("k", Some(&bound)).get("anthropic-beta").unwrap(),
            BINDING_BETA
        );
        // No key: no header for it.
        assert!(headers("", None).get("x-api-key").is_none());

        let client = reqwest::Client::new();
        let built = |url: &str, key: &str| {
            authorize(client.get(url), url, key)
                .build()
                .unwrap()
                .headers()
                .clone()
        };
        let anthropic = built("https://api.anthropic.com/v1/models", "k");
        assert_eq!(anthropic.get("x-api-key").unwrap(), "k");
        assert!(anthropic.get("authorization").is_none());
        let other = built("https://api.openai.com/v1/models", "k");
        assert_eq!(other.get("authorization").unwrap(), "Bearer k");
        assert!(other.get("x-api-key").is_none() && other.get("anthropic-version").is_none());
        assert!(built("http://localhost:11434/v1/models", " ")
            .get("authorization")
            .is_none());
    }

    #[test]
    fn refusals_change_what_is_sent() {
        let adaptive = json!({ "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": "high" }, "temperature": 0.2, "top_p": 0.9 });
        let run = |text: &str, sent: &Value, info: &mut ModelInfo, tweaks: &mut Tweaks| {
            adjust(text, sent, false, info, tweaks)
        };
        let mut info = ModelInfo::guess("claude-opus-4-1");
        let mut tweaks = Tweaks::default();
        // More output than the model writes: its limit, remembered.
        let text = "max_tokens: 64000 > 32000, which is the maximum allowed number of output tokens for claude-opus-4-1-20250805";
        assert!(run(text, &adaptive, &mut info, &mut tweaks).is_some());
        assert_eq!(info.max_output, Some(32_000));
        assert!(
            run(text, &adaptive, &mut info, &mut tweaks).is_none(),
            "once"
        );

        let mut info = ModelInfo::guess("claude-opus-4-6");
        // `display` isn't taken: thinking without it.
        let text = "thinking.adaptive.display: Extra inputs are not permitted";
        assert!(run(text, &adaptive, &mut info, &mut tweaks).is_some());
        assert!(!info.display && info.adaptive);
        // Adaptive thinking isn't: a budget from then on.
        let text = "thinking.type: Input tag 'adaptive' found using 'type' does not match any of the expected tags: 'disabled', 'enabled'";
        assert!(run(text, &adaptive, &mut info, &mut tweaks).is_some());
        assert!(!info.adaptive);
        // The effort isn't.
        let text = "output_config.effort: Extra inputs are not permitted";
        assert!(run(text, &adaptive, &mut info, &mut tweaks).is_some());
        assert!(info.efforts.is_empty());
        // Sampling parameters thinking doesn't allow: both left out, for
        // this request; thinking stays.
        let text = "`temperature` and `top_p` cannot both be specified for this model, and temperature may only be set to 1 when thinking is enabled.";
        assert_eq!(
            run(text, &adaptive, &mut info, &mut tweaks),
            Some("temperature")
        );
        assert!(tweaks.no_temperature && tweaks.no_top_p && info.thinking);

        // A budget a newer model doesn't take.
        let budget = json!({ "thinking": { "type": "enabled", "budget_tokens": 8192 } });
        let mut info = ModelInfo::guess("claude-opus-4-6");
        let text = "thinking.type.enabled is not supported for this model. Use thinking.type.adaptive and output_config.effort instead.";
        assert!(run(text, &budget, &mut info, &mut Tweaks::default()).is_some());
        assert!(!info.budget && info.adaptive);

        // Turning thinking off, where the model can't.
        let off = json!({ "thinking": { "type": "disabled" } });
        let mut info = ModelInfo::guess("claude-opus-5-5");
        let text = "thinking.type: 'disabled' is not supported for this model.";
        assert!(run(text, &off, &mut info, &mut Tweaks::default()).is_some());
        assert!(!info.disabled);
        let plan = Plan::new(&info, Some(ReasoningEffort::None), true, &Tweaks::default());
        assert_eq!(plan.effort, Some("low"));

        // A forced tool.
        let forced = json!({ "tool_choice": { "type": "tool", "name": "todo" } });
        let mut tweaks = Tweaks::default();
        let text = "tool_choice: type \"tool\" and \"any\" are not supported for this model.";
        assert!(run(text, &forced, &mut info, &mut tweaks).is_some());
        assert!(tweaks.no_forced_tool);

        // Thinking sent back with a signature it can't check.
        let echoed = json!({ "thinking": { "type": "adaptive" }, "messages": [
            { "role": "assistant", "content": [{ "type": "thinking", "thinking": "", "signature": "x" }] }
        ] });
        let mut tweaks = Tweaks::default();
        let text = "messages.1.content.0: Invalid `signature` in `thinking` block";
        assert!(run(text, &echoed, &mut info, &mut tweaks).is_some());
        assert!(tweaks.no_echo);

        // A reply whose thinking isn't at its start: thinking off this time.
        let mut tweaks = Tweaks::default();
        let text = "messages.3.content.0.type: Expected `thinking` or `redacted_thinking`, but found `tool_use`. When `thinking` is enabled, a final `assistant` message must start with a thinking block (preceeding the lastmost set of `tool_use` and `tool_result` blocks).";
        assert_eq!(
            run(text, &echoed, &mut info, &mut tweaks),
            Some("thinking, for a history without its thinking")
        );
        assert!(tweaks.no_thinking && !tweaks.no_echo);
        let plan = Plan::new(&info, Some(ReasoningEffort::High), true, &tweaks);
        assert!(!plan.thinks_by_default && plan.effort.is_none());

        // Nothing it names that can change: the refusal stands.
        let text = "messages: roles must alternate";
        assert!(run(text, &adaptive, &mut info, &mut Tweaks::default()).is_none());
    }

    #[test]
    fn a_reply_too_long_for_the_window_gets_the_room_left() {
        let mut info = ModelInfo::guess("claude-sonnet-4");
        let mut tweaks = Tweaks::default();
        let text = "input length and `max_tokens` exceed context limit: 197000 + 64000 > 200000, decrease input length or `max_tokens` and try again";
        let sent = json!({});
        assert!(adjust(text, &sent, true, &mut info, &mut tweaks).is_some());
        assert_eq!(tweaks.room, Some(3_000));
        // No room worth a reply: too long, as it says.
        let full = "input length and `max_tokens` exceed context limit: 199500 + 64000 > 200000";
        assert!(adjust(full, &sent, true, &mut info, &mut Tweaks::default()).is_none());
        // "Prompt is too long": nothing to change.
        let long = "prompt is too long: 250000 tokens > 200000 maximum";
        assert!(adjust(long, &sent, true, &mut info, &mut Tweaks::default()).is_none());
    }

    /// One request the mock saw: its request line and headers, and body.
    struct Seen {
        head: String,
        body: Value,
    }

    /// A local Anthropic API: `GET …/models/{id}` answers `model_info`, and
    /// `POST …/messages` the next of `replies` (raw HTTP responses).
    async fn mock(
        model_info: Option<Value>,
        replies: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut replies = replies.into_iter();
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut raw = Vec::new();
                let mut buf = vec![0u8; 65536];
                // Read the head, then as much body as it announces.
                loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        if n == 0 {
                            break;
                        }
                        continue;
                    };
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + length || n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                log.lock().unwrap().push(Seen {
                    head: head.to_string(),
                    body: serde_json::from_str(body).unwrap_or(Value::Null),
                });
                let reply = if head.starts_with("GET ") {
                    match &model_info {
                        Some(info) => {
                            let body = info.to_string();
                            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                        }
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    }
                } else {
                    replies.next().unwrap_or_else(|| "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string())
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (base, seen)
    }

    fn sse(events: &[Value]) -> String {
        let mut out = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        );
        for event in events {
            out.push_str(&format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap_or("")
            ));
        }
        out
    }

    fn refusal(message: &str) -> String {
        let body = json!({ "type": "error", "error": { "type": "invalid_request_error", "message": message } }).to_string();
        format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn hello() -> String {
        sse(&[
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 9, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "Greet." } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Hello!" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
            json!({ "type": "message_stop" }),
        ])
    }

    async fn send(
        base: &str,
        model: &str,
        effort: Option<ReasoningEffort>,
    ) -> (
        Result<(), ChatError>,
        Option<ReasoningSupport>,
        Vec<ChatStreamEvent>,
    ) {
        let client = reqwest::Client::new();
        let body = json!({ "model": model, "stream": true, "messages": [
            { "role": "system", "content": "Be brief." },
            { "role": "user", "content": "Hi" }
        ] });
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        std::mem::forget(tx);
        let limits = Limits {
            first_byte: std::time::Duration::from_secs(2),
            idle: std::time::Duration::from_secs(2),
            overall: std::time::Duration::from_secs(5),
        };
        let mut events = Vec::new();
        let formats = vec!["reasoning_effort".to_string(), "thinking".to_string()];
        let (result, support) = run(
            Clients::Only(&client),
            base,
            "sk-ant-test",
            &body,
            true,
            limits,
            effort,
            &formats,
            None,
            &mut rx,
            &mut |e| events.push(e),
        )
        .await;
        (result, support, events)
    }

    #[tokio::test]
    async fn a_chat_goes_to_the_messages_api_with_anthropics_headers() {
        let info = json!({ "id": "claude-test-5-5", "max_tokens": 128000, "max_input_tokens": 1000000,
            "capabilities": { "thinking": { "supported": true, "types": {
                "adaptive": { "supported": true }, "enabled": { "supported": false },
                "disabled": { "supported": false }, "between_tools": { "supported": false } } },
                "effort": { "supported": true, "low": { "supported": true }, "medium": { "supported": true },
                    "high": { "supported": true }, "xhigh": { "supported": true }, "max": { "supported": true } } } });
        let (base, seen) = mock(Some(info), vec![hello()]).await;
        let (result, support, events) =
            send(&base, "claude-test-5-5", Some(ReasoningEffort::High)).await;
        result.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the model's info, then the message");
        assert!(seen[0].head.starts_with("GET /v1/models/claude-test-5-5 "));
        let post = &seen[1];
        assert!(post.head.starts_with("POST /v1/messages "), "{}", post.head);
        let head = post.head.to_ascii_lowercase();
        assert!(head.contains("\r\nx-api-key: sk-ant-test"), "{head}");
        assert!(head.contains("\r\nanthropic-version: 2023-06-01"));
        assert!(!head.contains("\r\nauthorization:"));
        // The thinking settings say what to do with thinking whose
        // conversation changed, under their beta.
        assert!(head.contains(&format!("\r\nanthropic-beta: {BINDING_BETA}")));
        assert_eq!(
            post.body["thinking"]["block_binding"]["prefix_mismatch_behavior"],
            "drop_block"
        );
        // The model's own effort; its whole output limit (streamed).
        assert_eq!(post.body["thinking"]["type"], "adaptive");
        assert_eq!(post.body["thinking"]["display"], "summarized");
        assert_eq!(post.body["output_config"]["effort"], "high");
        assert_eq!(post.body["max_tokens"], 128000);
        assert_eq!(post.body["system"][0]["text"], "Be brief.");
        // The reply, its reasoning, and the effort reported taken.
        let said: String = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Delta { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(said, "Hello!");
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatStreamEvent::Reasoning { content } if content == "Greet.")));
        assert!(events.iter().any(|e| matches!(e, ChatStreamEvent::Done)));
        assert_eq!(support.unwrap().accepted, ["reasoning_effort", "thinking"]);
    }

    #[tokio::test]
    async fn refused_forms_are_dropped_and_remembered() {
        // No model info: judged by the name (adaptive, then a budget).
        let (base, seen) = mock(
            None,
            vec![
                refusal("thinking.type: Input tag 'adaptive' found using 'type' does not match any of the expected tags: 'disabled', 'enabled'"),
                hello(),
                hello(),
            ],
        )
        .await;
        let (result, support, _) =
            send(&base, "claude-mystery", Some(ReasoningEffort::Medium)).await;
        result.unwrap();
        assert!(support.is_some_and(|s| !s.accepted.is_empty()));
        {
            let seen = seen.lock().unwrap();
            let posts: Vec<&Seen> = seen.iter().filter(|s| s.head.starts_with("POST")).collect();
            assert_eq!(posts.len(), 2);
            assert_eq!(posts[0].body["thinking"]["type"], "adaptive");
            assert_eq!(posts[1].body["thinking"]["type"], "enabled");
            assert_eq!(posts[1].body["thinking"]["budget_tokens"], 8192);
        }
        // The next request starts where the last one ended.
        let (result, _, _) = send(&base, "claude-mystery", Some(ReasoningEffort::Medium)).await;
        result.unwrap();
        let seen = seen.lock().unwrap();
        let last = seen.iter().rfind(|s| s.head.starts_with("POST")).unwrap();
        assert_eq!(last.body["thinking"]["type"], "enabled");
    }

    #[tokio::test]
    async fn a_refused_binding_beta_is_not_sent_again() {
        let info = json!({ "id": "claude-bound-5", "capabilities": { "thinking": { "supported": true,
            "types": { "adaptive": { "supported": true } } } } });
        let refused = format!(
            "Unexpected value(s) `{BINDING_BETA}` for the `anthropic-beta` header. Please consult our documentation at docs.claude.com or try again without the header."
        );
        let (base, seen) = mock(Some(info), vec![refusal(&refused), hello(), hello()]).await;
        let (result, _, _) = send(&base, "claude-bound-5", Some(ReasoningEffort::High)).await;
        result.unwrap();
        let (result, _, _) = send(&base, "claude-bound-5", Some(ReasoningEffort::High)).await;
        result.unwrap();
        let seen = seen.lock().unwrap();
        let posts: Vec<&Seen> = seen.iter().filter(|s| s.head.starts_with("POST")).collect();
        assert_eq!(posts.len(), 3);
        let bound: Vec<bool> = posts
            .iter()
            .map(|p| p.body["thinking"].get("block_binding").is_some())
            .collect();
        assert_eq!(bound, [true, false, false]);
        assert!(!posts[2]
            .head
            .to_ascii_lowercase()
            .contains("anthropic-beta"));
    }

    #[tokio::test]
    async fn a_model_without_thinking_reports_the_effort_not_taken() {
        let info = json!({ "id": "claude-plain", "capabilities": {
            "thinking": { "supported": false, "types": { "disabled": { "supported": true } } },
            "effort": { "supported": false } } });
        let (base, seen) = mock(Some(info), vec![hello()]).await;
        let (result, support, _) = send(&base, "claude-plain", Some(ReasoningEffort::High)).await;
        result.unwrap();
        let support = support.unwrap();
        assert!(support.accepted.is_empty());
        assert_eq!(support.rejected, ["reasoning_effort", "thinking"]);
        let seen = seen.lock().unwrap();
        let post = seen.iter().find(|s| s.head.starts_with("POST")).unwrap();
        assert!(post.body.get("thinking").is_none() && post.body.get("output_config").is_none());
    }
}

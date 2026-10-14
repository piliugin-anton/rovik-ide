//! Prompt caching: keeping the start of every request identical to the
//! previous one, so the provider serves it from its cache instead of
//! reading it again (cached input costs a fraction and comes back faster).
//!
//! A request has two zones. Zone 1, the cacheable prefix: the system
//! prompt, the tool schemas and the history, append-only from one step to
//! the next (agent.rs builds it; chat_db/history.rs replaces and trims in
//! batches and with hysteresis, so the cut doesn't move every step; user
//! messages are sent as they were first sent). Zone 2, the tail: what
//! changes between steps (the repository map, the focus note, context
//! notes) rides on the request's last message, after everything cacheable.
//!
//! On top of that shape, what each provider wants:
//! - OpenAI (and Azure OpenAI) caches prefixes of 1024+ tokens on its own;
//!   `prompt_cache_key` routes a chat's requests to the same cache, which
//!   raises the hit rate and is required for the newest models' matching.
//! - OpenRouter caches on its own for most providers, but Anthropic models
//!   need `cache_control` breakpoints on content parts: one on the system
//!   prompt (the fixed zone, with the tools before it) and one on the last
//!   message, from which the provider looks back for the previous step's
//!   cached prefix. Four at most; providers without breakpoints ignore them.
//! - Anthropic's own OpenAI-compatible endpoint doesn't cache at all;
//!   Gemini, DeepSeek and most others cache on their own, with no hints.
//!
//! `cached_tokens` reads what a provider reports, for the run log and the
//! headless report: the number to watch when changing how requests are
//! built.

use serde_json::{json, Value};

/// Providers with cache hints of their own, by their base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    OpenAi,
    OpenRouter,
    Other,
}

/// The provider a base URL points at, as far as caching is concerned.
pub fn host(base_url: &str) -> Host {
    let url = base_url.trim().to_ascii_lowercase();
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host == "api.openai.com" || host.ends_with(".openai.azure.com") {
        Host::OpenAi
    } else if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        Host::OpenRouter
    } else {
        Host::Other
    }
}

/// Adds the provider's cache hints to a Chat Completions body: the cache
/// key (the chat's id) for OpenAI, breakpoints for OpenRouter. Others get
/// the body as is.
pub fn apply(body: &mut Value, base_url: &str, cache_key: Option<&str>) {
    match host(base_url) {
        Host::OpenAi => {
            if let Some(key) = cache_key.map(str::trim).filter(|k| !k.is_empty()) {
                body["prompt_cache_key"] = json!(key);
            }
        }
        Host::OpenRouter => mark_breakpoints(body),
        Host::Other => {}
    }
}

/// A breakpoint on the system prompt and one on the last message.
fn mark_breakpoints(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(first) = messages.first_mut().filter(|m| m["role"] == "system") {
        mark(first);
    }
    if let Some(last) = messages.last_mut().filter(|m| m["role"] != "system") {
        mark(last);
    }
}

/// Puts a breakpoint on the message's last text part. A string content
/// becomes a one-part array: breakpoints go on parts.
fn mark(message: &mut Value) {
    let breakpoint = json!({ "type": "ephemeral" });
    let content = &mut message["content"];
    match content {
        Value::String(text) => {
            *content = json!([{ "type": "text", "text": text, "cache_control": breakpoint }]);
        }
        Value::Array(parts) => {
            if let Some(part) = parts.iter_mut().rev().find(|p| p["type"] == "text") {
                part["cache_control"] = breakpoint;
            }
        }
        _ => {}
    }
}

/// Prompt tokens the provider served from its cache, as it reports them:
/// `prompt_tokens_details.cached_tokens` (OpenAI, OpenRouter, Azure and
/// gateways in their form), `input_tokens_details.cached_tokens` (the
/// Responses API), `cache_read_input_tokens` (Anthropic's form, passed
/// through by some gateways), `prompt_cache_hit_tokens` (DeepSeek).
pub fn cached_tokens(usage: &Value) -> Option<u64> {
    [
        "/prompt_tokens_details/cached_tokens",
        "/input_tokens_details/cached_tokens",
        "/cache_read_input_tokens",
        "/prompt_cache_hit_tokens",
    ]
    .iter()
    .find_map(|path| {
        usage
            .pointer(path)
            .and_then(crate::agent_run::log::token_count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_told_by_their_base_url() {
        assert_eq!(host("https://api.openai.com/v1"), Host::OpenAi);
        assert_eq!(
            host("https://my-res.openai.azure.com/openai/v1/"),
            Host::OpenAi
        );
        assert_eq!(host("https://openrouter.ai/api/v1"), Host::OpenRouter);
        assert_eq!(host("HTTPS://OpenRouter.ai/api/v1/"), Host::OpenRouter);
        assert_eq!(host("https://api.anthropic.com/v1/"), Host::Other);
        assert_eq!(host("http://localhost:11434/v1"), Host::Other);
        assert_eq!(host("https://evil.com/api.openai.com/v1"), Host::Other);
        assert_eq!(host("https://api.openai.com.evil.com/v1"), Host::Other);
        assert_eq!(host(""), Host::Other);
    }

    fn body() -> Value {
        json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "Be useful." },
                { "role": "user", "content": "Hi" },
                { "role": "assistant", "content": "", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{}" } }] },
                { "role": "tool", "tool_call_id": "c1", "content": "file text" }
            ]
        })
    }

    #[test]
    fn openai_gets_the_chats_id_as_cache_key() {
        let mut b = body();
        apply(&mut b, "https://api.openai.com/v1", Some("chat-1"));
        assert_eq!(b["prompt_cache_key"], "chat-1");
        assert_eq!(b["messages"], body()["messages"], "messages untouched");

        let mut none = body();
        apply(&mut none, "https://api.openai.com/v1", Some("  "));
        assert!(none.get("prompt_cache_key").is_none());
        apply(&mut none, "https://api.openai.com/v1", None);
        assert!(none.get("prompt_cache_key").is_none());
    }

    #[test]
    fn openrouter_gets_breakpoints_on_the_system_prompt_and_the_last_message() {
        let mut b = body();
        apply(&mut b, "https://openrouter.ai/api/v1", Some("chat-1"));
        assert!(b.get("prompt_cache_key").is_none());
        let messages = b["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"],
            json!([{ "type": "text", "text": "Be useful.", "cache_control": { "type": "ephemeral" } }])
        );
        assert_eq!(messages[1]["content"], "Hi", "the middle is untouched");
        assert_eq!(messages[2]["content"], "");
        assert_eq!(
            messages[3]["content"],
            json!([{ "type": "text", "text": "file text", "cache_control": { "type": "ephemeral" } }])
        );
        assert_eq!(messages[3]["tool_call_id"], "c1", "other fields stay");
    }

    #[test]
    fn a_breakpoint_goes_on_the_last_text_part_of_a_message_with_images() {
        let mut b = json!({ "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "see" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA==" } },
                { "type": "text", "text": "tail" }
            ] }
        ]});
        apply(&mut b, "https://openrouter.ai/api/v1", None);
        let parts = b["messages"][0]["content"].as_array().unwrap();
        assert!(parts[0].get("cache_control").is_none());
        assert!(parts[1].get("cache_control").is_none());
        assert_eq!(parts[2]["cache_control"], json!({ "type": "ephemeral" }));
        // A lone message that isn't a system prompt: one breakpoint, not two.
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn other_providers_get_no_hints() {
        for url in [
            "https://api.anthropic.com/v1/",
            "http://localhost:11434/v1",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        ] {
            let mut b = body();
            apply(&mut b, url, Some("chat-1"));
            assert_eq!(b, body(), "{url}");
        }
    }

    #[test]
    fn cached_tokens_in_each_providers_form() {
        let shapes = [
            (
                json!({ "prompt_tokens": 10, "prompt_tokens_details": { "cached_tokens": 8 } }),
                Some(8),
            ),
            (
                json!({ "input_tokens": 10, "input_tokens_details": { "cached_tokens": 7.0 } }),
                Some(7),
            ),
            (
                json!({ "cache_read_input_tokens": 6, "cache_creation_input_tokens": 2 }),
                Some(6),
            ),
            (
                json!({ "prompt_cache_hit_tokens": 5, "prompt_cache_miss_tokens": 5 }),
                Some(5),
            ),
            (
                json!({ "prompt_tokens": 10, "prompt_tokens_details": {} }),
                None,
            ),
            (
                json!({ "prompt_tokens": 10, "prompt_tokens_details": { "cached_tokens": -1 } }),
                None,
            ),
            (json!({ "prompt_tokens": 10 }), None),
            (json!(null), None),
        ];
        for (usage, expected) in shapes {
            assert_eq!(cached_tokens(&usage), expected, "{usage}");
        }
    }
}

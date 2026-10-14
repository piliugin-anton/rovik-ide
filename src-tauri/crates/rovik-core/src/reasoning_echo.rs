// src-tauri/src/reasoning_echo.rs
//
// Sending a model's reasoning back to it. A model that called a tool picks
// its reasoning up again from the previous step when that step's reasoning
// comes with the history; some refuse the request without it (Anthropic
// with thinking, DeepSeek in thinking mode). The forms:
// - `reasoning_details` (OpenRouter and gateways that follow it): an array
//   of entries (`reasoning.text` with an Anthropic `signature`,
//   `reasoning.summary`, `reasoning.encrypted`), sent back whole, in order,
//   unchanged;
// - `reasoning_content` (DeepSeek, vLLM, many gateways; OpenRouter takes it
//   as an alias of `reasoning`): the reasoning text;
// - on the Responses API, reasoning output items (`id`, `summary`,
//   `encrypted_content`), kept here as `reasoning_details` entries in
//   OpenRouter's form for OpenAI (`format: "openai-responses-v1"`) and
//   turned back into items there (chat_request/responses.rs).
//
// What is sent back, and for which messages: chat_db/history.rs
// (`attach_reasoning`). A server that refuses the fields gets the request
// again without them, and isn't sent them again until the app restarts.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// The format of Responses API reasoning, as OpenRouter names it.
pub const RESPONSES_FORMAT: &str = "openai-responses-v1";

/// The fields an assistant message carries its reasoning back in.
pub const FIELDS: [&str; 2] = ["reasoning_details", "reasoning_content"];

static REJECTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Whether reasoning may be sent back to this endpoint.
pub fn wanted(url: &str) -> bool {
    REJECTED.lock().map_or(true, |r| !r.contains(url))
}

/// The endpoint refused reasoning in its requests.
pub fn rejected(url: &str) {
    if let Ok(mut r) = REJECTED.lock() {
        r.insert(url.to_string());
    }
}

/// DeepSeek in thinking mode refuses a request in which an earlier
/// assistant message with tool calls lacks its `reasoning_content`, in any
/// later turn, not only the current one.
pub fn is_deepseek(model: &str) -> bool {
    model.to_ascii_lowercase().contains("deepseek")
}

/// Removes reasoning sent back from a chat request body (and the Responses
/// API's request for encrypted reasoning). True when there was any.
pub fn strip(body: &mut Value) -> bool {
    let mut removed = false;
    for message in body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .into_iter()
        .flatten()
    {
        if let Some(message) = message.as_object_mut() {
            for field in FIELDS {
                removed |= message.remove(field).is_some();
            }
        }
    }
    removed
}

/// Whether a request body sends any reasoning back.
pub fn sends_any(body: &Value) -> bool {
    body.get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|m| FIELDS.iter().any(|f| m.get(*f).is_some()))
        })
}

/// Adds one streamed `reasoning_details` fragment to the entries so far.
///
/// Streams don't keep `index` apart reliably: every fragment may say 0, and
/// a summary may come one fragment per delta. So a fragment continues the
/// last entry when it has the same type, format and index, the same id (if
/// both have one), and that entry isn't finished (a signed text, or an
/// encrypted blob); its `text` / `summary` are appended, and its other
/// fields (`signature`, `data`, `id`) replace the entry's. Otherwise it
/// starts a new entry. What's stored is the whole entry, never the fragments.
pub fn merge(details: &mut Vec<Value>, fragment: &Value) {
    let Some(fragment) = fragment.as_object() else {
        return;
    };
    let field =
        |v: &serde_json::Map<String, Value>, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let continues = details
        .last()
        .and_then(|l| l.as_object())
        .is_some_and(|last| {
            let same = ["type", "format", "index"]
                .iter()
                .all(|k| field(last, k) == field(fragment, k));
            let ids = match (last.get("id"), fragment.get("id")) {
                (Some(Value::String(a)), Some(Value::String(b)))
                    if !a.is_empty() && !b.is_empty() =>
                {
                    a == b
                }
                _ => true,
            };
            let finished = match last.get("type").and_then(|t| t.as_str()) {
                Some("reasoning.text") => has_text(last, "signature") && has_text(fragment, "text"),
                Some("reasoning.encrypted") => has_text(last, "data") && has_text(fragment, "data"),
                _ => false,
            };
            same && ids && !finished
        });
    if !continues {
        details.push(Value::Object(fragment.clone()));
        return;
    }
    let Some(last) = details.last_mut().and_then(|l| l.as_object_mut()) else {
        return;
    };
    for (key, value) in fragment {
        match (key.as_str(), value) {
            ("text" | "summary", Value::String(more)) => {
                let joined = format!(
                    "{}{more}",
                    last.get(key).and_then(|v| v.as_str()).unwrap_or("")
                );
                last.insert(key.clone(), json!(joined));
            }
            (_, Value::Null) => {}
            (_, Value::String(s)) if s.is_empty() => {}
            _ => {
                last.insert(key.clone(), value.clone());
            }
        }
    }
}

fn has_text(entry: &serde_json::Map<String, Value>, key: &str) -> bool {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// A Responses API reasoning output item, as `reasoning_details` entries.
pub fn from_responses_item(item: &Value) -> Vec<Value> {
    let id = item.get("id").cloned().unwrap_or(Value::Null);
    let mut out = Vec::new();
    for part in item
        .get("summary")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            out.push(json!({
                "type": "reasoning.summary", "summary": text, "id": id,
                "format": RESPONSES_FORMAT, "index": out.len(),
            }));
        }
    }
    if let Some(data) = item
        .get("encrypted_content")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
    {
        out.push(json!({
            "type": "reasoning.encrypted", "data": data, "id": id,
            "format": RESPONSES_FORMAT, "index": out.len(),
        }));
    }
    out
}

/// The Responses API reasoning items in `reasoning_details` (entries of
/// other formats are for other APIs), in order, one per id.
pub fn to_responses_items(details: &[Value]) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    for entry in details
        .iter()
        .filter(|e| e.get("format").and_then(|f| f.as_str()) == Some(RESPONSES_FORMAT))
    {
        let id = entry.get("id").cloned().unwrap_or(Value::Null);
        if items.last().is_none_or(|i| i["id"] != id) {
            items.push(json!({ "type": "reasoning", "id": id, "summary": [] }));
        }
        let Some(item) = items.last_mut() else {
            continue;
        };
        match entry.get("type").and_then(|t| t.as_str()) {
            Some("reasoning.summary") => {
                let text = entry.get("summary").cloned().unwrap_or(json!(""));
                if let Some(summary) = item["summary"].as_array_mut() {
                    summary.push(json!({ "type": "summary_text", "text": text }));
                }
            }
            Some("reasoning.encrypted") => {
                item["encrypted_content"] = entry.get("data").cloned().unwrap_or(Value::Null)
            }
            _ => {}
        }
    }
    // An item without an id can't be resumed.
    items.retain(|i| i["id"].as_str().is_some_and(|id| !id.is_empty()));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(fragments: &[Value]) -> Vec<Value> {
        let mut details = Vec::new();
        for f in fragments {
            merge(&mut details, f);
        }
        details
    }

    #[test]
    fn streamed_text_fragments_make_one_signed_entry() {
        let f = |v: Value| v;
        let details = merged(&[
            f(
                json!({ "type": "reasoning.text", "text": "Let me ", "format": "anthropic-claude-v1", "index": 0 }),
            ),
            f(
                json!({ "type": "reasoning.text", "text": "check.", "format": "anthropic-claude-v1", "index": 0 }),
            ),
            f(
                json!({ "type": "reasoning.text", "text": "", "signature": "sig", "format": "anthropic-claude-v1", "index": 0 }),
            ),
        ]);
        assert_eq!(
            details,
            [
                json!({ "type": "reasoning.text", "text": "Let me check.", "signature": "sig", "format": "anthropic-claude-v1", "index": 0 })
            ]
        );
    }

    #[test]
    fn blocks_that_all_say_index_0_stay_apart() {
        let text = |t: &str| json!({ "type": "reasoning.text", "text": t, "format": "anthropic-claude-v1", "index": 0 });
        let details = merged(&[
            text("first"),
            json!({ "type": "reasoning.text", "signature": "s1", "format": "anthropic-claude-v1", "index": 0 }),
            // New text after a signature: the next block.
            text("second"),
            json!({ "type": "reasoning.encrypted", "data": "blob", "format": "anthropic-claude-v1", "index": 0 }),
            json!({ "type": "reasoning.encrypted", "data": "blob2", "format": "anthropic-claude-v1", "index": 0 }),
        ]);
        assert_eq!(details.len(), 4, "{details:#?}");
        assert_eq!(details[0]["text"], "first");
        assert_eq!(details[0]["signature"], "s1");
        assert_eq!(details[1]["text"], "second");
        assert_eq!(details[2]["data"], "blob");
        assert_eq!(details[3]["data"], "blob2");
    }

    #[test]
    fn summaries_streamed_one_per_delta_are_joined_and_ids_keep_items_apart() {
        let s = |t: &str, id: &str| json!({ "type": "reasoning.summary", "summary": t, "id": id, "format": RESPONSES_FORMAT, "index": 0 });
        let details = merged(&[s("Look", "rs_1"), s("ing.", "rs_1"), s("Next", "rs_2")]);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0]["summary"], "Looking.");
        assert_eq!(details[1]["summary"], "Next");
        // Not objects: ignored.
        assert!(merged(&[json!("x"), json!(null)]).is_empty());
    }

    #[test]
    fn responses_items_round_trip() {
        let item = json!({
            "type": "reasoning", "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "A" }, { "type": "summary_text", "text": "B" }],
            "encrypted_content": "enc",
        });
        let details = from_responses_item(&item);
        assert_eq!(details.len(), 3);
        assert_eq!(
            to_responses_items(&details),
            [json!({
                "type": "reasoning", "id": "rs_1",
                "summary": [{ "type": "summary_text", "text": "A" }, { "type": "summary_text", "text": "B" }],
                "encrypted_content": "enc",
            })]
        );
        // Other formats aren't Responses items; an item without an id can't be resumed.
        let other =
            json!({ "type": "reasoning.text", "text": "t", "format": "anthropic-claude-v1" });
        assert!(to_responses_items(&[other]).is_empty());
        assert!(to_responses_items(&from_responses_item(
            &json!({ "type": "reasoning", "summary": [{ "text": "x" }] })
        ))
        .is_empty());
    }

    #[test]
    fn stripping_and_detecting_echoed_reasoning() {
        let mut body = json!({ "messages": [
            { "role": "user", "content": "q" },
            { "role": "assistant", "content": "", "reasoning_details": [{ "type": "reasoning.text" }] },
            { "role": "assistant", "content": "", "reasoning_content": "thinking" },
        ]});
        assert!(sends_any(&body));
        assert!(strip(&mut body));
        assert!(!sends_any(&body));
        assert!(!strip(&mut body));
        assert!(
            is_deepseek("deepseek/deepseek-v4")
                && is_deepseek("DeepSeek-Reasoner")
                && !is_deepseek("gpt-5")
        );
        assert!(wanted("https://example.test/v1"));
        rejected("https://example.test/v1");
        assert!(!wanted("https://example.test/v1"));
    }
}

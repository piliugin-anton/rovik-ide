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
//   turned back into items there (chat_request/responses.rs). The model's
//   commentary messages (`phase: "commentary"`, shown with the reasoning,
//   not as the reply) are kept beside them (`COMMENTARY`): OpenAI's guide
//   says to replay a step's output items, in their order, with its calls.
//   Each entry names the call it came right before (`before_call`), so it
//   goes back in its place;
// - on Anthropic's Messages API, `thinking` blocks (text and `signature`)
//   and `redacted_thinking` blocks (`data`), kept as entries in OpenRouter's
//   form for Anthropic (`format: "anthropic-claude-v1"`) and turned back
//   into blocks there (crate::anthropic). Such an entry may name the tool
//   call it came right before (`before_call`), so it goes back in its place.
//
// What is sent back, and for which messages: chat_db/history.rs
// (`attach_reasoning`). A server that refuses the fields gets the request
// again without them, and they aren't sent to that model there again until
// the app restarts.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// The format of Responses API reasoning, as OpenRouter names it.
pub const RESPONSES_FORMAT: &str = "openai-responses-v1";

/// The type of a Responses API commentary message kept with the reasoning
/// (`commentary_entry`). Not one of OpenRouter's: it only goes back to the
/// Responses API (`strip_commentary`).
pub const COMMENTARY: &str = "commentary";

/// The format of Anthropic's reasoning (thinking blocks), as OpenRouter
/// names it.
pub const ANTHROPIC_FORMAT: &str = "anthropic-claude-v1";

/// The fields an assistant message carries its reasoning back in.
pub const FIELDS: [&str; 2] = ["reasoning_details", "reasoning_content"];

static REJECTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn key(url: &str, model: &str) -> String {
    format!("{url}|{model}")
}

/// Whether reasoning may be sent back to this model at this endpoint.
pub fn wanted(url: &str, model: &str) -> bool {
    REJECTED
        .lock()
        .map_or(true, |r| !r.contains(&key(url, model)))
}

/// The endpoint refused reasoning in this model's requests. Remembered for
/// the model, not the endpoint: on the Responses API a model that doesn't
/// reason may refuse the request for encrypted reasoning, while the models
/// beside it need theirs back.
pub fn rejected(url: &str, model: &str) {
    if let Ok(mut r) = REJECTED.lock() {
        r.insert(key(url, model));
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

/// Whether a request body sends any reasoning back, or asks for it to send
/// back later: in its chat messages, or on the Responses API as reasoning
/// items and the request for encrypted reasoning.
pub fn sends_any(body: &Value) -> bool {
    let in_messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|m| FIELDS.iter().any(|f| m.get(*f).is_some()))
        });
    let in_input = body
        .get("input")
        .and_then(|i| i.as_array())
        .is_some_and(|items| {
            items
                .iter()
                .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
        });
    in_messages || in_input || body.get("include").is_some()
}

/// A commentary message kept with the reasoning (`commentary_entry`).
pub fn is_commentary(entry: &Value) -> bool {
    entry.get("type").and_then(|t| t.as_str()) == Some(COMMENTARY)
        && entry.get("format").and_then(|f| f.as_str()) == Some(RESPONSES_FORMAT)
}

/// A Responses API commentary message (an output item with `phase:
/// "commentary"`, at `index` in the output) as a `reasoning_details` entry:
/// its text, to go back where it was. None for one without text.
pub fn commentary_entry(item: &Value, index: u64) -> Option<Value> {
    let text: String = item
        .get("content")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
        .collect();
    if text.is_empty() {
        return None;
    }
    let mut entry = json!({
        "type": COMMENTARY, "text": text, "format": RESPONSES_FORMAT, "index": index,
    });
    if let Some(id) = item.get("id").filter(|id| id.is_string()) {
        entry["id"] = id.clone();
    }
    Some(entry)
}

/// Removes the commentary kept with the reasoning from a chat completions
/// body: it's the Responses API's, which alone takes it back. A message
/// left without entries loses the field.
pub fn strip_commentary(body: &mut Value) {
    for message in body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .into_iter()
        .flatten()
    {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        let Some(details) = message
            .get_mut("reasoning_details")
            .and_then(|d| d.as_array_mut())
        else {
            continue;
        };
        details.retain(|e| !is_commentary(e));
        if details.is_empty() {
            message.remove("reasoning_details");
        }
    }
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

/// What goes back to the Responses API from `reasoning_details` (entries
/// of other formats are for other APIs), in order: the reasoning items, one
/// per id, and the model's commentary messages, each with the call it came
/// right before (`before_call`), if any.
pub fn to_responses_items(details: &[Value]) -> Vec<(Option<String>, Value)> {
    let mut items: Vec<(Option<String>, Value)> = Vec::new();
    for entry in details
        .iter()
        .filter(|e| e.get("format").and_then(|f| f.as_str()) == Some(RESPONSES_FORMAT))
    {
        let before = entry
            .get("before_call")
            .and_then(|b| b.as_str())
            .filter(|b| !b.is_empty())
            .map(str::to_string);
        let kind = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if kind == COMMENTARY {
            if let Some(text) = entry
                .get("text")
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
            {
                items.push((
                    before,
                    json!({ "role": "assistant", "content": text, "phase": "commentary" }),
                ));
            }
            continue;
        }
        if !matches!(kind, "reasoning.summary" | "reasoning.encrypted") {
            continue;
        }
        let id = entry.get("id").cloned().unwrap_or(Value::Null);
        let same_item = items
            .last()
            .is_some_and(|(_, i)| i["type"] == "reasoning" && i["id"] == id);
        if !same_item {
            items.push((
                before,
                json!({ "type": "reasoning", "id": id, "summary": [] }),
            ));
        }
        let Some((_, item)) = items.last_mut() else {
            continue;
        };
        if kind == "reasoning.summary" {
            let text = entry.get("summary").cloned().unwrap_or(json!(""));
            if let Some(summary) = item["summary"].as_array_mut() {
                summary.push(json!({ "type": "summary_text", "text": text }));
            }
        } else {
            item["encrypted_content"] = entry.get("data").cloned().unwrap_or(Value::Null);
        }
    }
    // A reasoning item without an id can't be resumed.
    items.retain(|(_, i)| {
        i["type"] != "reasoning" || i["id"].as_str().is_some_and(|id| !id.is_empty())
    });
    items
}

/// The Anthropic thinking blocks in `reasoning_details` (entries of other
/// formats are for other APIs; one without a format is taken as
/// Anthropic's), in order, each with the id of the tool call it came right
/// before (`before_call`), if any. A thinking block goes back only with its
/// signature, which the API checks; one without isn't sent.
pub fn to_anthropic_blocks(details: &[Value]) -> Vec<(Option<String>, Value)> {
    details
        .iter()
        .filter(|e| {
            e.get("format")
                .and_then(|f| f.as_str())
                .is_none_or(|f| f == ANTHROPIC_FORMAT)
        })
        .filter_map(|entry| {
            let field = |k: &str| {
                entry
                    .get(k)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            };
            let block = match entry.get("type").and_then(|t| t.as_str()) {
                Some("reasoning.text") => json!({
                    "type": "thinking",
                    "thinking": field("text").unwrap_or(""),
                    "signature": field("signature")?,
                }),
                Some("reasoning.encrypted") => {
                    json!({ "type": "redacted_thinking", "data": field("data")? })
                }
                _ => return None,
            };
            Some((field("before_call").map(str::to_string), block))
        })
        .collect()
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
            [(
                None,
                json!({
                    "type": "reasoning", "id": "rs_1",
                    "summary": [{ "type": "summary_text", "text": "A" }, { "type": "summary_text", "text": "B" }],
                    "encrypted_content": "enc",
                })
            )]
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
    fn anthropic_blocks_are_the_signed_entries_in_order() {
        let details = [
            json!({ "type": "reasoning.text", "text": "Plan.", "signature": "s1", "format": ANTHROPIC_FORMAT, "index": 0 }),
            // Thinking shown as omitted: no text, still a block to send back.
            json!({ "type": "reasoning.text", "signature": "s2", "format": ANTHROPIC_FORMAT, "index": 2, "before_call": "toolu_2" }),
            json!({ "type": "reasoning.encrypted", "data": "blob", "format": ANTHROPIC_FORMAT, "index": 3 }),
            // Unsigned: the API would refuse it.
            json!({ "type": "reasoning.text", "text": "cut off", "format": ANTHROPIC_FORMAT, "index": 4 }),
            // Another API's.
            json!({ "type": "reasoning.encrypted", "data": "enc", "format": RESPONSES_FORMAT, "id": "rs_1" }),
            json!({ "type": "reasoning.summary", "summary": "s", "format": ANTHROPIC_FORMAT }),
        ];
        assert_eq!(
            to_anthropic_blocks(&details),
            [
                (
                    None,
                    json!({ "type": "thinking", "thinking": "Plan.", "signature": "s1" })
                ),
                (
                    Some("toolu_2".to_string()),
                    json!({ "type": "thinking", "thinking": "", "signature": "s2" })
                ),
                (None, json!({ "type": "redacted_thinking", "data": "blob" })),
            ]
        );
        // No format: taken as Anthropic's.
        let bare = json!({ "type": "reasoning.text", "text": "t", "signature": "s" });
        assert_eq!(to_anthropic_blocks(&[bare]).len(), 1);
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
        assert!(wanted("https://example.test/v1", "m"));
        rejected("https://example.test/v1", "m");
        assert!(!wanted("https://example.test/v1", "m"));
        assert!(
            wanted("https://example.test/v1", "other"),
            "a refusal is the model's"
        );
    }

    #[test]
    fn a_responses_step_goes_back_whole_and_each_item_in_its_place() {
        let reasoning = |id: &str, call: &str| {
            let mut entries = from_responses_item(
                &json!({ "type": "reasoning", "id": id, "summary": [], "encrypted_content": format!("enc-{id}") }),
            );
            for e in &mut entries {
                e["before_call"] = json!(call);
            }
            entries
        };
        let mut commentary = commentary_entry(
            &json!({ "type": "message", "id": "msg_1", "phase": "commentary", "content": [{ "type": "output_text", "text": "Reading both." }] }),
            1,
        )
        .unwrap();
        commentary["before_call"] = json!("c1");
        let mut details = reasoning("rs_1", "c1");
        details.push(commentary);
        details.extend(reasoning("rs_2", "c2"));
        let items = to_responses_items(&details);
        let placed: Vec<(Option<&str>, &str)> = items
            .iter()
            .map(|(at, i)| (at.as_deref(), i["type"].as_str().unwrap_or("message")))
            .collect();
        assert_eq!(
            placed,
            [
                (Some("c1"), "reasoning"),
                (Some("c1"), "message"),
                (Some("c2"), "reasoning"),
            ]
        );
        assert_eq!(items[0].1["encrypted_content"], "enc-rs_1");
        assert_eq!(
            items[1].1,
            json!({ "role": "assistant", "content": "Reading both.", "phase": "commentary" })
        );
        // Commentary without text isn't kept.
        assert!(commentary_entry(&json!({ "type": "message", "content": [] }), 0).is_none());
    }

    #[test]
    fn commentary_stays_off_chat_completions_and_what_counts_as_sent_back() {
        let commentary = commentary_entry(
            &json!({ "content": [{ "type": "output_text", "text": "Looking." }] }),
            0,
        )
        .unwrap();
        let summary = json!({ "type": "reasoning.summary", "summary": "s", "id": "rs_1", "format": RESPONSES_FORMAT });
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": "", "reasoning_details": [commentary.clone(), summary.clone()] },
            { "role": "assistant", "content": "", "reasoning_details": [commentary] },
        ]});
        strip_commentary(&mut body);
        assert_eq!(body["messages"][0]["reasoning_details"], json!([summary]));
        assert!(body["messages"][1].get("reasoning_details").is_none());
        // On the Responses API: its items, and the request for more.
        let responses = |input: Value| json!({ "model": "m", "input": input });
        assert!(sends_any(&responses(
            json!([{ "type": "reasoning", "id": "rs_1" }])
        )));
        // A labelled message alone isn't reasoning (the text sent with
        // calls is labelled commentary too).
        assert!(!sends_any(&responses(
            json!([{ "role": "assistant", "content": "x", "phase": "commentary" }])
        )));
        let mut asked = responses(json!([]));
        asked["include"] = json!(["reasoning.encrypted_content"]);
        assert!(sends_any(&asked));
        assert!(!sends_any(&responses(
            json!([{ "role": "user", "content": "q" }])
        )));
    }
}

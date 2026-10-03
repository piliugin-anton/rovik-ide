//! Builds the message history sent to the model, from the database.
//!
//! Port of the P1 `contextBudget.ts` rules. Trimming order, cheapest
//! information first:
//!  1. Old tool results (before the current request) are cut to a short head
//!     and lose their screenshots; the agent can re-run a tool.
//!  2. The oldest whole turns are dropped, so a tool call is never separated
//!     from its result; the first kept user message says so.
//!  3. As a last resort, earlier tool results in the current request are
//!     cut, keeping the latest one whole.
//! Only the copy sent to the model is trimmed; stored messages are untouched.

use super::attachments;
use super::store::{char_prefix, get_message_body, get_messages, tool_call_extras, DbResult, ToolCall};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// How far back a request can reach at most, before any budget applies.
const MAX_HISTORY_ROWS: usize = 2000;
const PAGE: usize = 500;
const MESSAGE_OVERHEAD: usize = 4;
const IMAGE_TOKENS: usize = 1000;
const TOOL_KEEP_CHARS: usize = 1200;
/// Screenshots re-sent on later turns; older ones become a note.
const MAX_IMAGES: usize = 2;
const OMITTED_NOTE: &str = "[Earlier messages in this chat were omitted to fit the context window.]";
/// Google's documented placeholder for function calls that have no real
/// thought signature (made before signatures were stored, or by another
/// model). `thought_signature` is a bytes field, base64 on the JSON wire, so
/// the documented value `skip_thought_signature_validator` must be sent
/// base64-encoded; the raw text isn't valid base64 and reads as "missing".
const GEMINI_SKIP_SIGNATURE: &str = "c2tpcF90aG91Z2h0X3NpZ25hdHVyZV92YWxpZGF0b3I=";

#[derive(Debug, Clone)]
pub struct Msg {
    pub id: String,
    pub role: String,
    pub text: String,
    /// Characters of the full content (the text may be a preview).
    pub full_chars: usize,
    /// The full body still has to be loaded before sending.
    pub needs_body: bool,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub attachments: Vec<String>,
}

/// Deliberately pessimistic (~3 characters per token): prose is closer to
/// 4, code and Cyrillic to 2-3, so this errs towards sending less.
fn text_tokens(chars: usize) -> usize {
    chars.div_ceil(3)
}

fn estimate(m: &Msg, vision: bool) -> usize {
    let mut tokens = MESSAGE_OVERHEAD + text_tokens(m.full_chars);
    for call in m.tool_calls.iter().flatten() {
        tokens += text_tokens(call.name.chars().count() + call.arguments.chars().count());
    }
    if vision {
        tokens += m.attachments.len() * IMAGE_TOKENS;
    }
    tokens
}

fn total(messages: &[Msg], vision: bool) -> usize {
    messages.iter().map(|m| estimate(m, vision)).sum()
}

fn truncate_tool_result(m: &mut Msg) {
    let mut parts = Vec::new();
    if m.full_chars > TOOL_KEEP_CHARS {
        parts.push(char_prefix(&m.text, TOOL_KEEP_CHARS).to_string());
        parts.push(format!(
            "[Truncated to fit the context window: {} more characters omitted. \
             Call the tool again if you need the full output.]",
            m.full_chars - TOOL_KEEP_CHARS
        ));
    } else if m.full_chars > 0 || m.attachments.is_empty() {
        parts.push(m.text.clone());
    }
    if !m.attachments.is_empty() {
        parts.push("[Screenshot omitted to fit the context window.]".into());
    }
    m.text = parts.join("\n\n");
    m.full_chars = m.text.chars().count();
    m.needs_body = false;
    m.attachments.clear();
}

#[derive(Debug, Default, PartialEq)]
pub struct FitInfo {
    pub trimmed: bool,
    pub dropped_turns: usize,
    pub estimated_tokens: usize,
}

/// The budget rules on resolved-or-estimated messages (pure, unit-tested).
pub fn fit(mut messages: Vec<Msg>, budget: usize, vision: bool) -> (Vec<Msg>, FitInfo) {
    let mut sum = total(&messages, vision);
    if sum <= budget {
        return (messages, FitInfo { trimmed: false, dropped_turns: 0, estimated_tokens: sum });
    }

    let mut current_start = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);

    // 1. Shrink old tool results.
    for m in messages.iter_mut().take(current_start) {
        if m.role == "tool" {
            truncate_tool_result(m);
        }
    }
    sum = total(&messages, vision);
    if sum <= budget {
        return (messages, FitInfo { trimmed: true, dropped_turns: 0, estimated_tokens: sum });
    }

    // 2. Drop the oldest whole turns.
    let turn_starts: Vec<usize> = (0..current_start)
        .filter(|&i| i == 0 || messages[i].role == "user")
        .collect();
    let mut cut = 0;
    let mut dropped = 0;
    for (t, &start) in turn_starts.iter().enumerate() {
        if sum <= budget {
            break;
        }
        let next = turn_starts.get(t + 1).copied().unwrap_or(current_start);
        sum -= total(&messages[start..next], vision);
        cut = next;
        dropped += 1;
    }
    if cut > 0 {
        messages.drain(..cut);
        current_start -= cut;
        if let Some(first) = messages.first_mut() {
            if first.role == "user" {
                first.text = format!("{OMITTED_NOTE}\n\n{}", first.text);
                first.full_chars += OMITTED_NOTE.chars().count() + 2;
            }
        }
        sum = total(&messages, vision);
    }
    if sum <= budget {
        return (messages, FitInfo { trimmed: true, dropped_turns: dropped, estimated_tokens: sum });
    }

    // 3. Last resort: earlier tool results of the current request, oldest
    //    first, sparing the most recent one.
    let tool_indexes: Vec<usize> = (current_start..messages.len())
        .filter(|&i| messages[i].role == "tool")
        .collect();
    if let Some((_, earlier)) = tool_indexes.split_last() {
        for &i in earlier {
            if sum <= budget {
                break;
            }
            truncate_tool_result(&mut messages[i]);
            sum = total(&messages, vision);
        }
    }
    // Still over: send anyway; trimming the user's words or the latest tool
    // result would change the task.
    (messages, FitInfo { trimmed: true, dropped_turns: dropped, estimated_tokens: sum })
}

/// Loads, fits and converts a chat's history to Chat Completions messages.
pub fn build_wire(
    conn: &Connection,
    data_dir: &Path,
    dialog_id: &str,
    token_budget: usize,
    vision: bool,
    current_user_content: Option<&str>,
    gemini: bool,
) -> DbResult<(Vec<Value>, FitInfo)> {
    // Newest pages first, until the cap.
    let mut rows = Vec::new();
    let mut before = None;
    loop {
        let page = get_messages(conn, dialog_id, before, PAGE)?;
        let has_older = page.has_older;
        before = page.messages.first().map(|m| m.seq);
        let mut batch = page.messages;
        batch.append(&mut rows);
        rows = batch;
        if !has_older || rows.len() >= MAX_HISTORY_ROWS {
            break;
        }
    }

    // Backfill provider data (Gemini thought signatures) saved by the
    // backend at parse time, matched by message and call id. Only calls
    // still lacking a signature are filled, so this composes with data that
    // arrived through the normal path.
    let with_calls: Vec<&str> = rows
        .iter()
        .filter(|m| m.tool_calls.as_ref().is_some_and(|t| !t.is_empty()))
        .map(|m| m.id.as_str())
        .collect();
    if !with_calls.is_empty() {
        let saved = tool_call_extras(conn, &with_calls)?;
        for row in rows.iter_mut() {
            let id = row.id.clone();
            for call in row.tool_calls.iter_mut().flatten() {
                if has_signature(call.extra_content.as_ref()) {
                    continue;
                }
                if let Some(extra) = saved.get(&(id.clone(), call.id.clone())) {
                    call.extra_content = Some(extra.clone());
                }
            }
        }
    }

    // Messages the IDE added itself (e.g. failed checks after an edit, see
    // the frontend's verification gate). They are user-role, but never the
    // user's request, so the request's editor context must not replace them.
    let harness: std::collections::HashSet<String> = rows
        .iter()
        .filter(|m| m.meta.as_ref().is_some_and(|meta| meta.get("harness").is_some()))
        .map(|m| m.id.clone())
        .collect();

    // What is never sent: failed requests, errors, unfinished placeholders.
    let messages: Vec<Msg> = rows
        .into_iter()
        .filter(|m| !m.failed && m.error.is_none())
        .filter(|m| m.role != "system")
        .filter(|m| {
            !(m.role == "assistant" && m.content.trim().is_empty() && m.tool_calls.as_ref().map_or(true, |t| t.is_empty()))
        })
        .map(|m| Msg {
            full_chars: if m.body_external { m.body_bytes as usize } else { m.content.chars().count() },
            needs_body: m.body_external,
            id: m.id,
            role: m.role,
            text: m.content,
            tool_calls: m.tool_calls,
            tool_call_id: m.tool_call_id,
            attachments: m.attachments,
        })
        .collect();

    // A tool call can be left without a result (the app closed while it
    // waited for approval). Providers reject such a history, and a model
    // that sees no result may assume the call ran, so say it didn't.
    let mut messages = answer_unanswered_calls(messages);

    // The current request's user message is sent with the editor context
    // the frontend added; it isn't stored that way.
    if let (Some(content), Some(last_user)) = (
        current_user_content,
        messages.iter_mut().rev().find(|m| m.role == "user" && !harness.contains(&m.id)),
    )
    {
        last_user.text = content.to_string();
        last_user.full_chars = content.chars().count();
    }

    let (mut fitted, info) = fit(std::mem::take(&mut messages), token_budget, vision);

    for m in fitted.iter_mut().filter(|m| m.needs_body) {
        m.text = get_message_body(conn, &m.id)?;
        m.needs_body = false;
    }

    Ok((to_wire(&fitted, data_dir, vision, gemini), info))
}

const UNANSWERED_NOTE: &str =
    "Error: this tool call never ran: the app was closed or the run was stopped before it was approved. Call it again if it's still needed.";

/// Adds a result after every assistant tool call that has none, right after
/// that call's other results, so each call is answered in its own step.
pub fn answer_unanswered_calls(messages: Vec<Msg>) -> Vec<Msg> {
    let mut out: Vec<Msg> = Vec::with_capacity(messages.len());
    let mut open: Vec<String> = Vec::new();

    let close = |out: &mut Vec<Msg>, open: &mut Vec<String>| {
        for id in open.drain(..) {
            out.push(Msg {
                id: format!("unanswered-{id}"),
                role: "tool".into(),
                text: UNANSWERED_NOTE.into(),
                full_chars: UNANSWERED_NOTE.chars().count(),
                needs_body: false,
                tool_calls: None,
                tool_call_id: Some(id),
                attachments: Vec::new(),
            });
        }
    };

    for m in messages {
        if m.role == "tool" {
            if let Some(id) = &m.tool_call_id {
                open.retain(|o| o != id);
            }
        } else {
            close(&mut out, &mut open);
            if m.role == "assistant" {
                open = m.tool_calls.iter().flatten().map(|c| c.id.clone()).collect();
            }
        }
        out.push(m);
    }
    close(&mut out, &mut open);
    out
}

/// A non-empty `google.thought_signature` string.
fn has_signature(extra: Option<&Value>) -> bool {
    extra
        .and_then(|e| e.pointer("/google/thought_signature"))
        .and_then(|s| s.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// `extra` with the placeholder signature set, other fields kept.
fn with_placeholder_signature(extra: Option<Value>) -> Value {
    let mut extra = match extra {
        Some(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    if !extra.get("google").is_some_and(|g| g.is_object()) {
        extra["google"] = json!({});
    }
    extra["google"]["thought_signature"] = json!(GEMINI_SKIP_SIGNATURE);
    extra
}

/// Chat Completions only allows text in role:"tool" messages, so images go
/// in a user message after the whole run of tool results (all results for
/// one assistant turn must directly follow it).
fn to_wire(messages: &[Msg], data_dir: &Path, vision: bool, gemini: bool) -> Vec<Value> {
    let with_images: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.attachments.is_empty())
        .map(|(i, _)| i)
        .collect();
    let attach: Vec<usize> = if vision {
        with_images.iter().rev().take(MAX_IMAGES).copied().collect()
    } else {
        Vec::new()
    };

    let mut out = Vec::with_capacity(messages.len() + 2);
    let mut pending: Vec<(Option<String>, Vec<String>)> = Vec::new();
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<(Option<String>, Vec<String>)>| {
        for (call_id, urls) in pending.drain(..) {
            let mut parts = vec![json!({
                "type": "text",
                "text": format!("Screenshot from tool call {}:", call_id.unwrap_or_default()).trim().to_string()
            })];
            parts.extend(urls.into_iter().map(|url| json!({ "type": "image_url", "image_url": { "url": url } })));
            out.push(json!({ "role": "user", "content": parts }));
        }
    };

    for (i, m) in messages.iter().enumerate() {
        if m.role != "tool" {
            flush(&mut out, &mut pending);
        }
        let mut content = m.text.clone();
        if !m.attachments.is_empty() {
            let urls: Vec<String> = if attach.contains(&i) {
                m.attachments.iter().filter_map(|id| attachments::data_url(data_dir, id)).collect()
            } else {
                Vec::new()
            };
            if urls.is_empty() {
                content.push_str("\n[The screenshot is no longer attached.]");
            } else {
                pending.push((m.tool_call_id.clone(), urls));
            }
        }

        let mut msg = json!({ "role": m.role, "content": content });
        if let Some(calls) = m.tool_calls.as_ref().filter(|c| !c.is_empty()) {
            msg["tool_calls"] = json!(calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut call = json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.arguments } });
                    let mut extra = c.extra_content.clone();
                    // Gemini requires a signature on the first call of every
                    // step (parallel siblings carry none). Without a real one,
                    // send the documented placeholder, keeping other data.
                    if gemini && i == 0 && !has_signature(extra.as_ref()) {
                        eprintln!(
                            "gemini: tool call {} ({}) has no thought signature; sending placeholder (extra_content: {})",
                            c.id,
                            c.name,
                            extra.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "none".into())
                        );
                        extra = Some(with_placeholder_signature(extra));
                    }
                    if let Some(extra) = extra {
                        call["extra_content"] = extra;
                    }
                    call
                })
                .collect::<Vec<_>>());
        }
        if let Some(id) = &m.tool_call_id {
            msg["tool_call_id"] = json!(id);
        }
        out.push(msg);
    }
    flush(&mut out, &mut pending);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> Msg {
        Msg {
            id: format!("{role}-{}", text.len()),
            role: role.into(),
            text: text.into(),
            full_chars: text.chars().count(),
            needs_body: false,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
        }
    }
    fn call(ids: &[&str]) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(ids.iter().map(|id| ToolCall { id: id.to_string(), name: "read_file".into(), arguments: "{}".into(), extra_content: None }).collect());
        m
    }
    fn tool(id: &str, size: usize) -> Msg {
        let mut m = msg("tool", &"x".repeat(size));
        m.tool_call_id = Some(id.into());
        m
    }
    fn shape(ms: &[Msg]) -> String {
        ms.iter()
            .map(|m| match m.role.as_str() {
                "tool" => format!("t({})", m.text.chars().count()),
                r => r[..1].to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// Every tool result follows an assistant message (within its turn) that issued its id.
    fn pairing_ok(ms: &[Msg]) -> bool {
        ms.iter().enumerate().all(|(i, m)| {
            m.role != "tool"
                || ms[..i].iter().rev().take_while(|p| p.role != "user").any(|p| {
                    p.tool_calls.iter().flatten().any(|c| Some(&c.id) == m.tool_call_id.as_ref())
                })
        })
    }
    const BIG: usize = 30_000;
    const BUDGET_32K: usize = 22_400; // 70% of the default 32k window
    const BUDGET_8K: usize = 5_600;

    #[test]
    fn unanswered_calls_get_a_result() {
        let history = vec![
            msg("user", "q1"), call(&["c1", "c2"]), tool("c1", 3),
            msg("user", "q2"), call(&["c3"]),
        ];
        let out = answer_unanswered_calls(history);
        let ids: Vec<_> = out.iter().map(|m| format!("{}:{}", &m.role[..1], m.tool_call_id.clone().unwrap_or_default())).collect();
        assert_eq!(ids, ["u:", "a:", "t:c1", "t:c2", "u:", "a:", "t:c3"]);
        assert!(pairing_ok(&out));
        assert_eq!(out[3].text, UNANSWERED_NOTE);
    }

    #[test]
    fn small_chat_untouched() {
        let (out, info) = fit(vec![msg("user", "hi"), msg("assistant", "hello")], BUDGET_32K, true);
        assert!(!info.trimmed);
        assert_eq!(shape(&out), "u a");
    }

    #[test]
    fn old_tool_results_shrink_current_kept() {
        let history = vec![
            msg("user", "q1"), call(&["c1"]), tool("c1", BIG), msg("assistant", "a1"),
            msg("user", "q2"), call(&["c2"]), tool("c2", BIG), msg("assistant", "a2"),
            msg("user", "q3"), call(&["c3"]), tool("c3", BIG), msg("assistant", "a3"),
            msg("user", "q4"), call(&["c4"]), tool("c4", 6000),
        ];
        let (out, info) = fit(history, BUDGET_32K, true);
        assert!(info.trimmed);
        assert_eq!(info.dropped_turns, 0);
        assert!(pairing_ok(&out));
        assert!(out.last().unwrap().text.chars().count() == 6000, "current request's result stays whole");
        assert!(out[2].text.contains("[Truncated"));
    }

    #[test]
    fn oldest_turns_dropped_with_note() {
        let mut history = Vec::new();
        for i in 1..=12 {
            history.push(msg("user", &format!("question {i} {}", "y".repeat(3000))));
            history.push(msg("assistant", &format!("answer {i} {}", "z".repeat(3000))));
        }
        history.push(msg("user", "current question"));
        let (out, info) = fit(history, BUDGET_8K, true);
        assert_eq!(info.dropped_turns, 10);
        assert!(out[0].text.starts_with(OMITTED_NOTE));
        assert!(out[0].text.contains("question 11"));
        assert_eq!(out.last().unwrap().text, "current question");
        assert!(info.estimated_tokens <= BUDGET_8K);
    }

    #[test]
    fn huge_current_run_spares_latest_result() {
        let history = vec![msg("user", "q"), call(&["x1", "x2"]), tool("x1", BIG), tool("x2", BIG), call(&["x3"]), tool("x3", BIG)];
        let (out, _) = fit(history, BUDGET_32K, true);
        assert!(pairing_ok(&out));
        assert!(out[2].text.contains("[Truncated"), "oldest result in the run is cut first");
        assert_eq!(out.last().unwrap().text.chars().count(), BIG, "latest result stays whole");
    }

    #[test]
    fn images_follow_the_whole_tool_block() {
        let dir = std::env::temp_dir().join(format!("history-test-{}", std::process::id()));
        let id = "a".repeat(64);
        std::fs::create_dir_all(dir.join(attachments::DIR)).unwrap();
        std::fs::write(dir.join(attachments::DIR).join(format!("{id}.jpg")), b"jpg").unwrap();

        let mut shot = tool("a", 10);
        shot.attachments = vec![id.clone()];
        let ms = vec![msg("user", "check"), call(&["a", "b"]), shot, tool("b", 10)];

        let roles = |w: &[Value]| w.iter().map(|m| m["role"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let vision = to_wire(&ms, &dir, true, false);
        assert_eq!(roles(&vision), ["user", "assistant", "tool", "tool", "user"]);
        assert_eq!(vision[4]["content"][1]["type"], "image_url");

        let text_only = to_wire(&ms, &dir, false, false);
        assert_eq!(roles(&text_only), ["user", "assistant", "tool", "tool"]);
        assert!(text_only[2]["content"].as_str().unwrap().contains("no longer attached"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gemini_signatures_are_echoed_or_placeholdered() {
        let dir = std::env::temp_dir();
        let signed = json!({ "google": { "thought_signature": "SIG-A" } });
        let sig = |w: &[Value], m: usize, c: usize| {
            w[m]["tool_calls"][c].pointer("/extra_content/google/thought_signature").cloned()
        };

        let mut first_signed = call(&["a", "b"]);
        first_signed.tool_calls.as_mut().unwrap()[0].extra_content = Some(signed.clone());
        // extra_content present but no signature in it (seen from some gateways/models).
        let mut empty_extra = call(&["c"]);
        empty_extra.tool_calls.as_mut().unwrap()[0].extra_content = Some(json!({ "google": {}, "other": 1 }));
        // The signature landed on a sibling, not the first call.
        let mut sibling_signed = call(&["d", "e"]);
        sibling_signed.tool_calls.as_mut().unwrap()[1].extra_content = Some(signed.clone());
        let unsigned = call(&["f"]);

        let ms = vec![
            msg("user", "q"),
            first_signed, tool("a", 1), tool("b", 1),
            empty_extra, tool("c", 1),
            sibling_signed, tool("d", 1), tool("e", 1),
            unsigned, tool("f", 1),
        ];
        let w = to_wire(&ms, &dir, false, true);
        let placeholder = Some(json!(GEMINI_SKIP_SIGNATURE));
        {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD.decode(GEMINI_SKIP_SIGNATURE).unwrap();
            assert_eq!(decoded, b"skip_thought_signature_validator", "placeholder is the base64 of the documented value");
        }

        assert_eq!(sig(&w, 1, 0), Some(json!("SIG-A")), "real signature echoed unchanged");
        assert_eq!(sig(&w, 1, 1), None, "parallel sibling stays unsigned");
        assert_eq!(sig(&w, 4, 0), placeholder, "extra_content without a signature gets one");
        assert_eq!(w[4]["tool_calls"][0]["extra_content"]["other"], json!(1), "other data kept");
        assert_eq!(sig(&w, 6, 0), placeholder, "first call always needs one");
        assert_eq!(sig(&w, 6, 1), Some(json!("SIG-A")), "sibling's data untouched");
        assert_eq!(sig(&w, 9, 0), placeholder);

        // Other providers: real data echoed, never a placeholder.
        let other = to_wire(&ms, &dir, false, false);
        assert_eq!(sig(&other, 1, 0), Some(json!("SIG-A")));
        assert_eq!(sig(&other, 9, 0), None);
        assert!(other[9]["tool_calls"][0].get("extra_content").is_none());
    }
}

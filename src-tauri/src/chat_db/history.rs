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
//!
//! Before any of that, once the history passes OUTDATED_AT of the budget,
//! content that later events made out of date is replaced with a short
//! note (`replace_outdated`): a
//! file read since re-read or edited, a command run again, an earlier
//! round of check failures, old write contents. Most of it can be
//! fetched again, so this costs the model little. It works in batches of
//! tool results, so the start of the history stays the same between steps
//! and providers' prompt caches keep working.
//!
//! "The current request" starts at the user's latest message. Messages the
//! IDE adds in the user role (check failures, `harness` in their meta) are
//! part of the run, not new requests: the user's request is never dropped.
//! Only the copy sent to the model is trimmed; stored messages are untouched.
//!
//! Once a run is long, the request ends with a focus note (`focus_note`):
//! the user's request and the agent's checklist, restated at the end where
//! the model attends to them most.

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
    /// Written by the IDE in the user role (check failures), not by the user.
    pub harness: bool,
    /// The user dismissed the open checklist with this message
    /// (`meta.checklistClosed`): don't carry an old checklist into it.
    pub checklist_closed: bool,
    /// Reasoning sent back with an assistant message (`attach_reasoning`):
    /// `{ details }` or `{ text }`.
    pub reasoning: Option<Value>,
}

/// Characters per token when nothing better is known. Deliberately
/// pessimistic: prose is closer to 4, code and Cyrillic to 2-3. Once a
/// provider reports real token counts, a per-model value is learned
/// (agent.rs) and passed in instead.
pub const DEFAULT_CHARS_PER_TOKEN: f64 = 3.0;

/// How token counts are estimated for one request.
#[derive(Clone, Copy, Debug)]
pub struct Cost {
    pub vision: bool,
    pub chars_per_token: f64,
}

impl Cost {
    pub fn tokens(&self, chars: usize) -> usize {
        (chars as f64 / self.chars_per_token.max(1.0)).ceil() as usize
    }
}

fn estimate(m: &Msg, cost: Cost) -> usize {
    let mut tokens = MESSAGE_OVERHEAD + cost.tokens(m.full_chars);
    if let Some(reasoning) = &m.reasoning {
        tokens += cost.tokens(reasoning.to_string().chars().count());
    }
    for call in m.tool_calls.iter().flatten() {
        tokens += cost.tokens(call.name.chars().count() + call.arguments.chars().count());
    }
    tokens
}

/// Images count only for the messages to_wire actually attaches them to
/// (the last MAX_IMAGES): older screenshots are sent as a note.
pub fn total(messages: &[Msg], cost: Cost) -> usize {
    let text: usize = messages.iter().map(|m| estimate(m, cost)).sum();
    if !cost.vision {
        return text;
    }
    let images: usize = messages
        .iter()
        .rev()
        .filter(|m| !m.attachments.is_empty())
        .take(MAX_IMAGES)
        .map(|m| m.attachments.len())
        .sum();
    text + images * IMAGE_TOKENS
}

pub fn truncate_tool_result(m: &mut Msg) {
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
    /// The whole history, before anything was replaced or trimmed.
    pub full_tokens: usize,
    /// Messages replaced with a note because later ones made them out of date.
    pub outdated: usize,
    /// The request ends with the focus note (`focus_note`).
    pub focus_note: bool,
}

/*
 * ---------------------------------------------------------------------------
 * Outdated content: what later events made out of date
 * ---------------------------------------------------------------------------
 */

/// Replacing starts once the history passes this share of the budget, not
/// at the limit: models get worse with long contexts well before they're full.
const OUTDATED_AT: f64 = 0.75;
/// The newest tool results are never replaced: the model is working with them.
const KEEP_RECENT_RESULTS: usize = 8;
/// Older results become replaceable this many at a time, so the start of the
/// history changes only every few steps (providers cache the prompt prefix).
const OUTDATED_BATCH: usize = 8;
/// Listings and page text keep this much when replaced.
const LISTING_KEEP_CHARS: usize = 600;
/// write / edit arguments longer than this are shortened.
const ARGS_KEEP_CHARS: usize = 300;

/// What one finished tool call did, for the supersession rules.
struct Event {
    /// Index of its result in the messages.
    at: usize,
    name: String,
    path: Option<String>,
    range: Option<(u64, u64)>,
    command: Option<String>,
    ok: bool,
    /// A read that showed only part of what it asked for (read_file.rs).
    partial: bool,
}

fn norm_path(p: &str) -> String {
    p.trim().trim_start_matches("./").replace('\\', "/")
}

fn line_arg(args: &Value, key: &str) -> Option<u64> {
    match args.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Whether a later read of a file (`later`, None = the whole file) covers an
/// earlier one (`earlier`).
fn covers(later: Option<(u64, u64)>, earlier: Option<(u64, u64)>) -> bool {
    match (later, earlier) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((ls, le)), Some((es, ee))) => ls <= es && le >= ee,
    }
}

fn replace_text(m: &mut Msg, text: String) {
    m.full_chars = text.chars().count();
    m.text = text;
    m.needs_body = false;
}

/// Shortens a call's string argument in place; the arguments stay valid JSON.
fn shorten_arg(args: &mut Value, key: &str, what: &str) -> bool {
    let Some(text) = args.get(key).and_then(|v| v.as_str()) else { return false };
    if text.chars().count() <= ARGS_KEEP_CHARS {
        return false;
    }
    let lines = text.lines().count();
    args[key] = json!(format!("[{lines} lines {what}; left out of this history]"));
    true
}

/// Replaces out-of-date content in place with short notes; returns how many
/// messages changed. Results of the newest tool calls are left alone; the
/// rest become replaceable in batches (see OUTDATED_BATCH).
fn replace_outdated(messages: &mut [Msg]) -> usize {
    // Each call's name and arguments, by id.
    let mut calls: std::collections::HashMap<String, (String, Value)> = Default::default();
    for m in messages.iter() {
        for c in m.tool_calls.iter().flatten() {
            let args = serde_json::from_str(&c.arguments).unwrap_or(Value::Null);
            calls.insert(c.id.clone(), (c.name.clone(), args));
        }
    }
    let events: Vec<Event> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "tool")
        .filter_map(|(at, m)| {
            let (name, args) = calls.get(m.tool_call_id.as_ref()?)?;
            let start = line_arg(args, "start_line");
            let end = line_arg(args, "end_line");
            // A read by symbol holds the lines its result names: not the
            // whole file. (One that names none, an error or a partial
            // read, covers nothing below.)
            let by_symbol = args.get("symbol").and_then(|s| s.as_str()).is_some_and(|s| !s.trim().is_empty());
            Some(Event {
                at,
                name: name.clone(),
                path: args.get("path").and_then(|p| p.as_str()).map(norm_path),
                range: match (start, end) {
                    _ if by_symbol => Some(crate::read_symbol::covered_lines(&m.text).unwrap_or((0, 0))),
                    (None, None) => None,
                    (s, e) => Some((s.unwrap_or(1), e.unwrap_or(u64::MAX))),
                },
                command: args.get("command").and_then(|c| c.as_str()).map(|c| c.trim().to_string()),
                ok: !m.text.starts_with("Error:"),
                partial: m.text.starts_with(crate::read_file::PARTIAL_PREFIX),
            })
        })
        .collect();

    let replaceable = events.len().saturating_sub(KEEP_RECENT_RESULTS) / OUTDATED_BATCH * OUTDATED_BATCH;
    // Messages before this index are old enough to replace.
    let horizon = events.get(replaceable).map_or(messages.len(), |e| e.at);
    let mut changed = 0;

    for (i, e) in events[..replaceable].iter().enumerate() {
        let later = &events[i + 1..];
        let new_text = match e.name.as_str() {
            // A read is out of date once the file was read again (covering
            // it) or changed. Not before: it's what the model knows of it.
            // A partial read (a large file's map, a range cut short) covers
            // nothing: it doesn't hold what it asked for.
            "read" if e.ok => {
                let path = e.path.as_deref().unwrap_or_default();
                let reread = later.iter().any(|l| {
                    l.ok && !l.partial && l.name == "read" && l.path.as_deref() == Some(path) && covers(l.range, e.range)
                });
                let edited = later.iter().any(|l| {
                    l.ok && l.name == "write" && l.path.as_deref() == Some(path)
                });
                let lines = e.range.map_or(String::new(), |(s, t)| {
                    if t == u64::MAX { format!(" from line {s}") } else { format!(" lines {s}-{t}") }
                });
                (reread || edited).then(|| {
                    format!(
                        "[Read {path}{lines} here. Left out of this history: the file was {} later, so this copy is out of date. Read it again if you need it.]",
                        if edited { "changed" } else { "read again" }
                    )
                })
            }
            // The same command ran again later: this run's output is history.
            "bash" => {
                let cmd = e.command.as_deref().unwrap_or_default();
                later
                    .iter()
                    .any(|l| l.name == "bash" && l.command.as_deref() == Some(cmd))
                    .then(|| {
                        let head: Vec<&str> = messages[e.at].text.lines().take(2).collect();
                        format!("{}\n[Output left out of this history: the same command ran again later.]", head.join("\n"))
                    })
            }
            // Listings and pages can be fetched again; keep their start.
            "search_code" | "list_dir" | "neighbours" | "callers" | "outline" | "diagnostics" | "web_search" | "browse_url"
                if messages[e.at].full_chars > LISTING_KEEP_CHARS * 2 && !messages[e.at].needs_body =>
            {
                Some(format!(
                    "{}\n[Shortened in this history ({} characters). Run it again if you need the rest.]",
                    char_prefix(&messages[e.at].text, LISTING_KEEP_CHARS),
                    messages[e.at].full_chars
                ))
            }
            _ => None,
        };
        if let Some(text) = new_text {
            replace_text(&mut messages[e.at], text);
            changed += 1;
        }
    }

    // Old write calls: the content they wrote is on disk
    // (and its result says what changed), so the arguments needn't repeat it.
    for m in messages[..horizon].iter_mut() {
        let mut touched = false;
        for c in m.tool_calls.iter_mut().flatten() {
            let mut args: Value = match serde_json::from_str(&c.arguments) {
                Ok(v @ Value::Object(_)) => v,
                _ => continue,
            };
            let shortened = match c.name.as_str() {
                "write" => {
                    let old = shorten_arg(&mut args, "old_text", "replaced");
                    let new = shorten_arg(&mut args, "content", "written");
                    old || new
                }
                _ => false,
            };
            if shortened {
                c.arguments = args.to_string();
                touched = true;
            }
        }
        if touched {
            changed += 1;
        }
    }

    // Earlier rounds of check failures: the latest round is what counts.
    let reports: Vec<usize> = (0..messages.len()).filter(|&i| messages[i].harness).collect();
    if let Some((_, earlier)) = reports.split_last() {
        for &i in earlier {
            if messages[i].text.starts_with("[Earlier") {
                continue;
            }
            let head = messages[i].text.lines().take(2).collect::<Vec<_>>().join("\n");
            replace_text(
                &mut messages[i],
                format!("[Earlier round of checks, left out of this history; a later round follows.]\n{head}"),
            );
            changed += 1;
        }
    }
    changed
}

/// A user message that starts a request (not one the IDE wrote).
pub fn starts_request(m: &Msg) -> bool {
    m.role == "user" && !m.harness
}

/*
 * ---------------------------------------------------------------------------
 * Focus note: the task restated at the end of a long run
 * ---------------------------------------------------------------------------
 */

/// Tool results in the current request before its task is restated: by
/// then the request and the checklist sit in the middle of the context,
/// where models attend to them least.
const FOCUS_AFTER_TOOL_RESULTS: usize = 6;
/// How much of the user's request the note repeats.
const FOCUS_REQUEST_CHARS: usize = 600;
/// A loop warning this many tool results back or fewer is named again.
const FOCUS_LOOP_WINDOW: usize = 5;

/// A typed request shorter than this says nothing about the task on its own
/// ("continue"): the focus note quotes the previous real request as well.
const SHORT_REQUEST_CHARS: usize = 80;

fn todo_in_msg(m: &Msg) -> Option<Vec<crate::agent_run::plan::TodoItem>> {
    m.tool_calls.iter().flatten().rev().filter(|c| c.name == "todo").find_map(|c| match serde_json::from_str::<Value>(&c.arguments) {
        Ok(Value::Object(args)) => Some(crate::agent_run::plan::parse_todo(&args)).filter(|i| !i.is_empty()),
        _ => None,
    })
}

/// The current request and its checklist, restated for the end of the
/// request (the CLM task card does this in CLM mode). None while the run
/// is short, after anything but a tool result (the IDE's own checks speak
/// for themselves), and once every checklist step is settled: a request
/// restated while the agent wraps up reads as being asked again, and it
/// starts over.
///
/// The checklist follows the same rule as `current_checklist`: the latest
/// `todo` in the current request, or the latest open one from the previous
/// request carried forward (unless the new message dismissed it). A second
/// user message stops the search. A short typed request ("continue") also
/// quotes the previous real request, so the model still sees what is being
/// continued.
pub fn focus_note(messages: &[Msg]) -> Option<String> {
    let start = messages.iter().rposition(starts_request)?;
    let run = &messages[start + 1..];
    if messages.last()?.role != "tool" {
        return None;
    }
    if run.iter().filter(|m| m.role == "tool").count() < FOCUS_AFTER_TOOL_RESULTS {
        return None;
    }
    // The latest todo call in this request; one the user dismissed (on its
    // own message) leaves no checklist, and nothing is carried past it.
    let latest = run.iter().rev().find_map(|m| todo_in_msg(m).map(|items| (items, m.checklist_closed)));
    let mut checklist = match &latest {
        Some((items, false)) => Some(items.clone()),
        _ => None,
    };
    if latest.is_none() && !messages[start].checklist_closed {
        let prev = messages[..start].iter().rposition(starts_request);
        let from = prev.map(|p| p + 1).unwrap_or(0);
        for m in messages[from..start].iter().rev() {
            if let Some(items) = todo_in_msg(m) {
                // A finished or dismissed one isn't carried.
                if !m.checklist_closed && !items.iter().all(crate::agent_run::plan::is_settled) {
                    checklist = Some(items);
                }
                break;
            }
        }
    }
    if checklist.as_ref().is_some_and(|items| items.iter().all(crate::agent_run::plan::is_settled)) {
        return None;
    }
    let text = messages[start].text.trim();
    let request = char_prefix(text, FOCUS_REQUEST_CHARS);
    let more = if request.len() < text.len() || messages[start].needs_body { " …" } else { "" };
    let mut note = "[Reminder from the IDE, not a new request: the request this run is for, quoted so it stays in view. \
                    Continue from where you are: don't start over or redo finished work, and when the request is done, \
                    stop and reply to the user.]"
        .to_string();
    if !request.is_empty() {
        note.push_str("\nThe request (quoted):");
        for line in format!("{request}{more}").lines() {
            note.push_str(&format!("\n> {line}"));
        }
    }
    if text.chars().count() < SHORT_REQUEST_CHARS {
        if let Some(prev) = messages[..start].iter().rposition(starts_request) {
            let prev_text = messages[prev].text.trim();
            if !prev_text.is_empty() && prev_text != text {
                let prev_request = char_prefix(prev_text, FOCUS_REQUEST_CHARS);
                let prev_more = if prev_request.len() < prev_text.len() || messages[prev].needs_body { " …" } else { "" };
                note.push_str("\nThe earlier request it continues (quoted):");
                for line in format!("{prev_request}{prev_more}").lines() {
                    note.push_str(&format!("\n> {line}"));
                }
            }
        }
    }
    if let Some(items) = checklist {
        let done = items.iter().filter(|i| crate::agent_run::plan::is_settled(i)).count();
        note.push_str(&format!("\nYour checklist ({done} of {} done):", items.len()));
        for item in &items {
            note.push_str(&format!("\n- [{}] {}", item.status, item.title));
        }
    }
    // A loop warning a few calls back (on the latest result, it's right there).
    let warning = run
        .iter()
        .rev()
        .filter(|m| m.role == "tool")
        .take(FOCUS_LOOP_WINDOW + 1)
        .enumerate()
        .skip(1)
        .find_map(|(back, m)| crate::agent_run::guard::loop_warning(&m.text).map(|w| (back, w)));
    if let Some((back, reason)) = warning {
        let ago = if back == 1 { "1 call".to_string() } else { format!("{back} calls") };
        note.push_str(&format!(
            "\nLoop warning {ago} ago: {reason}. Don't repeat it: change the approach, or stop and ask the user."
        ));
    }
    note.push_str("\n[end of reminder]");
    Some(note)
}

/// Puts the note on the request's last message (not stored).
pub fn add_focus_note(messages: &mut [Msg], note: &str) {
    if let Some(last) = messages.last_mut() {
        last.text = format!("{}\n\n{note}", last.text.trim_end());
        last.full_chars = last.text.chars().count();
    }
}

/// The budget rules with the default token estimate (tests).
#[cfg(test)]
pub fn fit(messages: Vec<Msg>, budget: usize, vision: bool) -> (Vec<Msg>, FitInfo) {
    fit_with(messages, budget, Cost { vision, chars_per_token: DEFAULT_CHARS_PER_TOKEN })
}

/// The budget rules on resolved-or-estimated messages (pure, unit-tested).
pub fn fit_with(mut messages: Vec<Msg>, budget: usize, cost: Cost) -> (Vec<Msg>, FitInfo) {
    let full_tokens = total(&messages, cost);
    let mut sum = full_tokens;
    let mut outdated = 0;

    // 0. Out-of-date content, once the history is getting long.
    if sum as f64 > budget as f64 * OUTDATED_AT {
        outdated = replace_outdated(&mut messages);
        if outdated > 0 {
            sum = total(&messages, cost);
        }
    }
    let info = |trimmed: bool, dropped_turns: usize, estimated_tokens: usize| FitInfo {
        focus_note: false,
        trimmed,
        dropped_turns,
        estimated_tokens,
        full_tokens,
        outdated,
    };
    if sum <= budget {
        return (messages, info(false, 0, sum));
    }

    let mut current_start = messages.iter().rposition(starts_request).unwrap_or(0);

    // 1. Shrink old tool results.
    for m in messages.iter_mut().take(current_start) {
        if m.role == "tool" {
            truncate_tool_result(m);
        }
    }
    sum = total(&messages, cost);
    if sum <= budget {
        return (messages, info(true, 0, sum));
    }

    // 2. Drop the oldest whole turns.
    let turn_starts: Vec<usize> = (0..current_start)
        .filter(|&i| i == 0 || starts_request(&messages[i]))
        .collect();
    let mut cut = 0;
    let mut dropped = 0;
    for (t, &start) in turn_starts.iter().enumerate() {
        if sum <= budget {
            break;
        }
        let next = turn_starts.get(t + 1).copied().unwrap_or(current_start);
        sum -= total(&messages[start..next], cost);
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
        sum = total(&messages, cost);
    }
    if sum <= budget {
        return (messages, info(true, dropped, sum));
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
            sum = total(&messages, cost);
        }
    }
    // Still over: send anyway; trimming the user's words or the latest tool
    // result would change the task.
    (messages, info(true, dropped, sum))
}

/// Loads, fits and converts a chat's history to Chat Completions messages.
#[allow(clippy::too_many_arguments)]
pub fn build_wire(
    conn: &Connection,
    data_dir: &Path,
    dialog_id: &str,
    model: &str,
    token_budget: usize,
    vision: bool,
    current_user_content: Option<&str>,
    gemini: bool,
    chars_per_token: f64,
) -> DbResult<(Vec<Value>, FitInfo)> {
    let mut messages = load_messages(conn, dialog_id, None)?;
    attach_reasoning(conn, &mut messages, model)?;
    let cost = Cost { vision, chars_per_token };
    // The guard's loop warnings end long results too: the latest ones whole.
    let tail = messages.len().saturating_sub(2 * (FOCUS_LOOP_WINDOW + 1));
    load_bodies(conn, &mut messages[tail..])?;
    // The note is made before fitting (which never drops the current
    // request or its calls) and its room kept out of the budget. It quotes
    // the request as typed: the copy sent starts with the editor context.
    let focus = focus_note(&messages);
    use_sent_content(&mut messages, current_user_content);
    let note_tokens = focus.as_ref().map_or(0, |n| cost.tokens(n.chars().count() + 2));
    let (mut fitted, mut info) = fit_with(messages, token_budget.saturating_sub(note_tokens), cost);
    load_bodies(conn, &mut fitted)?;
    if let Some(note) = focus {
        add_focus_note(&mut fitted, &note);
        info.estimated_tokens += note_tokens;
        info.focus_note = true;
    }
    Ok((to_wire(&fitted, data_dir, vision, gemini), info))
}

/// The reasoning assistant messages carry back to `model`
/// (reasoning_echo.rs): those of the current request, so a model resumes
/// its thinking after a tool call; and, for DeepSeek, which refuses the
/// request otherwise, every earlier one with tool calls. Only reasoning
/// that `model` itself wrote: signatures and encrypted reasoning are bound
/// to it. The structured form when there is one, else the text.
pub fn attach_reasoning(conn: &Connection, messages: &mut [Msg], model: &str) -> DbResult<()> {
    if model.is_empty() {
        return Ok(());
    }
    let current = messages.iter().rposition(starts_request).unwrap_or(0);
    let deepseek = crate::reasoning_echo::is_deepseek(model);
    let wanted: Vec<usize> = (0..messages.len())
        .filter(|&i| {
            let m = &messages[i];
            m.role == "assistant"
                && (i >= current || (deepseek && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())))
        })
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = wanted.iter().map(|&i| messages[i].id.as_str()).collect();
    let stored = super::store::reasoning_of(conn, &ids)?;
    for i in wanted {
        let Some(r) = stored.get(&messages[i].id) else { continue };
        if r.get("model").and_then(|m| m.as_str()) != Some(model) {
            continue;
        }
        let details = r.get("details").filter(|d| d.as_array().is_some_and(|a| !a.is_empty()));
        let text = r.get("text").filter(|t| t.as_str().is_some_and(|t| !t.is_empty()));
        let inline = r.get("inline").filter(|t| t.is_string());
        messages[i].reasoning = match (details, text, inline) {
            (Some(details), _, _) => Some(json!({ "details": details })),
            // Written in the reply text: back in the text, in its tag.
            (None, Some(text), Some(tag)) => Some(json!({ "text": text, "inline": tag })),
            (None, Some(text), None) => Some(json!({ "text": text })),
            _ => None,
        };
    }
    Ok(())
}

/// Full texts of messages whose body is stored apart (long ones).
pub fn load_bodies(conn: &Connection, messages: &mut [Msg]) -> DbResult<()> {
    for m in messages.iter_mut().filter(|m| m.needs_body) {
        m.text = get_message_body(conn, &m.id)?;
        m.full_chars = m.text.chars().count();
        m.needs_body = false;
    }
    Ok(())
}

/// The chat's history as it can be sent, before any budget: what is never
/// sent is left out, unanswered tool calls get a result, and the current
/// request carries `current_user_content`. Long bodies may still need
/// `load_bodies`.
pub fn load_messages(conn: &Connection, dialog_id: &str, current_user_content: Option<&str>) -> DbResult<Vec<Msg>> {
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

    // What is never sent: failed requests, errors, unfinished placeholders.
    let messages: Vec<Msg> = rows
        .into_iter()
        .filter(|m| !m.failed && m.error.is_none())
        .filter(|m| m.role != "system")
        .filter(|m| {
            !(m.role == "assistant" && m.content.trim().is_empty() && m.tool_calls.as_ref().map_or(true, |t| t.is_empty()))
        })
        .map(|m| Msg {
            // Added by the IDE (the frontend's verification gate): user-role,
            // but never the user's request.
            harness: m.meta.as_ref().is_some_and(|meta| meta.get("harness").is_some()),
            checklist_closed: m.meta.as_ref().is_some_and(|meta| {
                meta.get("checklistClosed").or_else(|| meta.get("checklist_closed")).is_some_and(|v| *v == serde_json::Value::Bool(true))
            }),
            full_chars: if m.body_external { m.body_bytes as usize } else { m.content.chars().count() },
            needs_body: m.body_external,
            id: m.id,
            role: m.role,
            text: m.content,
            tool_calls: m.tool_calls,
            tool_call_id: m.tool_call_id,
            attachments: m.attachments,
            reasoning: None,
        })
        .collect();

    // A tool call can be left without a result (the app closed while it
    // waited for approval). Providers reject such a history, and a model
    // that sees no result may assume the call ran, so say it didn't.
    let mut messages = answer_unanswered_calls(messages);
    use_sent_content(&mut messages, current_user_content);
    Ok(messages)
}

/// The current request's user message is sent with the editor context
/// the frontend added; it isn't stored that way.
pub fn use_sent_content(messages: &mut [Msg], current_user_content: Option<&str>) {
    if let (Some(content), Some(last_user)) = (
        current_user_content,
        messages.iter_mut().rev().find(|m| starts_request(m)),
    )
    {
        last_user.text = content.to_string();
        last_user.full_chars = content.chars().count();
        last_user.needs_body = false;
    }
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
                harness: false,
                checklist_closed: false,
                reasoning: None,
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

/// A call's arguments as sent back: providers reject the whole request when
/// any earlier call's arguments aren't a JSON object, and a model can emit
/// broken JSON (a reply cut off mid-call, an unescaped character). Valid
/// arguments go back unchanged; broken ones as an object holding the text,
/// so the model can still see what it sent next to the error it got.
fn wire_arguments(arguments: &str) -> String {
    if arguments.trim().is_empty() {
        return "{}".into();
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(_)) => arguments.to_string(),
        _ => {
            let mut raw: String = arguments.chars().take(ARGS_KEEP_CHARS).collect();
            if raw.len() < arguments.len() {
                raw.push('…');
            }
            json!({ "invalid_arguments": raw }).to_string()
        }
    }
}

/// Chat Completions only allows text in role:"tool" messages, so images go
/// in a user message after the whole run of tool results (all results for
/// one assistant turn must directly follow it).
pub fn to_wire(messages: &[Msg], data_dir: &Path, vision: bool, gemini: bool) -> Vec<Value> {
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

        let inline = m.reasoning.as_ref().and_then(|r| Some((r.get("inline")?.as_str()?, r.get("text")?.as_str()?)));
        if let Some((tag, text)) = inline {
            content = format!("<{tag}>{text}</{tag}>{content}");
        }
        let mut msg = json!({ "role": m.role, "content": content });
        match m.reasoning.as_ref().map(|r| (r.get("details"), r.get("text"))) {
            _ if inline.is_some() => {}
            Some((Some(details), _)) => msg["reasoning_details"] = details.clone(),
            Some((None, Some(text))) => msg["reasoning_content"] = text.clone(),
            _ => {}
        }
        if let Some(calls) = m.tool_calls.as_ref().filter(|c| !c.is_empty()) {
            msg["tool_calls"] = json!(calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut call = json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": wire_arguments(&c.arguments) } });
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
            harness: false,
            checklist_closed: false,
            reasoning: None,
        }
    }
    #[test]
    fn reasoning_goes_back_in_the_form_it_came_in() {
        let dir = std::env::temp_dir();
        let mut inline = msg("assistant", "\n\nThe answer.");
        inline.reasoning = Some(json!({ "text": "Plan.", "inline": "think" }));
        let mut field = msg("assistant", "Answer.");
        field.reasoning = Some(json!({ "text": "Plan." }));
        let wire = to_wire(&[msg("user", "q"), inline, field], &dir, false, false);
        // In its tag, as the model wrote it (MiniMax wants it back unchanged).
        assert_eq!(wire[1]["content"], "<think>Plan.</think>\n\nThe answer.");
        assert!(wire[1].get("reasoning_content").is_none());
        assert_eq!(wire[2]["content"], "Answer.");
        assert_eq!(wire[2]["reasoning_content"], "Plan.");
    }

    fn call(ids: &[&str]) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(ids.iter().map(|id| ToolCall { id: id.to_string(), name: "read".into(), arguments: "{}".into(), extra_content: None }).collect());
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

    fn todo(items: &str) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(vec![ToolCall { id: "todo-1".into(), name: "todo".into(), arguments: items.into(), extra_content: None }]);
        m
    }
    fn long_run(request: &str) -> Vec<Msg> {
        let mut history = vec![msg("user", "an earlier request"), msg("assistant", "done"), msg("user", request)];
        for i in 0..FOCUS_AFTER_TOOL_RESULTS {
            let id = format!("c{i}");
            history.push(call(&[&id]));
            history.push(tool(&id, 10));
        }
        history
    }

    #[test]
    fn focus_note_restates_a_long_run() {
        let mut short = long_run("fix the parser");
        short.truncate(short.len() - 2);
        assert_eq!(focus_note(&short), None);

        // A long request stands on its own: no earlier request quoted.
        let long_request = format!("fix the parser {}", "and more context ".repeat(10));
        let mut history = long_run(&long_request);
        history.insert(3, todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"in_progress"},"test"]}"#));
        let note = focus_note(&history).unwrap();
        assert!(note.contains("not a new request"), "{note}");
        assert!(note.contains("The request (quoted):"), "{note}");
        assert!(note.contains("Your checklist (1 of 3 done):\n- [done] read\n- [in_progress] fix\n- [pending] test"), "{note}");
        assert!(!note.contains("earlier request"));

        // A short request ("continue") also quotes the previous real request.
        let mut short_history = long_run("fix the parser");
        short_history.insert(3, todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"in_progress"},"test"]}"#));
        let short_note = focus_note(&short_history).unwrap();
        assert!(short_note.contains("The earlier request it continues (quoted):"), "{short_note}");
        assert!(short_note.contains("> an earlier request"), "{short_note}");

        // The latest checklist wins; once every step is settled the agent
        // is wrapping up, and no note pushes it back into the task.
        history.push(todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"in_progress"}]}"#));
        history.push(tool("todo-1", 10));
        assert!(focus_note(&history).unwrap().contains("(1 of 2 done)"));
        history.push(todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"skipped"}]}"#));
        history.push(tool("todo-1", 10));
        assert_eq!(focus_note(&history), None);
        // An open checklist from an earlier request is carried forward.
        let mut earlier = long_run("next");
        earlier.insert(1, todo(r#"{"items":["old step"]}"#));
        assert!(focus_note(&earlier).unwrap().contains("checklist"));
        // ...unless every step is settled: the new request's note stands
        // on its own, without the finished list.
        let mut settled_earlier = long_run("next");
        settled_earlier.insert(1, todo(r#"{"items":[{"title":"old step","status":"done"}]}"#));
        assert!(!focus_note(&settled_earlier).unwrap().contains("Your checklist"));
        // A dismissed one (on its own message) isn't carried either.
        let mut dismissed_earlier = long_run("next");
        let mut closed = todo(r#"{"items":["old step"]}"#);
        closed.checklist_closed = true;
        dismissed_earlier.insert(1, closed);
        assert!(!focus_note(&dismissed_earlier).unwrap().contains("Your checklist"));
    }

    #[test]
    fn focus_note_carries_a_checklist_into_a_short_request() {
        // A short follow-up ("continue") with a carried checklist quotes the
        // previous real request and lists the carried steps.
        let mut history = long_run("continue");
        history.insert(1, todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"pending"}]}"#));
        let note = focus_note(&history).expect("carried checklist with a short request");
        assert!(note.contains("The earlier request it continues (quoted):"), "{note}");
        assert!(note.contains("> an earlier request"), "{note}");
        assert!(note.contains("Your checklist (1 of 2 done):"), "{note}");
        assert!(note.contains("- [done] read") && note.contains("- [pending] fix"), "{note}");

        // Dismissed on the new message: not carried.
        let mut dismissed = long_run("continue");
        dismissed.insert(1, todo(r#"{"items":["old step"]}"#));
        let at = dismissed.iter().rposition(starts_request).unwrap();
        dismissed[at].checklist_closed = true;
        let note = focus_note(&dismissed).unwrap();
        assert!(!note.contains("Your checklist"), "{note}");
        assert!(note.contains("The earlier request it continues (quoted):"), "{note}");
    }

    #[test]
    fn focus_note_names_a_recent_loop_warning() {
        let warned = |id: &str| {
            let mut m = tool(id, 10);
            m.text.push_str("\n\n[Note from the IDE: read (a) was called 3 times. This looks like a loop. Step back.]");
            m
        };
        // Two calls after the warning: named, with how long ago.
        let mut history = long_run("q");
        let at = history.len() - 5;
        history[at] = warned("c3");
        let note = focus_note(&history).unwrap();
        assert!(
            note.contains("Loop warning 2 calls ago: read (a) was called 3 times. Don't repeat it"),
            "{note}"
        );
        // On the latest result: it's right there, not repeated.
        let mut history = long_run("q");
        let last = history.len() - 1;
        history[last] = warned("c5");
        assert!(!focus_note(&history).unwrap().contains("Loop warning"));
        // Long ago: no longer named.
        let mut history = long_run("q");
        history[4] = warned("c0");
        for i in 6..12 {
            let id = format!("c{i}");
            history.push(call(&[&id]));
            history.push(tool(&id, 10));
        }
        assert!(!focus_note(&history).unwrap().contains("Loop warning"));
    }

    #[test]
    fn focus_note_edge_cases() {
        // A history ending in the agent's reply isn't inside a run.
        let mut history = long_run("q");
        history.push(msg("assistant", "finished"));
        assert_eq!(focus_note(&history), None);
        assert_eq!(focus_note(&[]), None);
        // A request of only file references has no typed text of its own;
        // a short one still quotes the previous real request.
        let empty_note = focus_note(&long_run("")).unwrap();
        assert!(!empty_note.contains("The request (quoted)"), "{empty_note}");
        assert!(empty_note.contains("The earlier request it continues (quoted):"), "{empty_note}");
        // After the IDE's own check (a user-role harness message): no note.
        let mut checked = long_run("q");
        let mut check = msg("user", "tests failed");
        check.harness = true;
        checked.push(check);
        assert_eq!(focus_note(&checked), None);
        // Every line of a multi-line request is quoted.
        assert!(focus_note(&long_run("a\nb")).unwrap().contains("> a\n> b"));
        // A long request is cut.
        let note = focus_note(&long_run(&"é".repeat(FOCUS_REQUEST_CHARS + 5))).unwrap();
        assert!(note.contains(&format!("> {} …", "é".repeat(FOCUS_REQUEST_CHARS))));
        // The note rides on the last message.
        let mut history = long_run("q");
        add_focus_note(&mut history, "NOTE");
        let last = history.last().unwrap();
        assert!(last.text.ends_with("\n\nNOTE"));
        assert_eq!(last.full_chars, last.text.chars().count());
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

    #[test]
    fn broken_call_arguments_go_out_as_a_json_object() {
        let dir = std::env::temp_dir();
        let with_args = |args: &str| {
            let mut m = msg("assistant", "");
            m.tool_calls = Some(vec![ToolCall { id: "c".into(), name: "write".into(), arguments: args.into(), extra_content: None }]);
            m
        };
        let sent = |args: &str| {
            let w = to_wire(&[msg("user", "q"), with_args(args), result("c", "Error: x")], &dir, false, false);
            w[1]["tool_calls"][0]["function"]["arguments"].as_str().unwrap().to_string()
        };

        let valid = r#"{"path":"a.rs","old_text":"x","content":"y"}"#;
        assert_eq!(sent(valid), valid, "valid arguments are sent unchanged");
        assert_eq!(sent(""), "{}");
        assert_eq!(sent("  "), "{}");

        let cut_off = r#"{"old_text":"fn main() {"#;
        let back: Value = serde_json::from_str(&sent(cut_off)).expect("valid JSON");
        assert_eq!(back["invalid_arguments"], cut_off, "the model still sees what it sent");

        let array: Value = serde_json::from_str(&sent("[1,2]")).unwrap();
        assert!(array.is_object(), "not an object: wrapped too");

        let long = format!("{{\"old_text\":\"{}", "é".repeat(ARGS_KEEP_CHARS * 2));
        let back: Value = serde_json::from_str(&sent(&long)).unwrap();
        let kept = back["invalid_arguments"].as_str().unwrap();
        assert_eq!(kept.chars().count(), ARGS_KEEP_CHARS + 1, "capped, with an ellipsis");
        assert!(kept.ends_with('…'));
    }

    // ---- requests, the IDE's messages, outdated content ----

    fn call_with(id: &str, name: &str, args: Value) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(vec![ToolCall { id: id.into(), name: name.into(), arguments: args.to_string(), extra_content: None }]);
        m
    }
    fn result(id: &str, text: &str) -> Msg {
        let mut m = msg("tool", text);
        m.tool_call_id = Some(id.into());
        m
    }
    fn harness(text: &str) -> Msg {
        let mut m = msg("user", text);
        m.harness = true;
        m
    }
    fn generous(vision: bool) -> Cost {
        Cost { vision, chars_per_token: DEFAULT_CHARS_PER_TOKEN }
    }

    #[test]
    fn the_ides_messages_dont_start_a_request() {
        // Over budget, with a check-failure report after the work: the
        // user's request and its run are the current request, not old turns.
        let history = vec![
            msg("user", "old question"),
            msg("assistant", &"old answer ".repeat(4_000)),
            msg("user", "fix the bug"),
            call(&["a"]),
            tool("a", BIG),
            harness("[Automatic check by the IDE] tsc failed"),
        ];
        let (out, info) = fit(history, 12_000, false);
        assert!(info.dropped_turns >= 1, "the old turn goes first");
        assert!(out.iter().any(|m| m.text.contains("fix the bug")), "the request is kept");
        assert!(out.last().unwrap().harness, "the report is kept");
        assert!(!out.iter().any(|m| m.harness && m.text.contains("omitted")), "no note glued onto the report");
    }

    /// `n` reads of `path`, each with its own result.
    fn reads(n: usize, path: &str, size: usize) -> Vec<Msg> {
        (0..n)
            .flat_map(|i| {
                let id = format!("r{i}");
                vec![call_with(&id, "read", json!({ "path": path })), result(&id, &"x".repeat(size))]
            })
            .collect()
    }

    #[test]
    fn outdated_content_waits_for_its_threshold() {
        let mut history = vec![msg("user", "q")];
        history.extend(reads(20, "src/a.ts", 100));
        let (out, info) = fit_with(history.clone(), 1_000_000, generous(false));
        assert_eq!(info.outdated, 0);
        assert_eq!(out.len(), history.len());
    }

    #[test]
    fn out_of_date_reads_are_replaced_in_batches_and_recent_ones_kept() {
        let mut history = vec![msg("user", "q")];
        history.extend(reads(20, "src/a.ts", 3_000));
        // 20 results, 8 kept recent: 12 replaceable, rounded down to 8.
        let (out, info) = fit_with(history, 10_000, generous(false));
        let stubs: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, m)| m.text.starts_with("[Read src/a.ts here"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(stubs.len(), 8, "one batch of 8");
        assert!(info.outdated >= 8);
        assert_eq!(out.last().unwrap().text.len(), 3_000, "the latest read is whole");
        assert!(pairing_ok(&out));
    }

    #[test]
    fn a_read_nothing_replaced_is_kept() {
        let mut history = vec![msg("user", "q")];
        history.extend(reads(1, "src/only.ts", 3_000));
        for i in 0..16 {
            let id = format!("o{i}");
            history.push(call_with(&id, "read", json!({ "path": format!("src/f{i}.ts") })));
            history.push(result(&id, &"y".repeat(3_000)));
        }
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(out[2].text.len(), 3_000, "still the model's only copy of that file");
    }

    #[test]
    fn a_partial_read_doesnt_replace_what_it_didnt_show() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with("ra", "read", json!({ "path": "src/big.ts", "start_line": 900, "end_line": 960 })));
        history.push(result("ra", &"a".repeat(3_000)));
        let overview = format!("{}src/big.ts has 4000 lines…]\n{}", crate::read_file::PARTIAL_PREFIX, "m".repeat(3_000));
        history.push(call_with("rb", "read", json!({ "path": "src/big.ts" })));
        history.push(result("rb", &overview));
        for i in 0..16 {
            let id = format!("o{i}");
            history.push(call_with(&id, "read", json!({ "path": format!("src/f{i}.ts") })));
            history.push(result(&id, &"y".repeat(3_000)));
        }
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(out[2].text.len(), 3_000, "lines 900-960 are still the model's only copy");
    }

    #[test]
    fn a_symbol_read_covers_only_its_lines() {
        // Lines 1-50 read, then one function (lines 120-160) by symbol: the
        // first read isn't replaced, the symbol read didn't cover it.
        let mut history = vec![msg("user", "q")];
        history.push(call_with("ra", "read", json!({ "path": "src/a.rs", "start_line": 1, "end_line": 50 })));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with("rs", "read", json!({ "path": "src/a.rs", "symbol": "Parser::parse" })));
        history.push(result("rs", &format!("[Symbol read: Parser > parse, lines 120-160 of src/a.rs]\n{}", "s".repeat(3_000))));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(out[2].text.len(), 3_000, "lines 1-50 are still the model's only copy");
    }

    #[test]
    fn a_later_read_covering_a_symbol_read_replaces_it() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with("rs", "read", json!({ "path": "src/a.rs", "symbol": "parse" })));
        history.push(result("rs", &format!("[Symbol read: parse, lines 120-160 of src/a.rs]\n{}", "s".repeat(3_000))));
        history.push(call_with("rr", "read", json!({ "path": "src/a.rs", "start_line": 100, "end_line": 200 })));
        history.push(result("rr", &"r".repeat(3_000)));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert!(out[2].text.starts_with("[Read src/a.rs lines 120-160 here"), "{}", out[2].text);
    }

    #[test]
    fn a_symbol_read_that_found_nothing_covers_nothing() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with("ra", "read", json!({ "path": "src/a.rs", "start_line": 1, "end_line": 50 })));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with("rs", "read", json!({ "path": "src/a.rs", "symbol": "nope" })));
        history.push(result("rs", "No declaration \"nope\" in src/a.rs."));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(out[2].text.len(), 3_000);
    }

    #[test]
    fn edits_supersede_reads_and_old_write_arguments_shrink() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with("w0", "write", json!({ "path": "src/new.ts", "content": "line\n".repeat(400) })));
        history.push(result("w0", "wrote 2000 bytes to src/new.ts"));
        history.push(call_with("ra", "read", json!({ "path": "src/a.ts", "start_line": 1, "end_line": 50 })));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with("e0", "write", json!({ "path": "src/a.ts", "old_text": "o".repeat(500), "content": "n" })));
        history.push(result("e0", "Edited src/a.ts: replaced line 3"));
        history.extend(reads(14, "src/other.ts", 2_000));
        let (out, _) = fit_with(history, 12_000, generous(false));

        let write_args: Value = serde_json::from_str(&out[1].tool_calls.as_ref().unwrap()[0].arguments).unwrap();
        assert_eq!(write_args["path"], "src/new.ts", "arguments stay valid JSON with the path");
        assert_eq!(write_args["content"], "[400 lines written; left out of this history]");
        assert!(out[4].text.contains("Read src/a.ts lines 1-50 here") && out[4].text.contains("changed later"));
        let edit_args: Value = serde_json::from_str(&out[5].tool_calls.as_ref().unwrap()[0].arguments).unwrap();
        assert_eq!(edit_args["content"], "n", "short arguments are kept");
        assert!(edit_args["old_text"].as_str().unwrap().contains("left out"));
    }

    #[test]
    fn a_rerun_command_and_earlier_check_rounds_shrink() {
        let mut history = vec![msg("user", "q")];
        let out_text = format!("$ npm test\nexit code 1, 3.0 s\n{}", "fail\n".repeat(800));
        history.push(call_with("c0", "bash", json!({ "command": "npm test" })));
        history.push(result("c0", &out_text));
        history.push(harness(&format!("[Automatic check by the IDE, not written by the user]\nround 1\n{}", "e".repeat(3_000))));
        history.push(call_with("c1", "bash", json!({ "command": "npm test" })));
        history.push(result("c1", &out_text));
        history.push(harness("[Automatic check by the IDE, not written by the user]\nround 2"));
        history.extend(reads(14, "src/x.ts", 1_500));
        let (out, _) = fit_with(history, 9_000, generous(false));
        assert!(out[2].text.starts_with("$ npm test\nexit code 1") && out[2].text.contains("ran again later"));
        assert!(out[3].text.starts_with("[Earlier round of checks"));
        assert!(out[6].text.contains("round 2"), "the latest round is whole");
    }

    #[test]
    fn only_attached_screenshots_count() {
        let mut ms = vec![msg("user", "q")];
        for i in 0..5 {
            let id = format!("s{i}");
            ms.push(call(&[&id]));
            let mut t = tool(&id, 10);
            t.attachments = vec!["img".into()];
            ms.push(t);
        }
        let with = total(&ms, generous(true));
        let without = total(&ms, generous(false));
        assert_eq!(with - without, MAX_IMAGES * IMAGE_TOKENS);
    }
}

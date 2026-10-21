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
//! Once over the budget, 2 and 3 go down to TRIM_TO of it, and the chat's
//! next requests keep that cut while they fit the budget (`Cut`): the start
//! of the request, which providers cache, moves once in several steps.
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
//! Nor are the messages the user sends while the run works (`steer` in
//! their meta, steering-plan.md): they're part of the request they were
//! sent in.
//! Only the copy sent to the model is trimmed; stored messages are untouched.
//!
//! Once a run is long, the request ends with a focus note (`focus_note`):
//! the user's request and the agent's checklist, restated at the end where
//! the model attends to them most.

use super::attachments;
use super::store::{
    char_prefix, get_message_body, get_messages, tool_call_extras, DbResult, ToolCall,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// How far back a request can reach at most, before any budget applies.
const MAX_HISTORY_ROWS: usize = 2000;
const PAGE: usize = 500;
const MESSAGE_OVERHEAD: usize = 4;
const IMAGE_TOKENS: usize = 1000;
/// A PDF page goes to the model as its text and an image of it: 1,500 to
/// 3,000 tokens, Anthropic says, and about as much at OpenAI.
const PDF_PAGE_TOKENS: usize = 2000;
const TOOL_KEEP_CHARS: usize = 1200;
/// Messages whose files are re-sent on later turns (browser_screenshot
/// results, earlier requests' images and PDFs); older ones become a note.
/// The current request's own files are always sent.
const MAX_IMAGES: usize = 2;
const OMITTED_NOTE: &str =
    "[Earlier messages in this chat were omitted to fit the context window.]";
/// Once over budget, trimming goes down to this share of it, not just
/// under, and the next requests keep that cut while they fit the budget
/// (`Cut`): the steps in between fit in the room left. Trimming to just
/// under the budget, or afresh for every request, would drop one more turn
/// (or cut one more result) every step or two, and the start of the
/// request, which providers cache, would change every time.
const TRIM_TO: f64 = 0.85;
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
    /// What the user typed, when the message is sent in another form
    /// (`text`: with the editor context and file references the frontend
    /// put before it, `meta.sentContent`). The focus note quotes this: the
    /// request in the user's words. None: `text` is what was typed.
    pub typed: Option<String>,
    /// Characters of the full content (the text may be a preview).
    pub full_chars: usize,
    /// The full body still has to be loaded before sending.
    pub needs_body: bool,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Written by the IDE in the user role (check failures), not by the user.
    pub harness: bool,
    /// Sent by the user while the run worked (`meta.steer`): their words,
    /// part of the request it was sent in, never a request of its own.
    pub steer: bool,
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

/// What a model takes besides text: the settings' Vision and PDF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Accepts {
    pub images: bool,
    pub pdf: bool,
}

/// A file a message carries: a screenshot, or an image or a PDF the user
/// attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StoredAttachment")]
pub struct Attachment {
    pub id: String,
    /// A PDF's name and pages, from its message's `meta.pdfs`; None for an image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfInfo {
    pub name: String,
    pub pages: usize,
}

/// An attachment as a CLM context keeps it: from before PDFs, an id alone.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredAttachment {
    Id(String),
    Full {
        id: String,
        #[serde(default)]
        pdf: Option<PdfInfo>,
    },
}

impl From<StoredAttachment> for Attachment {
    fn from(stored: StoredAttachment) -> Self {
        match stored {
            StoredAttachment::Id(id) => Attachment::image(id),
            StoredAttachment::Full { id, pdf } => Attachment { id, pdf },
        }
    }
}

impl Attachment {
    pub fn image(id: impl Into<String>) -> Self {
        Attachment {
            id: id.into(),
            pdf: None,
        }
    }

    /// Whether a model that takes `accepts` takes this file.
    pub fn taken_by(&self, accepts: Accepts) -> bool {
        if self.pdf.is_some() {
            accepts.pdf
        } else {
            accepts.images
        }
    }

    fn tokens(&self) -> usize {
        self.pdf
            .as_ref()
            .map_or(IMAGE_TOKENS, |pdf| pdf.pages.max(1) * PDF_PAGE_TOKENS)
    }

    /// The content part it's sent as, from its `data:` URL.
    fn wire_part(&self, url: String) -> Value {
        match &self.pdf {
            Some(pdf) => {
                json!({ "type": "file", "file": { "filename": pdf.name, "file_data": url } })
            }
            None => json!({ "type": "image_url", "image_url": { "url": url } }),
        }
    }
}

/// A stored message's attachments, its PDFs named as `meta.pdfs` names
/// them (the chat input writes it: lib/composerSegments.ts).
fn attachments_of(m: &super::store::MessageRow) -> Vec<Attachment> {
    let pdfs: HashMap<&str, PdfInfo> = m
        .meta
        .as_ref()
        .and_then(|meta| meta.get("pdfs")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(|pdf| {
            let info = PdfInfo {
                name: pdf.get("name")?.as_str()?.to_string(),
                pages: pdf.get("pages")?.as_u64()? as usize,
            };
            Some((pdf.get("id")?.as_str()?, info))
        })
        .collect();
    m.attachments
        .iter()
        .map(|id| Attachment {
            id: id.clone(),
            pdf: pdfs.get(id.as_str()).cloned(),
        })
        .collect()
}

/// How token counts are estimated for one request.
#[derive(Clone, Copy, Debug)]
pub struct Cost {
    pub accepts: Accepts,
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

/// Which messages' files go with a request, by index (`to_wire` attaches
/// exactly these, of the kinds the model takes): the current request's
/// (the files the user sent with it and with the messages they sent while
/// the run worked, needed for the whole run), and those of the last
/// MAX_IMAGES other messages that have any the model takes.
fn attached_files(messages: &[Msg], accepts: Accepts) -> Vec<bool> {
    let takes = |m: &Msg| m.attachments.iter().any(|a| a.taken_by(accepts));
    let mut attached = vec![false; messages.len()];
    let request = messages.iter().rposition(starts_request);
    let users: Vec<usize> = match request {
        Some(i) => std::iter::once(i)
            .chain((i + 1..messages.len()).filter(|&k| messages[k].steer))
            .collect(),
        None => Vec::new(),
    };
    for &i in &users {
        attached[i] = takes(&messages[i]);
    }
    let others = (0..messages.len())
        .rev()
        .filter(|&i| !users.contains(&i) && takes(&messages[i]))
        .take(MAX_IMAGES);
    for i in others {
        attached[i] = true;
    }
    attached
}

/// Files count only where to_wire actually attaches them
/// (`attached_files`): the others are sent as a note.
pub fn total(messages: &[Msg], cost: Cost) -> usize {
    let attached = attached_files(messages, cost.accepts);
    messages
        .iter()
        .zip(attached)
        .map(|(m, a)| {
            let files = m
                .attachments
                .iter()
                .filter(|f| a && f.taken_by(cost.accepts));
            estimate(m, cost) + files.map(Attachment::tokens).sum::<usize>()
        })
        .sum()
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

/// How a note names files: "an image", "3 images", "the PDF a.pdf",
/// "2 PDFs (a.pdf, b.pdf) and an image".
fn files_phrase(files: &[&Attachment]) -> String {
    let pdfs: Vec<&str> = files
        .iter()
        .filter_map(|f| Some(f.pdf.as_ref()?.name.as_str()))
        .collect();
    let mut parts = Vec::new();
    match pdfs.as_slice() {
        [] => {}
        [name] => parts.push(format!("the PDF {name}")),
        names => parts.push(format!("{} PDFs ({})", names.len(), names.join(", "))),
    }
    match files.len() - pdfs.len() {
        0 => {}
        1 => parts.push("an image".into()),
        n => parts.push(format!("{n} images")),
    }
    parts.join(" and ")
}

/// Why a user message's files aren't sent with it.
enum Unsent {
    /// Earlier requests' files go first when the history is over budget.
    LeftOut,
    /// The model isn't set to take their kind.
    NotTaken,
    /// Older than the last few messages with files (`attached_files`).
    NoLongerShown,
}

/// What a user message says in place of files that aren't sent with it.
fn user_files_note(files: &[&Attachment], why: Unsent) -> String {
    let phrase = files_phrase(files);
    let mut phrase_chars = phrase.chars();
    let phrase = phrase_chars
        .next()
        .map(|c| c.to_uppercase().chain(phrase_chars).collect::<String>())
        .unwrap_or_default();
    let one = files.len() == 1;
    match why {
        Unsent::LeftOut => {
            let were = if one { "was" } else { "were" };
            format!("[{phrase} the user attached here {were} left out to fit the context window.]")
        }
        Unsent::NoLongerShown => {
            let are = if one { "is" } else { "are" };
            format!("[{phrase} the user attached here {are} no longer shown.]")
        }
        Unsent::NotTaken => {
            let were = if one { "wasn't" } else { "weren't" };
            let pdfs = files.iter().any(|f| f.pdf.is_some());
            let images = files.iter().any(|f| f.pdf.is_none());
            let what = match (images, pdfs) {
                (true, true) => "images or PDFs",
                (false, true) => "PDFs",
                _ => "images",
            };
            format!(
                "[{phrase} the user attached here {were} sent: this model isn't set up to accept {what}.]"
            )
        }
    }
}

/// The text with a note after it (alone when there's no text).
fn with_note(text: &str, note: &str) -> String {
    if text.trim().is_empty() {
        note.to_string()
    } else {
        format!("{text}\n{note}")
    }
}

/// An earlier request's files, left out to fit the context window; a note
/// says they were there.
fn drop_user_files(m: &mut Msg) {
    if m.attachments.is_empty() {
        return;
    }
    let files: Vec<&Attachment> = m.attachments.iter().collect();
    let note = user_files_note(&files, Unsent::LeftOut);
    m.text = with_note(&m.text, &note);
    m.full_chars = m.text.chars().count();
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
    /// Where the history was cut, for the chat's next request.
    pub cut: Cut,
}

/// Where a request's history was cut (`fit_keeping`), so the chat's next
/// request can be cut at the same place while that still fits: the start
/// of the request, which providers cache, then stays the same.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cut {
    /// The first message kept once the oldest turns were dropped; None
    /// when none were.
    first_kept: Option<String>,
    /// The latest of the current request's results cut short (they're cut
    /// oldest first); None when none were.
    shortened_through: Option<String>,
}

/// Each chat's latest cut, for its next request. In memory only: after a
/// restart, the first request cuts afresh.
static CUTS: LazyLock<Mutex<HashMap<String, Cut>>> = LazyLock::new(Default::default);

/// The cut the chat's last request was sent with, if it was cut.
pub fn last_cut(dialog_id: &str) -> Option<Cut> {
    CUTS.lock().ok()?.get(dialog_id).cloned()
}

/// Keeps the cut a request of the chat was sent with, for its next one.
pub fn keep_cut(dialog_id: &str, cut: Cut) {
    if let Ok(mut cuts) = CUTS.lock() {
        if cut == Cut::default() {
            cuts.remove(dialog_id);
        } else {
            cuts.insert(dialog_id.to_string(), cut);
        }
    }
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
    let Some(text) = args.get(key).and_then(|v| v.as_str()) else {
        return false;
    };
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
    // Each result's call (name and arguments): the one with its id in the
    // reply it follows. Ids aren't unique across a chat (Gemini numbers
    // them per reply: "function-call-1"), so they are only looked up
    // within the reply, never across the whole chat.
    let mut calls: std::collections::HashMap<String, (String, Value)> = Default::default();
    let mut resolved: Vec<Option<(String, Value)>> = Vec::with_capacity(messages.len());
    for m in messages.iter() {
        if m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|t| !t.is_empty()) {
            calls.clear();
            for c in m.tool_calls.iter().flatten() {
                let args = serde_json::from_str(&c.arguments).unwrap_or(Value::Null);
                calls.insert(c.id.clone(), (c.name.clone(), args));
            }
        } else if starts_request(m) {
            calls.clear();
        }
        resolved.push(match (m.role.as_str(), &m.tool_call_id) {
            ("tool", Some(id)) => calls.get(id).cloned(),
            _ => None,
        });
    }
    let events: Vec<Event> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "tool")
        .filter_map(|(at, m)| {
            let (name, args) = resolved[at].as_ref()?;
            let start = line_arg(args, "start_line");
            let end = line_arg(args, "end_line");
            // A read by symbol holds the lines its result names: not the
            // whole file. (One that names none, an error or a partial
            // read, covers nothing below.)
            let by_symbol = args
                .get("symbol")
                .and_then(|s| s.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            Some(Event {
                at,
                name: name.clone(),
                path: args.get("path").and_then(|p| p.as_str()).map(norm_path),
                range: match (start, end) {
                    _ if by_symbol => {
                        Some(crate::read_symbol::covered_lines(&m.text).unwrap_or((0, 0)))
                    }
                    (None, None) => None,
                    (s, e) => Some((s.unwrap_or(1), e.unwrap_or(u64::MAX))),
                },
                command: args
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.trim().to_string()),
                ok: !m.text.starts_with("Error:"),
                partial: m.text.starts_with(crate::read_file::PARTIAL_PREFIX),
            })
        })
        .collect();

    let replaceable =
        events.len().saturating_sub(KEEP_RECENT_RESULTS) / OUTDATED_BATCH * OUTDATED_BATCH;
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
    let reports: Vec<usize> = (0..messages.len())
        .filter(|&i| messages[i].harness)
        .collect();
    if let Some((_, earlier)) = reports.split_last() {
        for &i in earlier {
            if messages[i].text.starts_with("[Earlier") {
                continue;
            }
            let head = messages[i]
                .text
                .lines()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n");
            replace_text(
                &mut messages[i],
                format!("[Earlier round of checks, left out of this history; a later round follows.]\n{head}"),
            );
            changed += 1;
        }
    }
    changed
}

/// A user message that starts a request: not one the IDE wrote, nor one
/// the user sent while the run worked (steering-plan.md, 3.6).
pub fn starts_request(m: &Msg) -> bool {
    m.role == "user" && !m.harness && !m.steer
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
/// The messages the user sent while the run worked that the note quotes
/// (the latest), and how much of each.
const FOCUS_STEERS: usize = 5;
const FOCUS_STEER_CHARS: usize = 300;

/// A typed request shorter than this says nothing about the task on its own
/// ("continue"): the focus note quotes the previous real request as well.
const SHORT_REQUEST_CHARS: usize = 80;

fn todo_in_msg(m: &Msg) -> Option<Vec<crate::agent_run::plan::TodoItem>> {
    m.tool_calls
        .iter()
        .flatten()
        .rev()
        .filter(|c| c.name == "todo")
        .find_map(|c| match serde_json::from_str::<Value>(&c.arguments) {
            Ok(Value::Object(args)) => {
                Some(crate::agent_run::plan::parse_todo(&args)).filter(|i| !i.is_empty())
            }
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
    let latest = run
        .iter()
        .rev()
        .find_map(|m| todo_in_msg(m).map(|items| (items, m.checklist_closed)));
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
    if checklist
        .as_ref()
        .is_some_and(|items| items.iter().all(crate::agent_run::plan::is_settled))
    {
        return None;
    }
    // In the user's words: the editor context and the references'
    // instructions the frontend sends before them (`Msg::typed`) would fill
    // the quote and read as being told to go and read those files again.
    let text = typed_text(&messages[start]).trim();
    let request = char_prefix(text, FOCUS_REQUEST_CHARS);
    let more = if request.len() < text.len() || messages[start].needs_body {
        " …"
    } else {
        ""
    };
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
    let files: Vec<&Attachment> = messages[start].attachments.iter().collect();
    if !files.is_empty() {
        let files = files_phrase(&files);
        note.push_str(&format!("\nThe user attached {files} to the request."));
    }
    // What the user added while the run worked (steering-plan.md, D11): the
    // instructions given mid-run are the ones most often lost.
    let steers: Vec<&Msg> = run.iter().filter(|m| m.steer).collect();
    if !steers.is_empty() {
        note.push_str("\nAdded by the user while you worked (quoted, oldest first):");
        let earlier = steers.len().saturating_sub(FOCUS_STEERS);
        if earlier > 0 {
            let messages = if earlier == 1 { "message" } else { "messages" };
            note.push_str(&format!("\n(and {earlier} earlier {messages})"));
        }
        for m in &steers[earlier..] {
            let text = typed_text(m).trim();
            let quoted = char_prefix(text, FOCUS_STEER_CHARS);
            let more = if quoted.len() < text.len() || m.needs_body {
                " …"
            } else {
                ""
            };
            for (k, line) in format!("{quoted}{more}").lines().enumerate() {
                let lead = if k == 0 { "- >" } else { "  >" };
                note.push_str(&format!("\n{lead} {line}"));
            }
        }
    }
    if text.chars().count() < SHORT_REQUEST_CHARS {
        if let Some(prev) = messages[..start].iter().rposition(starts_request) {
            let prev_text = typed_text(&messages[prev]).trim();
            if !prev_text.is_empty() && prev_text != text {
                let prev_request = char_prefix(prev_text, FOCUS_REQUEST_CHARS);
                let prev_more = if prev_request.len() < prev_text.len() || messages[prev].needs_body
                {
                    " …"
                } else {
                    ""
                };
                note.push_str("\nThe earlier request it continues (quoted):");
                for line in format!("{prev_request}{prev_more}").lines() {
                    note.push_str(&format!("\n> {line}"));
                }
            }
        }
    }
    if let Some(items) = checklist {
        let done = items
            .iter()
            .filter(|i| crate::agent_run::plan::is_settled(i))
            .count();
        note.push_str(&format!(
            "\nYour checklist ({done} of {} done):",
            items.len()
        ));
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
        let ago = if back == 1 {
            "1 call".to_string()
        } else {
            format!("{back} calls")
        };
        note.push_str(&format!(
            "\nLoop warning {ago} ago: {reason}. Don't repeat it: change the approach, or stop and ask the user."
        ));
    }
    note.push_str("\n[end of reminder]");
    Some(note)
}

/// A message's text as the user typed it (`Msg::typed`).
fn typed_text(m: &Msg) -> &str {
    m.typed.as_deref().unwrap_or(&m.text)
}

/// Puts what the IDE made for the request (the repository map:
/// `ChatParams::request_context`) on the message of that request (by id),
/// before its text, on every step of the run: the request is then sent the
/// same way on every step, in the part providers cache, and ends with the
/// user's words. Each request ends with the step the model works from,
/// not with a map of where to start, which made it start over. Without
/// that message, it goes on the last one.
pub fn add_request_context(messages: &mut [Msg], request_id: Option<&str>, context: &str) {
    let at = request_id
        .and_then(|id| messages.iter().rposition(|m| m.id == id))
        .or(messages.len().checked_sub(1));
    if let Some(m) = at.map(|i| &mut messages[i]) {
        m.text = if m.text.trim().is_empty() {
            context.to_string()
        } else {
            format!("{context}\n\n{}", m.text)
        };
        m.full_chars = m.text.chars().count();
    }
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
pub fn fit(messages: Vec<Msg>, budget: usize, accepts: Accepts) -> (Vec<Msg>, FitInfo) {
    fit_with(
        messages,
        budget,
        Cost {
            accepts,
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
        },
    )
}

/// The budget rules on resolved-or-estimated messages (pure, unit-tested),
/// cutting afresh.
pub fn fit_with(messages: Vec<Msg>, budget: usize, cost: Cost) -> (Vec<Msg>, FitInfo) {
    fit_keeping(messages, budget, cost, None)
}

/// The history's size from message `k` on, as sent once the ones before it
/// are dropped: the first kept message then says so (OMITTED_NOTE).
fn size_from(messages: &[Msg], k: usize, cost: Cost) -> usize {
    let size = total(&messages[k..], cost);
    match messages.get(k) {
        Some(first) if k > 0 && first.role == "user" => {
            let note = OMITTED_NOTE.chars().count() + 2;
            size + cost.tokens(first.full_chars + note) - cost.tokens(first.full_chars)
        }
        _ => size,
    }
}

/// The budget rules, keeping the cut the chat's last request was sent with
/// (`kept`) while the history still fits the budget with it. Once it
/// doesn't, it's cut afresh, down to TRIM_TO of the budget: the steps after
/// that fit in the room left, at the same cut, so the start of the request
/// moves once in several steps, not every step or two.
pub fn fit_keeping(
    mut messages: Vec<Msg>,
    budget: usize,
    cost: Cost,
    kept: Option<&Cut>,
) -> (Vec<Msg>, FitInfo) {
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
    let info = |trimmed: bool, dropped_turns: usize, estimated_tokens: usize, cut: Cut| FitInfo {
        focus_note: false,
        trimmed,
        dropped_turns,
        estimated_tokens,
        full_tokens,
        outdated,
        cut,
    };
    if sum <= budget {
        return (messages, info(false, 0, sum, Cut::default()));
    }
    // Over budget: trimmed to the target, with room for the next steps.
    let target = (budget as f64 * TRIM_TO) as usize;

    let mut current_start = messages.iter().rposition(starts_request).unwrap_or(0);

    // 1. Shrink old tool results; earlier requests' files go too.
    for m in messages.iter_mut().take(current_start) {
        if m.role == "tool" {
            truncate_tool_result(m);
        } else if m.role == "user" {
            drop_user_files(m);
        }
    }
    sum = total(&messages, cost);
    if sum <= budget {
        return (messages, info(true, 0, sum, Cut::default()));
    }

    // 2. Drop the oldest whole turns: those the last request dropped while
    //    the rest fits, else as many as it takes to get down to the target.
    let turn_starts: Vec<usize> = (0..current_start)
        .filter(|&i| i == 0 || starts_request(&messages[i]))
        .collect();
    let kept_from = kept
        .and_then(|c| c.first_kept.as_deref())
        .and_then(|id| messages[..=current_start].iter().position(|m| m.id == id))
        .filter(|k| *k == current_start || turn_starts.contains(k))
        .filter(|&k| size_from(&messages, k, cost) <= budget);
    let (cut, dropped) = match kept_from {
        Some(k) => (k, turn_starts.iter().filter(|&&s| s < k).count()),
        None => {
            let (mut cut, mut dropped) = (0, 0);
            for (t, &start) in turn_starts.iter().enumerate() {
                if sum <= target {
                    break;
                }
                let next = turn_starts.get(t + 1).copied().unwrap_or(current_start);
                // No images left before the request (step 1).
                sum -= total(&messages[start..next], cost);
                cut = next;
                dropped += 1;
            }
            (cut, dropped)
        }
    };
    let first_kept = (cut > 0).then(|| messages[cut].id.clone());
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
        let cut = Cut {
            first_kept,
            shortened_through: None,
        };
        return (messages, info(true, dropped, sum, cut));
    }

    // 3. Last resort: earlier tool results of the current request, oldest
    //    first, sparing the most recent one: those the last request cut
    //    while the rest fits, else as many as it takes to get down to the
    //    target.
    let tool_indexes: Vec<usize> = (current_start..messages.len())
        .filter(|&i| messages[i].role == "tool")
        .collect();
    let mut shortened_through = None;
    if let Some((_, earlier)) = tool_indexes.split_last() {
        let mut through = kept
            .and_then(|c| c.shortened_through.as_deref())
            .and_then(|id| earlier.iter().position(|&i| messages[i].id == id));
        if let Some(p) = through {
            for &i in &earlier[..=p] {
                truncate_tool_result(&mut messages[i]);
            }
            sum = total(&messages, cost);
        }
        if through.is_none() || sum > budget {
            let from = through.map_or(0, |p| p + 1);
            for (p, &i) in earlier.iter().enumerate().skip(from) {
                if sum <= target {
                    break;
                }
                truncate_tool_result(&mut messages[i]);
                sum = total(&messages, cost);
                through = Some(p);
            }
        }
        shortened_through = through.map(|p| messages[earlier[p]].id.clone());
    }
    // Still over: send anyway; trimming the user's words or the latest tool
    // result would change the task.
    let cut = Cut {
        first_kept,
        shortened_through,
    };
    (messages, info(true, dropped, sum, cut))
}

/// Loads, fits and converts a chat's history to Chat Completions messages,
/// keeping the cut its last request was sent with (`kept`) while it fits.
/// `request_context` goes on the current request's message
/// (`add_request_context`); `token_budget` doesn't include it.
#[allow(clippy::too_many_arguments)]
pub fn build_wire(
    conn: &Connection,
    data_dir: &Path,
    dialog_id: &str,
    model: &str,
    token_budget: usize,
    accepts: Accepts,
    current_user_content: Option<&str>,
    request_context: Option<&str>,
    gemini: bool,
    chars_per_token: f64,
    kept: Option<&Cut>,
    window: Option<&Window>,
) -> DbResult<(Vec<Value>, FitInfo)> {
    let mut messages = load_messages_in(conn, dialog_id, None, window)?;
    attach_reasoning(conn, &mut messages, model)?;
    let cost = Cost {
        accepts,
        chars_per_token,
    };
    // The guard's loop warnings end long results too: the latest ones whole.
    let tail = messages.len().saturating_sub(2 * (FOCUS_LOOP_WINDOW + 1));
    load_bodies(conn, &mut messages[tail..])?;
    // The note is made before fitting (which never drops the current
    // request or its calls) and its room kept out of the budget. It quotes
    // the request as typed: the copy sent starts with the editor context.
    let focus = focus_note(&messages);
    use_sent_content(&mut messages, current_user_content);
    let note_tokens = focus
        .as_ref()
        .map_or(0, |n| cost.tokens(n.chars().count() + 2));
    let (mut fitted, mut info) = fit_keeping(
        messages,
        token_budget.saturating_sub(note_tokens),
        cost,
        kept,
    );
    load_bodies(conn, &mut fitted)?;
    if let Some(context) = request_context {
        let request = fitted
            .iter()
            .rfind(|m| starts_request(m))
            .map(|m| m.id.clone());
        add_request_context(&mut fitted, request.as_deref(), context);
    }
    if let Some(note) = focus {
        add_focus_note(&mut fitted, &note);
        info.estimated_tokens += note_tokens;
        info.focus_note = true;
    }
    Ok((to_wire(&fitted, data_dir, accepts, gemini), info))
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
                && (i >= current
                    || (deepseek && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())))
        })
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = wanted.iter().map(|&i| messages[i].id.as_str()).collect();
    let stored = super::store::reasoning_of(conn, &ids)?;
    for i in wanted {
        let Some(r) = stored.get(&messages[i].id) else {
            continue;
        };
        if r.get("model").and_then(|m| m.as_str()) != Some(model) {
            continue;
        }
        let details = r
            .get("details")
            .filter(|d| d.as_array().is_some_and(|a| !a.is_empty()));
        let text = r
            .get("text")
            .filter(|t| t.as_str().is_some_and(|t| !t.is_empty()));
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

/// Where a stored chat's history starts for a request (Autopilot's fresh
/// contexts, D2): the chat's rows from `from_seq` on, and before them the
/// row `keep` (the task's request), when it's the chat's. Without one, the
/// whole chat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub from_seq: i64,
    pub keep: Option<String>,
}

/// The chat's history as it can be sent, before any budget: what is never
/// sent is left out, unanswered tool calls get a result, and the current
/// request carries `current_user_content`. Long bodies may still need
/// `load_bodies`.
pub fn load_messages(
    conn: &Connection,
    dialog_id: &str,
    current_user_content: Option<&str>,
) -> DbResult<Vec<Msg>> {
    load_messages_in(conn, dialog_id, current_user_content, None)
}

/// `load_messages`, within `window` when one is given.
pub fn load_messages_in(
    conn: &Connection,
    dialog_id: &str,
    current_user_content: Option<&str>,
    window: Option<&Window>,
) -> DbResult<Vec<Msg>> {
    // Newest pages first, until the cap, or the window's start.
    let mut rows = Vec::new();
    let mut before = None;
    loop {
        let page = get_messages(conn, dialog_id, before, PAGE)?;
        let has_older = page.has_older;
        before = page.messages.first().map(|m| m.seq);
        let mut batch = page.messages;
        batch.append(&mut rows);
        rows = batch;
        let past_start = window.is_some_and(|w| before.is_some_and(|b| b <= w.from_seq));
        if !has_older || rows.len() >= MAX_HISTORY_ROWS || past_start {
            break;
        }
    }
    // The cap can fall between a reply and its results: a result whose
    // call was left behind can't start the history (providers reject a
    // `tool` message with no call before it).
    if rows.len() >= MAX_HISTORY_ROWS {
        let first_call = rows
            .iter()
            .position(|m| m.role != "tool")
            .unwrap_or(rows.len());
        rows.drain(..first_call);
    }
    // A window (D2): its rows, after the one it keeps before them.
    if let Some(w) = window {
        let kept = |m: &super::store::MessageRow| w.keep.as_deref() == Some(m.id.as_str());
        rows.retain(|m| m.seq >= w.from_seq || kept(m));
        if let Some(keep) = w.keep.as_ref().filter(|_| !rows.iter().any(kept)) {
            let request: Vec<_> = super::store::messages_by_id(conn, std::slice::from_ref(keep))?
                .into_iter()
                .filter(|m| m.dialog_id == dialog_id && m.seq < w.from_seq)
                .collect();
            rows.splice(0..0, request);
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
            !(m.role == "assistant"
                && m.content.trim().is_empty()
                && m.tool_calls.as_ref().is_none_or(|t| t.is_empty()))
        })
        .map(|m| {
            // The stored content is what was typed (or its preview, for a
            // long one: enough for the focus note's quote).
            let typed = sent_content(&m).map(|_| m.content.clone());
            let (text, full_chars, needs_body) = match sent_content(&m) {
                Some(sent) => (sent.to_string(), sent.chars().count(), false),
                None if m.body_external => (m.content.clone(), m.body_bytes as usize, true),
                None => (m.content.clone(), m.content.chars().count(), false),
            };
            let attachments = attachments_of(&m);
            Msg {
                // Added by the IDE (the frontend's verification gate): user-role,
                // but never the user's request.
                harness: m
                    .meta
                    .as_ref()
                    .is_some_and(|meta| meta.get("harness").is_some()),
                steer: m.role == "user"
                    && m.meta
                        .as_ref()
                        .is_some_and(|meta| meta.get("steer").is_some()),
                checklist_closed: m.meta.as_ref().is_some_and(|meta| {
                    meta.get("checklistClosed")
                        .or_else(|| meta.get("checklist_closed"))
                        .is_some_and(|v| *v == serde_json::Value::Bool(true))
                }),
                full_chars,
                needs_body,
                id: m.id,
                role: m.role,
                text,
                typed,
                tool_calls: m.tool_calls,
                tool_call_id: m.tool_call_id,
                attachments,
                reasoning: None,
            }
        })
        .collect();

    // A tool call can be left without a result (the app closed while it
    // waited for approval). Providers reject such a history, and a model
    // that sees no result may assume the call ran, so say it didn't.
    let mut messages = answer_unanswered_calls(messages);
    use_sent_content(&mut messages, current_user_content);
    Ok(messages)
}

/// A user message as it was sent to the model the first time (the editor
/// context and file references the frontend put before the text, kept in
/// `meta.sentContent`). Every later request sends it the same way, so the
/// start of the history doesn't change from one turn to the next and the
/// provider's prompt cache keeps serving it.
fn sent_content(m: &super::store::MessageRow) -> Option<&str> {
    if m.role != "user" {
        return None;
    }
    m.meta.as_ref()?.get("sentContent")?.as_str()
}

/// The current request's user message is sent with the editor context
/// the frontend added; it isn't stored that way.
pub fn use_sent_content(messages: &mut [Msg], current_user_content: Option<&str>) {
    if let (Some(content), Some(last_user)) = (
        current_user_content,
        messages.iter_mut().rev().find(|m| starts_request(m)),
    ) {
        if last_user.typed.is_none() {
            last_user.typed = Some(std::mem::take(&mut last_user.text));
        }
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
                typed: None,
                full_chars: UNANSWERED_NOTE.chars().count(),
                needs_body: false,
                tool_calls: None,
                tool_call_id: Some(id),
                attachments: Vec::new(),
                harness: false,
                steer: false,
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
                open = m
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|c| c.id.clone())
                    .collect();
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
/// one assistant turn must directly follow it). A user message's own
/// images and PDFs go in it, before its text.
pub fn to_wire(messages: &[Msg], data_dir: &Path, accepts: Accepts, gemini: bool) -> Vec<Value> {
    let attach = attached_files(messages, accepts);

    let mut out = Vec::with_capacity(messages.len() + 2);
    let mut pending: Vec<(Option<String>, Vec<String>)> = Vec::new();
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<(Option<String>, Vec<String>)>| {
        for (call_id, urls) in pending.drain(..) {
            let mut parts = vec![json!({
                "type": "text",
                "text": format!("Screenshot from tool call {}:", call_id.unwrap_or_default()).trim().to_string()
            })];
            parts.extend(
                urls.into_iter()
                    .map(|url| json!({ "type": "image_url", "image_url": { "url": url } })),
            );
            out.push(json!({ "role": "user", "content": parts }));
        }
    };

    for (i, m) in messages.iter().enumerate() {
        if m.role != "tool" {
            flush(&mut out, &mut pending);
        }
        let mut content = m.text.clone();
        // The user's images and PDFs, sent in their own message.
        let mut files = Vec::new();
        if m.role == "user" {
            let (mut not_taken, mut gone) = (Vec::new(), Vec::new());
            for file in &m.attachments {
                if !file.taken_by(accepts) {
                    not_taken.push(file);
                    continue;
                }
                match attach[i]
                    .then(|| attachments::data_url(data_dir, &file.id))
                    .flatten()
                {
                    Some(url) => files.push(file.wire_part(url)),
                    None => gone.push(file),
                }
            }
            for (unsent, why) in [(not_taken, Unsent::NotTaken), (gone, Unsent::NoLongerShown)] {
                if !unsent.is_empty() {
                    content = with_note(&content, &user_files_note(&unsent, why));
                }
            }
        } else if !m.attachments.is_empty() {
            let urls: Vec<String> = if attach[i] {
                m.attachments
                    .iter()
                    .filter_map(|file| attachments::data_url(data_dir, &file.id))
                    .collect()
            } else {
                Vec::new()
            };
            if urls.is_empty() {
                content.push_str("\n[The screenshot is no longer attached.]");
            } else {
                pending.push((m.tool_call_id.clone(), urls));
            }
        }

        let inline = m
            .reasoning
            .as_ref()
            .and_then(|r| Some((r.get("inline")?.as_str()?, r.get("text")?.as_str()?)));
        if let Some((tag, text)) = inline {
            content = format!("<{tag}>{text}</{tag}>{content}");
        }
        let mut msg = if files.is_empty() {
            json!({ "role": m.role, "content": content })
        } else {
            // The files, then the text: models read a document or an image
            // best ahead of what's asked about it, and the prompt cache's
            // breakpoint on the text (prompt_cache.rs) then covers them. No
            // empty text part for files alone: some providers reject one.
            let mut parts = files;
            if !content.trim().is_empty() {
                parts.push(json!({ "type": "text", "text": content }));
            }
            json!({ "role": m.role, "content": parts })
        };
        match m
            .reasoning
            .as_ref()
            .map(|r| (r.get("details"), r.get("text")))
        {
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

    /// A model that takes images (Vision), and one that takes text only.
    const IMAGES: Accepts = Accepts {
        images: true,
        pdf: false,
    };
    const TEXT_ONLY: Accepts = Accepts {
        images: false,
        pdf: false,
    };

    fn msg(role: &str, text: &str) -> Msg {
        Msg {
            id: format!("{role}-{}", text.len()),
            role: role.into(),
            text: text.into(),
            typed: None,
            full_chars: text.chars().count(),
            needs_body: false,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
            harness: false,
            steer: false,
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
        let wire = to_wire(&[msg("user", "q"), inline, field], &dir, TEXT_ONLY, false);
        // In its tag, as the model wrote it (MiniMax wants it back unchanged).
        assert_eq!(wire[1]["content"], "<think>Plan.</think>\n\nThe answer.");
        assert!(wire[1].get("reasoning_content").is_none());
        assert_eq!(wire[2]["content"], "Answer.");
        assert_eq!(wire[2]["reasoning_content"], "Plan.");
    }

    fn call(ids: &[&str]) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(
            ids.iter()
                .map(|id| ToolCall {
                    id: id.to_string(),
                    name: "read".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                })
                .collect(),
        );
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
                || ms[..i]
                    .iter()
                    .rev()
                    .take_while(|p| p.role != "user")
                    .any(|p| {
                        p.tool_calls
                            .iter()
                            .flatten()
                            .any(|c| Some(&c.id) == m.tool_call_id.as_ref())
                    })
        })
    }
    const BIG: usize = 30_000;
    const BUDGET_32K: usize = 22_400; // 70% of the default 32k window
    const BUDGET_8K: usize = 5_600;

    #[test]
    fn unanswered_calls_get_a_result() {
        let history = vec![
            msg("user", "q1"),
            call(&["c1", "c2"]),
            tool("c1", 3),
            msg("user", "q2"),
            call(&["c3"]),
        ];
        let out = answer_unanswered_calls(history);
        let ids: Vec<_> = out
            .iter()
            .map(|m| {
                format!(
                    "{}:{}",
                    &m.role[..1],
                    m.tool_call_id.clone().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(ids, ["u:", "a:", "t:c1", "t:c2", "u:", "a:", "t:c3"]);
        assert!(pairing_ok(&out));
        assert_eq!(out[3].text, UNANSWERED_NOTE);
    }

    /// Gemini numbers calls per reply ("function-call-1" in every one):
    /// a result must be read against its own reply's call, not another
    /// reply's call with the same id.
    #[test]
    fn repeated_call_ids_across_replies_stay_apart() {
        let named = |id: &str, name: &str, args: &str| {
            let mut m = msg("assistant", "");
            m.tool_calls = Some(vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args.into(),
                extra_content: None,
            }]);
            m
        };
        let mut history = vec![msg("user", "go")];
        for i in 0..20 {
            // Each a distinct file read or a distinct command: nothing is
            // out of date, so nothing may be replaced.
            if i % 2 == 0 {
                history.push(named(
                    "function-call-1",
                    "read",
                    &format!(r#"{{"path":"f{i}.rs"}}"#),
                ));
            } else {
                history.push(named(
                    "function-call-1",
                    "bash",
                    &format!(r#"{{"command":"echo {i}"}}"#),
                ));
            }
            let mut result = tool("function-call-1", 400);
            result.text = format!("result {i}\n{}", "x".repeat(400));
            history.push(result);
        }
        let before: Vec<String> = history.iter().map(|m| m.text.clone()).collect();
        assert_eq!(replace_outdated(&mut history), 0);
        let after: Vec<String> = history.iter().map(|m| m.text.clone()).collect();
        assert_eq!(before, after);
    }

    fn todo(items: &str) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(vec![ToolCall {
            id: "todo-1".into(),
            name: "todo".into(),
            arguments: items.into(),
            extra_content: None,
        }]);
        m
    }
    fn long_run(request: &str) -> Vec<Msg> {
        let mut history = vec![
            msg("user", "an earlier request"),
            msg("assistant", "done"),
            msg("user", request),
        ];
        for i in 0..FOCUS_AFTER_TOOL_RESULTS {
            let id = format!("c{i}");
            history.push(call(&[&id]));
            history.push(tool(&id, 10));
        }
        history
    }

    /// A user message sent as the frontend sends it: the editor context and
    /// the file references' instructions, then what was typed.
    fn sent(mut m: Msg) -> Msg {
        let typed = std::mem::take(&mut m.text);
        m.text = format!(
            "[Active editor context — file src/a.rs (rust); cursor at line 1, col 1.]\n\n\
             The user explicitly referenced these project files:\n- src/a.rs\n\n\
             Treat these as explicit file references from the user. Use the available file tools \
             to inspect them when relevant.\n\n{typed}"
        );
        m.full_chars = m.text.chars().count();
        m.typed = Some(typed);
        m
    }

    #[test]
    fn focus_note_quotes_the_request_as_typed() {
        let mut history = long_run("continue");
        history[0] = sent(history[0].clone());
        history[2] = sent(history[2].clone());
        let note = focus_note(&history).unwrap();
        // The words the user typed: a short one quotes the request it
        // continues, also as typed.
        assert!(
            note.contains("The request (quoted):\n> continue\n"),
            "{note}"
        );
        assert!(
            note.contains("The earlier request it continues (quoted):\n> an earlier request\n"),
            "{note}"
        );
        // Not the editor context or the references' instructions, which
        // would read as being told to go and read the files again.
        assert!(!note.contains("Active editor context"), "{note}");
        assert!(!note.contains("explicitly referenced"), "{note}");

        // The current request's sent form, set for the request, keeps what
        // was typed too.
        let mut history = long_run("continue");
        use_sent_content(
            &mut history,
            Some("[Active editor context — file b.rs]\n\ncontinue"),
        );
        assert_eq!(history[2].typed.as_deref(), Some("continue"));
        let note = focus_note(&history).unwrap();
        assert!(note.contains("> continue\nThe earlier request"), "{note}");
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
        assert!(
            short_note.contains("The earlier request it continues (quoted):"),
            "{short_note}"
        );
        assert!(short_note.contains("> an earlier request"), "{short_note}");
        assert!(!short_note.contains("attached"), "{short_note}");

        // The request's images are mentioned (they're with the request).
        let mut with_images = long_run("make it look like these");
        let at = with_images.len() - 1 - 2 * FOCUS_AFTER_TOOL_RESULTS;
        with_images[at].attachments = vec![Attachment::image("a"), Attachment::image("b")];
        let images_note = focus_note(&with_images).unwrap();
        assert!(
            images_note.contains("The user attached 2 images to the request."),
            "{images_note}"
        );

        // The latest checklist wins; once every step is settled the agent
        // is wrapping up, and no note pushes it back into the task.
        history.push(todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"in_progress"}]}"#));
        history.push(tool("todo-1", 10));
        assert!(focus_note(&history).unwrap().contains("(1 of 2 done)"));
        history.push(todo(
            r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"skipped"}]}"#,
        ));
        history.push(tool("todo-1", 10));
        assert_eq!(focus_note(&history), None);
        // An open checklist from an earlier request is carried forward.
        let mut earlier = long_run("next");
        earlier.insert(1, todo(r#"{"items":["old step"]}"#));
        assert!(focus_note(&earlier).unwrap().contains("checklist"));
        // ...unless every step is settled: the new request's note stands
        // on its own, without the finished list.
        let mut settled_earlier = long_run("next");
        settled_earlier.insert(
            1,
            todo(r#"{"items":[{"title":"old step","status":"done"}]}"#),
        );
        assert!(!focus_note(&settled_earlier)
            .unwrap()
            .contains("Your checklist"));
        // A dismissed one (on its own message) isn't carried either.
        let mut dismissed_earlier = long_run("next");
        let mut closed = todo(r#"{"items":["old step"]}"#);
        closed.checklist_closed = true;
        dismissed_earlier.insert(1, closed);
        assert!(!focus_note(&dismissed_earlier)
            .unwrap()
            .contains("Your checklist"));
    }

    #[test]
    fn focus_note_carries_a_checklist_into_a_short_request() {
        // A short follow-up ("continue") with a carried checklist quotes the
        // previous real request and lists the carried steps.
        let mut history = long_run("continue");
        history.insert(1, todo(r#"{"items":[{"title":"read","status":"done"},{"title":"fix","status":"pending"}]}"#));
        let note = focus_note(&history).expect("carried checklist with a short request");
        assert!(
            note.contains("The earlier request it continues (quoted):"),
            "{note}"
        );
        assert!(note.contains("> an earlier request"), "{note}");
        assert!(note.contains("Your checklist (1 of 2 done):"), "{note}");
        assert!(
            note.contains("- [done] read") && note.contains("- [pending] fix"),
            "{note}"
        );

        // Dismissed on the new message: not carried.
        let mut dismissed = long_run("continue");
        dismissed.insert(1, todo(r#"{"items":["old step"]}"#));
        let at = dismissed.iter().rposition(starts_request).unwrap();
        dismissed[at].checklist_closed = true;
        let note = focus_note(&dismissed).unwrap();
        assert!(!note.contains("Your checklist"), "{note}");
        assert!(
            note.contains("The earlier request it continues (quoted):"),
            "{note}"
        );
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
        assert!(
            empty_note.contains("The earlier request it continues (quoted):"),
            "{empty_note}"
        );
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

    /// A message the user sent while the run worked (steering-plan.md): its
    /// frame, then what they typed.
    fn steer(text: &str) -> Msg {
        let mut m = msg(
            "user",
            &format!("[The user sent this message while you were working.]\n\n{text}"),
        );
        m.id = format!("steer-{text}");
        m.typed = Some(text.into());
        m.steer = true;
        m
    }

    /// 3.6: a message sent while the run works is part of its request: the
    /// request's message keeps its sent form, and the steering message its own.
    #[test]
    fn a_message_sent_while_the_run_works_is_part_of_its_request() {
        let mut history = vec![
            msg("user", "an earlier request"),
            msg("assistant", "done"),
            sent(msg("user", "rename the helper")),
            call(&["c1"]),
            tool("c1", 10),
            steer("use the existing client"),
            call(&["c2"]),
            tool("c2", 10),
        ];
        assert_eq!(history.iter().rposition(starts_request), Some(2));
        use_sent_content(&mut history, Some("[ctx]\n\nrename the helper"));
        assert_eq!(history[2].text, "[ctx]\n\nrename the helper");
        assert!(
            history[5].text.starts_with("[The user sent this message"),
            "{}",
            history[5].text
        );
        // The repository map goes on the request's message, not on it.
        let request = history
            .iter()
            .rfind(|m| starts_request(m))
            .map(|m| m.id.clone());
        add_request_context(&mut history, request.as_deref(), "## Repository map");
        assert!(history[2].text.starts_with("## Repository map"));
        assert!(!history[5].text.contains("Repository map"));
    }

    /// 3.6: over budget, the run's own results before a steering message
    /// stay the current request's: only the earlier request's are cut.
    #[test]
    fn the_runs_results_before_a_steering_message_arent_an_old_requests() {
        let history = vec![
            msg("user", "q1"),
            call(&["c1"]),
            tool("c1", BIG),
            msg("assistant", "a1"),
            msg("user", "q2"),
            call(&["c2"]),
            tool("c2", BIG),
            steer("the tests are in tests/unit"),
            call(&["c3"]),
            tool("c3", 6000),
        ];
        let cost = Cost {
            accepts: TEXT_ONLY,
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
        };
        let budget = total(&history, cost) - 1000;
        let (out, info) = fit_with(history, budget, cost);
        assert!(info.trimmed);
        assert_eq!(info.dropped_turns, 0);
        assert!(out[2].text.chars().count() < BIG, "the earlier request's");
        assert_eq!(
            out[6].text.chars().count(),
            BIG,
            "the run's own stays whole"
        );
        assert!(pairing_ok(&out));
    }

    /// 3.6: a steering message's files go with every step of the run, as
    /// the request's do; the screenshots' rule is for the others.
    #[test]
    fn a_steering_messages_files_go_with_the_run() {
        let image = |mut m: Msg, id: &str| {
            m.attachments.push(Attachment::image(id));
            m
        };
        let history = vec![
            image(msg("user", "fix the layout"), "r"),
            call(&["c1"]),
            image(tool("c1", 10), "s1"),
            image(steer("like this mockup"), "m"),
            call(&["c2"]),
            image(tool("c2", 10), "s2"),
            call(&["c3"]),
            image(tool("c3", 10), "s3"),
        ];
        let attached = attached_files(&history, IMAGES);
        assert!(attached[0], "the request's");
        assert!(attached[3], "the steering message's");
        assert!(attached[5] && attached[7], "the two latest screenshots");
        assert!(!attached[2], "an older screenshot goes");
    }

    /// D11: a long run's focus note quotes what the user added while it
    /// worked, as typed: the latest five, oldest first.
    #[test]
    fn the_focus_note_quotes_the_steering_messages() {
        let mut history = long_run("rename the helper");
        history.insert(5, steer("use the existing client"));
        let note = focus_note(&history).unwrap();
        assert!(
            note.contains(
                "Added by the user while you worked (quoted, oldest first):\n- > use the existing client"
            ),
            "{note}"
        );
        assert!(
            !note.contains("[The user sent this message"),
            "as typed: {note}"
        );

        let mut history = long_run("rename the helper");
        for i in 0..7 {
            history.insert(5, steer(&format!("note {}", 7 - i)));
        }
        let note = focus_note(&history).unwrap();
        assert!(
            note.contains("(and 2 earlier messages)\n- > note 3"),
            "{note}"
        );
        assert!(note.contains("- > note 7"), "{note}");
        assert!(!note.contains("note 2"), "{note}");
    }

    #[test]
    fn small_chat_untouched() {
        let (out, info) = fit(
            vec![msg("user", "hi"), msg("assistant", "hello")],
            BUDGET_32K,
            IMAGES,
        );
        assert!(!info.trimmed);
        assert_eq!(shape(&out), "u a");
    }

    #[test]
    fn old_tool_results_shrink_current_kept() {
        let history = vec![
            msg("user", "q1"),
            call(&["c1"]),
            tool("c1", BIG),
            msg("assistant", "a1"),
            msg("user", "q2"),
            call(&["c2"]),
            tool("c2", BIG),
            msg("assistant", "a2"),
            msg("user", "q3"),
            call(&["c3"]),
            tool("c3", BIG),
            msg("assistant", "a3"),
            msg("user", "q4"),
            call(&["c4"]),
            tool("c4", 6000),
        ];
        let (out, info) = fit(history, BUDGET_32K, IMAGES);
        assert!(info.trimmed);
        assert_eq!(info.dropped_turns, 0);
        assert!(pairing_ok(&out));
        assert!(
            out.last().unwrap().text.chars().count() == 6000,
            "current request's result stays whole"
        );
        assert!(out[2].text.contains("[Truncated"));
    }

    #[test]
    fn oldest_turns_dropped_with_note() {
        let mut history = Vec::new();
        for i in 1..=12 {
            history.push(msg("user", &format!("question {i} {}", "y".repeat(3000))));
            history.push(msg(
                "assistant",
                &format!("answer {i} {}", "z".repeat(3000)),
            ));
        }
        history.push(msg("user", "current question"));
        let (out, info) = fit(history, BUDGET_8K, IMAGES);
        assert_eq!(info.dropped_turns, 10);
        assert!(out[0].text.starts_with(OMITTED_NOTE));
        assert!(out[0].text.contains("question 11"));
        assert_eq!(out.last().unwrap().text, "current question");
        assert!(info.estimated_tokens <= BUDGET_8K);
    }

    fn target(budget: usize) -> usize {
        (budget as f64 * TRIM_TO) as usize
    }

    /// A tool result of about this many tokens (the estimate's overhead aside).
    fn result_of_tokens(id: &str, tokens: usize) -> Msg {
        tool(id, tokens * DEFAULT_CHARS_PER_TOKEN as usize)
    }

    #[test]
    fn turns_are_dropped_past_the_budget_so_the_next_steps_keep_the_cut() {
        let mut history = Vec::new();
        for i in 1..=12 {
            history.push(msg("user", &format!("question {i} {}", "y".repeat(3000))));
            history.push(msg(
                "assistant",
                &format!("answer {i} {}", "z".repeat(3000)),
            ));
        }
        history.push(msg("user", "current question"));
        let (out, info) = fit(history.clone(), BUDGET_8K, IMAGES);
        assert!(info.estimated_tokens <= target(BUDGET_8K), "{info:?}");
        let first = out[0].text.clone();
        let slack = target(BUDGET_8K) - info.estimated_tokens;

        // The run goes on with a step that fits in the slack: the same
        // turns are dropped, so the request starts exactly as before.
        history.push(call(&["c1"]));
        history.push(result_of_tokens("c1", slack / 2));
        let (again, info_again) = fit(history.clone(), BUDGET_8K, IMAGES);
        assert_eq!(info_again.dropped_turns, info.dropped_turns);
        assert_eq!(again[0].text, first, "the cut didn't move");

        // Only a step that uses up the slack moves it.
        history.push(call(&["c2"]));
        history.push(result_of_tokens("c2", slack + 100));
        let (later, info_later) = fit(history, BUDGET_8K, IMAGES);
        assert!(info_later.dropped_turns > info.dropped_turns);
        assert_ne!(later[0].text, first);
        assert!(info_later.estimated_tokens <= target(BUDGET_8K));
    }

    #[test]
    fn a_run_over_budget_cuts_results_ahead_so_the_next_steps_cut_nothing_new() {
        // Five big results: the three oldest have to go for the run to fit.
        let mut history = vec![msg("user", "q")];
        for i in 0..5 {
            let id = format!("x{i}");
            history.push(call(&[&id]));
            history.push(tool(&id, BIG));
        }
        let (out, info) = fit(history.clone(), BUDGET_32K, IMAGES);
        assert!(info.trimmed);
        assert!(info.estimated_tokens <= target(BUDGET_32K), "{info:?}");
        let cut = |ms: &[Msg]| -> Vec<bool> {
            ms.iter().map(|m| m.text.contains("[Truncated")).collect()
        };
        let before = cut(&out);
        assert!(before.contains(&true) && before.contains(&false));
        let slack = target(BUDGET_32K) - info.estimated_tokens;

        history.push(call(&["y"]));
        history.push(result_of_tokens("y", slack / 2));
        let (again, _) = fit(history, BUDGET_32K, IMAGES);
        assert_eq!(
            cut(&again[..out.len()]),
            before,
            "the earlier results are cut exactly as before"
        );
        assert!(pairing_ok(&again));
    }

    /// A message with an id of its own (the helpers' ids repeat), as stored
    /// messages have: a kept cut is found by id.
    fn named(mut m: Msg, id: &str) -> Msg {
        m.id = id.into();
        m
    }

    const COST: Cost = Cost {
        accepts: TEXT_ONLY,
        chars_per_token: DEFAULT_CHARS_PER_TOKEN,
    };

    /// The most a cut afresh goes past the target: the note on the first
    /// message kept, which the count it stops at doesn't include.
    fn past_target() -> usize {
        COST.tokens(OMITTED_NOTE.chars().count() + 2) + 1
    }

    /// Thirty earlier turns of ~420 tokens, then the current request.
    fn long_chat() -> Vec<Msg> {
        let mut history = Vec::new();
        for i in 1..=30 {
            let question = format!("question {i} {}", "y".repeat(600));
            let answer = format!("answer {i} {}", "z".repeat(600));
            history.push(named(msg("user", &question), &format!("u{i}")));
            history.push(named(msg("assistant", &answer), &format!("a{i}")));
        }
        history.push(named(msg("user", "current question"), "now"));
        history
    }

    /// One more step of the run: a call and its result of `tokens`.
    fn step(history: &mut Vec<Msg>, n: usize, tokens: usize) {
        let id = format!("c{n}");
        history.push(call(&[&id]));
        history.push(named(result_of_tokens(&id, tokens), &format!("r{n}")));
    }

    #[test]
    fn a_kept_cut_stays_while_it_fits_then_goes_down_to_the_target() {
        const BUDGET: usize = 10_000;
        let mut history = long_chat();
        let (mut kept, mut kept_from): (Option<Cut>, Option<String>) = (None, None);
        let mut fresh_from: Option<String> = None;
        let (mut kept_moves, mut fresh_moves) = (0, 0);
        for n in 0..24 {
            if n > 0 {
                step(&mut history, n, 150);
            }
            let (out, info) = fit_keeping(history.clone(), BUDGET, COST, kept.as_ref());
            assert!(info.dropped_turns > 0, "step {n}: {info:?}");
            let from = out[0].id.clone();
            if let Some(before) = &kept_from {
                if *before == from {
                    // Kept: the history fits with it.
                    assert!(info.estimated_tokens <= BUDGET, "step {n}: {info:?}");
                } else {
                    // Moved: with the old cut it didn't fit; now there's room
                    // for the next steps.
                    let k = history.iter().position(|m| m.id == *before).unwrap();
                    assert!(size_from(&history, k, COST) > BUDGET, "step {n}");
                    assert!(
                        info.estimated_tokens <= target(BUDGET) + past_target(),
                        "step {n}: {info:?}"
                    );
                    kept_moves += 1;
                }
            }
            assert_eq!(info.cut.first_kept.as_deref(), Some(from.as_str()));
            kept = Some(info.cut);
            kept_from = Some(from);

            // Cut afresh for every request, as before the cut was kept.
            let (fresh, _) = fit_with(history.clone(), BUDGET, COST);
            if fresh_from.as_ref().is_some_and(|f| *f != fresh[0].id) {
                fresh_moves += 1;
            }
            fresh_from = Some(fresh[0].id.clone());
        }
        assert!(kept_moves > 0, "the run outgrew the room left");
        assert!(
            kept_moves * 2 <= fresh_moves,
            "kept: moved {kept_moves} times; afresh: {fresh_moves}"
        );
    }

    #[test]
    fn the_runs_results_stay_cut_while_they_fit_then_down_to_the_target() {
        const BUDGET: usize = 22_400;
        // A run alone, of results of ~1 500 tokens: cut ones keep ~440.
        let mut history = vec![named(msg("user", "q"), "now")];
        let shortened = |ms: &[Msg]| -> Vec<String> {
            ms.iter()
                .filter(|m| m.text.contains("[Truncated"))
                .map(|m| m.id.clone())
                .collect()
        };
        let mut kept: Option<Cut> = None;
        let (mut kept_cut, mut fresh_cut) = (Vec::new(), Vec::new());
        let (mut kept_changes, mut fresh_changes) = (0, 0);
        for n in 1..=30 {
            step(&mut history, n, 1_500);
            let (out, info) = fit_keeping(history.clone(), BUDGET, COST, kept.as_ref());
            assert!(pairing_ok(&out));
            let cut = shortened(&out);
            assert_eq!(info.cut.shortened_through.as_ref(), cut.last(), "step {n}");
            if cut == kept_cut {
                assert!(info.estimated_tokens <= BUDGET, "step {n}: {info:?}");
            } else {
                // The same results cut short no longer fit: more are, down
                // to the target, oldest first, the latest whole.
                assert!(cut.starts_with(&kept_cut), "step {n}: {cut:?}");
                assert!(
                    info.estimated_tokens <= target(BUDGET),
                    "step {n}: {info:?}"
                );
                kept_changes += 1;
            }
            assert!(
                !cut.contains(&format!("r{n}")),
                "the latest result is whole"
            );
            kept = Some(info.cut);
            kept_cut = cut;

            let (fresh, _) = fit_with(history.clone(), BUDGET, COST);
            let cut = shortened(&fresh);
            if cut != fresh_cut {
                fresh_changes += 1;
            }
            fresh_cut = cut;
        }
        assert!(kept_changes > 0, "the run outgrew the room left");
        assert!(
            kept_changes * 2 <= fresh_changes,
            "kept: changed {kept_changes} times; afresh: {fresh_changes}"
        );
    }

    #[test]
    fn a_kept_cut_that_no_longer_holds_is_cut_afresh() {
        const BUDGET: usize = 3_000;
        let history = long_chat();
        let (afresh, info) = fit_with(history.clone(), BUDGET, COST);
        let fresh_from = afresh[0].id.clone();
        let cut = |first_kept: &str| Cut {
            first_kept: Some(first_kept.into()),
            shortened_through: None,
        };
        // Its message gone (retried, deleted), or not starting a turn: afresh.
        for gone in ["u0", "a7", "c3"] {
            let (out, _) = fit_keeping(history.clone(), BUDGET, COST, Some(&cut(gone)));
            assert_eq!(out[0].id, fresh_from, "{gone}");
        }
        // Dropping too few turns to fit: afresh, down to the target.
        let (out, again) = fit_keeping(history.clone(), BUDGET, COST, Some(&cut("u2")));
        assert_eq!(out[0].id, fresh_from);
        assert_eq!(again, info);
        // Dropping more than needed still fits: kept (the start stays put).
        assert_ne!(fresh_from, "u28");
        let (out, more) = fit_keeping(history.clone(), BUDGET, COST, Some(&cut("u28")));
        assert_eq!(out[0].id, "u28");
        assert_eq!(more.cut, cut("u28"));
        assert!(out[0].text.starts_with(OMITTED_NOTE));

        // What a chat keeps for its next request; a request not cut keeps nothing.
        keep_cut("chat-with-a-cut", cut("u28"));
        assert_eq!(last_cut("chat-with-a-cut"), Some(cut("u28")));
        keep_cut("chat-with-a-cut", Cut::default());
        assert_eq!(last_cut("chat-with-a-cut"), None);
    }

    #[test]
    fn huge_current_run_spares_latest_result() {
        let history = vec![
            msg("user", "q"),
            call(&["x1", "x2"]),
            tool("x1", BIG),
            tool("x2", BIG),
            call(&["x3"]),
            tool("x3", BIG),
        ];
        let (out, _) = fit(history, BUDGET_32K, IMAGES);
        assert!(pairing_ok(&out));
        assert!(
            out[2].text.contains("[Truncated"),
            "oldest result in the run is cut first"
        );
        assert_eq!(
            out.last().unwrap().text.chars().count(),
            BIG,
            "latest result stays whole"
        );
    }

    #[test]
    fn images_follow_the_whole_tool_block() {
        let dir = std::env::temp_dir().join(format!("history-test-{}", std::process::id()));
        let id = "a".repeat(64);
        std::fs::create_dir_all(dir.join(attachments::DIR)).unwrap();
        std::fs::write(dir.join(attachments::DIR).join(format!("{id}.jpg")), b"jpg").unwrap();

        let mut shot = tool("a", 10);
        shot.attachments = vec![Attachment::image(id.clone())];
        let ms = vec![msg("user", "check"), call(&["a", "b"]), shot, tool("b", 10)];

        let roles = |w: &[Value]| {
            w.iter()
                .map(|m| m["role"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let vision = to_wire(&ms, &dir, IMAGES, false);
        assert_eq!(
            roles(&vision),
            ["user", "assistant", "tool", "tool", "user"]
        );
        assert_eq!(vision[4]["content"][1]["type"], "image_url");

        let text_only = to_wire(&ms, &dir, TEXT_ONLY, false);
        assert_eq!(roles(&text_only), ["user", "assistant", "tool", "tool"]);
        assert!(text_only[2]["content"]
            .as_str()
            .unwrap()
            .contains("no longer attached"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder with one stored image per id, as `attachments` keeps them.
    fn images_dir(tag: &str, ids: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("history-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(attachments::DIR)).unwrap();
        for (id, ext) in ids {
            std::fs::write(dir.join(attachments::DIR).join(format!("{id}.{ext}")), b"x").unwrap();
        }
        dir
    }

    #[test]
    fn a_users_images_go_in_their_own_message_before_its_text() {
        let id = "b".repeat(64);
        let dir = images_dir("user-images", &[(&id, "png")]);

        let mut asked = msg("user", "Why is this button blue?");
        asked.attachments = vec![Attachment::image(id.clone())];
        let wire = to_wire(&[asked.clone()], &dir, IMAGES, false);
        assert_eq!(wire.len(), 1, "no separate message for the user's images");
        assert_eq!(wire[0]["content"][0]["type"], "image_url");
        assert!(wire[0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(wire[0]["content"][1]["type"], "text");
        assert_eq!(wire[0]["content"][1]["text"], "Why is this button blue?");

        // Images only: no empty text part (some providers reject one).
        let mut shown = msg("user", "");
        shown.attachments = vec![Attachment::image(id.clone())];
        let wire = to_wire(&[shown.clone()], &dir, IMAGES, false);
        let parts = wire[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");

        // A model that doesn't take images is told they were there.
        let note = "[An image the user attached here wasn't sent: this model isn't set up to accept images.]";
        let wire = to_wire(&[asked, shown], &dir, TEXT_ONLY, false);
        assert_eq!(
            wire[0]["content"],
            format!("Why is this button blue?\n{note}")
        );
        assert_eq!(wire[1]["content"], note);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_requests_images_stay_while_screenshots_come_and_go() {
        let (pasted, shot) = ("b".repeat(64), "a".repeat(64));
        let dir = images_dir("request-images", &[(&pasted, "png"), (&shot, "jpg")]);
        let mut request = msg("user", "Make the page look like this");
        request.attachments = vec![Attachment::image(pasted.clone())];
        let mut ms = vec![request];
        for i in 0..3 {
            let id = format!("s{i}");
            ms.push(call(&[&id]));
            let mut t = tool(&id, 10);
            t.attachments = vec![Attachment::image(shot.clone())];
            ms.push(t);
        }

        let wire = to_wire(&ms, &dir, IMAGES, false);
        assert_eq!(
            wire[0]["content"][0]["type"], "image_url",
            "still the request's"
        );
        let screenshots = wire
            .iter()
            .filter(|m| {
                m["content"][0]["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("Screenshot from tool call"))
            })
            .count();
        assert_eq!(screenshots, MAX_IMAGES, "only the latest screenshots");
        assert!(wire[2]["content"]
            .as_str()
            .unwrap()
            .contains("no longer attached"));

        // The estimate counts exactly the images that are sent.
        let with = total(&ms, generous(true));
        let without = total(&ms, generous(false));
        assert_eq!(with - without, (1 + MAX_IMAGES) * IMAGE_TOKENS);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn earlier_requests_images_go_first_when_over_budget() {
        let mut earlier = msg("user", "Here's the mockup");
        earlier.attachments = vec![Attachment::image("mockup")];
        let mut current = msg("user", "Now match this one");
        current.attachments = vec![Attachment::image("target")];
        let ms = vec![earlier, msg("assistant", &"x".repeat(3000)), current];

        let (fitted, info) = fit(ms.clone(), total(&ms, generous(true)) - 500, IMAGES);
        assert!(info.trimmed);
        assert_eq!(info.dropped_turns, 0, "the image went, not the turn");
        assert!(fitted[0].attachments.is_empty());
        assert_eq!(
            fitted[0].text,
            "Here's the mockup\n[An image the user attached here was left out to fit the context window.]"
        );
        assert_eq!(
            fitted[2].attachments,
            [Attachment::image("target")],
            "the request keeps its own"
        );
    }

    fn pdf(id: &str, name: &str, pages: usize) -> Attachment {
        Attachment {
            id: id.into(),
            pdf: Some(PdfInfo {
                name: name.into(),
                pages,
            }),
        }
    }

    #[test]
    fn a_users_pdfs_go_as_files_to_models_that_take_them() {
        let (doc, shot) = ("c".repeat(64), "d".repeat(64));
        let dir = images_dir("user-pdfs", &[(&doc, "pdf"), (&shot, "png")]);
        let mut asked = msg("user", "Does the chart match the report?");
        asked.attachments = vec![pdf(&doc, "report.pdf", 3), Attachment::image(shot.clone())];
        let everything = Accepts {
            images: true,
            pdf: true,
        };

        let wire = to_wire(&[asked.clone()], &dir, everything, false);
        let parts = wire[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0]["type"], "file");
        assert_eq!(parts[0]["file"]["filename"], "report.pdf");
        assert!(parts[0]["file"]["file_data"]
            .as_str()
            .unwrap()
            .starts_with("data:application/pdf;base64,"));
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[2]["text"], "Does the chart match the report?");

        // Images but no PDFs: the image goes, the PDF is named in a note.
        let wire = to_wire(&[asked.clone()], &dir, IMAGES, false);
        let parts = wire[0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(
            parts[1]["text"],
            "Does the chart match the report?\n[The PDF report.pdf the user attached here \
             wasn't sent: this model isn't set up to accept PDFs.]"
        );

        // Text only: one note for both.
        let wire = to_wire(&[asked], &dir, TEXT_ONLY, false);
        assert_eq!(
            wire[0]["content"],
            "Does the chart match the report?\n[The PDF report.pdf and an image the user \
             attached here weren't sent: this model isn't set up to accept images or PDFs.]"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pdf_costs_its_pages_and_goes_first_when_over_budget() {
        let mut earlier = msg("user", "Here's the spec");
        earlier.attachments = vec![pdf("spec", "spec.pdf", 10), pdf("old", "old.pdf", 1)];
        let mut current = msg("user", "Implement section 2");
        current.attachments = vec![pdf("brief", "brief.pdf", 2)];
        let ms = vec![earlier, msg("assistant", "Read it."), current];
        let cost = |pdf| Cost {
            accepts: Accepts { images: true, pdf },
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
        };

        // Only a model that takes PDFs gets them: their pages are counted then.
        assert_eq!(
            total(&ms, cost(true)) - total(&ms, cost(false)),
            (10 + 1 + 2) * PDF_PAGE_TOKENS
        );

        let accepts = Accepts {
            images: false,
            pdf: true,
        };
        let budget = total(&ms, cost(true)) - 1000;
        let (fitted, info) = fit(ms, budget, accepts);
        assert!(info.trimmed);
        assert_eq!(
            fitted[0].text,
            "Here's the spec\n[2 PDFs (spec.pdf, old.pdf) the user attached here were left out \
             to fit the context window.]"
        );
        assert_eq!(fitted[2].attachments, [pdf("brief", "brief.pdf", 2)]);
        let note = focus_note(&{
            let mut long = vec![fitted[2].clone()];
            for i in 0..FOCUS_AFTER_TOOL_RESULTS + 1 {
                let id = format!("c{i}");
                long.push(call(&[&id]));
                long.push(tool(&id, 10));
            }
            long
        });
        assert!(
            note.as_deref()
                .is_some_and(|n| n.contains("The user attached the PDF brief.pdf to the request.")),
            "{note:?}"
        );
    }

    #[test]
    fn attachments_kept_with_a_clm_context_read_back_from_before_pdfs() {
        let old: Vec<Attachment> = serde_json::from_str(r#"["img1"]"#).unwrap();
        assert_eq!(old, [Attachment::image("img1")]);
        let now = vec![pdf("p1", "a.pdf", 2), Attachment::image("img2")];
        let json = serde_json::to_string(&now).unwrap();
        assert_eq!(
            json,
            r#"[{"id":"p1","pdf":{"name":"a.pdf","pages":2}},{"id":"img2"}]"#
        );
        assert_eq!(serde_json::from_str::<Vec<Attachment>>(&json).unwrap(), now);
    }

    #[test]
    fn dropping_turns_with_images_keeps_the_estimate_true() {
        // Three earlier requests with an image each, then the current one:
        // more images than are sent, in turns that get dropped.
        let mut ms = Vec::new();
        for i in 0..3 {
            let mut m = msg("user", &format!("request {i} {}", "y".repeat(600)));
            m.id = format!("u{i}");
            m.attachments = vec![Attachment::image(format!("img{i}"))];
            ms.push(m);
            ms.push(msg("assistant", &"x".repeat(600)));
        }
        ms.push(msg("user", "the current request"));
        let budget = total(&ms, generous(false)) / 2;
        let (fitted, info) = fit(ms, budget, IMAGES);
        assert!(info.dropped_turns > 0);
        assert_eq!(info.estimated_tokens, total(&fitted, generous(true)));
        assert!(fitted.iter().all(|m| m.attachments.is_empty()));
    }

    #[test]
    fn gemini_signatures_are_echoed_or_placeholdered() {
        let dir = std::env::temp_dir();
        let signed = json!({ "google": { "thought_signature": "SIG-A" } });
        let sig = |w: &[Value], m: usize, c: usize| {
            w[m]["tool_calls"][c]
                .pointer("/extra_content/google/thought_signature")
                .cloned()
        };

        let mut first_signed = call(&["a", "b"]);
        first_signed.tool_calls.as_mut().unwrap()[0].extra_content = Some(signed.clone());
        // extra_content present but no signature in it (seen from some gateways/models).
        let mut empty_extra = call(&["c"]);
        empty_extra.tool_calls.as_mut().unwrap()[0].extra_content =
            Some(json!({ "google": {}, "other": 1 }));
        // The signature landed on a sibling, not the first call.
        let mut sibling_signed = call(&["d", "e"]);
        sibling_signed.tool_calls.as_mut().unwrap()[1].extra_content = Some(signed.clone());
        let unsigned = call(&["f"]);

        let ms = vec![
            msg("user", "q"),
            first_signed,
            tool("a", 1),
            tool("b", 1),
            empty_extra,
            tool("c", 1),
            sibling_signed,
            tool("d", 1),
            tool("e", 1),
            unsigned,
            tool("f", 1),
        ];
        let w = to_wire(&ms, &dir, TEXT_ONLY, true);
        let placeholder = Some(json!(GEMINI_SKIP_SIGNATURE));
        {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(GEMINI_SKIP_SIGNATURE)
                .unwrap();
            assert_eq!(
                decoded, b"skip_thought_signature_validator",
                "placeholder is the base64 of the documented value"
            );
        }

        assert_eq!(
            sig(&w, 1, 0),
            Some(json!("SIG-A")),
            "real signature echoed unchanged"
        );
        assert_eq!(sig(&w, 1, 1), None, "parallel sibling stays unsigned");
        assert_eq!(
            sig(&w, 4, 0),
            placeholder,
            "extra_content without a signature gets one"
        );
        assert_eq!(
            w[4]["tool_calls"][0]["extra_content"]["other"],
            json!(1),
            "other data kept"
        );
        assert_eq!(sig(&w, 6, 0), placeholder, "first call always needs one");
        assert_eq!(
            sig(&w, 6, 1),
            Some(json!("SIG-A")),
            "sibling's data untouched"
        );
        assert_eq!(sig(&w, 9, 0), placeholder);

        // Other providers: real data echoed, never a placeholder.
        let other = to_wire(&ms, &dir, TEXT_ONLY, false);
        assert_eq!(sig(&other, 1, 0), Some(json!("SIG-A")));
        assert_eq!(sig(&other, 9, 0), None);
        assert!(other[9]["tool_calls"][0].get("extra_content").is_none());
    }

    #[test]
    fn broken_call_arguments_go_out_as_a_json_object() {
        let dir = std::env::temp_dir();
        let with_args = |args: &str| {
            let mut m = msg("assistant", "");
            m.tool_calls = Some(vec![ToolCall {
                id: "c".into(),
                name: "write".into(),
                arguments: args.into(),
                extra_content: None,
            }]);
            m
        };
        let sent = |args: &str| {
            let w = to_wire(
                &[msg("user", "q"), with_args(args), result("c", "Error: x")],
                &dir,
                TEXT_ONLY,
                false,
            );
            w[1]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let valid = r#"{"path":"a.rs","old_text":"x","content":"y"}"#;
        assert_eq!(sent(valid), valid, "valid arguments are sent unchanged");
        assert_eq!(sent(""), "{}");
        assert_eq!(sent("  "), "{}");

        let cut_off = r#"{"old_text":"fn main() {"#;
        let back: Value = serde_json::from_str(&sent(cut_off)).expect("valid JSON");
        assert_eq!(
            back["invalid_arguments"], cut_off,
            "the model still sees what it sent"
        );

        let array: Value = serde_json::from_str(&sent("[1,2]")).unwrap();
        assert!(array.is_object(), "not an object: wrapped too");

        let long = format!("{{\"old_text\":\"{}", "é".repeat(ARGS_KEEP_CHARS * 2));
        let back: Value = serde_json::from_str(&sent(&long)).unwrap();
        let kept = back["invalid_arguments"].as_str().unwrap();
        assert_eq!(
            kept.chars().count(),
            ARGS_KEEP_CHARS + 1,
            "capped, with an ellipsis"
        );
        assert!(kept.ends_with('…'));
    }

    // ---- requests, the IDE's messages, outdated content ----

    fn call_with(id: &str, name: &str, args: Value) -> Msg {
        let mut m = msg("assistant", "");
        m.tool_calls = Some(vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
            extra_content: None,
        }]);
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
        Cost {
            accepts: Accepts {
                images: vision,
                pdf: false,
            },
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
        }
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
        let (out, info) = fit(history, 12_000, TEXT_ONLY);
        assert!(info.dropped_turns >= 1, "the old turn goes first");
        assert!(
            out.iter().any(|m| m.text.contains("fix the bug")),
            "the request is kept"
        );
        assert!(out.last().unwrap().harness, "the report is kept");
        assert!(
            !out.iter().any(|m| m.harness && m.text.contains("omitted")),
            "no note glued onto the report"
        );
    }

    /// `n` reads of `path`, each with its own result.
    fn reads(n: usize, path: &str, size: usize) -> Vec<Msg> {
        (0..n)
            .flat_map(|i| {
                let id = format!("r{i}");
                vec![
                    call_with(&id, "read", json!({ "path": path })),
                    result(&id, &"x".repeat(size)),
                ]
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
        assert_eq!(
            out.last().unwrap().text.len(),
            3_000,
            "the latest read is whole"
        );
        assert!(pairing_ok(&out));
    }

    #[test]
    fn a_read_nothing_replaced_is_kept() {
        let mut history = vec![msg("user", "q")];
        history.extend(reads(1, "src/only.ts", 3_000));
        for i in 0..16 {
            let id = format!("o{i}");
            history.push(call_with(
                &id,
                "read",
                json!({ "path": format!("src/f{i}.ts") }),
            ));
            history.push(result(&id, &"y".repeat(3_000)));
        }
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(
            out[2].text.len(),
            3_000,
            "still the model's only copy of that file"
        );
    }

    #[test]
    fn a_partial_read_doesnt_replace_what_it_didnt_show() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with(
            "ra",
            "read",
            json!({ "path": "src/big.ts", "start_line": 900, "end_line": 960 }),
        ));
        history.push(result("ra", &"a".repeat(3_000)));
        let overview = format!(
            "{}src/big.ts has 4000 lines…]\n{}",
            crate::read_file::PARTIAL_PREFIX,
            "m".repeat(3_000)
        );
        history.push(call_with("rb", "read", json!({ "path": "src/big.ts" })));
        history.push(result("rb", &overview));
        for i in 0..16 {
            let id = format!("o{i}");
            history.push(call_with(
                &id,
                "read",
                json!({ "path": format!("src/f{i}.ts") }),
            ));
            history.push(result(&id, &"y".repeat(3_000)));
        }
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(
            out[2].text.len(),
            3_000,
            "lines 900-960 are still the model's only copy"
        );
    }

    #[test]
    fn a_symbol_read_covers_only_its_lines() {
        // Lines 1-50 read, then one function (lines 120-160) by symbol: the
        // first read isn't replaced, the symbol read didn't cover it.
        let mut history = vec![msg("user", "q")];
        history.push(call_with(
            "ra",
            "read",
            json!({ "path": "src/a.rs", "start_line": 1, "end_line": 50 }),
        ));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with(
            "rs",
            "read",
            json!({ "path": "src/a.rs", "symbol": "Parser::parse" }),
        ));
        history.push(result(
            "rs",
            &format!(
                "[Symbol read: Parser > parse, lines 120-160 of src/a.rs]\n{}",
                "s".repeat(3_000)
            ),
        ));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(
            out[2].text.len(),
            3_000,
            "lines 1-50 are still the model's only copy"
        );
    }

    #[test]
    fn a_later_read_covering_a_symbol_read_replaces_it() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with(
            "rs",
            "read",
            json!({ "path": "src/a.rs", "symbol": "parse" }),
        ));
        history.push(result(
            "rs",
            &format!(
                "[Symbol read: parse, lines 120-160 of src/a.rs]\n{}",
                "s".repeat(3_000)
            ),
        ));
        history.push(call_with(
            "rr",
            "read",
            json!({ "path": "src/a.rs", "start_line": 100, "end_line": 200 }),
        ));
        history.push(result("rr", &"r".repeat(3_000)));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert!(
            out[2].text.starts_with("[Read src/a.rs lines 120-160 here"),
            "{}",
            out[2].text
        );
    }

    #[test]
    fn a_symbol_read_that_found_nothing_covers_nothing() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with(
            "ra",
            "read",
            json!({ "path": "src/a.rs", "start_line": 1, "end_line": 50 }),
        ));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with(
            "rs",
            "read",
            json!({ "path": "src/a.rs", "symbol": "nope" }),
        ));
        history.push(result("rs", "No declaration \"nope\" in src/a.rs."));
        history.extend(reads(16, "src/other.rs", 3_000));
        let (out, _) = fit_with(history, 20_000, generous(false));
        assert_eq!(out[2].text.len(), 3_000);
    }

    #[test]
    fn edits_supersede_reads_and_old_write_arguments_shrink() {
        let mut history = vec![msg("user", "q")];
        history.push(call_with(
            "w0",
            "write",
            json!({ "path": "src/new.ts", "content": "line\n".repeat(400) }),
        ));
        history.push(result("w0", "wrote 2000 bytes to src/new.ts"));
        history.push(call_with(
            "ra",
            "read",
            json!({ "path": "src/a.ts", "start_line": 1, "end_line": 50 }),
        ));
        history.push(result("ra", &"a".repeat(3_000)));
        history.push(call_with(
            "e0",
            "write",
            json!({ "path": "src/a.ts", "old_text": "o".repeat(500), "content": "n" }),
        ));
        history.push(result("e0", "Edited src/a.ts: replaced line 3"));
        history.extend(reads(14, "src/other.ts", 2_000));
        let (out, _) = fit_with(history, 12_000, generous(false));

        let write_args: Value =
            serde_json::from_str(&out[1].tool_calls.as_ref().unwrap()[0].arguments).unwrap();
        assert_eq!(
            write_args["path"], "src/new.ts",
            "arguments stay valid JSON with the path"
        );
        assert_eq!(
            write_args["content"],
            "[400 lines written; left out of this history]"
        );
        assert!(
            out[4].text.contains("Read src/a.ts lines 1-50 here")
                && out[4].text.contains("changed later")
        );
        let edit_args: Value =
            serde_json::from_str(&out[5].tool_calls.as_ref().unwrap()[0].arguments).unwrap();
        assert_eq!(edit_args["content"], "n", "short arguments are kept");
        assert!(edit_args["old_text"].as_str().unwrap().contains("left out"));
    }

    #[test]
    fn a_rerun_command_and_earlier_check_rounds_shrink() {
        let mut history = vec![msg("user", "q")];
        let out_text = format!("$ npm test\nexit code 1, 3.0 s\n{}", "fail\n".repeat(800));
        history.push(call_with("c0", "bash", json!({ "command": "npm test" })));
        history.push(result("c0", &out_text));
        history.push(harness(&format!(
            "[Automatic check by the IDE, not written by the user]\nround 1\n{}",
            "e".repeat(3_000)
        )));
        history.push(call_with("c1", "bash", json!({ "command": "npm test" })));
        history.push(result("c1", &out_text));
        history.push(harness(
            "[Automatic check by the IDE, not written by the user]\nround 2",
        ));
        history.extend(reads(14, "src/x.ts", 1_500));
        let (out, _) = fit_with(history, 9_000, generous(false));
        assert!(
            out[2].text.starts_with("$ npm test\nexit code 1")
                && out[2].text.contains("ran again later")
        );
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
            t.attachments = vec![Attachment::image("img")];
            ms.push(t);
        }
        let with = total(&ms, generous(true));
        let without = total(&ms, generous(false));
        assert_eq!(with - without, MAX_IMAGES * IMAGE_TOKENS);
    }
}

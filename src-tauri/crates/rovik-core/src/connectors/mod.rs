//! Connectors: coding harnesses a chat uses in place of a model provider
//! (rovik_protocol::connectors). A harness such as Claude Code brings its
//! own model loop and tools: the IDE hands it the system prompt, the model,
//! the message and the project folder, follows what it does, and shows its
//! steps, its tokens and its cost with the reply.
//!
//! The chat request path is the same as a provider's: `agent::run_chat`
//! sees a connector's base URL (`connector://claude-code`) and hands the
//! request to [`run_chat`] here, which turns the stored chat into one turn
//! for the harness and reports it through the same `ChatStreamEvent`s. The
//! run loop offers such a model no tools of the IDE's (the frontend marks
//! connector models `noTools`): the harness runs its own, and each shows as
//! a step the IDE never ran (`ChatStreamEvent::HarnessStep`).
//!
//! A harness keeps its own conversation: the session it opens for a chat
//! is remembered (sessions.rs), and later turns continue it, so it keeps
//! what it read and did. Where the session is gone (another computer, a
//! cleared folder), the turn starts a new one with a recap of the chat.
//!
//! Every connector implements [`Connector`]: a probe for "Test & refresh
//! models" and a turn. A new harness (Codex, say) is one more module here
//! and one more `ConnectorKind`.

mod claude_code;
pub mod sessions;

use crate::agent::{ChatParams, ChatStreamEvent, ReasoningEffort};
use crate::chat_db::store::MessageRow;
use crate::chat_db::DbState;
use crate::chat_request::ChatError;
use futures_util::future::BoxFuture;
pub use rovik_protocol::connectors::{
    is_connector, kind_of, ConnectorAuth, ConnectorKind, ConnectorModel, ConnectorProbe,
    ProbeRequest, ProbeRequestArgs,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

/// The most of a chat's earlier turns a new harness session is told about.
const RECAP_CHARS: usize = 24_000;
/// How many stored messages are read to build a turn.
const RECENT_MESSAGES: usize = 400;

/// A step the harness ran itself (a tool of its own), as the chat shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStep {
    pub id: String,
    /// The tool's name as the harness names it (`Read`, `Bash`).
    pub name: String,
    /// What it was called with, cut to a few hundred characters per field.
    pub input: Value,
    /// Its result's first lines, once it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Whether it succeeded, once it has a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// A subagent's step: the harness's call that started the subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// One turn for a harness: what `run_chat` makes of a chat request.
#[derive(Debug)]
pub struct Turn {
    /// The connector's settings, as the frontend keeps them with its entry
    /// (`ModelProfile::connector`).
    pub settings: Value,
    /// Its token or key, as the profile's `api_key`.
    pub api_key: String,
    /// The model the harness is told, as its model list names it.
    pub model: String,
    pub system_prompt: String,
    /// The message for the harness: the user's request, the IDE's notes
    /// since the last reply, or a recap and the request for a new session.
    pub prompt: String,
    /// The folder it works in: the chat's project (or worktree on a host).
    pub cwd: Option<PathBuf>,
    /// The harness's conversation for this chat, to continue; none starts one.
    pub session: Option<String>,
    /// The prompt for a new session when the one to continue is gone: the
    /// request with a recap of the chat. None when `session` is none.
    pub recap: Option<String>,
    /// A one-off request (a chat's title, a review): no tools, no session.
    pub one_shot: bool,
    pub effort: Option<ReasoningEffort>,
    pub first_byte_timeout: Duration,
}

/// What a connector reports as its turn runs, besides the reply's text.
pub trait Connector: Send + Sync {
    fn kind(&self) -> ConnectorKind;

    /// "Test & refresh models": the program, its version and its models.
    fn probe<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
    ) -> BoxFuture<'a, Result<ConnectorProbe, String>>;

    /// Whether the program is signed in as a chat's run would be, without
    /// a request: cheap enough for the app's start. `Err` when it can't
    /// say (not installed, a version without the check).
    fn auth_status<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
    ) -> BoxFuture<'a, Result<ConnectorAuth, String>>;

    /// Runs one turn, reporting it through `on_event` as a provider's
    /// request would: `Delta`, `Reasoning`, `HarnessStep`, `HarnessSession`,
    /// `Finished` (its usage), `Done`, or `Error`. `Err` is for what fails
    /// before the harness runs.
    fn run<'a>(
        &'a self,
        turn: Turn,
        cancel: &'a mut watch::Receiver<bool>,
        on_event: &'a mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// The connector of a kind.
pub fn connector(kind: ConnectorKind) -> &'static dyn Connector {
    match kind {
        ConnectorKind::ClaudeCode => &claude_code::ClaudeCode,
    }
}

/// "Test & refresh models" on `args.kind`, on this computer.
pub async fn probe(args: ProbeRequestArgs) -> Result<ConnectorProbe, String> {
    connector(args.kind).probe(&args).await
}

/// Whether `args.kind`'s program is signed in on this computer.
pub async fn auth_status(args: ProbeRequestArgs) -> Result<ConnectorAuth, String> {
    connector(args.kind).auth_status(&args).await
}

/// A chat request on a connector's model (`agent::run_chat` hands it here):
/// the stored chat, or explicit messages, made into one turn and run.
pub async fn run_chat(
    db: &DbState,
    kind: ConnectorKind,
    params: ChatParams,
    cancel: &mut watch::Receiver<bool>,
    on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<(), String> {
    let data_dir = db.data_dir().to_path_buf();
    let has_history = params.dialog_id.is_some();
    // A one-off request's system message is the harness's system prompt.
    let (messages_system, messages_prompt) = match &params.messages {
        Some(messages) => {
            let (system, prompt) = split_messages(messages);
            (system, Some(prompt))
        }
        None => (None, None),
    };
    let system_prompt = messages_system
        .or_else(|| params.system_prompt.clone())
        .unwrap_or_else(|| {
            crate::agent::lead_system_prompt(false, params.project_context.as_deref(), false)
        });
    let effort = params.options.as_ref().and_then(|o| o.reasoning_effort);
    let first_byte_timeout = Duration::from_millis(params.first_byte_timeout_ms.unwrap_or(60_000));
    let settings = params.connector.clone().unwrap_or(Value::Null);
    let (prompt, session, recap) = match (messages_prompt, &params.dialog_id) {
        (Some(prompt), _) => (prompt, None, None),
        (None, Some(dialog_id)) => {
            let id = dialog_id.clone();
            let rows = db
                .read(move |c| crate::chat_db::store::get_messages(c, &id, None, RECENT_MESSAGES))
                .await
                .map(|page| page.messages)?;
            let session = sessions::get(&data_dir, kind, dialog_id);
            let continuing = params.continue_reply.unwrap_or(false);
            let current = params.current_user_content.as_deref();
            let prompt = prompt_for(&rows, current, continuing, session.is_none());
            let recap = session
                .is_some()
                .then(|| prompt_for(&rows, current, continuing, true));
            (prompt, session, recap)
        }
        (None, None) => return Err("send_chat_message needs messages or a dialogId".into()),
    };
    if prompt.trim().is_empty() {
        return Err("Nothing to send: the chat has no message for the harness.".into());
    }
    let turn = Turn {
        settings,
        api_key: params.api_key.clone(),
        model: params.model.clone(),
        system_prompt,
        prompt,
        cwd: params.cwd.clone(),
        session,
        recap,
        one_shot: !has_history,
        effort,
        first_byte_timeout,
    };
    // The session the harness opens or continues is kept for the chat's
    // next turn, as the events say it.
    let dialog_id = params.dialog_id.clone();
    let mut remember = |event: ChatStreamEvent| {
        if let (ChatStreamEvent::HarnessSession { session, .. }, Some(dialog)) =
            (&event, &dialog_id)
        {
            sessions::put(&data_dir, kind, dialog, session);
        }
        on_event(event);
    };
    connector(kind).run(turn, cancel, &mut remember).await
}

/// Explicit messages (a one-off request) as the harness takes them: the
/// system message as its system prompt, the rest as one prompt, the
/// user's words as they are and the other roles marked.
fn split_messages(messages: &[crate::agent::WireChatMessage]) -> (Option<String>, String) {
    let mut system = None;
    let mut out = String::new();
    for m in messages {
        let text = text_of_content(m.content());
        if text.trim().is_empty() {
            continue;
        }
        if m.role() == "system" {
            system = Some(text);
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        match m.role() {
            "user" => out.push_str(&text),
            role => out.push_str(&format!("[{role}]\n{text}")),
        }
    }
    (system, out)
}

/// The text of a content block list (`[{type: "text", text}]`).
fn text_of_content(content: &Value) -> String {
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

/// A stored message's text as it was sent (`meta.sentContent` carries the
/// editor context), else as stored.
fn sent_text(m: &MessageRow) -> String {
    m.meta
        .as_ref()
        .and_then(|meta| meta.get("sentContent")?.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| m.content.clone())
}

/// What the harness is told this turn: every message since its last reply
/// (the user's request, the IDE's notes such as failed checks), the current
/// one as sent. `continue_reply` asks it to go on with an interrupted one.
/// A `new_session` with earlier turns gets a recap of them first, since
/// the harness has none of its own.
pub fn prompt_for(
    rows: &[MessageRow],
    current_user_content: Option<&str>,
    continue_reply: bool,
    new_session: bool,
) -> String {
    // Rows up to the pending reply this turn fills (unfinished, at the end).
    let settled: Vec<&MessageRow> = rows
        .iter()
        .filter(|m| m.finalized || m.role != "assistant")
        .collect();
    // The last reply the harness gave: what came after it is this turn's.
    let last_reply = settled.iter().rposition(|m| {
        m.role == "assistant" && (!m.content.trim().is_empty() || m.error.is_some())
    });
    let since = match last_reply {
        Some(i) => &settled[i + 1..],
        None => &settled[..],
    };
    let mut parts: Vec<String> = Vec::new();
    let mut found_current = false;
    for m in since.iter().filter(|m| m.role == "user" && !m.failed) {
        let is_request = m
            .meta
            .as_ref()
            .is_none_or(|meta| meta.get("harness").is_none() && meta.get("steer").is_none());
        let text = if is_request && !found_current {
            found_current = true;
            current_user_content
                .map(str::to_string)
                .unwrap_or_else(|| sent_text(m))
        } else {
            sent_text(m)
        };
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    if continue_reply {
        parts.push(
            "Your previous reply was interrupted. Continue exactly where it stopped, without \
             repeating what you already wrote."
                .to_string(),
        );
    }
    if parts.is_empty() {
        if let Some(current) = current_user_content {
            parts.push(current.to_string());
        }
    }
    let mut prompt = parts.join("\n\n");
    if new_session {
        let earlier = match last_reply {
            Some(i) => &settled[..=i],
            None => &[][..],
        };
        if let Some(recap) = recap(earlier) {
            prompt = format!(
                "Earlier in this conversation, with another session of yours (for context; the \
                 project is in your working folder as it is now):\n\n{recap}\n\n---\n\n{prompt}"
            );
        }
    }
    prompt
}

/// The chat's earlier turns, in words: the user's requests and the
/// replies, the most recent kept when they don't all fit.
fn recap(rows: &[&MessageRow]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for m in rows {
        let text = match m.role.as_str() {
            "user" if !m.failed => sent_text(m),
            "assistant" => m.content.clone(),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let who = if m.role == "user" { "User" } else { "You" };
        lines.push(format!("{who}:\n{}", text.trim()));
    }
    if lines.is_empty() {
        return None;
    }
    let mut kept: Vec<String> = Vec::new();
    let mut chars = 0;
    for line in lines.iter().rev() {
        let n = line.chars().count() + 2;
        if chars + n > RECAP_CHARS && !kept.is_empty() {
            kept.push("[earlier turns left out]".into());
            break;
        }
        if chars + n > RECAP_CHARS {
            kept.push(line.chars().take(RECAP_CHARS).collect::<String>() + "…");
            chars = RECAP_CHARS;
            continue;
        }
        chars += n;
        kept.push(line.clone());
    }
    kept.reverse();
    Some(kept.join("\n\n"))
}

/// A harness's usage as providers report theirs (`prompt_tokens`,
/// `completion_tokens`, cached tokens, `cost`), so the IDE's counts, its
/// cache figures and the subagents' totals read it as they read a
/// provider's; what only a harness says goes under `connector`.
pub fn usage_value(
    kind: ConnectorKind,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    cost_usd: Option<f64>,
    extra: Value,
) -> Value {
    let prompt = input + cache_creation + cache_read;
    let mut usage = json!({
        "prompt_tokens": prompt,
        "completion_tokens": output,
        "total_tokens": prompt + output,
        "prompt_tokens_details": { "cached_tokens": cache_read },
        "connector": { "kind": kind.id() },
    });
    if let Some(cost) = cost_usd {
        usage["cost"] = json!(cost);
    }
    if let Value::Object(fields) = extra {
        for (key, value) in fields {
            usage["connector"][key] = value;
        }
    }
    usage
}

/// A harness that ended its turn in an error: the IDE's error for it.
pub fn harness_error(
    kind: ConnectorKind,
    message: impl Into<String>,
    details: Option<String>,
) -> ChatError {
    let message: String = message.into();
    let lower = message.to_lowercase();
    let auth = [
        "not logged in",
        "login expired",
        "please run /login",
        "authentication",
        "invalid api key",
        "oauth token",
        "setup-token",
        "401",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let kind_of_error = if auth {
        crate::chat_request::ErrorKind::Auth
    } else if lower.contains("rate limit") || lower.contains("usage limit") || lower.contains("429")
    {
        crate::chat_request::ErrorKind::RateLimited
    } else {
        crate::chat_request::ErrorKind::Provider
    };
    let mut error = ChatError::new(kind_of_error, format!("{}: {message}", kind.title()));
    // A harness isn't asked again by the IDE: it has its own retries.
    error.retryable = false;
    if let Some(details) = details {
        error = error.with_details(details);
    }
    error
}

/// Cuts a value's strings to `max` characters each, for a step's record.
pub fn trimmed_value(value: &Value, max: usize) -> Value {
    match value {
        Value::String(s) if s.chars().count() > max => {
            Value::String(s.chars().take(max).collect::<String>() + "…")
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(50)
                .map(|v| trimmed_value(v, max))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), trimmed_value(v, max)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The first `max` characters of a result.
pub fn trimmed_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, content: &str, meta: Option<Value>, finalized: bool) -> MessageRow {
        MessageRow {
            id: uuid::Uuid::new_v4().to_string(),
            dialog_id: "d".into(),
            seq: 0,
            role: role.into(),
            content: content.into(),
            body_external: false,
            body_bytes: 0,
            tool_calls: None,
            tool_call_id: None,
            meta,
            error: None,
            failed: false,
            finalized,
            attachments: Vec::new(),
            created_at: 0,
        }
    }

    #[test]
    fn the_prompt_is_what_came_since_the_last_reply_the_request_as_sent() {
        let rows = vec![
            row("user", "First ask", None, true),
            row("assistant", "Did it.", None, true),
            row(
                "user",
                "Checks failed:\n- lint",
                Some(json!({ "harness": "verification" })),
                true,
            ),
            row(
                "user",
                "Also rename it",
                Some(json!({ "sentContent": "Also rename it\n\n[editor: foo.rs]" })),
                true,
            ),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, false, false);
        assert_eq!(
            prompt,
            "Checks failed:\n- lint\n\nAlso rename it\n\n[editor: foo.rs]"
        );
        let prompt = prompt_for(&rows, Some("Also rename it (sent)"), false, false);
        assert!(prompt.ends_with("Also rename it (sent)"), "{prompt}");
        assert!(
            !prompt.contains("First ask"),
            "earlier turns are the session's own"
        );
    }

    #[test]
    fn a_new_session_gets_a_recap_of_the_earlier_turns() {
        let rows = vec![
            row("user", "First ask", None, true),
            row("assistant", "Did it.", None, true),
            row("user", "Next", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, false, true);
        assert!(
            prompt.starts_with("Earlier in this conversation"),
            "{prompt}"
        );
        assert!(prompt.contains("User:\nFirst ask"));
        assert!(prompt.contains("You:\nDid it."));
        assert!(prompt.ends_with("---\n\nNext"), "{prompt}");
        // The first turn of a chat has nothing to recap.
        let first = vec![
            row("user", "Hello", None, true),
            row("assistant", "", None, false),
        ];
        assert_eq!(prompt_for(&first, None, false, true), "Hello");
    }

    #[test]
    fn continuing_an_interrupted_reply_asks_for_it() {
        let rows = vec![
            row("user", "Ask", None, true),
            row("assistant", "Half a re", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, true, false);
        assert!(prompt.contains("interrupted"), "{prompt}");
    }

    #[test]
    fn a_long_recap_keeps_the_latest_turns() {
        let long = "x".repeat(RECAP_CHARS);
        let rows = vec![
            row("user", &long, None, true),
            row("assistant", "old", None, true),
            row("user", "recent ask", None, true),
            row("assistant", "recent reply", None, true),
            row("user", "Now", None, true),
        ];
        let prompt = prompt_for(&rows, None, false, true);
        assert!(prompt.contains("[earlier turns left out]"));
        assert!(prompt.contains("recent reply"));
        assert!(!prompt.contains(&long));
    }

    #[test]
    fn usage_reads_as_a_providers() {
        let usage = usage_value(
            ConnectorKind::ClaudeCode,
            2,
            2918,
            649,
            4,
            Some(0.0006),
            json!({ "numTurns": 1 }),
        );
        assert_eq!(usage["prompt_tokens"], 3569);
        assert_eq!(usage["completion_tokens"], 4);
        assert_eq!(crate::prompt_cache::cached_tokens(&usage), Some(649));
        let (i, o, cost) = crate::agent_run::log::usage_counts(&usage);
        assert_eq!((i, o), (Some(3569), Some(4)));
        assert_eq!(cost, Some(0.0006));
        assert_eq!(usage["connector"]["kind"], "claude-code");
        assert_eq!(usage["connector"]["numTurns"], 1);
    }

    #[test]
    fn harness_errors_are_classified() {
        let e = harness_error(
            ConnectorKind::ClaudeCode,
            "Not logged in · Please run /login",
            None,
        );
        assert_eq!(e.kind, crate::chat_request::ErrorKind::Auth);
        assert!(!e.retryable);
        assert!(e.message.starts_with("Claude Code: "));
        let e = harness_error(ConnectorKind::ClaudeCode, "boom", Some("stderr".into()));
        assert_eq!(e.kind, crate::chat_request::ErrorKind::Provider);
        assert_eq!(e.details.as_deref(), Some("stderr"));
    }

    #[test]
    fn transcripts_keep_the_users_words_and_mark_the_rest() {
        let messages = vec![
            crate::agent::WireChatMessage::text("system", "sys"),
            crate::agent::WireChatMessage::text("user", "Name this chat"),
            crate::agent::WireChatMessage::text("assistant", "Sure"),
        ];
        let (system, prompt) = split_messages(&messages);
        assert_eq!(system.as_deref(), Some("sys"));
        assert_eq!(prompt, "Name this chat\n\n[assistant]\nSure");
    }

    #[test]
    fn values_are_trimmed_for_the_record() {
        let v = trimmed_value(&json!({ "a": "x".repeat(10), "b": [1, "yyyyy"] }), 3);
        assert_eq!(v["a"], "xxx…");
        assert_eq!(v["b"][1], "yyy…");
        assert_eq!(trimmed_text("abc", 5), "abc");
        assert_eq!(trimmed_text("abcdef", 3), "abc…");
    }
}

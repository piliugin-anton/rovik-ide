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
//! models", its sign-in check, a turn, and, where it has them, what its
//! "Set up" steps run (setup.rs runs them, the same for every connector),
//! its commands, its newest version and its plan mode. A new harness
//! (Codex, say) is one more module here implementing it, one more
//! `ConnectorKind`, and its line in [`connector`]; nothing else here or in
//! the run changes. The app's side is its `ConnectorSpec` and its
//! `ConnectorFields` (src/lib/connectors.ts).

mod claude_code;
mod harness;
pub mod mcp;
pub mod sessions;
mod setup;

use crate::agent::{ChatParams, ChatStreamEvent, ReasoningEffort};
use crate::chat_db::store::MessageRow;
use crate::chat_db::DbState;
use crate::chat_request::ChatError;
use futures_util::future::BoxFuture;
pub use rovik_protocol::connectors::ConnectorCommand;
pub use rovik_protocol::connectors::{
    is_connector, kind_of, ConnectorAuth, ConnectorKind, ConnectorModel, ConnectorProbe,
    ProbeRequest, ProbeRequestArgs, SetupAction, SetupJob, SetupOp,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub use setup::{setup, SetupCommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// The most of a chat's earlier turns a new harness session is told about.
const RECAP_CHARS: usize = 24_000;
/// How many stored messages are read to build a turn.
const RECENT_MESSAGES: usize = 400;

/// A step the harness ran itself (a tool of its own), or a stretch of its
/// thinking between them (`thinking`), as the chat shows it.
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
    /// A stretch of thinking rather than a tool call: its text so far (a
    /// summary, as the model's API gives it), in its place among the
    /// steps. Its `name` and `input` are empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// How long that thinking took, once it's over (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
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
    /// The project's section of the system prompt, when it's apart from
    /// `system_prompt`: it changes between runs, the rest doesn't.
    pub system_context: Option<String>,
    /// The message for the harness: the user's request, the IDE's notes
    /// since the last reply, or a recap and the request for a new session.
    pub prompt: String,
    /// The folder it works in: the chat's project (or worktree on a host).
    pub cwd: Option<PathBuf>,
    /// `cwd` is the chat's copy of the project (its overlay, or a server's
    /// worktree): it changes the project by landing, so git that changes
    /// the repository is denied to the harness's own tools.
    pub isolated: bool,
    /// The run is in Plan mode: a harness running its own tools plans in
    /// its own plan mode, changing nothing (Claude Code's `plan`).
    pub plan_mode: bool,
    /// The harness's conversation for this chat, to continue; none starts one.
    pub session: Option<String>,
    /// The prompt for a new session when the one to continue is gone: the
    /// request with a recap of the chat. None when `session` is none.
    pub recap: Option<String>,
    /// A one-off request (a chat's title, a review): no tools, no session.
    pub one_shot: bool,
    pub effort: Option<ReasoningEffort>,
    pub first_byte_timeout: Duration,
    /// The chat it's for.
    pub dialog_id: Option<String>,
    /// The tools of the run's request (OpenAI function schemas): what a
    /// harness offers the model as Rovik's, in that mode.
    pub tools: Vec<Value>,
    /// Rovik's tools mode: the calls of the chat's last reply and their
    /// stored results, which a harness waiting on them is given.
    pub answers: Option<Answers>,
    /// Continue an interrupted reply.
    pub continue_reply: bool,
    /// Where the harness asks before one of its own tools runs, when its
    /// runs follow Rovik's permissions: the run answers. None (a one-off
    /// request, a request outside a run): what would ask is denied.
    pub asks: Option<ToolAsks>,
}

/// A harness's request to run one of its own tools, put to the run's
/// permissions and approvals as a call of Rovik's tool of the same kind
/// would be (Claude Code's `Bash` as `bash`, its file edits as `write`):
/// refused, run, or shown for the user's or the AI reviewer's approval.
#[derive(Debug)]
pub struct ToolAsk {
    /// The harness's tool use: the id of its step in the reply.
    pub call_id: String,
    /// Rovik's tool it's asked as, and that tool's arguments (JSON).
    pub tool: String,
    pub arguments: String,
    pub reply: oneshot::Sender<ToolAnswer>,
}

/// The run's answer to a `ToolAsk`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolAnswer {
    /// It may run; with these arguments (Rovik's tool's) when the user
    /// changed them: some of an edit's changes accepted.
    Allow { arguments: Option<String> },
    /// It may not: why, for the model.
    Deny { reason: String },
}

/// Where a harness's `ToolAsk`s go.
pub type ToolAsks = mpsc::UnboundedSender<ToolAsk>;

/// The tool calls of the chat's last reply, as Rovik ran them: what a
/// harness that made them (Rovik's tools mode) is answered with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answers {
    /// The calls, in order: id and name.
    pub calls: Vec<(String, String)>,
    /// Their results as Rovik stored them, by call id.
    pub results: HashMap<String, String>,
    /// What came after them for the model: Rovik's notes (a check that
    /// failed), a message the user sent while it worked.
    pub notes: String,
}

/// Ends the chat's harness run waiting on Rovik's results, if any: its
/// run is over (agent_run's `Run` drops).
pub fn end_live(dialog_id: &str) {
    for kind in ConnectorKind::ALL {
        connector(kind).end_live(dialog_id);
    }
}

/// Ends every waiting harness run, as the process exits.
pub fn end_all() {
    for kind in ConnectorKind::ALL {
        connector(kind).end_all();
    }
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

    /// What a "Set up" step runs for the connector `args` names: its
    /// installer, its sign-in. setup.rs runs it, shows its output, opens
    /// the page it prints and gives it the code the user types. None when
    /// the connector has no such step (the user installs or signs in as its
    /// own docs say); `Err` when it can't run now (no program to sign in).
    fn setup_command(
        &self,
        action: SetupAction,
        args: &ProbeRequestArgs,
    ) -> Result<Option<SetupCommand>, String> {
        let _ = (action, args);
        Ok(None)
    }

    /// Ends the chat's run left waiting between Rovik's turns (Rovik's
    /// tools mode), if it keeps one.
    fn end_live(&self, dialog_id: &str) {
        let _ = dialog_id;
    }

    /// Ends every run left waiting, as the process exits.
    fn end_all(&self) {}

    /// The commands the program takes as a message (`/name arguments`), as
    /// it finds them working in `cwd`: for the chat's command picker. The
    /// default: none, and a connector without commands shows no picker.
    fn commands<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
        cwd: &'a std::path::Path,
    ) -> BoxFuture<'a, Result<Vec<ConnectorCommand>, String>> {
        let _ = (args, cwd);
        Box::pin(async { Ok(Vec::new()) })
    }

    /// The newest version of the program its makers publish, when it can
    /// be found out: a probe says when the installed one is older, and
    /// "Set up" offers to update it (`SetupAction::Update`). None: unknown.
    fn latest_version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    /// Whether, running its own tools, it plans in a mode of its own when
    /// the run is in Plan mode (`Turn::plan_mode`): it reads, changes
    /// nothing, and its plan is the reply. Without one (the default), Plan
    /// mode refuses its own tools, which would carry the plan out.
    fn plan_mode(&self) -> bool {
        false
    }
}

/// The connector of a kind.
pub fn connector(kind: ConnectorKind) -> &'static dyn Connector {
    match kind {
        ConnectorKind::ClaudeCode => &claude_code::ClaudeCode,
    }
}

/// How long finding out the newest version may take: it's the probe's
/// extra, never what holds it up.
const LATEST_WITHIN: Duration = Duration::from_secs(5);

/// "Test & refresh models" on `args.kind`, on this computer: the program,
/// its models, and the newer version published, if there is one.
pub async fn probe(args: ProbeRequestArgs) -> Result<ConnectorProbe, String> {
    let connector = connector(args.kind);
    let latest = tokio::time::timeout(LATEST_WITHIN, connector.latest_version());
    let (probe, latest) = tokio::join!(connector.probe(&args), latest);
    let mut probe = probe?;
    probe.update = latest
        .ok()
        .flatten()
        .filter(|latest| is_newer(latest, &probe.version));
    Ok(probe)
}

/// The commands `args.kind`'s program takes, found in `cwd` (the chat's
/// project, where its own commands are), on this computer.
pub async fn commands(
    args: ProbeRequestArgs,
    cwd: Option<String>,
) -> Result<Vec<ConnectorCommand>, String> {
    let cwd = cwd
        .map(PathBuf::from)
        .filter(|c| c.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    connector(args.kind).commands(&args, &cwd).await
}

/// Whether version `candidate` is newer than `installed`, each read from
/// its first run of numbers and dots (`2.1.309 (Claude Code)`). False when
/// either has none: unknown isn't newer.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    fn numbers(version: &str) -> Option<Vec<u64>> {
        let start = version.find(|c: char| c.is_ascii_digit())?;
        let run: String = version[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        run.split('.')
            .filter(|n| !n.is_empty())
            .map(|n| n.parse().ok())
            .collect()
    }
    match (numbers(candidate), numbers(installed)) {
        (Some(candidate), Some(installed)) => candidate > installed,
        _ => false,
    }
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
    let settings = params.connector.clone().unwrap_or(Value::Null);
    let tools = params.tools.clone().unwrap_or_default();
    // Rovik's tools mode: the harness takes the run's tools over MCP.
    let rovik_tools =
        has_history && settings.get("tools").and_then(|t| t.as_str()) == Some("rovik");
    // The system prompt: a one-off request's own, the run's (a subagent's,
    // Autopilot's), or the coding agent's with the project's section apart.
    let (system_prompt, system_context) = match (messages_system, params.system_prompt.clone()) {
        (Some(system), _) | (None, Some(system)) => (system, None),
        (None, None) => (
            crate::agent::lead_system_prompt(rovik_tools && !tools.is_empty(), None, false),
            params
                .project_context
                .clone()
                .filter(|c| !c.trim().is_empty()),
        ),
    };
    let effort = params.options.as_ref().and_then(|o| o.reasoning_effort);
    let first_byte_timeout = Duration::from_millis(params.first_byte_timeout_ms.unwrap_or(60_000));
    let continuing = params.continue_reply.unwrap_or(false);
    let (prompt, session, recap, answers) = match (messages_prompt, &params.dialog_id) {
        (Some(prompt), _) => (prompt, None, None, None),
        (None, Some(dialog_id)) => {
            let id = dialog_id.clone();
            let rows = db
                .read(move |c| crate::chat_db::store::get_messages(c, &id, None, RECENT_MESSAGES))
                .await
                .map(|page| page.messages)?;
            let answers = match rovik_tools {
                true => tool_answers(db, &rows).await?,
                false => None,
            };
            let session = sessions::get(&data_dir, kind, dialog_id);
            let current = params.current_user_content.as_deref();
            let ask = |new_session| {
                prompt_for(
                    &rows,
                    current,
                    continuing,
                    new_session,
                    rovik_tools,
                    answers.as_ref(),
                )
            };
            let prompt = ask(session.is_none());
            let recap = session.is_some().then(|| ask(true));
            (prompt, session, recap, answers)
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
        system_context,
        prompt,
        cwd: params.cwd.clone(),
        isolated: params.isolated,
        plan_mode: params.plan_mode,
        session,
        recap,
        one_shot: !has_history,
        effort,
        first_byte_timeout,
        dialog_id: params.dialog_id.clone(),
        tools,
        answers,
        continue_reply: continuing,
        asks: params.tool_asks.clone(),
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
///
/// In Rovik's tools mode (`rovik_tools`) every message of the harness's is
/// a reply, its tool calls' too, and when the last one made calls whose
/// results its session never got (`answers`: the run ended while it
/// waited), they're given first.
pub fn prompt_for(
    rows: &[MessageRow],
    current_user_content: Option<&str>,
    continue_reply: bool,
    new_session: bool,
    rovik_tools: bool,
    answers: Option<&Answers>,
) -> String {
    // Rows up to the pending reply this turn fills (unfinished, at the end).
    let settled: Vec<&MessageRow> = rows
        .iter()
        .filter(|m| m.finalized || m.role != "assistant")
        .collect();
    // The last reply the harness gave: what came after it is this turn's.
    let last_reply = settled.iter().rposition(|m| {
        m.role == "assistant" && (rovik_tools || !m.content.trim().is_empty() || m.error.is_some())
    });
    let since = match last_reply {
        Some(i) => &settled[i + 1..],
        None => &settled[..],
    };
    // A command of the harness's (`/compact`, a skill): the frontend marks a
    // message the user picked a listed command for (`meta.command`). It
    // goes as typed, alone, since the harness reads a command only at the
    // start of the message: no editor context, no recap before it.
    let request = since
        .iter()
        .find(|m| m.role == "user" && !m.failed && is_request(m));
    if let Some(command) = request.filter(|m| is_command(m)) {
        return command.content.trim().to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut found_current = false;
    for m in since.iter().filter(|m| m.role == "user" && !m.failed) {
        let text = if is_request(m) && !found_current {
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
    if let Some(answers) = answers.filter(|a| !a.calls.is_empty()) {
        if parts.is_empty() {
            parts.push("Go on with the task.".into());
        }
        parts.insert(0, results_block(answers));
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
    // Not a command the user picked, but it starts like one: a harness runs
    // a message that starts with `/name` as its command (`/compact` typed by
    // hand would compact). After a space it reads it as text.
    if prompt.starts_with('/') {
        prompt.insert(0, ' ');
    }
    prompt
}

/// The user's own request, not the IDE's notes (`harness`) or what the
/// user added while a run worked (`steer`).
fn is_request(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .is_none_or(|meta| meta.get("harness").is_none() && meta.get("steer").is_none())
}

/// A request the frontend marked as a command of the harness's.
fn is_command(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| meta.get("command"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The most of one result a new session is told.
const RESULT_CHARS: usize = 20_000;

/// The results of the last calls, for a session that never got them.
fn results_block(answers: &Answers) -> String {
    let mut block = String::from(
        "Your previous run ended while you waited for the results of your last tool calls. Rovik ran them; here are their results:",
    );
    for (id, name) in &answers.calls {
        let result = answers
            .results
            .get(id)
            .map(|r| trimmed_text(r, RESULT_CHARS))
            .unwrap_or_else(|| "[Rovik didn't run this call.]".into());
        block.push_str(&format!("\n\n### {name} ({id})\n{result}"));
    }
    block
}

/// The calls of the chat's last reply and Rovik's results for them, when
/// it made any (Rovik's tools mode): large results read whole.
async fn tool_answers(db: &DbState, rows: &[MessageRow]) -> Result<Option<Answers>, String> {
    let settled: Vec<&MessageRow> = rows
        .iter()
        .filter(|m| m.finalized || m.role != "assistant")
        .collect();
    let Some(last) = settled.iter().rposition(|m| m.role == "assistant") else {
        return Ok(None);
    };
    let calls: Vec<(String, String)> = settled[last]
        .tool_calls
        .iter()
        .flatten()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    if calls.is_empty() {
        return Ok(None);
    }
    let after = &settled[last + 1..];
    let mut results = HashMap::new();
    for m in after.iter().filter(|m| m.role == "tool") {
        let Some(call) = m
            .tool_call_id
            .as_ref()
            .filter(|id| calls.iter().any(|(c, _)| c == *id))
        else {
            continue;
        };
        let text = if m.body_external {
            let id = m.id.clone();
            db.read(move |c| crate::chat_db::store::get_message_body(c, &id))
                .await?
        } else {
            m.content.clone()
        };
        results.insert(call.clone(), text);
    }
    let notes = after
        .iter()
        .filter(|m| m.role == "user" && !m.failed)
        .map(|m| sent_text(m))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Some(Answers {
        calls,
        results,
        notes,
    }))
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

/// A stand-in for `claude` in Rovik's tools mode: Python, speaking MCP
/// to Rovik's server as Claude Code does, printing what a run prints.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude_with_mcp(dir: &std::path::Path, read_path: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    if !crate::process::on_path("python3") {
        return None;
    }
    let script = r#"#!/usr/bin/env python3
import json, os, sys, urllib.request
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
open(os.path.join(here, 'args.txt'), 'w').write(' '.join(args))
prompt = sys.stdin.read()
open(os.path.join(here, 'prompt.txt'), 'w').write(prompt)
open(os.path.join(here, 'system.txt'), 'w').write(open(args[args.index('--system-prompt-file') + 1]).read())
server = json.load(open(args[args.index('--mcp-config') + 1]))['mcpServers']['rovik']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def post(body):
    headers = dict(server['headers'])
    headers.update({'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream'})
    request = urllib.request.Request(server['url'], data=json.dumps(body).encode(), headers=headers)
    with opener.open(request) as response:
        text = response.read().decode()
    if text.startswith('{'):
        return json.loads(text)
    for line in text.splitlines():
        if line.startswith('data: '):
            return json.loads(line[6:])
def out(value):
    print(json.dumps(value), flush=True)
def event(e):
    out({'type': 'stream_event', 'event': e, 'parent_tool_use_id': None})
post({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {'protocolVersion': '2025-11-25'}})
tools = post({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list'})['result']['tools']
out({'type': 'system', 'subtype': 'init', 'session_id': 's-9', 'model': 'claude-sonnet-5', 'tools': ['mcp__rovik__' + t['name'] for t in tools]})
event({'type': 'message_start', 'message': {'id': 'm1', 'usage': {'input_tokens': 4}}})
event({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': 'toolu_1', 'name': 'mcp__rovik__read', 'input': {}}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': '{"path": "READ_PATH"}'}})
event({'type': 'content_block_stop', 'index': 0})
event({'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 9}})
event({'type': 'message_stop'})
result = post({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'read', 'arguments': {'path': 'READ_PATH'}, '_meta': {'claudecode/toolUseId': 'toolu_1'}}})
text = result['result']['content'][0]['text']
event({'type': 'message_start', 'message': {'id': 'm2'}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'Got: ' + text}})
event({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'input_tokens': 6, 'output_tokens': 2}})
event({'type': 'message_stop'})
out({'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'Got: ' + text, 'usage': {}, 'total_cost_usd': 0.01, 'num_turns': 2})
"#;
    let path = dir.join("claude");
    std::fs::write(&path, script.replace("READ_PATH", read_path)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(path.to_string_lossy().into_owned())
}

/// A stand-in for `claude` (tests): a shell script, `script`, that prints
/// the lines a run would, as its arguments say.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude(dir: &std::path::Path, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A stand-in for `claude` with its own tools under Rovik's permissions
/// (tests): for each of `asks` (Claude Code's tool name and input, and the
/// tool use's id) it shows the tool use, asks Rovik's `approve` tool over
/// MCP as Claude Code does, and keeps the answer in `decisions.json`; its
/// reply lists the answers. None without python3.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude_asking(dir: &std::path::Path, asks: &Value) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    if !crate::process::on_path("python3") {
        return None;
    }
    let script = r#"#!/usr/bin/env python3
import json, os, sys, urllib.request
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
open(os.path.join(here, 'args.txt'), 'w').write(' '.join(args))
sys.stdin.read()
server = json.load(open(args[args.index('--mcp-config') + 1]))['mcpServers']['rovik']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def post(body):
    headers = dict(server['headers'])
    headers.update({'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream'})
    request = urllib.request.Request(server['url'], data=json.dumps(body).encode(), headers=headers)
    with opener.open(request) as response:
        text = response.read().decode()
    if text.startswith('{'):
        return json.loads(text)
    for line in text.splitlines():
        if line.startswith('data: '):
            return json.loads(line[6:])
def out(value):
    print(json.dumps(value), flush=True)
def event(e):
    out({'type': 'stream_event', 'event': e, 'parent_tool_use_id': None})
post({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {'protocolVersion': '2025-11-25'}})
out({'type': 'system', 'subtype': 'init', 'session_id': 's-7', 'model': 'claude-sonnet-5', 'tools': ['Bash', 'Edit']})
decisions = []
for n, (tool, given, use) in enumerate(ASKS):
    given = dict(given)
    if 'file_path' in given:
        given['file_path'] = os.path.join(os.getcwd(), given['file_path'])
    event({'type': 'message_start', 'message': {'id': 'm%d' % n, 'usage': {'input_tokens': 4}}})
    event({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': use, 'name': tool, 'input': {}}})
    event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(given)}})
    event({'type': 'content_block_stop', 'index': 0})
    event({'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 3}})
    event({'type': 'message_stop'})
    asked = {'tool_name': tool, 'input': given, 'tool_use_id': use}
    result = post({'jsonrpc': '2.0', 'id': 10 + n, 'method': 'tools/call', 'params': {'name': 'approve', 'arguments': asked, '_meta': {'claudecode/toolUseId': use}}})
    decision = json.loads(result['result']['content'][0]['text'])
    decisions.append(decision)
    out({'type': 'user', 'message': {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': use, 'content': decision['behavior']}]}})
json.dump(decisions, open(os.path.join(here, 'decisions.json'), 'w'))
reply = ', '.join(d['behavior'] for d in decisions)
event({'type': 'message_start', 'message': {'id': 'last'}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': reply}})
event({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 2}})
event({'type': 'message_stop'})
out({'type': 'result', 'subtype': 'success', 'is_error': False, 'result': reply, 'usage': {}, 'total_cost_usd': 0.01, 'num_turns': 1})
"#;
    let path = dir.join("claude");
    std::fs::write(&path, script.replace("ASKS", &asks.to_string())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(path.to_string_lossy().into_owned())
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
        let prompt = prompt_for(&rows, None, false, false, false, None);
        assert_eq!(
            prompt,
            "Checks failed:\n- lint\n\nAlso rename it\n\n[editor: foo.rs]"
        );
        let prompt = prompt_for(
            &rows,
            Some("Also rename it (sent)"),
            false,
            false,
            false,
            None,
        );
        assert!(prompt.ends_with("Also rename it (sent)"), "{prompt}");
        assert!(
            !prompt.contains("First ask"),
            "earlier turns are the session's own"
        );
    }

    #[test]
    fn only_a_picked_command_goes_as_one_typed_and_alone() {
        let command = Some(json!({ "command": true, "sentContent": "/compact keep the API" }));
        let rows = vec![
            row("user", "First ask", None, true),
            row("assistant", "Did it.", None, true),
            row("user", " /compact keep the API ", command, true),
            row("assistant", "", None, false),
        ];
        // No editor context before it, and no recap even for a new session.
        let sent = Some("[editor: foo.rs]\n\n/compact keep the API");
        let prompt = prompt_for(&rows, sent, false, true, false, None);
        assert_eq!(prompt, "/compact keep the API");

        // Not one of the harness's commands: a message like any other.
        let rows = vec![
            row("user", "/etc/passwd has a typo", None, true),
            row("assistant", "", None, false),
        ];
        let sent = Some("[editor: foo.rs]\n\n/etc/passwd has a typo");
        let prompt = prompt_for(&rows, sent, false, false, false, None);
        assert_eq!(prompt, "[editor: foo.rs]\n\n/etc/passwd has a typo");

        // Typed by hand, not picked: a message, though nothing comes before
        // it. After a space, the harness doesn't run it as its command.
        let rows = vec![
            row("user", "/compact keep the API", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, false, false, false, None);
        assert_eq!(prompt, " /compact keep the API");
    }

    #[test]
    fn a_new_session_gets_a_recap_of_the_earlier_turns() {
        let rows = vec![
            row("user", "First ask", None, true),
            row("assistant", "Did it.", None, true),
            row("user", "Next", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, false, true, false, None);
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
        assert_eq!(prompt_for(&first, None, false, true, false, None), "Hello");
    }

    #[test]
    fn continuing_an_interrupted_reply_asks_for_it() {
        let rows = vec![
            row("user", "Ask", None, true),
            row("assistant", "Half a re", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, true, false, false, None);
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
        let prompt = prompt_for(&rows, None, false, true, false, None);
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
    fn versions_compare_by_their_numbers() {
        assert!(is_newer("2.1.310", "2.1.309 (Claude Code)"));
        assert!(is_newer("2.10.0", "2.9.9"));
        assert!(is_newer("2.1.0.1", "2.1.0"));
        assert!(!is_newer("2.1.309", "2.1.309 (Claude Code)"));
        assert!(!is_newer("2.1.308", "2.1.309"));
        assert!(!is_newer("2.1.310", ""), "an unknown version isn't older");
        assert!(!is_newer("", "2.1.309"));
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

//! Connectors: coding harnesses a chat uses in place of a model provider
//! (rovik_protocol::connectors). A harness such as Claude Code or Codex
//! brings its own model loop and tools: the IDE hands it the system prompt,
//! the model, the message and the project folder, follows what it does, and
//! shows its steps, its tokens and its cost with the reply.
//!
//! The chat request path is the same as a provider's: `agent::run_chat`
//! sees a connector's base URL (`connector://claude-code`) and hands the
//! request to [`run_chat`] here, which turns the stored chat into one turn
//! for the harness and reports it through the same `ChatStreamEvent`s.
//!
//! Whose tools the harness works with is the run's mode's: a lead's run in
//! Agent mode offers such a model no tools of the IDE's (agent_run marks
//! its profile `no_tools`), and the harness runs its own, each shown as a
//! step the IDE never ran (`ChatStreamEvent::HarnessStep`). What those
//! steps change is the run's change all the same: its checks run on it
//! ([`Connector::step_calls`]). Every other run (Plan mode, Autopilot's
//! stages, a subagent) offers Rovik's: the harness is the model behind
//! Rovik's agent, with Rovik's prompt, and its calls of Rovik's tools (over
//! MCP, mcp.rs) are the reply's own (`Turn::rovik_tools`).
//!
//! A harness keeps its own conversation: the session it opens for a chat
//! is remembered (sessions.rs), and later turns continue it, so it keeps
//! what it read and did. Where the session is gone (another computer, a
//! cleared folder), the turn starts a new one with a recap of the chat.
//!
//! Every connector implements [`Connector`]: a probe for "Test & refresh
//! models", its sign-in check, whether its program is installed, a turn,
//! what each of its own steps did, and, where it has them, what its "Set
//! up" steps run (setup.rs runs them, the same for every connector), its
//! commands and its newest version. A new harness is one more module here
//! implementing it (claude_code.rs, codex/), one more `ConnectorKind`, and
//! its line in [`connector`]; nothing else here or in the run changes. The
//! app's side is its `ConnectorSpec` and its `ConnectorFields`
//! (src/lib/connectors.ts).

mod claude_code;
mod codex;
mod harness;
mod live;
pub mod mcp;
pub mod sessions;
mod setup;
mod steps;

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

/// What a message says first when it changes the harness's tools in the
/// middle of its session (`prompt_for`): the tools it called before stay in
/// its conversation, and it would call them again. Its own tools now (Agent
/// mode), or Rovik's (Plan mode, Autopilot).
const NOW_ITS_OWN_TOOLS: &str = "Rovik IDE: you work with your own tools from this message on. \
     Rovik's tools, which you called earlier in this conversation, aren't available any more.";
const NOW_ROVIK_TOOLS: &str = "Rovik IDE: you work with Rovik's tools from this message on, as \
     your system prompt lists them. Your own tools, which you used earlier in this conversation, \
     aren't available any more.";

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
    /// Where in the reply's text it came: the text's length then, in UTF-16
    /// code units (as the chat slices strings), so the chat shows the text
    /// and the steps in the order they went. The run sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<usize>,
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
    /// The chat's section of the prompt for a harness running its own
    /// tools, which reads no instructions at the top of a workspace chat's
    /// view (4.10): Claude Code's appended prompt, Codex's developer
    /// instructions. None in a folder, whose own it reads.
    pub own_tools_context: Option<String>,
    /// The run offers its tools (`tools`): Plan mode, Autopilot's stages, a
    /// subagent. The harness runs as Rovik's agent, with Rovik's prompt and
    /// those tools over MCP, which Rovik runs. Otherwise (a lead's run in
    /// Agent mode, a one-off request) it runs its own, or none.
    pub rovik_tools: bool,
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
    /// harness offers the model as Rovik's (`rovik_tools`).
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
    /// Where the harness reports the commands its own shell runs, before
    /// each and after one fails, so the run makes ready what they need
    /// (agent_run/environment.rs): the run answers. None (a one-off
    /// request, a request outside a run): nothing is made ready.
    pub commands: Option<CommandChecks>,
    /// The project's files its own tools may not read or change
    /// (restricted_files.rs), relative to `cwd`: a harness that can deny its
    /// tools files does.
    pub denied: crate::restricted_files::Denied,
    /// The user's allowed hosts are on (allowed_hosts.rs): its program is
    /// held to them, and its hosted web search, which its provider runs
    /// where the list can't reach, is off.
    pub allowed_hosts_on: bool,
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

/// A call of the harness's own tools, put to the run before it runs (and
/// a command, after it failed): the run refuses one that would read or
/// change a restricted file, makes ready what a command needs (a cloud
/// host's Node, Yarn through Corepack, the project's dependencies) and
/// answers with what the model should know of it, if anything.
#[derive(Debug)]
pub struct CommandCheck {
    /// The harness's tool use: the id of its step in the reply.
    pub call_id: String,
    /// What it runs.
    pub call: HarnessCall,
    /// The folder its shell is in, which a command runs in: one `cd`
    /// takes it to stays for the next. None: the project's.
    pub cwd: Option<String>,
    /// None: it's about to run. Some: it failed, with its exit code (None
    /// when its harness doesn't say) and its output.
    pub failed: Option<(Option<i32>, String)>,
    /// The run's answer.
    pub reply: oneshot::Sender<CommandAnswer>,
}

/// What a harness's own tool call does: what the run checks before it runs
/// (`CommandCheck`), and what it records of a step that ran
/// ([`Connector::step_calls`]).
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessCall {
    /// A command line, as its shell runs it.
    Command(String),
    /// A call of its own file tools, as the Rovik tool call that does the
    /// same (`read` for Claude Code's `Read`, a `bash` `rg` for its
    /// `Grep`): before it runs, only checked against the restricted files;
    /// a `write` that ran is one of the run's changes.
    Tool {
        name: String,
        args: serde_json::Map<String, Value>,
    },
}

/// The run's answer to a `CommandCheck`.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandAnswer {
    /// It runs, or ran: what the model should be told of it, if anything.
    Note(Option<String>),
    /// It may not run (asked before it does): why, for the model.
    Deny(String),
}

/// Where a harness's `CommandCheck`s go.
pub type CommandChecks = mpsc::UnboundedSender<CommandCheck>;

/// Why a call before which Rovik couldn't be asked doesn't run.
pub(crate) const UNCHECKED: &str = "Error: Rovik IDE couldn't check this call against the project's restricted files, so it didn't run. Try it again.";

/// The run's answer to a check as a harness's hook gives it back for its
/// event (`PreToolUse`…), in the hook output Claude Code and Codex both
/// read: a refusal, whose reason the model reads; what the model should be
/// told beside the call's result; or nothing to say.
pub(crate) fn hook_answer(event: &str, said: CommandAnswer) -> Value {
    match said {
        CommandAnswer::Deny(reason) => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            },
        }),
        CommandAnswer::Note(Some(text)) => json!({
            "hookSpecificOutput": { "hookEventName": event, "additionalContext": text },
        }),
        CommandAnswer::Note(None) => json!({}),
    }
}

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

    /// Whether its program is installed on this computer, where a chat's
    /// run finds it when the connector's settings name no program: what a
    /// cloud host reports it has (its page, `rovik-agentd doctor`). It
    /// looks for the program and runs nothing.
    fn installed(&self) -> bool;

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

    /// What one of the harness's own steps did, as the Rovik calls that do
    /// the same ([`HarnessCall`]): a command of its shell, a call of its
    /// file tools (a change to a file is a `write` of it; a move, of both
    /// its files). Empty for a step that ran none (a web search, a
    /// thought). After each turn the run records what the steps that
    /// succeeded changed, as it does for its own calls
    /// (agent_run/harness_changes.rs): its checks then run on the
    /// harness's changes as on its own.
    fn step_calls(&self, step: &HarnessStep) -> Vec<HarnessCall>;

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
}

/// The connector of a kind.
pub fn connector(kind: ConnectorKind) -> &'static dyn Connector {
    match kind {
        ConnectorKind::ClaudeCode => &claude_code::ClaudeCode,
        ConnectorKind::Codex => &codex::Codex,
    }
}

/// The connectors whose program is installed on this computer
/// ([`Connector::installed`]), in the order of `ConnectorKind::ALL`.
pub fn installed() -> Vec<ConnectorKind> {
    ConnectorKind::ALL
        .into_iter()
        .filter(|kind| connector(*kind).installed())
        .collect()
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
    // Rovik's tools mode: the run offers its tools (it doesn't in Agent
    // mode, agent_run), which the harness takes over MCP.
    let rovik_tools = has_history && !tools.is_empty();
    // The system prompt: a one-off request's own, the run's (a subagent's,
    // Autopilot's), or the coding agent's with the project's section apart.
    let (system_prompt, system_context) = match (messages_system, params.system_prompt.clone()) {
        (Some(system), _) | (None, Some(system)) => (system, None),
        (None, None) => (
            crate::agent::lead_system_prompt(rovik_tools, None, false),
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
            // The chat's mode changed whose tools the session works with
            // since its last turn (Claude Code keeps one session for both).
            let tools_changed = session
                .as_deref()
                .and_then(|s| worked_with_rovik_tools(&rows, s))
                .is_some_and(|had| had != rovik_tools);
            let current = params.current_user_content.as_deref();
            let ask = |new_session| {
                prompt_for(
                    &rows,
                    current,
                    continuing,
                    new_session,
                    rovik_tools,
                    tools_changed,
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
    // What its own tools may not touch, found in the project (a walk: off
    // the async threads).
    let denied = match (&params.cwd, &params.restricted) {
        (Some(cwd), Some(restricted)) if has_history => {
            let (cwd, restricted) = (cwd.clone(), restricted.clone());
            crate::rt::spawn_blocking(move || restricted.denied(&cwd))
                .await
                .unwrap_or_default()
        }
        _ => Default::default(),
    };
    // A workspace chat's view has no instructions at its top: a harness
    // with its own tools is given the chat's section instead.
    let own_tools_context = (params.folders_view && has_history && !rovik_tools)
        .then(|| params.project_context.clone())
        .flatten()
        .filter(|c| !c.trim().is_empty());
    let turn = Turn {
        settings,
        api_key: params.api_key.clone(),
        model: params.model.clone(),
        system_prompt,
        system_context,
        prompt,
        cwd: params.cwd.clone(),
        isolated: params.isolated,
        own_tools_context,
        rovik_tools,
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
        commands: params.command_checks.clone(),
        denied,
        allowed_hosts_on: crate::allowed_hosts::current().is_some(),
    };
    // The session the harness opens or continues is kept for the chat's
    // next turn, as the events say it.
    let dialog_id = params.dialog_id.clone();
    let started = std::time::Instant::now();
    let mut remember = |event: ChatStreamEvent| {
        if let (ChatStreamEvent::HarnessSession { session, .. }, Some(dialog)) =
            (&event, &dialog_id)
        {
            sessions::put(&data_dir, kind, dialog, session);
        }
        // A run that failed says what the user's allowed hosts kept from
        // it: a host it needed, likely (allowed-hosts-plan.md, J10).
        let event = match event {
            ChatStreamEvent::Error { error } => {
                use crate::allowed_hosts::filter::Client;
                let chat = dialog_id.as_deref();
                let hosts = crate::allowed_hosts::refused_since(started, Client::Commands, chat);
                ChatStreamEvent::Error {
                    error: with_kept_out(error, &hosts),
                }
            }
            other => other,
        };
        on_event(event);
    };
    connector(kind).run(turn, cancel, &mut remember).await
}

/// A run's error, with the hosts the user's allowed hosts kept from its
/// program (`hosts`) after it: what to add for the run to work.
fn with_kept_out(mut error: ChatError, hosts: &[String]) -> ChatError {
    if !hosts.is_empty() {
        error.message = format!(
            "{} The allowed hosts kept these out: {}. Add the ones it needs in Settings → \
             Permissions & approval → Network.",
            error.message.trim_end(),
            hosts.join(", ")
        );
    }
    error
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
/// waited), they're given first. When the session it goes on with last
/// worked with the other tools (`tools_changed`), it's told which it has
/// now before anything else.
pub fn prompt_for(
    rows: &[MessageRow],
    current_user_content: Option<&str>,
    continue_reply: bool,
    new_session: bool,
    rovik_tools: bool,
    tools_changed: bool,
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
    } else if tools_changed {
        let now = match rovik_tools {
            true => NOW_ROVIK_TOOLS,
            false => NOW_ITS_OWN_TOOLS,
        };
        prompt = format!("{now}\n\n{prompt}");
    }
    // Not a command the user picked, but it starts like one: a harness runs
    // a message that starts with `/name` as its command (`/compact` typed by
    // hand would compact). After a space it reads it as text.
    if prompt.starts_with('/') {
        prompt.insert(0, ' ');
    }
    prompt
}

/// Whether the harness's `session` worked with Rovik's tools on its last
/// turn in the chat, as the latest reply naming it records
/// (`meta.connector`, agent_run); none when no reply does.
fn worked_with_rovik_tools(rows: &[MessageRow], session: &str) -> Option<bool> {
    rows.iter().rev().find_map(|m| {
        let connector = m.meta.as_ref()?.get("connector")?;
        (connector.get("session")?.as_str()? == session)
            .then(|| connector.get("rovikTools").and_then(Value::as_bool) == Some(true))
    })
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
/// to Rovik's server as Claude Code does, printing what a run prints. It
/// calls Rovik's `tool` with `arguments`, then replies with the result.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude_with_mcp(
    dir: &std::path::Path,
    tool: &str,
    arguments: &Value,
) -> Option<String> {
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
tool, arguments = __TOOL__, json.loads(__ARGUMENTS__)
post({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {'protocolVersion': '2025-11-25'}})
tools = post({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list'})['result']['tools']
out({'type': 'system', 'subtype': 'init', 'session_id': 's-9', 'model': 'claude-sonnet-5', 'tools': ['mcp__rovik__' + t['name'] for t in tools]})
event({'type': 'message_start', 'message': {'id': 'm1', 'usage': {'input_tokens': 4}}})
event({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': 'toolu_1', 'name': 'mcp__rovik__' + tool, 'input': {}}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(arguments)}})
event({'type': 'content_block_stop', 'index': 0})
event({'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 9}})
event({'type': 'message_stop'})
result = post({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': tool, 'arguments': arguments, '_meta': {'claudecode/toolUseId': 'toolu_1'}}})
text = result['result']['content'][0]['text']
event({'type': 'message_start', 'message': {'id': 'm2'}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'Got: ' + text}})
event({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'input_tokens': 6, 'output_tokens': 2}})
event({'type': 'message_stop'})
out({'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'Got: ' + text, 'usage': {}, 'total_cost_usd': 0.01, 'num_turns': 2})
"#;
    let path = dir.join("claude");
    // Python string literals, as JSON writes them.
    let literal = |text: &str| Value::String(text.into()).to_string();
    let script = script
        .replace("__TOOL__", &literal(tool))
        .replace("__ARGUMENTS__", &literal(&arguments.to_string()));
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(path.to_string_lossy().into_owned())
}

/// A stand-in for `codex` (tests): codex/fake_codex.py, an app server that
/// plays `scenario`'s turn as Codex's would, calling Rovik's hooks and
/// tools as Codex does, and keeps what happened in `codex.json` beside it.
/// None without a python3 that reads TOML (3.11 and later).
#[cfg(all(test, unix))]
pub(crate) fn fake_codex(dir: &std::path::Path, scenario: &Value) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let reads_toml = crate::process::on_path("python3")
        && std::process::Command::new("python3")
            .args(["-c", "import tomllib"])
            .status()
            .is_ok_and(|s| s.success());
    if !reads_toml {
        return None;
    }
    let script = include_str!("codex/fake_codex.py");
    let path = dir.join("codex");
    std::fs::write(&path, script.replace("__SCENARIO__", &scenario.to_string())).unwrap();
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

/// A stand-in for `claude` running its own tools under Rovik's hooks
/// (tests), one turn of [`fake_claude_turns`]: `commands`, its steps.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude_with_hooks(dir: &std::path::Path, commands: &Value) -> Option<String> {
    fake_claude_turns(dir, &json!([commands]))
}

/// A stand-in for `claude` running its own tools under Rovik's hooks, a
/// turn a run (tests): its `n`th run plays `turns[n]` (the last one, past
/// them), and keeps what it was asked in `prompt-<n>.txt`. A step is a
/// command (the command, the `error` Claude Code reports when it fails or
/// null, the tool use's id, and the folder its shell is in, from the
/// project's, if not that), or an object of a call of its other tools:
/// `tool`, `input`, `use`, its `error` if it fails, and the `files` it
/// changes when it runs (each path's new text; null deletes it). Each step
/// shows its tool use, and posts its `PreToolUse` event to the hooks its
/// `--settings` name for that tool, as Claude Code does: one the hook
/// refuses doesn't run, its result the refusal. For a command that fails,
/// its `PostToolUseFailure` event is posted too. The answers, with each
/// event's name and tool use (null for a tool no hook is for), go to
/// `hooks.json`, and what its `CLAUDE_ENV_FILE` holds to `env.txt`: the
/// last run's. None without python3.
#[cfg(all(test, unix))]
pub(crate) fn fake_claude_turns(dir: &std::path::Path, turns: &Value) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    if !crate::process::on_path("python3") {
        return None;
    }
    let script = r#"#!/usr/bin/env python3
import json, os, re, sys, urllib.request
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
runs = os.path.join(here, 'runs.txt')
run = int(open(runs).read()) if os.path.exists(runs) else 0
open(runs, 'w').write(str(run + 1))
open(os.path.join(here, 'args.txt'), 'w').write(' '.join(args))
env_file = os.environ.get('CLAUDE_ENV_FILE')
open(os.path.join(here, 'env.txt'), 'w').write(open(env_file).read() if env_file else '')
open(os.path.join(here, 'prompt-%d.txt' % run), 'w').write(sys.stdin.read())
settings = json.load(open(args[args.index('--settings') + 1]))
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def hook(event):
    entries = [e for e in settings['hooks'][event['hook_event_name']] if re.fullmatch(e['matcher'], event['tool_name'])]
    if not entries:
        return None
    target = entries[0]['hooks'][0]
    headers = dict(target['headers'])
    headers['Content-Type'] = 'application/json'
    request = urllib.request.Request(target['url'], data=json.dumps(event).encode(), headers=headers)
    with opener.open(request) as response:
        return json.loads(response.read().decode().strip() or '{}')
def out(value):
    print(json.dumps(value), flush=True)
def event(e):
    out({'type': 'stream_event', 'event': e, 'parent_tool_use_id': None})
out({'type': 'system', 'subtype': 'init', 'session_id': 's-8', 'model': 'claude-sonnet-5', 'tools': ['Bash']})
answers = []
turns = json.loads(r'''TURNS''')
for n, step in enumerate(turns[min(run, len(turns) - 1)]):
    files = {}
    if isinstance(step, dict):
        tool, given, error, use, folder = step['tool'], step['input'], step.get('error'), step['use'], None
        files = step.get('files', {})
    else:
        command, error, use = step[:3]
        folder = step[3] if len(step) > 3 else None
        tool, given = 'Bash', {'command': command, 'description': 'run it'}
    event({'type': 'message_start', 'message': {'id': 'm%d' % n, 'usage': {'input_tokens': 4}}})
    event({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': use, 'name': tool, 'input': {}}})
    event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(given)}})
    event({'type': 'content_block_stop', 'index': 0})
    event({'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 3}})
    event({'type': 'message_stop'})
    out({'type': 'assistant', 'message': {'id': 'm%d' % n, 'content': [{'type': 'tool_use', 'id': use, 'name': tool, 'input': given}]}, 'parent_tool_use_id': None})
    cwd = os.path.join(os.getcwd(), folder) if folder else os.getcwd()
    common = {'session_id': 's-8', 'cwd': cwd, 'tool_name': tool, 'tool_input': given, 'tool_use_id': use}
    answer = hook(dict(common, hook_event_name='PreToolUse'))
    answers.append({'event': 'PreToolUse', 'use': use, 'answer': answer})
    said = (answer or {}).get('hookSpecificOutput') or {}
    if said.get('permissionDecision') == 'deny':
        error = said.get('permissionDecisionReason') or 'denied'
    else:
        for path, text in files.items():
            path = os.path.join(os.getcwd(), path)
            if text is None:
                os.remove(path)
            else:
                os.makedirs(os.path.dirname(path), exist_ok=True)
                open(path, 'w').write(text)
        if error is not None:
            failed = dict(common, hook_event_name='PostToolUseFailure', error=error, is_interrupt=False)
            answers.append({'event': 'PostToolUseFailure', 'use': use, 'answer': hook(failed)})
    out({'type': 'user', 'message': {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': use, 'content': error or 'ok', 'is_error': error is not None}]}})
json.dump(answers, open(os.path.join(here, 'hooks.json'), 'w'))
event({'type': 'message_start', 'message': {'id': 'last'}})
event({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'Done.'}})
event({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 2}})
event({'type': 'message_stop'})
out({'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'Done.', 'usage': {}, 'total_cost_usd': 0.01, 'num_turns': 1})
"#;
    let path = dir.join("claude");
    std::fs::write(&path, script.replace("TURNS", &turns.to_string())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_run_names_the_hosts_the_allowed_hosts_kept_out() {
        use crate::chat_request::ErrorKind;
        let error = || ChatError::new(ErrorKind::Network, "Claude Code: API Error: 403 \n");
        assert_eq!(with_kept_out(error(), &[]).message, error().message);
        let hosts = [
            "api.anthropic.com".to_string(),
            "statsig.anthropic.com".into(),
        ];
        assert_eq!(
            with_kept_out(error(), &hosts).message,
            "Claude Code: API Error: 403 The allowed hosts kept these out: api.anthropic.com, \
             statsig.anthropic.com. Add the ones it needs in Settings → Permissions & approval \
             → Network."
        );
    }

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
        let prompt = prompt_for(&rows, None, false, false, false, false, None);
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
        let prompt = prompt_for(&rows, sent, false, true, false, false, None);
        assert_eq!(prompt, "/compact keep the API");

        // Not one of the harness's commands: a message like any other.
        let rows = vec![
            row("user", "/etc/passwd has a typo", None, true),
            row("assistant", "", None, false),
        ];
        let sent = Some("[editor: foo.rs]\n\n/etc/passwd has a typo");
        let prompt = prompt_for(&rows, sent, false, false, false, false, None);
        assert_eq!(prompt, "[editor: foo.rs]\n\n/etc/passwd has a typo");

        // Typed by hand, not picked: a message, though nothing comes before
        // it. After a space, the harness doesn't run it as its command.
        let rows = vec![
            row("user", "/compact keep the API", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, false, false, false, false, None);
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
        let prompt = prompt_for(&rows, None, false, true, false, false, None);
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
        assert_eq!(
            prompt_for(&first, None, false, true, false, false, None),
            "Hello"
        );
    }

    #[test]
    fn a_message_that_changes_the_tools_mid_session_says_which_it_has_now() {
        let rows = vec![
            row("user", "Plan it", None, true),
            row("assistant", "Saved the plan.", None, true),
            row("user", "Implement it", None, true),
            row("assistant", "", None, false),
        ];
        let own = prompt_for(&rows, None, false, false, false, true, None);
        assert_eq!(own, format!("{NOW_ITS_OWN_TOOLS}\n\nImplement it"));
        let rovik = prompt_for(&rows, None, false, false, true, true, None);
        assert_eq!(rovik, format!("{NOW_ROVIK_TOOLS}\n\nImplement it"));
        // The same tools: nothing to say. A new session has none it called.
        assert_eq!(
            prompt_for(&rows, None, false, false, false, false, None),
            "Implement it"
        );
        let fresh = prompt_for(&rows, None, false, true, false, true, None);
        assert!(!fresh.contains(NOW_ITS_OWN_TOOLS), "{fresh}");
        // A picked command goes alone, as typed.
        let command = Some(json!({ "command": true }));
        let rows = vec![
            row("user", "/compact", command, true),
            row("assistant", "", None, false),
        ];
        assert_eq!(
            prompt_for(&rows, None, false, false, false, true, None),
            "/compact"
        );
    }

    #[test]
    fn whose_tools_a_session_last_worked_with_is_its_latest_reply_s() {
        let reply = |connector: Value| {
            row(
                "assistant",
                "x",
                Some(json!({ "connector": connector })),
                true,
            )
        };
        let rows = vec![
            reply(json!({ "session": "s-1", "rovikTools": true })),
            reply(json!({ "session": "s-2" })),
            row("assistant", "a provider's", None, true),
            reply(json!({ "session": "s-1" })),
        ];
        assert_eq!(worked_with_rovik_tools(&rows, "s-1"), Some(false));
        assert_eq!(worked_with_rovik_tools(&rows[..1], "s-1"), Some(true));
        assert_eq!(worked_with_rovik_tools(&rows, "s-2"), Some(false));
        assert_eq!(worked_with_rovik_tools(&rows, "s-3"), None);
    }

    #[test]
    fn continuing_an_interrupted_reply_asks_for_it() {
        let rows = vec![
            row("user", "Ask", None, true),
            row("assistant", "Half a re", None, true),
            row("assistant", "", None, false),
        ];
        let prompt = prompt_for(&rows, None, true, false, false, false, None);
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
        let prompt = prompt_for(&rows, None, false, true, false, false, None);
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

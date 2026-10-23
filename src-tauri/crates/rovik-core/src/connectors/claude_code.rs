//! Claude Code as a connector (connectors/mod.rs): Anthropic's coding
//! harness, run headless (`claude -p`, code.claude.com/docs/en/headless)
//! with its stream of JSON lines read as it works.
//!
//! One turn is one `claude -p` run in the project folder, told the model
//! (`--model`), the effort, the IDE's system prompt (appended to its own,
//! or in its place) and the chat's message on stdin. Its conversation for
//! the chat is kept with `--session-id` and continued with `--resume`, so
//! it keeps its own context between turns. It runs unattended: the
//! permission mode of the settings decides what it may do, nothing waits
//! for a prompt (`--permission-prompts none`), and a turn is bounded by the
//! settings' turns and budget (`--max-turns`, `--max-budget-usd`).
//!
//! What it does comes back as the IDE shows a reply: its text as it streams
//! (`--include-partial-messages`), its thinking as reasoning, each tool it
//! runs as a step with its result, and at the end its tokens and its cost
//! (`result`). A run on a server finds the program as the user's shell
//! would (PATH), or where a server installs it once for everyone
//! (docs/cloud-hosts.md, "Claude Code").
//!
//! Authentication is the program's own: the login of the computer's user
//! (`claude` → `/login`), a long-lived token from `claude setup-token`
//! (`CLAUDE_CODE_OAUTH_TOKEN`), or an API key (`ANTHROPIC_API_KEY`), as
//! the settings say; the token or key travels as the profile's `api_key`,
//! kept where providers' keys are.

use super::mcp::{McpCall, McpReply, McpServer, SERVER_NAME, TOOL_PREFIX};
use super::{
    harness_error, trimmed_text, trimmed_value, usage_value, Answers, Connector, ConnectorAuth,
    ConnectorKind, ConnectorModel, ConnectorProbe, HarnessStep, ProbeRequest, ProbeRequestArgs,
    SetupAction, SetupCommand, Turn,
};
use crate::agent::{ChatStreamEvent, ReasoningEffort};
use crate::chat_request::{ChatError, ErrorKind};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

/// Where a step's input is cut, per field, and its result.
const STEP_INPUT_CHARS: usize = 400;
const STEP_RESULT_CHARS: usize = 1_200;
/// The program's first line is due within this, however slow the start.
const MIN_FIRST_LINE: Duration = Duration::from_secs(120);
/// Between lines once it runs: a long command of its own prints nothing.
const IDLE: Duration = Duration::from_secs(20 * 60);
/// After a Stop, how long the program gets to end before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Of its stderr, kept for an error's details.
const STDERR_KEPT: usize = 16 * 1024;
/// Claude Code's exit status after SIGTERM.
const EXIT_TERMINATED: i32 = 143;
/// Where a replacement system prompt splits: what's above is the same on
/// every run, and stays cached; what's below (the project's section)
/// changes (code.claude.com/docs/en/cli-reference, system prompt flags).
const PROMPT_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";
/// How long an MCP call may wait for Rovik's answer: a day, for an
/// approval the user gives late.
const MCP_TOOL_TIMEOUT_MS: &str = "86400000";
/// What the model is told about Rovik's tools in that mode.
const ROVIK_TOOLS_NOTE: &str = "## Your tools\n\nYour tools are Rovik IDE's, served to you over MCP: each one appears as `mcp__rovik__<name>`, and this prompt names it without the prefix (`read` is `mcp__rovik__read`). Rovik runs each call in the project: its permissions apply, a call may wait for the user's approval, and one that isn't allowed comes back saying so.";
/// The MCP server's instructions, for the model.
const MCP_INSTRUCTIONS: &str = "Rovik IDE's tools for the open project: reading, searching and editing its files, running commands, and the agent's checklist. Rovik runs every call under its own permissions.";

/// Anthropic's native installer (code.claude.com/docs/en/setup): `claude`
/// in the user's `~/.local/bin` (found there, `well_known_places`), with
/// no admin rights, keeping itself up to date.
const INSTALL: &str = if cfg!(windows) {
    "irm https://claude.ai/install.ps1 | iex"
} else {
    "curl -fsSL https://claude.ai/install.sh | bash"
};

/// The connector.
pub struct ClaudeCode;

/// How the program signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Auth {
    /// The login of this computer's user (`claude` → `/login`).
    #[default]
    Login,
    /// A long-lived token from `claude setup-token`, as `CLAUDE_CODE_OAUTH_TOKEN`.
    Token,
    /// An Anthropic API key, as `ANTHROPIC_API_KEY`.
    ApiKey,
}

/// Where the IDE's system prompt goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SystemPromptMode {
    /// After Claude Code's own (`--append-system-prompt`): its tool
    /// guidance and conventions stay.
    #[default]
    Append,
    /// In place of it (`--system-prompt`).
    Replace,
}

/// Whose tools a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolsMode {
    /// Claude Code's own (Read, Edit, Bash…), under the permission mode of
    /// the settings: Rovik shows what it did.
    #[default]
    Own,
    /// Rovik's, over MCP (connectors/mcp.rs): Claude Code is the model
    /// behind Rovik's agent loop, and each call is a tool call of the
    /// reply, which Rovik runs under its own permissions and approvals.
    Rovik,
}

/// The connector's settings, as the frontend keeps them with its entry
/// (src/lib/connectors.ts has the same fields and defaults).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The program: `claude`, found on the PATH, or a path to it.
    pub program: String,
    pub auth: Auth,
    /// `acceptEdits` (the default), `bypassPermissions`, `default`, `plan`,
    /// `dontAsk` or `auto`.
    pub permission_mode: String,
    /// `--allowedTools` rules, one per line or separated by commas.
    pub allowed_tools: String,
    pub disallowed_tools: String,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub system_prompt: SystemPromptMode,
    /// `CLAUDE_CONFIG_DIR`: another login and settings folder, such as one
    /// shared on a server. Empty: the program's own (`~/.claude`).
    pub config_dir: String,
    /// More arguments, as typed on a command line.
    pub extra_args: String,
    /// `--bare`: no hooks, skills, plugins, MCP servers or CLAUDE.md of the
    /// user's. Bare mode doesn't read a login or a token: an API key then.
    pub bare: bool,
    /// Claude Code's own tools, or Rovik's.
    pub tools: ToolsMode,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            program: "claude".into(),
            auth: Auth::Login,
            permission_mode: "acceptEdits".into(),
            allowed_tools: String::new(),
            disallowed_tools: String::new(),
            max_turns: None,
            max_budget_usd: None,
            system_prompt: SystemPromptMode::Append,
            config_dir: String::new(),
            extra_args: String::new(),
            bare: false,
            tools: ToolsMode::Own,
        }
    }
}

impl Settings {
    /// The settings a profile carries; absent fields take their defaults.
    pub fn from_value(value: &Value) -> Settings {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

/// The models Claude Code offers: its aliases, and the names it knows
/// (code.claude.com/docs/en/model-config). The program has no command
/// that lists them; what the account has shows at the first request.
pub fn models() -> Vec<ConnectorModel> {
    const MILLION: u64 = 1_000_000;
    const K200: u64 = 200_000;
    let model = |id: &str, description: &str, context: u64| ConnectorModel {
        id: id.into(),
        owned_by: Some("anthropic".into()),
        context_length: Some(context),
        description: Some(description.into()),
    };
    vec![
        model("sonnet", "The latest Sonnet, for daily coding", MILLION),
        model("opus", "The latest Opus, for complex reasoning", MILLION),
        model("haiku", "Haiku, fast and cheap, for simple tasks", MILLION),
        model("best", "Fable where the account has it, else Opus", MILLION),
        model("fable", "Fable, for the hardest and longest tasks", MILLION),
        model(
            "opusplan",
            "Opus while planning, Sonnet for the work",
            MILLION,
        ),
        model(
            "sonnet[1m]",
            "Sonnet with a 1M-token context window",
            MILLION,
        ),
        model("opus[1m]", "Opus with a 1M-token context window", MILLION),
        model("claude-fable-5-1", "Fable 5.1", MILLION),
        model("claude-fable-5", "Fable 5", MILLION),
        model("claude-opus-5-5", "Opus 5.5", MILLION),
        model("claude-opus-5", "Opus 5", MILLION),
        model("claude-sonnet-5", "Sonnet 5", MILLION),
        model("claude-haiku-5-5", "Haiku 5.5", MILLION),
        model("claude-opus-4-6", "Opus 4.6", K200),
        model("claude-sonnet-4-6", "Sonnet 4.6", K200),
        model("claude-sonnet-4-5", "Sonnet 4.5", K200),
        model("claude-haiku-4-5", "Haiku 4.5", K200),
    ]
}

/// Where the program is looked for besides the PATH: the native
/// installer's place, the package managers', and a server's shared copy.
fn well_known_places() -> Vec<PathBuf> {
    let mut places = Vec::new();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if cfg!(windows) {
        if let Some(home) = &home {
            places.push(home.join(".local").join("bin").join("claude.exe"));
        }
    } else {
        if let Some(home) = &home {
            places.push(home.join(".local/bin/claude"));
        }
        for dir in [
            "/usr/local/bin",
            "/usr/bin",
            "/opt/rovik/bin",
            "/opt/homebrew/bin",
        ] {
            places.push(Path::new(dir).join("claude"));
        }
    }
    places
}

/// The program to run: `program` as given when it names a path, else the
/// first `claude` found on the PATH the IDE's programs get, else one of
/// the places it's installed to. An error says how to install it.
pub fn resolve_program(program: &str) -> Result<String, String> {
    let program = program.trim();
    let program = if program.is_empty() {
        "claude"
    } else {
        program
    };
    if program.contains('/') || program.contains('\\') {
        let path = expand_home(program);
        return if path.is_file() {
            Ok(path.to_string_lossy().into_owned())
        } else {
            Err(format!("{}: no such program.", path.display()))
        };
    }
    if crate::process::on_path(program) {
        return Ok(program.to_string());
    }
    if program == "claude" {
        if let Some(found) = well_known_places().into_iter().find(|p| p.is_file()) {
            return Ok(found.to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "{program} isn't installed, or isn't on the PATH. Install Claude Code \
         (curl -fsSL https://claude.ai/install.sh | bash, or your package manager), \
         or name the program's path in the connector's settings."
    ))
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Splits extra arguments as a shell would, with quotes and backslashes.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut any = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match (quote, c) {
            (None, '\\') | (Some('"'), '\\') => escaped = true,
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                any = true;
            }
            (Some(q), c) if q == c => quote = None,
            (None, c) if c.is_whitespace() => {
                if any || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            (_, c) => current.push(c),
        }
    }
    if any || !current.is_empty() {
        args.push(current);
    }
    args
}

/// `--allowedTools` rules from the settings' text: one per line, or comma-separated.
fn rules(text: &str) -> Vec<String> {
    text.split(['\n', ','])
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

/// The effort as `--effort` takes it.
fn effort_arg(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None | ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Where a turn's system prompt goes: as the settings say, but in place of
/// Claude Code's for a one-off request (a chat's title, a review), which
/// has no tools for Claude Code's prompt to describe, and costs less
/// without it.
fn prompt_mode(settings: &Settings, turn: &Turn) -> SystemPromptMode {
    if turn.one_shot {
        SystemPromptMode::Replace
    } else {
        settings.system_prompt
    }
}

/// The system prompt a turn's run gets: the IDE's, Rovik's tools named
/// when it has them, then the project's section. Replacing Claude Code's,
/// the two are split for its prompt cache (`PROMPT_BOUNDARY`).
pub fn system_text(settings: &Settings, turn: &Turn, rovik_tools: bool) -> String {
    let mut text = turn.system_prompt.trim_end().to_string();
    if rovik_tools && !turn.tools.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(ROVIK_TOOLS_NOTE);
    }
    match turn
        .system_context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(context) if prompt_mode(settings, turn) == SystemPromptMode::Replace => {
            format!("{text}\n{PROMPT_BOUNDARY}\n{context}")
        }
        Some(context) if text.is_empty() => context.to_string(),
        Some(context) => format!("{text}\n\n{context}"),
        None => text,
    }
}

/// The arguments of one turn's `claude` run: the common ones, then the
/// turn's. `session` is the session to resume, or the new one's id;
/// `prompt_file` holds the system prompt (`system_text`), read from a file
/// rather than the command line, which a long prompt would overflow on
/// Windows; `mcp_config` is Rovik's tools' server, in that mode.
pub fn args_for(
    settings: &Settings,
    turn: &Turn,
    session: &Session,
    prompt_file: Option<&Path>,
    mcp_config: Option<&Path>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--permission-prompts".into(),
        "none".into(),
    ];
    if settings.bare {
        args.push("--bare".into());
    }
    let model = turn.model.trim();
    if !model.is_empty() && model != "default" {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(effort) = turn.effort {
        args.push("--effort".into());
        args.push(effort_arg(effort).into());
    }
    if let Some(file) = prompt_file {
        let flag = match prompt_mode(settings, turn) {
            SystemPromptMode::Append => "--append-system-prompt-file",
            SystemPromptMode::Replace => "--system-prompt-file",
        };
        args.push(flag.into());
        args.push(file.to_string_lossy().into_owned());
        // A resumed session takes the prompt as it is now, not as it was
        // at the session's first request: the project's section changes.
        args.push("--system-prompt-snapshot".into());
        args.push("off".into());
    }
    if turn.one_shot {
        // A chat's title, a review: an answer in words, nothing kept.
        args.push("--tools".into());
        args.push(String::new());
        args.push("--max-turns".into());
        args.push("1".into());
        args.push("--no-session-persistence".into());
    } else if let Some(config) = mcp_config {
        // Rovik's tools alone: none of Claude Code's own, Rovik's server
        // alone, and its tools allowed, since Rovik decides on each call.
        args.push("--tools".into());
        args.push(String::new());
        args.push("--mcp-config".into());
        args.push(config.to_string_lossy().into_owned());
        args.push("--strict-mcp-config".into());
        args.push("--permission-mode".into());
        args.push("dontAsk".into());
        args.push("--allowedTools".into());
        args.push(format!("mcp__{SERVER_NAME}"));
    } else {
        let mode = settings.permission_mode.trim();
        if !mode.is_empty() {
            args.push("--permission-mode".into());
            args.push(mode.into());
        }
        let allowed = rules(&settings.allowed_tools);
        if !allowed.is_empty() {
            args.push("--allowedTools".into());
            args.extend(allowed);
        }
        let disallowed = rules(&settings.disallowed_tools);
        if !disallowed.is_empty() {
            args.push("--disallowedTools".into());
            args.extend(disallowed);
        }
    }
    if !turn.one_shot {
        if let Some(turns) = settings.max_turns.filter(|t| *t > 0) {
            args.push("--max-turns".into());
            args.push(turns.to_string());
        }
        if let Some(budget) = settings.max_budget_usd.filter(|b| *b > 0.0) {
            args.push("--max-budget-usd".into());
            args.push(format!("{budget}"));
        }
        match session {
            Session::Resume(id) => {
                args.push("--resume".into());
                args.push(id.clone());
            }
            Session::New(id) => {
                args.push("--session-id".into());
                args.push(id.clone());
            }
        }
    }
    args.extend(split_args(&settings.extra_args));
    args
}

/// The environment a run gets besides the IDE's: how it signs in, and
/// where its settings are.
pub fn env_for(settings: &Settings, api_key: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let key = api_key.trim();
    match settings.auth {
        Auth::Login => {}
        Auth::Token if !key.is_empty() => env.push(("CLAUDE_CODE_OAUTH_TOKEN".into(), key.into())),
        Auth::ApiKey if !key.is_empty() => env.push(("ANTHROPIC_API_KEY".into(), key.into())),
        Auth::Token | Auth::ApiKey => {}
    }
    let config_dir = settings.config_dir.trim();
    if !config_dir.is_empty() {
        env.push((
            "CLAUDE_CONFIG_DIR".into(),
            expand_home(config_dir).to_string_lossy().into_owned(),
        ));
    }
    // The IDE drives it: no terminal, no auto-update mid-run.
    env.push(("DISABLE_AUTOUPDATER".into(), "1".into()));
    env
}

/// What Rovik's tools mode adds to a run's environment: its tools loaded
/// up front (no tool search), a call never moved to the background while
/// Rovik waits for an approval, a day for it, their full descriptions,
/// and the loopback server reached directly, never through a proxy.
pub fn rovik_tools_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("ENABLE_TOOL_SEARCH", "false"),
        ("CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS", "0"),
        ("CLAUDE_AUTO_BACKGROUND_TASKS", "0"),
        ("MCP_TOOL_TIMEOUT", MCP_TOOL_TIMEOUT_MS),
        ("CLAUDE_CODE_MAX_MCP_DESCRIPTION_LENGTH", "16384"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for key in ["NO_PROXY", "no_proxy"] {
        let now = crate::process::child_var(key)
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default();
        let value = if now.trim().is_empty() {
            "127.0.0.1,localhost".to_string()
        } else {
            format!("{now},127.0.0.1,localhost")
        };
        env.push((key.to_string(), value));
    }
    env
}

/// The session a turn runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    Resume(String),
    New(String),
}

/// What a run's lines meant, kept as they come: the text shown so far,
/// the steps by id, and the end.
#[derive(Default)]
pub struct Parser {
    kind_model: Option<String>,
    version: Option<String>,
    session: Option<String>,
    /// Text was streamed for these API messages: their `assistant` lines
    /// repeat it and aren't shown again.
    streamed: HashSet<String>,
    /// A new message's text follows the last one's after a blank line.
    needs_break: bool,
    text_shown: bool,
    steps: HashMap<String, HarnessStep>,
    order: Vec<String>,
    /// The `result` line came: the run is over.
    pub finished: bool,
    /// How it ended, when `result` said: an error of the harness's.
    pub failure: Option<ChatError>,
    /// `--resume` named a session the program doesn't have.
    pub session_gone: bool,
    /// Rovik's tools mode: calls to them are the reply's tool calls, which
    /// Rovik runs, and Rovik's turn ends with each message that makes any.
    rovik_tools: bool,
    /// Rovik's tools being streamed, by content block: id, name, arguments.
    open_calls: HashMap<u64, (String, String, String)>,
    /// The streamed message's calls to them, complete.
    message_calls: Vec<(String, String, String)>,
    /// What the streamed message used, as its events say.
    message_usage: [u64; 4],
    /// What this Rovik turn's messages used: input, cache creation, cache
    /// read, output.
    turn_usage: [u64; 4],
    /// The calls a message made: Rovik's turn ends with them (id, name,
    /// arguments).
    pub yielded: Option<Vec<(String, String, String)>>,
}

/// A message's token counts as the stream gives them.
fn usage_counts(usage: &Value) -> [u64; 4] {
    let n = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    [
        n("input_tokens"),
        n("cache_creation_input_tokens"),
        n("cache_read_input_tokens"),
        n("output_tokens"),
    ]
}

impl Parser {
    /// A parser for Rovik's tools mode.
    pub fn with_rovik_tools() -> Parser {
        Parser {
            rovik_tools: true,
            ..Parser::default()
        }
    }

    /// A Rovik turn begins on the same run (its results were given): a
    /// new reply, its own usage.
    pub fn begin_turn(&mut self) {
        self.text_shown = false;
        self.needs_break = false;
        self.turn_usage = [0; 4];
        self.yielded = None;
    }

    /// Says the session again, for a Rovik turn that goes on with it.
    pub fn announce(&self, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if let Some(session) = self.session.as_ref().filter(|s| !s.is_empty()) {
            emit(ChatStreamEvent::HarnessSession {
                kind: ConnectorKind::ClaudeCode.id().into(),
                session: session.clone(),
                model: self.kind_model.clone(),
                version: self.version.clone(),
                rovik_tools: self.rovik_tools,
            });
        }
    }

    /// This turn's usage, as providers report theirs.
    fn turn_usage_value(&self, cost: Option<f64>, extra: Value) -> Value {
        let [input, creation, read, output] = self.turn_usage;
        usage_value(
            ConnectorKind::ClaudeCode,
            input,
            creation,
            read,
            output,
            cost,
            extra,
        )
    }

    /// One line of stdout. Lines that aren't JSON (a warning) are ignored.
    pub fn line(&mut self, line: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let parent = value
            .get("parent_tool_use_id")
            .and_then(|p| p.as_str())
            .map(str::to_string);
        match kind {
            "system" => self.system(&value, emit),
            "stream_event" => self.stream_event(&value, parent.as_deref(), emit),
            "assistant" => self.assistant(&value, parent, emit),
            "user" => self.tool_results(&value, emit),
            "result" => self.result(&value, emit),
            _ => {}
        }
    }

    fn system(&mut self, value: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        match value.get("subtype").and_then(|s| s.as_str()).unwrap_or("") {
            "init" => {
                let session = value
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let model = value
                    .get("model")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                let version = value
                    .get("claude_code_version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                self.session = Some(session.to_string());
                self.kind_model = model.clone();
                self.version = version.clone();
                if !session.is_empty() {
                    emit(ChatStreamEvent::HarnessSession {
                        kind: ConnectorKind::ClaudeCode.id().into(),
                        session: session.to_string(),
                        model,
                        version,
                        rovik_tools: self.rovik_tools,
                    });
                }
            }
            "api_retry" => {
                let attempt = value.get("attempt").and_then(|a| a.as_u64()).unwrap_or(1) as u32;
                let max = value
                    .get("max_retries")
                    .and_then(|a| a.as_u64())
                    .unwrap_or(0) as u32;
                let delay = value
                    .get("retry_delay_ms")
                    .and_then(|a| a.as_u64())
                    .unwrap_or(0);
                let why = value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("error");
                emit(ChatStreamEvent::Retrying {
                    attempt,
                    max_attempts: max.max(attempt),
                    delay_ms: delay,
                    reason: ChatError::new(ErrorKind::Provider, format!("Claude Code: {why}")),
                });
            }
            _ => {}
        }
    }

    fn stream_event(
        &mut self,
        value: &Value,
        parent: Option<&str>,
        emit: &mut dyn FnMut(ChatStreamEvent),
    ) {
        // A subagent's words aren't the reply's.
        if parent.is_some() {
            return;
        }
        let Some(event) = value.get("event") else {
            return;
        };
        let api_message = value
            .get("api_message_id")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .or_else(|| {
                event
                    .pointer("/message/id")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            });
        let index = event.get("index").and_then(|i| i.as_u64());
        match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "message_start" => {
                if let Some(id) = api_message {
                    self.streamed.insert(id);
                }
                if self.text_shown {
                    self.needs_break = true;
                }
                self.message_usage = event
                    .pointer("/message/usage")
                    .map(usage_counts)
                    .unwrap_or_default();
                self.message_calls.clear();
                self.open_calls.clear();
            }
            "content_block_start" if self.rovik_tools => {
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if let (Some(index), Some(name), false) = (
                    index,
                    name.strip_prefix(TOOL_PREFIX),
                    id.is_empty() || block.get("type").and_then(|t| t.as_str()) != Some("tool_use"),
                ) {
                    self.open_calls
                        .insert(index, (id.to_string(), name.to_string(), String::new()));
                    emit(ChatStreamEvent::ToolCallStart {
                        id: id.to_string(),
                        name: name.to_string(),
                    });
                }
            }
            "content_block_stop" if self.rovik_tools => {
                if let Some((id, name, arguments)) = index.and_then(|i| self.open_calls.remove(&i))
                {
                    let arguments = if arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        arguments
                    };
                    emit(ChatStreamEvent::ToolCallComplete {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                        extra_content: None,
                    });
                    self.message_calls.push((id, name, arguments));
                }
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    let counts = usage_counts(usage);
                    for (kept, new) in self.message_usage.iter_mut().zip(counts) {
                        *kept = (*kept).max(new);
                    }
                }
            }
            "message_stop" if self.rovik_tools => {
                for (total, n) in self.turn_usage.iter_mut().zip(self.message_usage) {
                    *total += n;
                }
                self.message_usage = [0; 4];
                if !self.message_calls.is_empty() {
                    // Rovik's turn ends here: it runs the calls, and the
                    // next turn answers them.
                    let usage = self.turn_usage_value(None, json!({ "model": self.kind_model }));
                    emit(ChatStreamEvent::Finished {
                        finish_reason: Some("tool_calls".into()),
                        usage: Some(usage),
                    });
                    emit(ChatStreamEvent::Done);
                    self.yielded = Some(std::mem::take(&mut self.message_calls));
                }
            }
            "content_block_delta" => {
                if let Some(id) = api_message {
                    self.streamed.insert(id);
                }
                let Some(delta) = event.get("delta") else {
                    return;
                };
                match delta.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "input_json_delta" if self.rovik_tools => {
                        let part = delta
                            .get("partial_json")
                            .and_then(|p| p.as_str())
                            .unwrap_or("");
                        if let Some((id, _, arguments)) =
                            index.and_then(|i| self.open_calls.get_mut(&i))
                        {
                            arguments.push_str(part);
                            if !part.is_empty() {
                                emit(ChatStreamEvent::ToolCallArgumentsDelta {
                                    id: id.clone(),
                                    arguments_delta: part.to_string(),
                                });
                            }
                        }
                    }
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            self.text(text, emit);
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                            emit(ChatStreamEvent::Reasoning {
                                content: text.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if text.is_empty() {
            return;
        }
        let mut content = String::new();
        if std::mem::take(&mut self.needs_break) {
            content.push_str("\n\n");
        }
        content.push_str(text);
        self.text_shown = true;
        emit(ChatStreamEvent::Delta { content });
    }

    fn assistant(
        &mut self,
        value: &Value,
        parent: Option<String>,
        emit: &mut dyn FnMut(ChatStreamEvent),
    ) {
        let Some(message) = value.get("message") else {
            return;
        };
        let id = message.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let streamed = self.streamed.contains(id);
        let blocks = message
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        if parent.is_none() && !streamed && self.text_shown {
            self.needs_break = true;
        }
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "text" if parent.is_none() && !streamed => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        self.text(text, emit);
                    }
                }
                "thinking" if parent.is_none() && !streamed => {
                    if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                        emit(ChatStreamEvent::Reasoning {
                            content: text.into(),
                        });
                    }
                }
                // Rovik's tools: the reply's own tool calls, from the stream.
                "tool_use"
                    if self.rovik_tools
                        && block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .is_some_and(|n| n.starts_with(TOOL_PREFIX)) => {}
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    if id.is_empty() {
                        continue;
                    }
                    let step = HarnessStep {
                        id: id.clone(),
                        name: block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        input: trimmed_value(
                            block.get("input").unwrap_or(&Value::Null),
                            STEP_INPUT_CHARS,
                        ),
                        result: None,
                        ok: None,
                        parent: parent.clone(),
                    };
                    if !self.steps.contains_key(&id) {
                        self.order.push(id.clone());
                    }
                    self.steps.insert(id, step.clone());
                    emit(ChatStreamEvent::HarnessStep { step });
                }
                _ => {}
            }
        }
    }

    fn tool_results(&mut self, value: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let Some(blocks) = value.pointer("/message/content").and_then(|c| c.as_array()) else {
            return;
        };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let id = block
                .get("tool_use_id")
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let Some(step) = self.steps.get_mut(id) else {
                continue;
            };
            let text = match block.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            step.result = Some(trimmed_text(&text, STEP_RESULT_CHARS));
            step.ok = Some(
                !block
                    .get("is_error")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false),
            );
            emit(ChatStreamEvent::HarnessStep { step: step.clone() });
        }
    }

    fn result(&mut self, value: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        self.finished = true;
        let subtype = value.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
        let is_error = value
            .get("is_error")
            .and_then(|e| e.as_bool())
            .unwrap_or(false);
        // A session to resume that the program doesn't have: nothing of it
        // is reported, the turn starts a new one (`Outcome::SessionGone`).
        let said = value
            .get("errors")
            .map(|e| e.to_string())
            .unwrap_or_default()
            + value.get("result").and_then(|r| r.as_str()).unwrap_or("");
        if is_error && said.contains("No conversation found") {
            self.session_gone = true;
            return;
        }
        let usage = value.get("usage").cloned().unwrap_or(Value::Null);
        let count = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        let model = value
            .get("modelUsage")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next().cloned())
            .or_else(|| self.kind_model.clone());
        let denials = value
            .get("permission_denials")
            .and_then(|d| d.as_array())
            .map(|d| d.len())
            .unwrap_or(0);
        let extra = json!({
            "numTurns": value.get("num_turns").cloned().unwrap_or(Value::Null),
            "durationMs": value.get("duration_ms").cloned().unwrap_or(Value::Null),
            "durationApiMs": value.get("duration_api_ms").cloned().unwrap_or(Value::Null),
            "model": model,
            "version": self.version,
            "sessionId": value.get("session_id").cloned().unwrap_or(Value::Null),
            "subtype": subtype,
            "steps": self.order.len(),
            "permissionDenials": denials,
        });
        let cost = value.get("total_cost_usd").and_then(|c| c.as_f64());
        let usage_out = if self.rovik_tools {
            // This Rovik turn's tokens; the cost is the whole run's, which
            // only the end says: its earlier replies were told none.
            let mut extra = extra;
            extra["costCovers"] = json!("harnessTurn");
            self.turn_usage_value(cost, extra)
        } else {
            usage_value(
                ConnectorKind::ClaudeCode,
                count("input_tokens"),
                count("cache_creation_input_tokens"),
                count("cache_read_input_tokens"),
                count("output_tokens"),
                cost,
                extra,
            )
        };
        let errors: Vec<String> = value
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|e| {
                e.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let result_text = value.get("result").and_then(|r| r.as_str()).unwrap_or("");
        let limit = match subtype {
            "error_max_turns" => Some("it reached the turn limit of its settings".to_string()),
            "error_max_budget_usd" => Some("it reached the budget of its settings".to_string()),
            _ => None,
        };
        // The last reply's text comes whole in `result` too: shown only
        // when nothing streamed it (an older version, a run without events).
        if (!is_error || limit.is_some()) && !self.text_shown && !result_text.is_empty() {
            self.text(result_text, emit);
        }
        if let Some(why) = &limit {
            self.text(&format!("\n\n_[Claude Code stopped here: {why}.]_"), emit);
        }
        if denials > 0 {
            self.text(
                &format!(
                    "\n\n_[Claude Code: {denials} tool call{} denied by the permission mode of the connector's settings.]_",
                    if denials == 1 { " was" } else { "s were" }
                ),
                emit,
            );
        }
        if is_error && limit.is_none() {
            let message = if errors.is_empty() {
                if result_text.is_empty() {
                    format!("the run ended with {subtype}")
                } else {
                    result_text.to_string()
                }
            } else {
                errors.join("\n")
            };
            let mut error = harness_error(ConnectorKind::ClaudeCode, message, None);
            error.partial = self.text_shown;
            emit(ChatStreamEvent::Finished {
                finish_reason: Some(subtype.to_string()),
                usage: Some(usage_out),
            });
            self.failure = Some(error.clone());
            emit(ChatStreamEvent::Error { error });
            return;
        }
        emit(ChatStreamEvent::Finished {
            finish_reason: value
                .get("stop_reason")
                .and_then(|s| s.as_str())
                .map(str::to_string)
                .or_else(|| Some(subtype.to_string())),
            usage: Some(usage_out),
        });
        emit(ChatStreamEvent::Done);
    }

    /// Whether any of the reply was shown (for an error's `partial`).
    pub fn shown(&self) -> bool {
        self.text_shown || !self.order.is_empty()
    }
}

/// A `claude` command, as the IDE starts programs, in `cwd`, with the
/// turn's environment and its stdio piped.
fn command(program: &str, cwd: Option<&Path>, env: &[(String, String)]) -> std::process::Command {
    let mut cmd = crate::process::command(program);
    crate::process::no_window(&mut cmd);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
        // A server's shared toolchains, as the agent's own commands get them.
        for (key, value) in crate::toolchains::env_for(cwd) {
            cmd.env(key, value);
        }
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group: a Stop ends what it started too.
    crate::process::own_group(&mut cmd);
    cmd
}

/// The tail of stderr, for an error's details.
async fn read_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = stderr.read_to_end(&mut buf).await;
    let text = String::from_utf8_lossy(&buf);
    let kept: String = text
        .chars()
        .rev()
        .take(STDERR_KEPT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    kept.trim().to_string()
}

/// A private temporary file (a system prompt, an MCP configuration with
/// its token), gone with the run it's for.
struct TempFile(PathBuf);

impl TempFile {
    fn write(what: &str, extension: &str, text: &str) -> Result<TempFile, String> {
        let path = std::env::temp_dir().join(format!(
            "rovik-claude-{what}-{}.{extension}",
            uuid::Uuid::new_v4().simple()
        ));
        crate::atomic_file::write_atomic_private(&path, text.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(TempFile(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A `claude` run: its output's lines as they come, its stderr, and the
/// files it reads, which go with it. Dropped while it runs, it's killed
/// with everything it started.
struct Process {
    child: tokio::process::Child,
    pid: Option<u32>,
    lines: mpsc::UnboundedReceiver<String>,
    stderr: Option<tokio::task::JoinHandle<String>>,
    got_line: bool,
    first_line_within: Duration,
    _files: Vec<TempFile>,
}

impl Process {
    /// Starts `program`, the turn's message on its stdin.
    fn spawn(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        prompt: String,
        first_line_within: Duration,
        files: Vec<TempFile>,
    ) -> Result<Process, String> {
        let mut cmd = tokio::process::Command::from(command(program, cwd, env));
        cmd.args(args).kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Couldn't start {program}: {e}"))?;
        // The message on stdin, which closes once written.
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin.write_all(prompt.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        let stderr = Some(tokio::spawn(read_stderr(child.stderr.take())));
        let Some(stdout) = child.stdout.take() else {
            return Err("Claude Code's output couldn't be read.".into());
        };
        let (sender, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("claude code: reading its output: {e}");
                        break;
                    }
                }
            }
        });
        Ok(Process {
            pid: child.id(),
            child,
            lines,
            stderr,
            got_line: false,
            first_line_within,
            _files: files,
        })
    }

    fn kill(&mut self, force: bool) {
        crate::process::kill_group(self.pid, &mut self.child, force);
    }

    /// How it ended: its exit code (none when killed) and its stderr's end.
    /// What it left running is ended.
    async fn finish(&mut self) -> (Option<i32>, String) {
        let status = tokio::time::timeout(STOP_GRACE, self.child.wait()).await;
        let code = match status {
            Ok(Ok(status)) => status.code(),
            _ => None,
        };
        // Anything of the group still running, and its hold on stderr.
        crate::process::kill_group(self.pid, &mut self.child, true);
        let stderr = match self.stderr.take() {
            Some(task) => tokio::time::timeout(STOP_GRACE, task)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default(),
            None => String::new(),
        };
        (code, stderr)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill(true);
        }
    }
}

/// Why `pump` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The output ended: the program is done.
    Ended,
    /// Rovik's tools mode: a message made calls to them, which Rovik runs.
    Yielded,
    /// Stop: it was ended.
    Stopped,
    /// It printed nothing for too long, and was ended.
    Stalled,
}

/// Reads the run's lines into events until it ends, makes Rovik's tool
/// calls, is stopped or stalls; the MCP calls that come meanwhile go to
/// `on_call`.
async fn pump(
    process: &mut Process,
    parser: &mut Parser,
    mut calls: Option<&mut mpsc::UnboundedReceiver<McpCall>>,
    on_call: &mut (dyn FnMut(McpCall) + Send),
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Stop {
    let mut stopping = false;
    loop {
        let timeout = if process.got_line {
            IDLE
        } else {
            process.first_line_within
        };
        let call = async {
            match calls.as_deref_mut() {
                Some(calls) => calls.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = process.lines.recv() => match line {
                Some(line) => {
                    process.got_line = true;
                    parser.line(&line, emit);
                    if parser.yielded.is_some() && !stopping {
                        return Stop::Yielded;
                    }
                }
                None => return if stopping { Stop::Stopped } else { Stop::Ended },
            },
            Some(call) = call => on_call(call),
            _ = crate::chat_request::cancelled(cancel), if !stopping => {
                stopping = true;
                process.kill(false);
            }
            _ = tokio::time::sleep(STOP_GRACE), if stopping => {
                process.kill(true);
                return Stop::Stopped;
            }
            _ = tokio::time::sleep(timeout), if !stopping => {
                process.kill(true);
                return Stop::Stalled;
            }
        }
    }
}

/// How a run ended.
pub enum Outcome {
    /// Its events said how (`Done` or `Error`).
    Done,
    /// `--resume` named a session the program doesn't have: the caller
    /// starts a new one.
    SessionGone,
}

/// A run that ended (not a yield), told: as its events said, or with the
/// error its stderr or the way it ended gives.
async fn conclude(
    process: &mut Process,
    parser: &Parser,
    stop: Stop,
    resumed: bool,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Outcome {
    let (code, stderr_text) = process.finish().await;
    if parser.finished {
        return if parser.session_gone && resumed {
            Outcome::SessionGone
        } else {
            Outcome::Done
        };
    }
    // No `result` line: the program ended, was stopped, or stalled.
    if stop == Stop::Stopped {
        emit(ChatStreamEvent::Error {
            error: ChatError::stopped(),
        });
        return Outcome::Done;
    }
    if resumed && stderr_text.contains("No conversation found") {
        return Outcome::SessionGone;
    }
    let mut error = if stop == Stop::Stalled {
        let mut e = ChatError::new(
            ErrorKind::Stalled,
            format!(
                "Claude Code printed nothing for {} s.",
                if process.got_line {
                    IDLE.as_secs()
                } else {
                    process.first_line_within.as_secs()
                }
            ),
        );
        e.retryable = false;
        e
    } else if code == Some(EXIT_TERMINATED) {
        ChatError::stopped()
    } else {
        let what = if stderr_text.is_empty() {
            match code {
                Some(code) => format!("the program ended with exit code {code} before it answered"),
                None => "the program ended before it answered".to_string(),
            }
        } else {
            stderr_text.lines().last().unwrap_or("").to_string()
        };
        harness_error(ConnectorKind::ClaudeCode, what, Some(stderr_text.clone()))
    };
    error.partial = parser.shown();
    if error.details.is_none() && !stderr_text.is_empty() {
        error = error.with_details(&stderr_text);
    }
    emit(ChatStreamEvent::Error { error });
    Outcome::Done
}

/// The folder a run works in: the turn's, or the system's temporary one.
fn cwd_of(turn: &Turn) -> PathBuf {
    turn.cwd
        .clone()
        .filter(|c| c.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// The system prompt's file, when there's a prompt.
fn prompt_file(
    settings: &Settings,
    turn: &Turn,
    rovik_tools: bool,
) -> Result<Option<TempFile>, String> {
    let text = system_text(settings, turn, rovik_tools);
    if text.trim().is_empty() {
        return Ok(None);
    }
    TempFile::write("prompt", "md", &text).map(Some)
}

/// Runs one `claude` turn with its own tools, reporting it through `emit`.
async fn run_turn(
    turn: &Turn,
    session: Session,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<Outcome, String> {
    let settings = Settings::from_value(&turn.settings);
    let program = resolve_program(&settings.program)?;
    let prompt = prompt_file(&settings, turn, false)?;
    let args = args_for(
        &settings,
        turn,
        &session,
        prompt.as_ref().map(TempFile::path),
        None,
    );
    let env = env_for(&settings, &turn.api_key);
    let mut process = Process::spawn(
        &program,
        &args,
        &env,
        Some(&cwd_of(turn)),
        turn.prompt.clone(),
        turn.first_byte_timeout.max(MIN_FIRST_LINE),
        prompt.into_iter().collect(),
    )?;
    let mut parser = Parser::default();
    let stop = pump(&mut process, &mut parser, None, &mut |_| {}, cancel, emit).await;
    let resumed = matches!(session, Session::Resume(_));
    Ok(conclude(&mut process, &parser, stop, resumed, emit).await)
}

/*
 * ---------------------------------------------------------------------------
 * Rovik's tools mode
 * ---------------------------------------------------------------------------
 *
 * Claude Code is the model behind Rovik's agent loop. A turn of Rovik's is
 * a message of Claude Code's: the run starts (or resumes its session) with
 * Rovik's tools offered over MCP (connectors/mcp.rs), and when a message
 * calls any of them, Rovik's turn ends with those calls as the reply's
 * (`Stop::Yielded`) while Claude Code waits, its calls held open on the
 * MCP server. Rovik runs them, under its permissions and approvals, and
 * stores the results; its next turn finds the run waiting (`LIVE`), gives
 * each call its stored result, and reads on to the next message. A message
 * without calls ends Claude Code's run, and the turn with it.
 *
 * However Rovik's run ends (done, stopped, paused, failed), the waiting
 * run goes with it (`end_live`, from the run's `Drop`). One started again
 * resumes the session, told the results it never got.
 */

/// The runs waiting for Rovik's results, by chat.
static LIVE: LazyLock<Mutex<HashMap<String, Live>>> = LazyLock::new(Default::default);

fn live_runs() -> std::sync::MutexGuard<'static, HashMap<String, Live>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ends the chat's waiting run, if any: Rovik's run is over.
pub fn end_live(dialog_id: &str) {
    let ended = live_runs().remove(dialog_id);
    drop(ended);
}

/// Ends every waiting run: the process is exiting.
pub fn end_all() {
    let all: Vec<Live> = live_runs().drain().map(|(_, live)| live).collect();
    drop(all);
}

/// A run of Claude Code's waiting between Rovik's turns, or being read.
struct Live {
    process: Process,
    parser: Parser,
    calls: mpsc::UnboundedReceiver<McpCall>,
    matcher: Matcher,
    _server: McpServer,
    resumed: bool,
}

/// Matches Claude Code's MCP calls to the tool calls Rovik ran, and
/// answers each with its result once both are there.
#[derive(Default)]
struct Matcher {
    /// The calls handed to Rovik at the last yield: id, name, arguments.
    pending: Vec<(String, String, Value)>,
    /// Calls made, waiting for their result, by tool use id.
    waiting: HashMap<String, oneshot::Sender<McpReply>>,
    /// Results given, waiting for their call, by tool use id.
    answers: HashMap<String, McpReply>,
    /// Ids already matched to a call that named none.
    matched: HashSet<String>,
}

impl Matcher {
    /// A call Claude Code made: answered if its result is there, else kept.
    fn take(&mut self, call: McpCall) {
        let id = call.tool_use_id.clone().or_else(|| {
            // A version that doesn't say the id: the first pending call of
            // that name and those arguments.
            let found = self
                .pending
                .iter()
                .find(|(id, name, arguments)| {
                    *name == call.name && *arguments == call.arguments && !self.matched.contains(id)
                })
                .map(|(id, ..)| id.clone());
            if let Some(id) = &found {
                self.matched.insert(id.clone());
            }
            found
        });
        let Some(id) = id else {
            let _ = call
                .reply
                .send(McpReply::error("Rovik has no call like this to answer."));
            return;
        };
        match self.answers.remove(&id) {
            Some(answer) => {
                let _ = call.reply.send(answer);
            }
            None => {
                self.waiting.insert(id, call.reply);
            }
        }
    }

    /// Rovik's results for the pending calls, answering those made.
    fn deliver(&mut self, answers: &Answers) {
        let count = self.pending.len();
        for (n, (id, ..)) in self.pending.iter().enumerate() {
            let mut text = answers
                .results
                .get(id)
                .cloned()
                .unwrap_or_else(|| "[Rovik didn't run this call.]".into());
            // What Rovik said after the results (a check's failure, a
            // message the user sent) goes with the last.
            if n + 1 == count && !answers.notes.trim().is_empty() {
                text = format!("{text}\n\n---\n{}", answers.notes.trim());
            }
            let answer = McpReply::text(text);
            match self.waiting.remove(id) {
                Some(reply) => {
                    let _ = reply.send(answer);
                }
                None => {
                    self.answers.insert(id.clone(), answer);
                }
            }
        }
    }
}

impl Live {
    /// Whether Rovik's turn goes on with this run: the chat's last message
    /// is the one with the calls this run is waiting on.
    fn takes(&self, answers: Option<&Answers>) -> bool {
        let Some(answers) = answers else {
            return false;
        };
        !self.matcher.pending.is_empty()
            && self
                .matcher
                .pending
                .iter()
                .all(|(id, ..)| answers.calls.iter().any(|(call, _)| call == id))
    }
}

/// Starts a run with Rovik's tools, for `prompt`.
async fn start_live(
    turn: &Turn,
    settings: &Settings,
    session: &Session,
    prompt: String,
) -> Result<Live, String> {
    let program = resolve_program(&settings.program)?;
    let (server, calls) = McpServer::start(&turn.tools, MCP_INSTRUCTIONS)
        .await
        .map_err(|e| format!("Rovik's tools couldn't be served to Claude Code: {e}"))?;
    let config = TempFile::write("mcp", "json", &server.config_json())?;
    let system = prompt_file(settings, turn, true)?;
    let args = args_for(
        settings,
        turn,
        session,
        system.as_ref().map(TempFile::path),
        Some(config.path()),
    );
    let mut env = env_for(settings, &turn.api_key);
    env.extend(rovik_tools_env());
    let mut files = vec![config];
    files.extend(system);
    let process = Process::spawn(
        &program,
        &args,
        &env,
        Some(&cwd_of(turn)),
        prompt,
        turn.first_byte_timeout.max(MIN_FIRST_LINE),
        files,
    )?;
    Ok(Live {
        process,
        parser: Parser::with_rovik_tools(),
        calls,
        matcher: Matcher::default(),
        _server: server,
        resumed: matches!(session, Session::Resume(_)),
    })
}

/// One of Rovik's turns, with Rovik's tools: the chat's waiting run goes
/// on with the results, or a run starts.
async fn run_with_rovik_tools(
    turn: Turn,
    settings: Settings,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<(), String> {
    let Some(dialog) = turn.dialog_id.clone() else {
        return Err("Rovik's tools are for a chat's run.".into());
    };
    let waiting = live_runs().remove(&dialog);
    let mut live = match waiting {
        Some(mut live) if !turn.continue_reply && live.takes(turn.answers.as_ref()) => {
            if let Some(answers) = &turn.answers {
                live.matcher.deliver(answers);
            }
            live.parser.begin_turn();
            live.parser.announce(emit);
            live
        }
        other => {
            drop(other);
            let session = match &turn.session {
                Some(id) => Session::Resume(id.clone()),
                None => Session::New(uuid::Uuid::new_v4().to_string()),
            };
            start_live(&turn, &settings, &session, turn.prompt.clone()).await?
        }
    };
    loop {
        let Live {
            process,
            parser,
            calls,
            matcher,
            ..
        } = &mut live;
        let stop = pump(
            process,
            parser,
            Some(calls),
            &mut |call| matcher.take(call),
            cancel,
            emit,
        )
        .await;
        if stop == Stop::Yielded {
            let made = live.parser.yielded.take().unwrap_or_default();
            live.matcher.pending = made
                .into_iter()
                .map(|(id, name, arguments)| {
                    let arguments = serde_json::from_str(&arguments).unwrap_or(Value::Null);
                    (id, name, arguments)
                })
                .collect();
            live.matcher.matched.clear();
            live_runs().insert(dialog, live);
            return Ok(());
        }
        let resumed = live.resumed;
        let outcome = conclude(&mut live.process, &live.parser, stop, resumed, emit).await;
        drop(live);
        match outcome {
            Outcome::Done => return Ok(()),
            // The session is gone: a new one, told what the chat was about.
            Outcome::SessionGone => {
                let prompt = turn.recap.clone().unwrap_or_else(|| turn.prompt.clone());
                let session = Session::New(uuid::Uuid::new_v4().to_string());
                live = start_live(&turn, &settings, &session, prompt).await?;
            }
        }
    }
}

/// How long `claude auth status` may take: it reads files, and asks nothing.
const AUTH_STATUS_WITHIN: Duration = Duration::from_secs(20);

/// What `claude auth status --json` said (`{"loggedIn", "authMethod",
/// "configDirectory", …}`, exit 0 when signed in, 1 when not), as the
/// connector's settings would see it. `Err` when it said nothing usable:
/// a version without the command, say, which isn't "signed out".
pub fn parse_auth_status(
    stdout: &str,
    stderr: &str,
    success: bool,
    settings: &Settings,
    api_key: &str,
) -> Result<ConnectorAuth, String> {
    let json = stdout
        .find('{')
        .and_then(|start| serde_json::from_str::<Value>(stdout[start..].trim()).ok());
    let Some(json) = json else {
        let said = [stderr, stdout]
            .iter()
            .flat_map(|t| t.lines())
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        let lower = said.to_lowercase();
        if lower.contains("unknown command")
            || lower.contains("unknown option")
            || lower.contains("too many arguments")
        {
            return Err(
                "This Claude Code can't report its sign-in (`claude auth status` is from v2.1.268): update it."
                    .into(),
            );
        }
        return Err(if said.is_empty() {
            "`claude auth status` said nothing.".into()
        } else {
            format!("`claude auth status`: {said}")
        });
    };
    let method = json
        .get("authMethod")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    let mut auth = ConnectorAuth {
        logged_in: json
            .get("loggedIn")
            .and_then(|l| l.as_bool())
            .unwrap_or(success)
            && method.as_deref() != Some("none"),
        method,
        config_dir: json
            .get("configDirectory")
            .and_then(|d| d.as_str())
            .map(str::to_string),
        detail: None,
    };
    let via_key = auth.method.as_deref() == Some("api_key")
        || json.get("apiKeySource").is_some_and(|s| !s.is_null());
    if settings.bare && !via_key {
        // Bare mode reads no login and no token: a run would find none.
        auth.logged_in = false;
        auth.detail = Some("Bare mode reads no login or token: give it an API key.".into());
    } else if !auth.logged_in {
        auth.detail = Some(match settings.auth {
            Auth::Login => "Not signed in: run `claude` once on this computer and log in, or use a token from `claude setup-token`.".into(),
            Auth::Token => "Not signed in: paste a token from `claude setup-token` in the connector's settings.".into(),
            Auth::ApiKey => "Not signed in: set an Anthropic API key in the connector's settings.".into(),
        });
    } else if settings.auth != Auth::Login && api_key.trim().is_empty() {
        auth.detail = Some(format!(
            "No {} is set, so runs use this computer's login.",
            if settings.auth == Auth::Token {
                "token"
            } else {
                "API key"
            }
        ));
    } else if settings.auth != Auth::Login {
        auth.detail =
            Some("The token or key is taken as good until a request says otherwise.".into());
    }
    Ok(auth)
}

/// `claude auth status --json`, with the turn's environment: the sign-in
/// a chat's run would have.
async fn check_auth(
    program: &str,
    settings: &Settings,
    env: &[(String, String)],
    api_key: &str,
) -> Result<ConnectorAuth, String> {
    let mut cmd = tokio::process::Command::from(command(program, None, env));
    cmd.args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(AUTH_STATUS_WITHIN, cmd.output())
        .await
        .map_err(|_| {
            format!(
                "`{program} auth status` didn't answer in {} s.",
                AUTH_STATUS_WITHIN.as_secs()
            )
        })?
        .map_err(|e| format!("Couldn't run {program}: {e}"))?;
    parse_auth_status(
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
        settings,
        api_key,
    )
}

impl Connector for ClaudeCode {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::ClaudeCode
    }

    fn probe<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
    ) -> BoxFuture<'a, Result<ConnectorProbe, String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&args.settings);
            let program = resolve_program(&settings.program)?;
            let env = env_for(&settings, &args.api_key);
            let mut version_cmd = tokio::process::Command::from(command(&program, None, &env));
            version_cmd.arg("--version").stdin(Stdio::null());
            let out = tokio::time::timeout(Duration::from_secs(30), version_cmd.output())
                .await
                .map_err(|_| format!("{program} --version didn't answer in 30 s."))?
                .map_err(|e| format!("Couldn't run {program}: {e}"))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(format!(
                    "{program} --version failed: {}",
                    err.lines().last().unwrap_or("no output")
                ));
            }
            let version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let mut probe = ConnectorProbe {
                program: program.clone(),
                version,
                models: models(),
                request: None,
                notes: Vec::new(),
                auth: check_auth(&program, &settings, &env, &args.api_key)
                    .await
                    .ok(),
            };
            match settings.auth {
                Auth::Login => probe.notes.push(
                    "Signs in as this computer's user: run `claude` once and log in, or choose a token or key."
                        .into(),
                ),
                Auth::Token if args.api_key.trim().is_empty() => {
                    probe.notes.push("No token yet: paste one from `claude setup-token`.".into())
                }
                Auth::ApiKey if args.api_key.trim().is_empty() => {
                    probe.notes.push("No API key yet.".into())
                }
                _ => {}
            }
            if settings.bare && settings.auth != Auth::ApiKey {
                probe.notes.push(
                    "Bare mode reads no login or token: an API key is needed with it.".into(),
                );
            }
            if args.request {
                probe.request = Some(test_request(&program, &settings, &env).await?);
            }
            Ok(probe)
        })
    }

    fn auth_status<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
    ) -> BoxFuture<'a, Result<ConnectorAuth, String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&args.settings);
            let program = resolve_program(&settings.program)?;
            let env = env_for(&settings, &args.api_key);
            check_auth(&program, &settings, &env, &args.api_key).await
        })
    }

    /// The native installer; `claude auth login` and `logout` with a run's
    /// settings folder, a Console login for an API-key connector. The
    /// login opens the browser where it runs, and prints the page to open
    /// elsewhere (a cloud host's), whose code it reads from its input.
    fn setup_command(
        &self,
        action: SetupAction,
        args: &ProbeRequestArgs,
    ) -> Result<Option<SetupCommand>, String> {
        let settings = Settings::from_value(&args.settings);
        let auth = |verb: &str| -> Result<SetupCommand, String> {
            let program = resolve_program(&settings.program)?;
            Ok(SetupCommand::program(program, &["auth", verb])
                .with_env(env_for(&settings, &args.api_key)))
        };
        Ok(Some(match action {
            SetupAction::Install => SetupCommand::shell(INSTALL),
            SetupAction::Login if settings.auth == Auth::ApiKey => auth("login")?.arg("--console"),
            SetupAction::Login => auth("login")?,
            SetupAction::Logout => auth("logout")?,
        }))
    }

    fn run<'a>(
        &'a self,
        turn: Turn,
        cancel: &'a mut watch::Receiver<bool>,
        on_event: &'a mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&turn.settings);
            if settings.tools == ToolsMode::Rovik && !turn.one_shot {
                return run_with_rovik_tools(turn, settings, cancel, on_event).await;
            }
            let session = match &turn.session {
                Some(id) => Session::Resume(id.clone()),
                None => Session::New(uuid::Uuid::new_v4().to_string()),
            };
            match run_turn(&turn, session, cancel, on_event).await? {
                Outcome::Done => Ok(()),
                Outcome::SessionGone => {
                    // The program lost the conversation: a new one, told
                    // what the chat was about (connectors/mod.rs recap).
                    let fresh = Turn {
                        session: None,
                        prompt: match &turn.recap {
                            Some(recap) => recap.clone(),
                            None => turn.prompt.clone(),
                        },
                        ..turn
                    };
                    let session = Session::New(uuid::Uuid::new_v4().to_string());
                    run_turn(&fresh, session, cancel, on_event)
                        .await
                        .map(|_| ())
                }
            }
        })
    }
}

/// A small request, to see that the program is signed in and answers:
/// what it cost, and the model that answered.
async fn test_request(
    program: &str,
    settings: &Settings,
    env: &[(String, String)],
) -> Result<ProbeRequest, String> {
    let mut cmd = tokio::process::Command::from(command(program, None, env));
    cmd.args([
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--max-turns",
        "1",
        "--no-session-persistence",
        "--permission-prompts",
        "none",
    ]);
    if settings.bare {
        cmd.arg("--bare");
    }
    cmd.args(split_args(&settings.extra_args));
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't run {program}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"Reply with the single word: pong").await;
        let _ = stdin.shutdown().await;
    }
    let out = tokio::time::timeout(Duration::from_secs(180), child.wait_with_output())
        .await
        .map_err(|_| "The test request didn't answer in 3 minutes.".to_string())?
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value: Value = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .ok_or_else(|| {
            let why = stderr.lines().last().unwrap_or("").trim().to_string();
            if why.is_empty() {
                format!(
                    "The test request gave no answer (exit code {:?}).",
                    out.status.code()
                )
            } else {
                why
            }
        })?;
    if value
        .get("is_error")
        .and_then(|e| e.as_bool())
        .unwrap_or(false)
    {
        let message = value
            .get("errors")
            .and_then(|e| e.as_array())
            .and_then(|e| e.first())
            .and_then(|e| e.as_str())
            .or_else(|| value.get("result").and_then(|r| r.as_str()))
            .unwrap_or("the request failed");
        return Err(format!("Claude Code answered with an error: {message}"));
    }
    let usage = value.get("usage").cloned().unwrap_or(Value::Null);
    let count = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    Ok(ProbeRequest {
        model: value
            .get("modelUsage")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next().cloned())
            .unwrap_or_default(),
        input_tokens: count("input_tokens")
            + count("cache_creation_input_tokens")
            + count("cache_read_input_tokens"),
        output_tokens: count("output_tokens"),
        cost_usd: value.get("total_cost_usd").and_then(|c| c.as_f64()),
        ms: value
            .get("duration_ms")
            .and_then(|d| d.as_u64())
            .unwrap_or(started.elapsed().as_millis() as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Connector;

    fn turn(prompt: &str) -> Turn {
        Turn {
            settings: json!({}),
            api_key: String::new(),
            model: "sonnet".into(),
            system_prompt: "Be brief.".into(),
            prompt: prompt.into(),
            cwd: None,
            session: None,
            recap: None,
            one_shot: false,
            effort: None,
            first_byte_timeout: Duration::from_secs(60),
            system_context: None,
            dialog_id: None,
            tools: Vec::new(),
            answers: None,
            continue_reply: false,
        }
    }

    fn names(events: &[ChatStreamEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                ChatStreamEvent::Delta { content } => format!("delta:{content}"),
                ChatStreamEvent::Reasoning { content } => format!("reasoning:{content}"),
                ChatStreamEvent::HarnessSession { session, model, .. } => {
                    format!("session:{session}:{}", model.clone().unwrap_or_default())
                }
                ChatStreamEvent::HarnessStep { step } => format!(
                    "step:{}:{}:{}",
                    step.name,
                    step.result.clone().unwrap_or_default(),
                    step.ok.map(|ok| ok.to_string()).unwrap_or_default()
                ),
                ChatStreamEvent::Finished {
                    finish_reason,
                    usage,
                } => format!(
                    "finished:{}:{}",
                    finish_reason.clone().unwrap_or_default(),
                    usage
                        .as_ref()
                        .map(|u| u["prompt_tokens"].to_string())
                        .unwrap_or_default()
                ),
                ChatStreamEvent::Retrying { attempt, .. } => format!("retrying:{attempt}"),
                ChatStreamEvent::Done => "done".into(),
                ChatStreamEvent::Error { error } => {
                    format!("error:{:?}:{}", error.kind, error.message)
                }
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_arguments_of_a_turn() {
        let settings = Settings::from_value(&json!({
            "permissionMode": "acceptEdits",
            "allowedTools": "Bash(npm test *)\nRead",
            "maxTurns": 30,
            "maxBudgetUsd": 2.5,
            "extraArgs": "--add-dir \"../lib dir\" --debug",
        }));
        let mut t = turn("Fix it");
        t.effort = Some(ReasoningEffort::High);
        let file = Path::new("/tmp/prompt.md");
        let args = args_for(&settings, &t, &Session::New("s-1".into()), Some(file), None);
        let joined = args.join(" ");
        assert!(joined.starts_with("-p --output-format stream-json --verbose --include-partial-messages --permission-prompts none"), "{joined}");
        assert!(
            joined.contains("--model sonnet --effort high --append-system-prompt-file /tmp/prompt.md --system-prompt-snapshot off"),
            "{joined}"
        );
        assert!(joined.contains("--permission-mode acceptEdits --allowedTools Bash(npm test *) Read --max-turns 30 --max-budget-usd 2.5 --session-id s-1"), "{joined}");
        assert_eq!(
            &args[args.len() - 3..],
            ["--add-dir", "../lib dir", "--debug"]
        );
        // Continuing the chat's session; the prompt replaced; no model override.
        let settings =
            Settings::from_value(&json!({ "systemPrompt": "replace", "permissionMode": "" }));
        let mut t = turn("Go on");
        t.model = "default".into();
        t.session = Some("s-0".into());
        let joined = args_for(
            &settings,
            &t,
            &Session::Resume("s-0".into()),
            Some(file),
            None,
        )
        .join(" ");
        assert!(!joined.contains("--model"), "{joined}");
        assert!(
            joined.contains("--system-prompt-file /tmp/prompt.md"),
            "{joined}"
        );
        // No prompt: no prompt flags.
        let none = args_for(&settings, &t, &Session::Resume("s-0".into()), None, None).join(" ");
        assert!(!none.contains("system-prompt"), "{none}");
        assert!(!joined.contains("--permission-mode"), "{joined}");
        assert!(joined.ends_with("--resume s-0"), "{joined}");
    }

    #[test]
    fn a_one_shot_request_has_no_tools_and_keeps_nothing() {
        let mut t = turn("Name this chat");
        t.one_shot = true;
        let file = Path::new("/tmp/p.md");
        let args = args_for(
            &Settings::default(),
            &t,
            &Session::New("s".into()),
            Some(file),
            None,
        );
        let joined = args.join(" ");
        // Rovik's prompt alone, whatever the settings say for a chat's runs.
        assert!(
            joined.contains("--system-prompt-file /tmp/p.md"),
            "{joined}"
        );
        assert!(
            joined.contains("--tools  --max-turns 1 --no-session-persistence"),
            "{joined}"
        );
        assert!(
            !joined.contains("--session-id") && !joined.contains("--permission-mode"),
            "{joined}"
        );
    }

    #[test]
    fn set_up_installs_natively_and_signs_in_with_the_runs_settings() {
        let args = |settings: Value| ProbeRequestArgs {
            kind: ConnectorKind::ClaudeCode,
            settings,
            api_key: String::new(),
            request: false,
        };
        let step = |action, settings| ClaudeCode.setup_command(action, &args(settings));

        let install = step(SetupAction::Install, json!({})).unwrap().unwrap();
        assert_eq!(install.shown, INSTALL);
        assert!(INSTALL.contains("https://claude.ai/install."), "{INSTALL}");

        // Signing in needs the program; a path that isn't one says so.
        let missing = json!({ "program": "/nowhere/claude" });
        let err = step(SetupAction::Login, missing).unwrap_err();
        assert!(err.contains("no such program"), "{err}");

        let dir = crate::testing::temp_dir();
        let program = dir.0.join("claude");
        std::fs::write(&program, "").unwrap();
        let settings = json!({
            "program": program.to_string_lossy(),
            "auth": "apiKey",
            "configDir": "/tmp/claude-work",
        });
        let login = step(SetupAction::Login, settings.clone()).unwrap().unwrap();
        assert_eq!(login.args, ["auth", "login", "--console"]);
        assert!(login
            .env
            .contains(&("CLAUDE_CONFIG_DIR".into(), "/tmp/claude-work".into())));
        let logout = step(SetupAction::Logout, settings).unwrap().unwrap();
        assert_eq!(logout.args, ["auth", "logout"]);
        let plain = json!({ "program": program.to_string_lossy() });
        let login = step(SetupAction::Login, plain).unwrap().unwrap();
        assert_eq!(login.args, ["auth", "login"]);
    }

    #[test]
    fn the_environment_carries_the_sign_in() {
        let token =
            Settings::from_value(&json!({ "auth": "token", "configDir": "~/.claude-work" }));
        let env = env_for(&token, " tok ");
        assert!(env.contains(&("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "tok".to_string())));
        assert!(env.iter().any(|(k, v)| k == "CLAUDE_CONFIG_DIR"
            && v.ends_with(".claude-work")
            && !v.starts_with('~')));
        assert!(env.contains(&("DISABLE_AUTOUPDATER".to_string(), "1".to_string())));
        let key = Settings::from_value(&json!({ "auth": "apiKey" }));
        assert!(
            env_for(&key, "sk-1").contains(&("ANTHROPIC_API_KEY".to_string(), "sk-1".to_string()))
        );
        let login = env_for(&Settings::default(), "ignored");
        assert!(login.iter().all(|(k, _)| k == "DISABLE_AUTOUPDATER"));
    }

    #[test]
    fn extra_arguments_split_as_a_shell_would() {
        assert_eq!(
            split_args(r#"--a "b c" 'd e' f\ g "" h"#),
            ["--a", "b c", "d e", "f g", "", "h"]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(
            rules("Read, Edit\nBash(git *)"),
            ["Read", "Edit", "Bash(git *)"]
        );
    }

    #[test]
    fn the_stream_becomes_text_steps_and_usage() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let lines = [
            json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5","claude_code_version":"2.1.309","tools":["Read"]}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}},"parent_tool_use_id":null,"api_message_id":"m1"}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Let me look."}},"parent_tool_use_id":null,"api_message_id":"m1"}),
            json!({"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Let me look."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/a.rs"}}]},"parent_tool_use_id":null}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"fn a() {}","is_error":false}]},"parent_tool_use_id":null}),
            // A subagent's words aren't the reply's; its steps show.
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"sub says"}},"parent_tool_use_id":"t9"}),
            json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"sub text"},{"type":"tool_use","id":"t2","name":"Grep","input":{"pattern":"x"}}]},"parent_tool_use_id":"t9"}),
            // A second message of the main conversation, not streamed: shown after a blank line.
            json!({"type":"assistant","message":{"id":"m3","content":[{"type":"text","text":"Done."}]},"parent_tool_use_id":null}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"Done.","stop_reason":"end_turn","num_turns":2,"duration_ms":1500,"duration_api_ms":1200,"total_cost_usd":0.01,"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":5},"modelUsage":{"claude-sonnet-5":{}},"permission_denials":[],"session_id":"s-1"}),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut emit);
        }
        p.line("not json", &mut emit);
        assert_eq!(
            names(&events),
            [
                "session:s-1:claude-sonnet-5",
                "reasoning:hmm",
                "delta:Let me look.",
                "step:Read::",
                "step:Read:fn a() {}:true",
                "step:Grep::",
                "delta:\n\nDone.",
                "finished:end_turn:60",
                "done",
            ]
        );
        let ChatStreamEvent::Finished {
            usage: Some(usage), ..
        } = &events[7]
        else {
            panic!()
        };
        assert_eq!(usage["cost"], 0.01);
        assert_eq!(usage["connector"]["numTurns"], 2);
        assert_eq!(usage["connector"]["model"], "claude-sonnet-5");
        assert_eq!(usage["connector"]["version"], "2.1.309");
        assert_eq!(usage["connector"]["steps"], 2);
        let ChatStreamEvent::HarnessStep { step } = &events[5] else {
            panic!()
        };
        assert_eq!(step.parent.as_deref(), Some("t9"));
        assert!(p.finished && p.failure.is_none());
    }

    #[test]
    fn a_result_without_streamed_text_shows_it_and_limits_are_noted() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        p.line(&json!({"type":"result","subtype":"error_max_turns","is_error":true,"result":"Half done","usage":{},"permission_denials":[{"tool_name":"Bash"}]}).to_string(), &mut emit);
        let shown = names(&events);
        assert_eq!(shown[0], "delta:Half done");
        assert!(shown[1].contains("turn limit"), "{shown:?}");
        assert!(shown[2].contains("1 tool call was denied"), "{shown:?}");
        assert_eq!(shown[3], "finished:error_max_turns:0");
        assert_eq!(shown[4], "done");
    }

    #[test]
    fn a_failed_result_is_the_harnesss_error() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        p.line(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Part"}},"parent_tool_use_id":null}).to_string(), &mut emit);
        p.line(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Not logged in · Please run /login"],"usage":{}}).to_string(), &mut emit);
        let shown = names(&events);
        assert_eq!(shown[0], "delta:Part");
        assert!(
            shown[2].starts_with("error:Auth:Claude Code: Not logged in"),
            "{shown:?}"
        );
        let ChatStreamEvent::Error { error } = &events[2] else {
            panic!()
        };
        assert!(error.partial, "what streamed is kept");
        assert!(p.failure.is_some());
        // A resumed session the program doesn't have.
        let mut p = Parser::default();
        p.line(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: abc"]}).to_string(), &mut |_| {});
        assert!(p.session_gone);
    }

    #[test]
    fn api_retries_show_as_retrying() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        p.line(&json!({"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"retry_delay_ms":500,"error":"overloaded"}).to_string(), &mut |e| events.push(e));
        assert_eq!(names(&events), ["retrying:2"]);
    }

    #[test]
    fn auth_status_reads_the_programs_answer() {
        let login = Settings::default();
        let ok = parse_auth_status(
            r#"{"loggedIn": true, "authMethod": "claude.ai", "configDirectory": "/home/ann/.claude"}"#,
            "",
            true,
            &login,
            "",
        )
        .unwrap();
        assert!(ok.logged_in);
        assert_eq!(ok.method.as_deref(), Some("claude.ai"));
        assert_eq!(ok.config_dir.as_deref(), Some("/home/ann/.claude"));
        assert_eq!(ok.detail, None);
        let out = parse_auth_status(
            r#"{"loggedIn": false, "authMethod": "none"}"#,
            "",
            false,
            &login,
            "",
        )
        .unwrap();
        assert!(!out.logged_in);
        assert!(out.detail.unwrap().contains("run `claude` once"));
        // `none` is signed out whatever `loggedIn` says.
        assert!(
            !parse_auth_status(
                r#"{"loggedIn": true, "authMethod": "none"}"#,
                "",
                true,
                &login,
                ""
            )
            .unwrap()
            .logged_in
        );
        // Without `loggedIn`, the exit status says.
        assert!(
            parse_auth_status("{}", "", true, &login, "")
                .unwrap()
                .logged_in
        );
        assert!(
            !parse_auth_status("{}", "", false, &login, "")
                .unwrap()
                .logged_in
        );
    }

    #[test]
    fn auth_status_follows_the_connectors_settings() {
        let token = Settings::from_value(&json!({ "auth": "token" }));
        let signed = r#"{"loggedIn": true, "authMethod": "oauth_token"}"#;
        let with = parse_auth_status(signed, "", true, &token, "tok").unwrap();
        assert!(with.detail.unwrap().contains("taken as good"));
        let without = parse_auth_status(
            r#"{"loggedIn": true, "authMethod": "claude.ai"}"#,
            "",
            true,
            &token,
            "",
        )
        .unwrap();
        assert!(without.detail.unwrap().contains("No token is set"));
        let out = parse_auth_status(r#"{"loggedIn": false}"#, "", false, &token, "").unwrap();
        assert!(out.detail.unwrap().contains("setup-token"));
        // Bare mode reads no login: only a key counts.
        let bare = Settings::from_value(&json!({ "bare": true }));
        assert!(
            !parse_auth_status(signed, "", true, &bare, "")
                .unwrap()
                .logged_in
        );
        let keyed =
            r#"{"loggedIn": true, "authMethod": "api_key", "apiKeySource": "ANTHROPIC_API_KEY"}"#;
        assert!(
            parse_auth_status(keyed, "", true, &bare, "sk")
                .unwrap()
                .logged_in
        );
    }

    #[test]
    fn a_version_without_the_check_is_unknown_not_signed_out() {
        let err = parse_auth_status(
            "",
            "error: unknown command 'auth'",
            false,
            &Settings::default(),
            "",
        )
        .unwrap_err();
        assert!(err.contains("update it"), "{err}");
        let err = parse_auth_status("", "", false, &Settings::default(), "").unwrap_err();
        assert!(err.contains("said nothing"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_status_runs_the_program_with_the_runs_environment() {
        let dir = crate::testing::temp_dir();
        let program = fake_claude(
            &dir.0,
            r#"
echo "$@" > "$(dirname "$0")/args.txt"
if [ -n "$CLAUDE_CODE_OAUTH_TOKEN" ]; then
  echo '{"loggedIn": true, "authMethod": "oauth_token"}'
else
  echo '{"loggedIn": false, "authMethod": "none"}'; exit 1
fi
"#,
        );
        let args = |key: &str| ProbeRequestArgs {
            kind: ConnectorKind::ClaudeCode,
            settings: json!({ "program": program, "auth": "token" }),
            api_key: key.into(),
            request: false,
        };
        let signed = ClaudeCode.auth_status(&args("tok")).await.unwrap();
        assert!(signed.logged_in);
        assert_eq!(signed.method.as_deref(), Some("oauth_token"));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("args.txt"))
                .unwrap()
                .trim(),
            "auth status --json"
        );
        let out = ClaudeCode.auth_status(&args("")).await.unwrap();
        assert!(!out.logged_in);
        // Not installed: it can't say.
        let missing = ProbeRequestArgs {
            settings: json!({ "program": "/no/such/claude" }),
            ..args("")
        };
        assert!(ClaudeCode.auth_status(&missing).await.is_err());
    }

    fn read_tool() -> Value {
        json!({ "type": "function", "function": { "name": "read", "description": "Read a file.", "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } } })
    }

    #[test]
    fn the_system_prompt_names_rovik_tools_and_splits_for_the_cache() {
        let mut t = turn("x");
        t.system_prompt = "You are Rovik's agent.".into();
        t.system_context = Some("## This project\nRust.".into());
        let append = Settings::default();
        assert_eq!(
            system_text(&append, &t, false),
            "You are Rovik's agent.\n\n## This project\nRust."
        );
        let replace = Settings::from_value(&json!({ "systemPrompt": "replace" }));
        assert_eq!(
            system_text(&replace, &t, false),
            "You are Rovik's agent.\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n## This project\nRust."
        );
        // Rovik's tools named only when there are some.
        assert!(!system_text(&replace, &t, true).contains("mcp__rovik__"));
        t.tools = vec![read_tool()];
        let text = system_text(&replace, &t, true);
        assert!(text.contains("`mcp__rovik__read`"), "{text}");
        assert!(text.find("## Your tools").unwrap() < text.find(PROMPT_BOUNDARY).unwrap());
    }

    #[test]
    fn rovik_tools_mode_offers_its_server_alone() {
        let settings = Settings::from_value(&json!({
            "tools": "rovik",
            "permissionMode": "bypassPermissions",
            "allowedTools": "Bash",
            "maxTurns": 40,
        }));
        assert_eq!(settings.tools, ToolsMode::Rovik);
        let t = turn("Fix it");
        let args = args_for(
            &settings,
            &t,
            &Session::New("s".into()),
            Some(Path::new("/tmp/p.md")),
            Some(Path::new("/tmp/mcp.json")),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--tools  --mcp-config /tmp/mcp.json --strict-mcp-config --permission-mode dontAsk --allowedTools mcp__rovik --max-turns 40 --session-id s"), "{joined}");
        assert!(
            !joined.contains("bypassPermissions") && !joined.contains("Bash"),
            "the settings' own permissions aren't used: {joined}"
        );
        let env = rovik_tools_env();
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("ENABLE_TOOL_SEARCH").as_deref(), Some("false"));
        assert_eq!(
            get("CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS").as_deref(),
            Some("0")
        );
        assert!(get("NO_PROXY").unwrap().contains("127.0.0.1"));
    }

    /// Stream events of one message calling Rovik's `read` (and Claude
    /// Code's own `Skill`, which stays a step).
    fn calling_read(id: &str, message: &str) -> Vec<Value> {
        vec![
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":message,"usage":{"input_tokens":5,"cache_read_input_tokens":100}}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Reading."}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_stop","index":0},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":id,"name":"mcp__rovik__read","input":{}}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\": "}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}},"parent_tool_use_id":null}),
            json!({"type":"assistant","message":{"id":message,"content":[{"type":"tool_use","id":id,"name":"mcp__rovik__read","input":{"path":"a.rs"}}]},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_stop","index":1},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":5,"cache_read_input_tokens":100,"output_tokens":20}},"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"message_stop"},"parent_tool_use_id":null}),
        ]
    }

    #[test]
    fn rovik_tool_calls_are_the_replys_and_end_rovik_turn() {
        let mut p = Parser::with_rovik_tools();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        p.line(
            &json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5"})
                .to_string(),
            &mut emit,
        );
        for line in calling_read("toolu_1", "m1") {
            p.line(&line.to_string(), &mut emit);
        }
        let shown: Vec<String> = events
            .iter()
            .map(|e| match e {
                ChatStreamEvent::HarnessSession { rovik_tools, .. } => {
                    format!("session:{rovik_tools}")
                }
                ChatStreamEvent::Delta { content } => format!("delta:{content}"),
                ChatStreamEvent::ToolCallStart { id, name } => format!("start:{id}:{name}"),
                ChatStreamEvent::ToolCallArgumentsDelta {
                    arguments_delta, ..
                } => format!("args:{arguments_delta}"),
                ChatStreamEvent::ToolCallComplete {
                    name, arguments, ..
                } => format!("call:{name}:{arguments}"),
                ChatStreamEvent::Finished {
                    finish_reason,
                    usage,
                } => format!(
                    "finished:{}:{}:{}",
                    finish_reason.clone().unwrap_or_default(),
                    usage.as_ref().unwrap()["prompt_tokens"],
                    usage.as_ref().unwrap()["completion_tokens"]
                ),
                ChatStreamEvent::Done => "done".into(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "session:true",
                "delta:Reading.",
                "start:toolu_1:read",
                "args:{\"path\": ",
                "args:\"a.rs\"}",
                "call:read:{\"path\": \"a.rs\"}",
                "finished:tool_calls:105:20",
                "done",
            ]
        );
        assert_eq!(
            p.yielded,
            Some(vec![(
                "toolu_1".to_string(),
                "read".to_string(),
                "{\"path\": \"a.rs\"}".to_string()
            )])
        );
        // The next turn: its own text and usage; the end says the run's cost.
        p.begin_turn();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        p.line(&json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}},"parent_tool_use_id":null}).to_string(), &mut emit);
        p.line(&json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done."}},"parent_tool_use_id":null}).to_string(), &mut emit);
        p.line(&json!({"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":7,"output_tokens":3}},"parent_tool_use_id":null}).to_string(), &mut emit);
        p.line(&json!({"type":"stream_event","event":{"type":"message_stop"},"parent_tool_use_id":null}).to_string(), &mut emit);
        p.line(&json!({"type":"result","subtype":"success","is_error":false,"result":"Done.","usage":{"input_tokens":12,"output_tokens":23},"total_cost_usd":0.02,"num_turns":2}).to_string(), &mut emit);
        assert!(
            matches!(&events[0], ChatStreamEvent::Delta { content } if content == "Done."),
            "no break before a new turn's text"
        );
        let ChatStreamEvent::Finished {
            usage: Some(usage), ..
        } = &events[1]
        else {
            panic!("{events:?}")
        };
        assert_eq!(usage["prompt_tokens"], 7, "this turn's own tokens");
        assert_eq!(usage["completion_tokens"], 3);
        assert_eq!(usage["cost"], 0.02);
        assert_eq!(usage["connector"]["costCovers"], "harnessTurn");
        assert!(matches!(events[2], ChatStreamEvent::Done));
    }

    #[test]
    fn a_lost_session_reports_nothing_of_itself() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        p.line(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: abc"]}).to_string(), &mut |e| events.push(e));
        assert!(p.session_gone && p.finished);
        assert!(
            events.is_empty(),
            "the turn goes on in a new session: {events:?}"
        );
    }

    #[test]
    fn calls_are_matched_by_their_tool_use_or_their_arguments() {
        let mut m = Matcher {
            pending: vec![
                ("t1".into(), "read".into(), json!({ "path": "a" })),
                ("t2".into(), "read".into(), json!({ "path": "b" })),
            ],
            ..Matcher::default()
        };
        let call = |id: Option<&str>, path: &str| {
            let (reply, rx) = oneshot::channel();
            (
                McpCall {
                    tool_use_id: id.map(str::to_string),
                    name: "read".into(),
                    arguments: json!({ "path": path }),
                    reply,
                },
                rx,
            )
        };
        // Made before the results: waits.
        let (c1, mut r1) = call(Some("t1"), "a");
        m.take(c1);
        assert!(r1.try_recv().is_err());
        let answers = Answers {
            calls: vec![("t1".into(), "read".into()), ("t2".into(), "read".into())],
            results: HashMap::from([
                ("t1".to_string(), "A".to_string()),
                ("t2".to_string(), "B".to_string()),
            ]),
            notes: "Checks failed.".into(),
        };
        m.deliver(&answers);
        assert_eq!(r1.try_recv().unwrap(), McpReply::text("A"));
        // Made after, naming no id: matched by its arguments; the notes go with the last.
        let (c2, mut r2) = call(None, "b");
        m.take(c2);
        assert_eq!(
            r2.try_recv().unwrap(),
            McpReply::text("B\n\n---\nChecks failed.")
        );
        // A call nothing matches is an error.
        let (c3, mut r3) = call(None, "zzz");
        m.take(c3);
        assert!(r3.try_recv().unwrap().is_error);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rovik_tools_mode_yields_rovik_runs_the_call_and_the_run_goes_on() {
        let dir = crate::testing::temp_dir();
        let Some(program) = crate::connectors::fake_claude_with_mcp(&dir.0, "a.rs") else {
            eprintln!("no python3: skipped");
            return;
        };
        let dialog = format!("d-{}", uuid::Uuid::new_v4());
        let mut first = turn("Read a.rs");
        first.settings = json!({ "program": program, "tools": "rovik", "systemPrompt": "replace" });
        first.dialog_id = Some(dialog.clone());
        first.tools = vec![read_tool()];
        first.system_context = Some("## This project".into());
        let (_tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        ClaudeCode
            .run(first, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        let calls: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::ToolCallComplete {
                    id,
                    name,
                    arguments,
                    ..
                } => Some(format!("{id}:{name}:{arguments}")),
                _ => None,
            })
            .collect();
        assert_eq!(calls, ["toolu_1:read:{\"path\": \"a.rs\"}"], "{events:?}");
        assert!(
            matches!(events.last(), Some(ChatStreamEvent::Done)),
            "{events:?}"
        );
        assert!(
            live_runs().contains_key(&dialog),
            "it waits for Rovik's result"
        );
        let args = std::fs::read_to_string(dir.0.join("args.txt")).unwrap();
        assert!(
            args.contains("--strict-mcp-config") && args.contains("mcp__rovik"),
            "{args}"
        );
        let system = std::fs::read_to_string(dir.0.join("system.txt")).unwrap();
        assert!(
            system.contains("mcp__rovik__read") && system.contains(PROMPT_BOUNDARY),
            "{system}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("prompt.txt")).unwrap(),
            "Read a.rs"
        );

        // Rovik ran it: the next turn gives the result, and the run goes on.
        let mut second = turn("");
        second.settings = json!({ "program": program, "tools": "rovik" });
        second.dialog_id = Some(dialog.clone());
        second.tools = vec![read_tool()];
        second.answers = Some(Answers {
            calls: vec![("toolu_1".into(), "read".into())],
            results: HashMap::from([("toolu_1".to_string(), "fn a() {}".to_string())]),
            notes: String::new(),
        });
        let mut events = Vec::new();
        ClaudeCode
            .run(second, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Delta { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Got: fn a() {}");
        assert!(
            matches!(
                events[0],
                ChatStreamEvent::HarnessSession {
                    rovik_tools: true,
                    ..
                }
            ),
            "{events:?}"
        );
        assert!(
            matches!(events.last(), Some(ChatStreamEvent::Done)),
            "{events:?}"
        );
        assert!(!live_runs().contains_key(&dialog), "its run is over");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_waiting_run_ends_with_rovik_run() {
        let dir = crate::testing::temp_dir();
        let Some(program) = crate::connectors::fake_claude_with_mcp(&dir.0, "a.rs") else {
            return;
        };
        let dialog = format!("d-{}", uuid::Uuid::new_v4());
        let mut first = turn("Read a.rs");
        first.settings = json!({ "program": program, "tools": "rovik" });
        first.dialog_id = Some(dialog.clone());
        first.tools = vec![read_tool()];
        let (_tx, mut cancel) = watch::channel(false);
        ClaudeCode
            .run(first, &mut cancel, &mut |_| {})
            .await
            .unwrap();
        assert!(live_runs().contains_key(&dialog));
        crate::connectors::end_live(&dialog);
        assert!(!live_runs().contains_key(&dialog));
        // A turn that doesn't answer the waiting calls (another reply came
        // between) starts afresh instead of going on.
        let mut other = turn("Something else");
        other.settings = json!({ "program": program, "tools": "rovik" });
        other.dialog_id = Some(dialog.clone());
        other.tools = vec![read_tool()];
        ClaudeCode
            .run(other, &mut cancel, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.0.join("prompt.txt")).unwrap(),
            "Something else"
        );
        crate::connectors::end_live(&dialog);
    }

    /// The real `claude`, in Rovik's tools mode: it must call Rovik's `read`
    /// over MCP, then answer from the result Rovik gives. Costs a request
    /// or two on the account `claude` is signed in to.
    ///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored
    /// The real `claude auth login` as "Set up" runs it, in an empty
    /// settings folder: it prints the sign-in page and waits for a code,
    /// until stopped. Nothing is signed in.
    ///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored
    #[tokio::test]
    #[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
    async fn real_claude_sign_in_prints_its_page_and_stops() {
        if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
            return;
        }
        let dir = crate::testing::temp_dir();
        let args = ProbeRequestArgs {
            kind: ConnectorKind::ClaudeCode,
            settings: json!({ "configDir": dir.0.to_string_lossy() }),
            api_key: String::new(),
            request: false,
        };
        let poll =
            |id: &str| crate::connectors::setup(crate::connectors::SetupOp::Poll { id: id.into() });
        let start = crate::connectors::SetupOp::Start {
            args,
            action: SetupAction::Login,
        };
        let job = crate::connectors::setup(start).await.unwrap();
        assert!(job.command.ends_with("auth login"), "{}", job.command);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut job = job;
        while job.url.is_none() && !job.done && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            job = poll(&job.id).await.unwrap();
        }
        let url = job.url.clone().unwrap_or_default();
        assert!(url.contains("/oauth/authorize"), "{}", job.output);
        assert!(!job.done, "{:?}", job.error);
        let cancel = crate::connectors::SetupOp::Cancel { id: job.id.clone() };
        crate::connectors::setup(cancel).await.unwrap();
        while !job.done && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            job = poll(&job.id).await.unwrap();
        }
        assert_eq!(job.error.as_deref(), Some("Stopped."));
    }

    #[tokio::test]
    #[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
    async fn real_claude_runs_rovik_tools() {
        if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
            return;
        }
        let dir = crate::testing::temp_dir();
        let dialog = format!("real-{}", uuid::Uuid::new_v4());
        let settings = json!({ "tools": "rovik", "systemPrompt": "replace" });
        let mut first = turn(
            "Read the file notes.txt with your read tool, then reply with only the secret word it contains.",
        );
        first.model = "haiku".into();
        first.settings = settings.clone();
        first.system_prompt = "You are a coding agent working in Rovik IDE.".into();
        first.dialog_id = Some(dialog.clone());
        first.tools = vec![read_tool()];
        first.cwd = Some(dir.0.clone());
        let (_tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        ClaudeCode
            .run(first, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        let calls: Vec<(String, String, String)> = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::ToolCallComplete {
                    id,
                    name,
                    arguments,
                    ..
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "{events:#?}");
        let (id, name, arguments) = calls[0].clone();
        assert_eq!(name, "read");
        assert!(arguments.contains("notes.txt"), "{arguments}");
        assert!(
            live_runs().contains_key(&dialog),
            "waiting for Rovik's result"
        );
        // As long as an approval can take: ROVIK_REAL_CLAUDE_WAIT seconds.
        if let Some(wait) = std::env::var("ROVIK_REAL_CLAUDE_WAIT")
            .ok()
            .and_then(|w| w.parse().ok())
        {
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }

        let mut second = turn("");
        second.model = "haiku".into();
        second.settings = settings;
        second.dialog_id = Some(dialog.clone());
        second.tools = vec![read_tool()];
        second.answers = Some(Answers {
            calls: vec![(id.clone(), "read".into())],
            results: HashMap::from([(id, "1: The secret word is: pelican".to_string())]),
            notes: String::new(),
        });
        let mut events = Vec::new();
        ClaudeCode
            .run(second, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Delta { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text.to_lowercase().contains("pelican"),
            "{text:?} {events:#?}"
        );
        let usage = events.iter().find_map(|e| match e {
            ChatStreamEvent::Finished { usage: Some(u), .. } => Some(u.clone()),
            _ => None,
        });
        eprintln!("usage: {usage:?}");
        assert!(!live_runs().contains_key(&dialog));
    }

    #[test]
    fn the_model_list_has_the_aliases_and_the_names() {
        let all = models();
        let ids: Vec<&str> = all.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"sonnet") && ids.contains(&"opus") && ids.contains(&"haiku"));
        assert!(ids.contains(&"claude-opus-5-5") && ids.contains(&"opus[1m]"));
        assert!(models()
            .iter()
            .all(|m| m.description.is_some() && m.context_length.is_some()));
    }

    #[test]
    fn a_missing_program_says_how_to_install_it() {
        let err = resolve_program("rovik-no-such-harness-xyz").unwrap_err();
        assert!(err.contains("isn't installed"), "{err}");
        let err = resolve_program("/no/such/dir/claude").unwrap_err();
        assert!(err.contains("no such program"), "{err}");
    }

    /// A stand-in for `claude`: a script that prints the lines a run would,
    /// as its arguments say (unix: it's a shell script).
    #[cfg(unix)]
    fn fake_claude(dir: &Path, script: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_turn_runs_the_program_and_reports_what_it_did() {
        let dir = crate::testing::temp_dir();
        let program = fake_claude(
            &dir.0,
            r#"
echo "$@" > "$(dirname "$0")/args.txt"
cat > "$(dirname "$0")/prompt.txt"
echo '{"type":"system","subtype":"init","session_id":"s-77","model":"claude-sonnet-5","claude_code_version":"2.1.309"}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi there"}},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Hi there","usage":{"input_tokens":3,"output_tokens":2},"total_cost_usd":0.001,"num_turns":1}'
"#,
        );
        let mut t = turn("Say hi");
        t.settings = json!({ "program": program, "auth": "token" });
        t.api_key = "tok".into();
        t.cwd = Some(dir.0.clone());
        let (_tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        ClaudeCode
            .run(t, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        assert_eq!(
            names(&events),
            [
                "session:s-77:claude-sonnet-5",
                "delta:Hi there",
                "finished:success:3",
                "done"
            ]
        );
        let args = std::fs::read_to_string(dir.0.join("args.txt")).unwrap();
        assert!(args.contains("--session-id"), "a new session: {args}");
        assert!(
            args.contains("--append-system-prompt-file")
                && args.contains("--system-prompt-snapshot off"),
            "{args}"
        );
        // The prompt's file is gone with the run.
        let file = args
            .split_whitespace()
            .skip_while(|a| *a != "--append-system-prompt-file")
            .nth(1)
            .unwrap();
        assert!(!Path::new(file).exists(), "{file}");
        assert_eq!(
            std::fs::read_to_string(dir.0.join("prompt.txt")).unwrap(),
            "Say hi"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_lost_session_starts_a_new_one_with_the_recap() {
        let dir = crate::testing::temp_dir();
        let program = fake_claude(
            &dir.0,
            r#"
case "$*" in
  *--resume*) echo "No conversation found with session ID: old" >&2; exit 1;;
esac
cat > "$(dirname "$0")/prompt.txt"
echo '{"type":"system","subtype":"init","session_id":"s-new","model":"m"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{}}'
"#,
        );
        let mut t = turn("Next step");
        t.settings = json!({ "program": program });
        t.session = Some("old".into());
        t.recap = Some("Earlier: ...\n\nNext step".into());
        let (_tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        ClaudeCode
            .run(t, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        assert_eq!(
            names(&events),
            ["session:s-new:m", "delta:ok", "finished:success:0", "done"]
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("prompt.txt")).unwrap(),
            "Earlier: ...\n\nNext step"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_that_fails_before_answering_is_an_error_with_its_stderr() {
        let dir = crate::testing::temp_dir();
        let program = fake_claude(
            &dir.0,
            "echo 'Invalid API key · Fix external API key' >&2; exit 1\n",
        );
        let mut t = turn("x");
        t.settings = json!({ "program": program });
        let (_tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        ClaudeCode
            .run(t, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        let ChatStreamEvent::Error { error } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(error.kind, ErrorKind::Auth);
        assert!(
            error.message.contains("Invalid API key"),
            "{}",
            error.message
        );
        assert!(error
            .details
            .as_deref()
            .unwrap_or("")
            .contains("Invalid API key"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_ends_the_program() {
        let dir = crate::testing::temp_dir();
        let program = fake_claude(
            &dir.0,
            r#"
echo '{"type":"system","subtype":"init","session_id":"s","model":"m"}'
sleep 30
"#,
        );
        let mut t = turn("x");
        t.settings = json!({ "program": program });
        let (tx, mut cancel) = watch::channel(false);
        let mut events = Vec::new();
        let started = Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = tx.send(true);
        });
        ClaudeCode
            .run(t, &mut cancel, &mut |e| events.push(e))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(names(&events)[1], "error:Cancelled:Stopped.");
    }
}

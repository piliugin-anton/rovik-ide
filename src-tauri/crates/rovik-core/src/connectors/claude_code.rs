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

use super::{
    harness_error, trimmed_text, trimmed_value, usage_value, Connector, ConnectorAuth,
    ConnectorKind, ConnectorModel, ConnectorProbe, HarnessStep, ProbeRequest, ProbeRequestArgs,
    Turn,
};
use crate::agent::{ChatStreamEvent, ReasoningEffort};
use crate::chat_request::{ChatError, ErrorKind};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

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
    text.split(|c| c == '\n' || c == ',')
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

/// The arguments of one turn's `claude` run: the common ones, then the
/// turn's. `session` is the session to resume, or the new one's id.
pub fn args_for(settings: &Settings, turn: &Turn, session: &Session) -> Vec<String> {
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
    if !turn.system_prompt.trim().is_empty() {
        let flag = match settings.system_prompt {
            SystemPromptMode::Append => "--append-system-prompt",
            SystemPromptMode::Replace => "--system-prompt",
        };
        args.push(flag.into());
        args.push(turn.system_prompt.clone());
    }
    if turn.one_shot {
        // A chat's title, a review: an answer in words, nothing kept.
        args.push("--tools".into());
        args.push(String::new());
        args.push("--max-turns".into());
        args.push("1".into());
        args.push("--no-session-persistence".into());
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
}

impl Parser {
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
        match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "message_start" => {
                if let Some(id) = api_message {
                    self.streamed.insert(id);
                }
                if self.text_shown {
                    self.needs_break = true;
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
        let usage_out = usage_value(
            ConnectorKind::ClaudeCode,
            count("input_tokens"),
            count("cache_creation_input_tokens"),
            count("cache_read_input_tokens"),
            count("output_tokens"),
            value.get("total_cost_usd").and_then(|c| c.as_f64()),
            extra,
        );
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
            if message.contains("No conversation found") {
                self.session_gone = true;
            }
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a Stop ends what it started too.
        cmd.process_group(0);
    }
    cmd
}

/// Ends the program and everything it started.
fn kill_tree(child: &mut tokio::process::Child, force: bool) {
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|id| libc::pid_t::try_from(id).ok()) {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        unsafe { libc::kill(-pid, signal) };
        if !force {
            return;
        }
    }
    let _ = child.start_kill();
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

/// How a run ended.
pub enum Outcome {
    /// Its events said how (`Done` or `Error`).
    Done,
    /// `--resume` named a session the program doesn't have: the caller
    /// starts a new one.
    SessionGone,
}

/// Runs one `claude` turn, reporting it through `emit`.
async fn run_turn(
    turn: &Turn,
    session: Session,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<Outcome, String> {
    let settings = Settings::from_value(&turn.settings);
    let program = resolve_program(&settings.program)?;
    let args = args_for(&settings, turn, &session);
    let env = env_for(&settings, &turn.api_key);
    let cwd: Option<PathBuf> = turn
        .cwd
        .clone()
        .filter(|c| c.is_dir())
        .or_else(|| Some(std::env::temp_dir()));
    let mut cmd = tokio::process::Command::from(command(&program, cwd.as_deref(), &env));
    cmd.args(&args).kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't start {program}: {e}"))?;
    // The message on stdin, which closes once written.
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = turn.prompt.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(read_stderr(stderr));
    let Some(stdout) = child.stdout.take() else {
        return Err("Claude Code's output couldn't be read.".into());
    };
    let mut lines = BufReader::new(stdout).lines();
    let mut parser = Parser::default();
    let first_line_within = turn.first_byte_timeout.max(MIN_FIRST_LINE);
    let mut got_line = false;
    let started = Instant::now();
    let mut stopping: Option<Instant> = None;
    let mut stalled = false;
    loop {
        let timeout = if got_line { IDLE } else { first_line_within };
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        got_line = true;
                        parser.line(&line, emit);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("claude code: reading its output: {e}");
                        break;
                    }
                }
            }
            _ = crate::chat_request::cancelled(cancel), if stopping.is_none() => {
                stopping = Some(Instant::now());
                kill_tree(&mut child, false);
            }
            _ = tokio::time::sleep(STOP_GRACE), if stopping.is_some() => {
                kill_tree(&mut child, true);
                break;
            }
            _ = tokio::time::sleep(timeout), if stopping.is_none() => {
                stalled = true;
                kill_tree(&mut child, true);
                break;
            }
        }
    }
    let status = tokio::time::timeout(STOP_GRACE, child.wait()).await;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if parser.finished {
        return Ok(
            if parser.session_gone && matches!(session, Session::Resume(_)) {
                Outcome::SessionGone
            } else {
                Outcome::Done
            },
        );
    }
    // No `result` line: the program ended, was stopped, or stalled.
    if stopping.is_some() {
        emit(ChatStreamEvent::Error {
            error: ChatError::stopped(),
        });
        return Ok(Outcome::Done);
    }
    if matches!(session, Session::Resume(_)) && stderr_text.contains("No conversation found") {
        return Ok(Outcome::SessionGone);
    }
    let code = match &status {
        Ok(Ok(status)) => status.code(),
        _ => None,
    };
    let mut error = if stalled {
        let mut e = ChatError::new(
            ErrorKind::Stalled,
            format!(
                "Claude Code printed nothing for {} s.",
                if got_line {
                    IDLE.as_secs()
                } else {
                    first_line_within.as_secs()
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
    let _ = started;
    emit(ChatStreamEvent::Error { error });
    Ok(Outcome::Done)
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

    fn run<'a>(
        &'a self,
        turn: Turn,
        cancel: &'a mut watch::Receiver<bool>,
        on_event: &'a mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
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
        let args = args_for(&settings, &t, &Session::New("s-1".into()));
        let joined = args.join(" ");
        assert!(joined.starts_with("-p --output-format stream-json --verbose --include-partial-messages --permission-prompts none"), "{joined}");
        assert!(
            joined.contains("--model sonnet --effort high --append-system-prompt Be brief."),
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
        let joined = args_for(&settings, &t, &Session::Resume("s-0".into())).join(" ");
        assert!(!joined.contains("--model"), "{joined}");
        assert!(joined.contains("--system-prompt Be brief."), "{joined}");
        assert!(!joined.contains("--permission-mode"), "{joined}");
        assert!(joined.ends_with("--resume s-0"), "{joined}");
    }

    #[test]
    fn a_one_shot_request_has_no_tools_and_keeps_nothing() {
        let mut t = turn("Name this chat");
        t.one_shot = true;
        let args = args_for(&Settings::default(), &t, &Session::New("s".into()));
        let joined = args.join(" ");
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
        assert!(args.contains("--append-system-prompt Be brief."), "{args}");
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

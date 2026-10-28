//! Claude Code as a connector (connectors/mod.rs): Anthropic's coding
//! harness, run headless (`claude -p`, code.claude.com/docs/en/headless)
//! with its stream of JSON lines read as it works.
//!
//! One turn is one `claude -p` run in the project folder, told the model
//! (`--model`), the effort and the chat's message on stdin. Its system
//! prompt goes with its tools: its own with its own (in Agent mode), Rovik's
//! in its place with Rovik's (in Plan mode and Autopilot, `--system-prompt-file`;
//! connectors/mod.rs). Its conversation for the chat is
//! kept with `--session-id` and continued with `--resume`, so it keeps its
//! own context between turns, whichever tools each has. It runs unattended:
//! what it would ask about, Rovik answers (under Rovik's permissions) or its
//! permission mode decides, without a prompt in a terminal; a turn is
//! bounded by the settings' turns and budget (`--max-turns`,
//! `--max-budget-usd`). Around each command its own shell runs, Rovik's
//! hooks (`hook_settings`) have the run make ready what it needs, before
//! it runs and after it failed (agent_run/environment.rs), and a file its
//! shell runs first (`CLAUDE_ENV_FILE`) gives it a cloud host's toolchains
//! as they're installed.
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

use super::harness::{
    self, expand_home, pump, split_args, Lines, Process, Stop, TempFile, MIN_FIRST_LINE,
};
use super::live::{LiveRuns, Matcher};
use super::mcp::{McpCall, McpReply, McpServer, HOOK, SERVER_NAME, TOOL_PREFIX};
use super::steps::{relative_paths, thought_id, thousands, Thought};
use super::{
    harness_error, hook_answer, trimmed_text, trimmed_value, usage_value, CommandAnswer,
    CommandCheck, CommandChecks, Connector, ConnectorAuth, ConnectorCommand, ConnectorKind,
    ConnectorModel, ConnectorProbe, HarnessCall, HarnessStep, ProbeRequest, ProbeRequestArgs,
    SetupAction, SetupCommand, ToolAnswer, ToolAsk, ToolAsks, Turn, UNCHECKED,
};
use crate::agent::{ChatStreamEvent, ReasoningEffort};
use crate::chat_request::{ChatError, ErrorKind};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

/// Where a step's input is cut, per field, and its result.
const STEP_INPUT_CHARS: usize = 400;
const STEP_RESULT_CHARS: usize = 1_200;
/// What of its model's thinking Claude Code asks the API to return: a
/// readable summary (`--thinking-display`), where current models return
/// none unless asked. The chat shows it: among the steps with Claude Code's
/// own tools, as the reply's reasoning with Rovik's. The thinking costs the
/// same either way. The flag is from v2.1.94, older than flags every run
/// already passes (`--permission-prompts`, v2.1.259).
const THINKING_DISPLAY: &str = "summarized";
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

/// The permission mode in which Rovik decides: Claude Code runs as in its
/// manual mode, and each call that would ask goes to Rovik's permissions
/// and approvals (`--permission-prompt-tool`), as a call of Rovik's tool of
/// the same kind would (`asked_call`).
pub const ROVIK_PERMISSIONS: &str = "rovik";
/// The tool on Rovik's MCP server that Claude Code asks, in that mode.
const APPROVE_TOOL: &str = "approve";

/// Where the newest version is published: Anthropic's release bucket, as
/// the native installer reads it, on its default channel, `latest`
/// (code.claude.com/docs/en/setup, "Configure release channel"). Its answer
/// is the version alone, as text.
const LATEST: &str = "https://downloads.claude.ai/claude-code-releases/latest";

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

/// The connector's settings, as the frontend keeps them with its entry
/// (src/lib/claudeCode.ts has the same fields and defaults). Whose tools a
/// turn uses isn't one: it's the run's mode's (`Turn::rovik_tools`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The program: `claude`, found on the PATH, or a path to it.
    pub program: String,
    pub auth: Auth,
    /// With its own tools: `rovik` (the default: Rovik's permissions and
    /// approvals decide what would ask, `ROVIK_PERMISSIONS`), or Claude
    /// Code's own: `acceptEdits`, `bypassPermissions`, `default`, `plan`,
    /// `dontAsk` or `auto`.
    pub permission_mode: String,
    /// `--allowedTools` rules, one per line or separated by commas.
    pub allowed_tools: String,
    pub disallowed_tools: String,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
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
            permission_mode: ROVIK_PERMISSIONS.into(),
            allowed_tools: String::new(),
            disallowed_tools: String::new(),
            max_turns: None,
            max_budget_usd: None,
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
    if !cfg!(windows) {
        return harness::unix_places("claude");
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("claude.exe")
    })
    .into_iter()
    .collect()
}

/// The program to run: `program` as given when it names a path, else the
/// first `claude` found on the PATH the IDE's programs get, else one of
/// the places it's installed to. An error says how to install it.
pub fn resolve_program(program: &str) -> Result<String, String> {
    let install = format!("Install Claude Code ({INSTALL}, or your package manager)");
    harness::resolve_program(program, "claude", &well_known_places(), &install)
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

/// Whether Rovik's system prompt goes, in place of Claude Code's
/// (`--system-prompt-file`): where Claude Code has no tools of its own for
/// its prompt to describe, with Rovik's tools (Claude Code is the model
/// behind Rovik's agent) and in a one-off request (a chat's title, a
/// review, which costs less without it). With its own tools, Claude Code's
/// prompt goes unchanged, and the project's instructions are what it reads
/// itself (its CLAUDE.md).
fn sends_rovik_prompt(turn: &Turn) -> bool {
    turn.one_shot || turn.rovik_tools
}

/// The system prompt a turn's run gets, in place of Claude Code's: the
/// IDE's, Rovik's tools named when it has them, then the project's section,
/// split from it for Claude Code's prompt cache (`PROMPT_BOUNDARY`).
pub fn system_text(turn: &Turn, rovik_tools: bool) -> String {
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
        Some(context) => format!("{text}\n{PROMPT_BOUNDARY}\n{context}"),
        None => text,
    }
}

/// `--disallowedTools`: the settings' rules, and in a chat's copy of the
/// project (`Turn::isolated`) the git commands that change the repository,
/// whatever the permission mode: they'd change files that then land in the
/// project as edits, or reach the remote. Claude Code matches each part of
/// a compound command; other spellings (`git -C . checkout`) it doesn't,
/// and Rovik refuses those when it's asked (agent_run `answer_tool_ask`).
/// While the user's allowed hosts are on, its web search too: Anthropic's
/// servers run it, where the list can't reach (allowed-hosts-plan.md, J10).
fn push_disallowed(args: &mut Vec<String>, settings: &Settings, turn: &Turn) {
    let mut disallowed = rules(&settings.disallowed_tools);
    if turn.isolated {
        disallowed.extend(git_deny_rules());
    }
    disallowed.extend(restricted_rules(&turn.denied));
    if turn.allowed_hosts_on {
        disallowed.push("WebSearch".into());
    }
    if !disallowed.is_empty() {
        args.push("--disallowedTools".into());
        args.extend(disallowed);
    }
}

/// Claude Code's deny rules for the project's restricted files
/// (restricted_files.rs), relative to its folder: `Read(./.env)`, which its
/// searches follow too, and `Edit(./.env)`, which every tool that changes
/// files does; a folder's for all in it (`Read(./secrets/**)`). Its shell
/// is Rovik's to check, when it asks.
fn restricted_rules(denied: &crate::restricted_files::Denied) -> Vec<String> {
    let rule = |tool: &str, path: &String| {
        // A rule's path is a gitignore pattern: its wildcards as characters.
        let (path, all) = match path.strip_suffix('/') {
            Some(folder) => (folder, "/**"),
            None => (path.as_str(), ""),
        };
        let mut escaped = String::new();
        for c in path.chars() {
            if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        format!("{tool}(./{escaped}{all})")
    };
    let reads = denied.read.iter().map(|path| rule("Read", path));
    reads
        .chain(denied.write.iter().map(|path| rule("Edit", path)))
        .collect()
}

/// Claude Code's deny rules for the git commands a chat's copy of the
/// project refuses (overlay::chats `git_changes_repo`), as far as a rule on
/// how a command starts can say them: creating a branch or a tag, and
/// changing the repository's own config, only touch the copy's `.git`,
/// which never lands, and are left to Rovik's check.
fn git_deny_rules() -> Vec<String> {
    use crate::overlay::chats::{
        GIT_BRANCH_CHANGES, GIT_CHANGES_REPO, GIT_REFLOG_CHANGES, GIT_REMOTE_CHANGES,
    };
    /// `git stash` without `list` or `show`, and git's config outside the project.
    const MORE: &[&str] = &[
        "stash push",
        "stash pop",
        "stash apply",
        "stash drop",
        "stash clear",
        "stash save",
        "stash store",
        "stash create",
        "stash branch",
        "config --global",
        "config --system",
    ];
    let words = GIT_CHANGES_REPO
        .iter()
        .map(|sub| sub.to_string())
        .chain(GIT_BRANCH_CHANGES.iter().map(|o| format!("branch {o}")))
        .chain(GIT_REMOTE_CHANGES.iter().map(|s| format!("remote {s}")))
        .chain(GIT_REFLOG_CHANGES.iter().map(|s| format!("reflog {s}")))
        .chain(MORE.iter().map(|s| s.to_string()));
    // `Bash(git stash *)` would take `git stash list` too: bare `git stash`
    // is a rule of its own.
    words
        .map(|w| format!("Bash(git {w} *)"))
        .chain(std::iter::once("Bash(git stash)".to_string()))
        .collect()
}

/// The arguments of one turn's `claude` run: the common ones, then the
/// turn's. `session` is the session to resume, or the new one's id;
/// `prompt_file` holds the system prompt (`system_text`), read from a file
/// rather than the command line, which a long prompt would overflow on
/// Windows; `mcp_config` is Rovik's tools' server, in that mode;
/// `hooks_file` is the settings with Rovik's hooks around its own shell's
/// commands (`hook_settings`), when the run makes them ready.
pub fn args_for(
    settings: &Settings,
    turn: &Turn,
    session: &Session,
    prompt_file: Option<&Path>,
    mcp_config: Option<&Path>,
    hooks_file: Option<&Path>,
) -> Vec<String> {
    // Rovik answers what would ask, through its server; otherwise nobody
    // is there to, and it's denied.
    let rovik_decides = rovik_answers(settings, turn) && mcp_config.is_some();
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--permission-prompts".into(),
        if rovik_decides { "host" } else { "none" }.into(),
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
    // Its thinking, for the chat to show; a one-off request's isn't shown.
    if !turn.one_shot {
        args.push("--thinking-display".into());
        args.push(THINKING_DISPLAY.into());
    }
    if let Some(file) = prompt_file.filter(|_| sends_rovik_prompt(turn)) {
        args.push("--system-prompt-file".into());
        args.push(file.to_string_lossy().into_owned());
    }
    // A resumed session takes the prompt as it is now, not as it was at
    // the session's first request: the project's section changes, and so
    // may whose prompt it is (the run's mode's).
    args.push("--system-prompt-snapshot".into());
    args.push("off".into());
    if turn.one_shot {
        // A chat's title, a review: an answer in words, nothing kept.
        args.push("--tools".into());
        args.push(String::new());
        args.push("--max-turns".into());
        args.push("1".into());
        args.push("--no-session-persistence".into());
    } else if rovik_answers(settings, turn) {
        // Claude Code's own tools, as in its manual mode: what it would ask
        // about, Rovik decides (none of the settings' allowed tools, which
        // would pass it by).
        args.push("--permission-mode".into());
        args.push("default".into());
        if let Some(config) = mcp_config {
            args.push("--mcp-config".into());
            args.push(config.to_string_lossy().into_owned());
            args.push("--permission-prompt-tool".into());
            args.push(format!("{TOOL_PREFIX}{APPROVE_TOOL}"));
        }
        push_disallowed(&mut args, settings, turn);
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
        push_disallowed(&mut args, settings, turn);
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
    // Before the settings' own arguments: a `--settings` of theirs wins.
    if let Some(hooks) = hooks_file {
        args.push("--settings".into());
        args.push(hooks.to_string_lossy().into_owned());
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
    env.extend(super::mcp::loopback_env());
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
    /// Its auto-compaction, as `autocompact_state` says: the window it
    /// counts against and the tokens at which it compacts.
    auto_compact: Option<(u64, u64)>,
    /// The conversation was compacted in this run (`/compact`, or on its
    /// own): its tokens before and after.
    compacted: Option<(u64, u64)>,
    /// With its own tools, the thinking being streamed: a step of its own
    /// (`HarnessStep::thinking`), in its place among the tool calls.
    thought: Option<Thought>,
    /// The thoughts of this run so far, which number their steps' ids.
    thoughts: usize,
    /// The folder the run works in: the paths its steps name are shown
    /// relative to it (`relative_paths`).
    root: Option<PathBuf>,
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
            "autocompact_state" => {
                let state = value.get("value");
                let count = |key: &str| state.and_then(|s| s.get(key)).and_then(Value::as_u64);
                if let (Some(window), Some(threshold)) =
                    (count("effective_window"), count("threshold"))
                {
                    self.auto_compact = Some((window, threshold));
                }
            }
            _ => {}
        }
    }

    /// The conversation's context after the run, as the reply's usage keeps
    /// it (`usage.connector.context`): what it holds (the last request's
    /// tokens, or what a compaction left) of the model's window, and where
    /// Claude Code compacts it. None when the run made no request and
    /// compacted nothing (`/context`): the chat shows the last it knew.
    fn context_of(&self, result: &Value) -> Option<Value> {
        let turns = result.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
        let last = result
            .pointer("/usage/iterations")
            .and_then(Value::as_array)
            .and_then(|i| i.last());
        let used = match (turns, self.compacted, last) {
            (0, Some((_, after)), _) => after,
            (_, _, Some(request)) => [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
            ]
            .iter()
            .filter_map(|key| request.get(*key).and_then(Value::as_u64))
            .sum(),
            _ => return None,
        };
        let window = result
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|models| models.values().find_map(|m| m.get("contextWindow")))
            .and_then(Value::as_u64)
            .or(self.auto_compact.map(|(window, _)| window))?;
        let mut context = json!({ "used": used, "window": window });
        if let Some((_, threshold)) = self.auto_compact {
            context["compactsAt"] = json!(threshold);
        }
        Some(context)
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
            "compact_boundary" => {
                let count = |key: &str| {
                    value
                        .pointer(&format!("/compact_metadata/{key}"))
                        .and_then(Value::as_u64)
                };
                if let (Some(before), Some(after)) = (count("pre_tokens"), count("post_tokens")) {
                    self.compacted = Some((before, after));
                }
            }
            "task_started" => self.task_started(value, emit),
            "task_notification" => self.task_ended(value, emit),
            _ => {}
        }
    }

    /// A task of Claude Code's began. A skill the user runs that works in
    /// a forked context (`/name` of one with `context: fork`) is a task no
    /// tool call stands for, and it runs before anything else of the turn
    /// is printed: its steps and its words come only once it's done. So it
    /// is a step of its own meanwhile, a `Skill` call as the model's would
    /// be. Other tasks are a tool call's (`tool_use_id`), which its step
    /// shows, or a command of a task's own (`parent_task_id`).
    fn task_started(&mut self, value: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if value.get("tool_use_id").is_some() || value.get("parent_task_id").is_some() {
            return;
        }
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("").trim();
        let id = text("task_id");
        let Some(command) = text("description").strip_prefix('/') else {
            return;
        };
        if id.is_empty() || command.is_empty() {
            return;
        }
        let (skill, args) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let mut input = json!({ "skill": skill });
        if !args.trim().is_empty() {
            input["args"] = json!(args.trim());
        }
        let step = HarnessStep {
            id: id.to_string(),
            name: "Skill".into(),
            input,
            ..HarnessStep::default()
        };
        if !self.steps.contains_key(id) {
            self.order.push(id.to_string());
        }
        self.steps.insert(id.to_string(), step.clone());
        emit(ChatStreamEvent::HarnessStep { step });
    }

    /// A task ended: the step that stands for it (`task_started`) has its
    /// outcome. What the skill found is the reply's text, which follows.
    fn task_ended(&mut self, value: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let id = value.get("task_id").and_then(Value::as_str).unwrap_or("");
        let Some(step) = self.steps.get_mut(id) else {
            return;
        };
        let status = value.get("status").and_then(Value::as_str).unwrap_or("");
        step.ok = Some(status == "completed");
        step.result = Some(
            match status {
                "completed" => "The skill completed.",
                "stopped" => "The skill was stopped.",
                _ => "The skill failed.",
            }
            .into(),
        );
        emit(ChatStreamEvent::HarnessStep { step: step.clone() });
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
            // With its own tools: a block of thinking begins a thought, and
            // any block ends the one before (blocks stream one at a time).
            "content_block_start" => {
                self.end_thought(emit);
                if event.pointer("/content_block/type").and_then(Value::as_str) == Some("thinking")
                {
                    self.thought = Some(Thought::new());
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
            "content_block_stop" => self.end_thought(emit),
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
            "message_stop" => self.end_thought(emit),
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
                            // Text after thinking: the thought is over, even
                            // where the stream didn't say so.
                            self.end_thought(emit);
                            self.text(text, emit);
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                            self.thinking(text, emit);
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

    /// Thinking streamed: with Rovik's tools, the reply's reasoning; with
    /// its own, the step of the thought it's part of, shown once there's
    /// text (thinking comes back empty where its text isn't asked for).
    fn thinking(&mut self, text: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if self.rovik_tools {
            emit(ChatStreamEvent::Reasoning {
                content: text.into(),
            });
            return;
        }
        let thought = self.thought.get_or_insert_with(Thought::new);
        let so_far = thought.step.thinking.get_or_insert_with(String::new);
        so_far.push_str(text);
        if so_far.is_empty() {
            return;
        }
        if thought.step.id.is_empty() {
            self.thoughts += 1;
            thought.step.id = thought_id(self.thoughts);
        }
        emit(ChatStreamEvent::HarnessStep {
            step: thought.step.clone(),
        });
    }

    /// The thought being streamed is over: shown with its time, when it
    /// said anything.
    fn end_thought(&mut self, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let Some(Thought { mut step, started }) = self.thought.take() else {
            return;
        };
        if !step.id.is_empty() {
            step.ms = Some(started.elapsed().as_millis() as u64);
            emit(ChatStreamEvent::HarnessStep { step });
        }
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
                // A message that wasn't streamed: its thinking whole, a
                // thought of its own before its tool calls (no time known).
                "thinking" if parent.is_none() && !streamed => {
                    if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                        if self.rovik_tools {
                            emit(ChatStreamEvent::Reasoning {
                                content: text.into(),
                            });
                        } else if !text.is_empty() {
                            self.thoughts += 1;
                            let step = HarnessStep {
                                id: thought_id(self.thoughts),
                                thinking: Some(text.into()),
                                ..HarnessStep::default()
                            };
                            emit(ChatStreamEvent::HarnessStep { step });
                        }
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
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                    if id.is_empty() {
                        continue;
                    }
                    let input = relative_paths(
                        block.get("input").unwrap_or(&Value::Null),
                        self.root.as_deref(),
                    );
                    let step = HarnessStep {
                        id: id.clone(),
                        name: name.to_string(),
                        input: trimmed_value(&input, STEP_INPUT_CHARS),
                        parent: parent.clone(),
                        ..HarnessStep::default()
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
        self.end_thought(emit);
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
            .map(Vec::len)
            .unwrap_or(0);
        let mut extra = json!({
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
        if let Some(context) = self.context_of(value) {
            extra["context"] = context;
        }
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
        // A compaction says nothing of its own (`/compact`): what it did.
        if let (false, Some((before, after))) = (self.text_shown, self.compacted) {
            self.text(
                &format!(
                    "_Compacted the conversation: {} → {} tokens._",
                    thousands(before),
                    thousands(after)
                ),
                emit,
            );
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
impl Lines for Parser {
    fn on_line(&mut self, line: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        self.line(line, emit);
    }

    fn yielded(&self) -> bool {
        self.yielded.is_some()
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
    let (code, stderr) = process.finish().await;
    if parser.finished {
        return if parser.session_gone && resumed {
            Outcome::SessionGone
        } else {
            Outcome::Done
        };
    }
    // No `result` line: the program ended, was stopped, or stalled.
    if stop != Stop::Stopped && resumed && stderr.contains("No conversation found") {
        return Outcome::SessionGone;
    }
    let (kind, partial) = (ConnectorKind::ClaudeCode, parser.shown());
    let error = harness::ended_error(kind, process, stop, code, &stderr, partial);
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
fn prompt_file(turn: &Turn) -> Result<Option<TempFile>, String> {
    if !sends_rovik_prompt(turn) {
        return Ok(None);
    }
    let text = system_text(turn, turn.rovik_tools);
    if text.trim().is_empty() {
        return Ok(None);
    }
    TempFile::write("prompt", "md", &text).map(Some)
}

/// Whether Rovik's permissions decide on Claude Code's own tools.
fn follows_rovik(settings: &Settings) -> bool {
    settings.permission_mode.trim() == ROVIK_PERMISSIONS
}

/// Whether Rovik answers what Claude Code's own tools would ask about: a
/// chat's turn with its own tools (Agent mode), under Rovik's permissions.
fn rovik_answers(settings: &Settings, turn: &Turn) -> bool {
    !turn.one_shot && !turn.rovik_tools && follows_rovik(settings)
}

/// How long listing the commands may take: the program starts, reads its
/// settings, skills and plugins, and answers, without a turn of the model.
const COMMANDS_WITHIN: Duration = Duration::from_secs(30);
/// The id of the request that asks for them.
const COMMANDS_REQUEST: &str = "rovik-commands";

/// Claude Code's own commands the chat's picker doesn't offer
/// (code.claude.com/docs/en/commands): what they change is Rovik's to say,
/// or they don't work in a run that ends with its reply. Rovik keeps:
/// - the chat's session (`clear` would start another behind it);
/// - its model and effort (the picker's, sent with each run; `model`,
///   `effort`, `fast`, `config` save theirs as Claude Code's defaults);
/// - its settings and approvals (the connector's settings; `config`,
///   `update-config`, `fewer-permission-prompts` and `doctor` write
///   Claude Code's settings or permission rules);
/// - the sign-in (the connector's Set up).
///
/// The others need a terminal, or work on after the run (`loop`, `goal`,
/// `batch`), or are Claude Code's internals. Only its built-in ones: a
/// project's or the user's own command of the same name is theirs to run.
const HIDDEN_COMMANDS: &[&str] = &[
    // The session.
    "clear",
    "resume",
    "rewind",
    "branch",
    "rename",
    "exit",
    // The model and effort.
    "model",
    "effort",
    "fast",
    "advisor",
    "output-style",
    // Settings, permissions, plugins, MCP.
    "config",
    "permissions",
    "autocompact",
    "mcp",
    "plugin",
    "reload-plugins",
    "reload-skills",
    "import",
    "hooks",
    "update-config",
    "fewer-permission-prompts",
    "doctor",
    "sandbox",
    "add-dir",
    // The account.
    "login",
    "logout",
    "upgrade",
    "design-consent",
    "design-revoke",
    // A terminal's, or longer than a run.
    "color",
    "focus",
    "theme",
    "vim",
    "statusline",
    "terminal-setup",
    "keybindings",
    "ide",
    "heapdump",
    "debug",
    "loop",
    "goal",
    "batch",
    "tasks",
    "agents",
    "list-agents",
    "remote-control",
    "install-github-app",
    "workflow-launch-exec",
];

/// Whether the chat offers `command`: not one of Claude Code's own that
/// `HIDDEN_COMMANDS` names, nor an internal one (`__remote-workflow`).
fn offered(command: &ConnectorCommand) -> bool {
    !command.builtin
        || !(HIDDEN_COMMANDS.contains(&command.name.as_str()) || command.name.starts_with("__"))
}

/// The commands of the SDK's `initialize` answer (`supportedCommands()`,
/// code.claude.com/docs/en/agent-sdk/typescript): None until `line` is
/// that answer; an error when the program refused the request. Those the
/// chat doesn't offer are left out (`offered`).
fn commands_of(line: &str) -> Option<Result<Vec<ConnectorCommand>, String>> {
    let value: Value = serde_json::from_str(line).ok()?;
    let response = value
        .get("response")
        .filter(|_| value.get("type").and_then(Value::as_str) == Some("control_response"))
        .filter(|r| r.get("request_id").and_then(Value::as_str) == Some(COMMANDS_REQUEST))?;
    if response.get("subtype").and_then(Value::as_str) == Some("error") {
        let why = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("it refused");
        return Some(Err(format!(
            "Claude Code couldn't list its commands: {why}"
        )));
    }
    let listed = response
        .pointer("/response/commands")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = |c: &Value, key: &str| {
        c.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let commands = listed
        .iter()
        .filter(|c| !text(c, "name").is_empty())
        .map(|c| ConnectorCommand {
            name: text(c, "name"),
            description: text(c, "description"),
            argument_hint: Some(text(c, "argumentHint")).filter(|h| !h.is_empty()),
            aliases: c
                .get("aliases")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            builtin: c.get("builtin").and_then(Value::as_bool).unwrap_or(false),
        })
        .filter(offered)
        .collect();
    Some(Ok(commands))
}

/// The version the release bucket answered with: one line of numbers and
/// dots (`2.1.310`); anything else (a page of an error) isn't one.
fn latest_of(text: &str) -> Option<String> {
    let version = text.trim();
    let is_version = !version.is_empty()
        && version.len() <= 32
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    is_version.then(|| version.to_string())
}

/// The tool Claude Code asks before a call, as `--permission-prompt-tool`
/// calls it: the tool, its input, and the tool use.
fn approve_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": APPROVE_TOOL,
            "description": "Decides whether one of Claude Code's tool calls may run, by Rovik IDE's permissions and approvals.",
            "parameters": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "input": { "type": "object" },
                    "tool_use_id": { "type": "string" },
                },
                "required": ["tool_name", "input"],
            },
        },
    })
}

/// A call Claude Code asks about, as the call of Rovik's tool of the same
/// kind that Rovik's permissions judge.
#[derive(Debug, Clone, PartialEq)]
struct AskedCall {
    tool: String,
    arguments: Value,
    /// An edit's file as it is: what a partly accepted edit replaces.
    current: Option<String>,
}

/// Claude Code's `tool_name` and `input` as Rovik's call: `Bash` as `bash`;
/// its file edits as `write` of the whole file they make, so the change is
/// reviewed as Rovik's own edits are (a file outside `root` is refused, as
/// Rovik's are); any other tool by its own name, under the approval of
/// Rovik's other tools.
fn asked_call(tool: &str, input: &Value, root: Option<&Path>) -> AskedCall {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("");
    let plain = |tool: &str, arguments: Value| AskedCall {
        tool: tool.into(),
        arguments,
        current: None,
    };
    let edits: Vec<(String, String, bool)> = match tool {
        "Edit" => vec![(
            text("old_string").into(),
            text("new_string").into(),
            input
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )],
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        let s =
                            |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        let all = e
                            .get("replace_all")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        (s("old_string"), s("new_string"), all)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    match tool {
        "Bash" => plain("bash", json!({ "command": text("command") })),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let file = match tool {
                "NotebookEdit" => text("notebook_path"),
                _ => text("file_path"),
            };
            let path = root
                .and_then(|root| Path::new(file).strip_prefix(root).ok())
                .map(|rel| rel.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.to_string());
            let current = std::fs::read_to_string(file).ok();
            let content = match tool {
                "Write" => Some(text("content").to_string()),
                "Edit" | "MultiEdit" => current.as_ref().and_then(|current| {
                    edits
                        .iter()
                        .try_fold(current.clone(), |content, (old, new, all)| {
                            (!old.is_empty() && content.contains(old.as_str())).then(|| {
                                if *all {
                                    content.replace(old.as_str(), new)
                                } else {
                                    content.replacen(old.as_str(), new, 1)
                                }
                            })
                        })
                }),
                _ => None,
            };
            // Without the content it would make (an edit that won't apply,
            // a notebook), it's asked about as an edit of the file.
            let arguments = match content {
                Some(content) => json!({ "path": path, "content": content }),
                None => json!({ "path": path }),
            };
            AskedCall {
                tool: "write".into(),
                arguments,
                current,
            }
        }
        _ => plain(tool, input.clone()),
    }
}

/// Rovik's answer as Claude Code's permission tool returns it: allowed,
/// with the input Rovik changed (an edit of which only some changes were
/// accepted: the whole file replaced with what was), or denied, why.
fn permission_result(tool: &str, input: &Value, asked: &AskedCall, answer: ToolAnswer) -> Value {
    let arguments = match answer {
        ToolAnswer::Deny { reason } => return json!({ "behavior": "deny", "message": reason }),
        ToolAnswer::Allow { arguments } => arguments,
    };
    let content = arguments
        .and_then(|a| serde_json::from_str::<Value>(&a).ok())
        .and_then(|a| a.get("content").and_then(Value::as_str).map(str::to_string));
    let mut input = input.clone();
    match (tool, content, &asked.current) {
        ("Write", Some(content), _) => input["content"] = json!(content),
        ("Edit", Some(content), Some(current)) => {
            input["old_string"] = json!(current);
            input["new_string"] = json!(content);
            input["replace_all"] = json!(false);
        }
        ("MultiEdit", Some(content), Some(current)) => {
            input["edits"] = json!([{ "old_string": current, "new_string": content }]);
        }
        _ => {}
    }
    json!({ "behavior": "allow", "updatedInput": input })
}

/// Answers Claude Code's permission tool: the call it asks about goes to
/// the run as Rovik's call of the same kind, and the run's answer back.
fn ask_rovik(call: McpCall, asks: &ToolAsks, root: Option<PathBuf>) {
    let tool = call
        .arguments
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = call.arguments.get("input").cloned().unwrap_or(json!({}));
    let call_id = call
        .arguments
        .get("tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(call.tool_use_id)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let asked = asked_call(&tool, &input, root.as_deref());
    let (reply, answer) = oneshot::channel();
    let sent = asks.send(ToolAsk {
        call_id,
        tool: asked.tool.clone(),
        arguments: asked.arguments.to_string(),
        reply,
    });
    tokio::spawn(async move {
        let answer = match (sent, answer.await) {
            (Ok(()), Ok(answer)) => answer,
            _ => ToolAnswer::Deny {
                reason: "Rovik couldn't decide on this call: its run has ended.".into(),
            },
        };
        let result = permission_result(&tool, &input, &asked, answer);
        let _ = call.reply.send(McpReply::text(result.to_string()));
    });
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
    let prompt = prompt_file(turn)?;
    // Rovik's permissions: the server Claude Code asks before a call.
    let asks = turn.asks.clone().filter(|_| rovik_answers(&settings, turn));
    // The commands its own shell runs, made ready around each by the run,
    // whatever its permission mode: Rovik's hooks.
    let checks = turn.commands.clone().filter(|_| !turn.one_shot);
    // In the agent's network (on a team server the user's private one,
    // where the servers it starts take their ports), held to the user's
    // allowed hosts while they're on (allowed_hosts.rs).
    let chat = turn.dialog_id.as_deref().unwrap_or_default();
    let held = crate::allowed_hosts::hold_program(chat)?;
    let network = held.network.clone();
    let mut served = None;
    if asks.is_some() || checks.is_some() {
        let tools = match asks {
            Some(_) => vec![approve_tool()],
            None => Vec::new(),
        };
        let instructions = "Rovik IDE decides whether Claude Code's tool calls may run.";
        let started = McpServer::start(&tools, instructions, network.as_ref())
            .await
            .map_err(|e| format!("Couldn't start Rovik's server for Claude Code: {e}"))?;
        served = Some(started);
    }
    let cwd = cwd_of(turn);
    let (mcp_config, hooks, env_file) = match &served {
        Some((server, _)) => (
            asks.as_ref()
                .map(|_| TempFile::write("mcp", "json", &server.config_json()))
                .transpose()?,
            checks
                .as_ref()
                .map(|_| TempFile::write("settings", "json", &hook_settings(server)))
                .transpose()?,
            // Its shell runs this before each command: what's installed
            // while it works reaches the next one (a cloud host's).
            checks
                .as_ref()
                .map(|_| crate::toolchains::env_script(&cwd))
                .filter(|script| !script.is_empty())
                .map(|script| TempFile::write("env", "sh", &script))
                .transpose()?,
        ),
        None => (None, None, None),
    };
    let args = args_for(
        &settings,
        turn,
        &session,
        prompt.as_ref().map(TempFile::path),
        mcp_config.as_ref().map(TempFile::path),
        hooks.as_ref().map(TempFile::path),
    );
    let mut env = env_for(&settings, &turn.api_key);
    let env_path = env_file.as_ref().map(|file| file.path().to_path_buf());
    if let Some(path) = &env_path {
        env.push((
            "CLAUDE_ENV_FILE".into(),
            path.to_string_lossy().into_owned(),
        ));
    }
    let files: Vec<TempFile> = prompt
        .into_iter()
        .chain(mcp_config)
        .chain(hooks)
        .chain(env_file)
        .collect();
    let mut process = Process::spawn(
        &program,
        &args,
        &env,
        Some(&cwd),
        Some((&held, chat)),
        turn.prompt.clone(),
        turn.first_byte_timeout.max(MIN_FIRST_LINE),
        files,
    )?;
    let mut parser = Parser {
        root: Some(cwd.clone()),
        ..Parser::default()
    };
    let root = turn.cwd.clone();
    let mut on_call = |call: McpCall| match (call.name == HOOK, &checks, &asks) {
        (true, Some(checks), _) => answer_hook(call, checks, env_path.clone(), cwd.clone()),
        (false, _, Some(asks)) => ask_rovik(call, asks, root.clone()),
        _ => drop(call),
    };
    let calls = served.as_mut().map(|(_, calls)| calls);
    let stop = pump(&mut process, &mut parser, calls, &mut on_call, cancel, emit).await;
    let resumed = matches!(session, Session::Resume(_));
    Ok(conclude(&mut process, &parser, stop, resumed, emit).await)
}

/// How long Claude Code waits on one of Rovik's hooks: an hour, for a Rust
/// version and the project's dependencies to install.
const HOOK_TIMEOUT_SECS: u64 = 3600;

/// Claude Code's own tools Rovik's hook sees before they run: its shell,
/// and those that read, change or search files, which a restricted file
/// is kept from (restricted_files.rs).
const CHECKED_TOOLS: &str = "Bash|Read|Edit|MultiEdit|Write|NotebookEdit|Grep";

/// Claude Code's settings with Rovik's hooks around its own tools: before
/// each (`PreToolUse`) and after a command fails (`PostToolUseFailure`),
/// the event is posted to Rovik's server for this turn with its token (the
/// settings are a private file, as the MCP configuration is), and Claude
/// Code waits for the answer. The hooks refuse only what would read or
/// change a restricted file: anything else runs as Claude Code's
/// permission mode, or Rovik's permissions, have it.
fn hook_settings(server: &McpServer) -> String {
    let hook = |matcher: &str| {
        json!([{
            "matcher": matcher,
            "hooks": [{
                "type": "http",
                "url": server.hooks_url(),
                "timeout": HOOK_TIMEOUT_SECS,
                "headers": { "Authorization": server.authorization() },
            }],
        }])
    };
    json!({
        "hooks": { "PreToolUse": hook(CHECKED_TOOLS), "PostToolUseFailure": hook("Bash") },
    })
    .to_string()
}

/// One of Claude Code's own tool calls as the Rovik call that does the
/// same with files, for the restricted files' check and the run's record
/// of what its step did (`Connector::step_calls`): its shell's command,
/// `Read` a `read`, `Write` a whole `write`, `Edit` (and the notebook's) a
/// partial one, and `Grep` that prints lines the `rg` it runs. None: one
/// that reads no file's text (`Glob`, a `Grep` for names or counts).
fn harness_call(tool: &str, input: &Value) -> Option<HarnessCall> {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let call = |name: &str, args: Value| HarnessCall::Tool {
        name: name.to_string(),
        args: args.as_object().cloned().unwrap_or_default(),
    };
    match tool {
        "Bash" => (!text("command").trim().is_empty())
            .then(|| HarnessCall::Command(text("command").to_string())),
        "Read" => Some(call("read", json!({ "path": text("file_path") }))),
        "Write" => Some(call(
            "write",
            json!({ "path": text("file_path"), "content": text("content") }),
        )),
        // Changing part of a file: a partial write, which takes its text.
        "Edit" | "MultiEdit" => Some(call(
            "write",
            json!({ "path": text("file_path"), "old_text": "" }),
        )),
        "NotebookEdit" => Some(call(
            "write",
            json!({ "path": text("notebook_path"), "old_text": "" }),
        )),
        "Grep" if text("output_mode") == "content" => {
            let mut args = vec!["rg".to_string(), "--hidden".to_string()];
            let flag = |key: &str| input.get(key).and_then(Value::as_bool) == Some(true);
            if flag("-i") {
                args.push("-i".into());
            }
            // Claude Code's multiline: `.` takes a line's end too.
            if flag("multiline") {
                args.extend(["-U".into(), "--multiline-dotall".into()]);
            }
            for (option, key) in [("--glob", "glob"), ("--type", "type")] {
                if !text(key).is_empty() {
                    args.extend([option.to_string(), text(key).to_string()]);
                }
            }
            args.extend(["-e".to_string(), text("pattern").to_string()]);
            args.push(
                Some(text("path"))
                    .filter(|p| !p.is_empty())
                    .unwrap_or(".")
                    .to_string(),
            );
            let command: Vec<String> = args.iter().map(|a| shell_quoted(a)).collect();
            Some(call("bash", json!({ "command": command.join(" ") })))
        }
        _ => None,
    }
}

/// `word` quoted for a POSIX shell, which reads it back as it is.
fn shell_quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Answers one of Rovik's hooks: the tool call it's for goes to the run
/// (`CommandCheck`), from the folder its shell is in: a command before it
/// runs or after it failed, a call of its file tools before it runs. One
/// the run refuses doesn't run, whatever Claude Code's permission mode,
/// nor does one before which the run can't be asked; what the run says of
/// a command reaches the model beside the call's result
/// (`additionalContext`). Its shell's environment file (`env_file`) is
/// written again first, so its next command finds what was installed.
/// Any other event, or an interrupted command, is answered with nothing.
fn answer_hook(call: McpCall, checks: &CommandChecks, env_file: Option<PathBuf>, cwd: PathBuf) {
    let event = &call.arguments;
    let name = event
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool = event
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("Bash");
    let input = event.get("tool_input").cloned().unwrap_or_default();
    let interrupted = event.get("is_interrupt").and_then(Value::as_bool) == Some(true);
    let failed = match name.as_str() {
        "PreToolUse" => None,
        "PostToolUseFailure" if !interrupted && tool == "Bash" => {
            let error = event.get("error").and_then(Value::as_str).unwrap_or("");
            Some(exit_and_output(error))
        }
        _ => {
            let _ = call.reply.send(McpReply::text("{}"));
            return;
        }
    };
    let Some(checked) = harness_call(tool, &input) else {
        let _ = call.reply.send(McpReply::text("{}"));
        return;
    };
    let before = failed.is_none();
    let (reply, said) = oneshot::channel();
    // A check that can't be sent is dropped here, its reply with it: the
    // answer below is then an error, not a wait for one that never comes.
    let sent = checks
        .send(CommandCheck {
            call_id: call.tool_use_id.clone().unwrap_or_default(),
            call: checked,
            cwd: event
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_string),
            failed,
            reply,
        })
        .is_ok();
    tokio::spawn(async move {
        let said = match (sent, said.await) {
            (true, Ok(said)) => said,
            // Not checked against the restricted files: it doesn't run.
            _ if before => CommandAnswer::Deny(UNCHECKED.to_string()),
            _ => CommandAnswer::Note(None),
        };
        // The file is the turn's, gone with it: never made again here.
        if let Some(file) = env_file.filter(|file| file.exists()) {
            let script = crate::toolchains::env_script(&cwd);
            if let Err(e) = crate::atomic_file::write_atomic_private(&file, script.as_bytes()) {
                ::log::warn!("claude code: its environment file {}: {e}", file.display());
            }
        }
        // Refused before it runs, whatever Claude Code's permission mode.
        let answer = hook_answer(&name, said);
        let _ = call.reply.send(McpReply::text(answer.to_string()));
    });
}

/// A failed command's exit code and output from Claude Code's `error`,
/// which says the code on its first line (`Exit code 127`) where the
/// command ran and exited.
fn exit_and_output(error: &str) -> (Option<i32>, String) {
    let (first, rest) = error.split_once('\n').unwrap_or((error, ""));
    match first
        .trim()
        .strip_prefix("Exit code ")
        .and_then(|code| code.trim().parse().ok())
    {
        Some(code) => (Some(code), rest.to_string()),
        None => (None, error.to_string()),
    }
}

/*
 * ---------------------------------------------------------------------------
 * Rovik's tools mode
 * ---------------------------------------------------------------------------
 *
 * Claude Code is the model behind Rovik's agent loop (connectors/live.rs).
 * A turn of Rovik's is a message of Claude Code's: the run starts (or
 * resumes its session) with Rovik's tools offered over MCP
 * (connectors/mcp.rs), and when a message calls any of them, Rovik's turn
 * ends with those calls as the reply's (`Stop::Yielded`) while Claude Code
 * waits. Its next turn finds the run waiting (`LIVE`), gives each call its
 * stored result, and reads on to the next message. A message without calls
 * ends Claude Code's run, and the turn with it. One started again resumes
 * the session, told the results it never got.
 */

/// The runs waiting for Rovik's results, by chat.
static LIVE: LiveRuns<Live> = LiveRuns::new();

/// A run of Claude Code's waiting between Rovik's turns, or being read.
struct Live {
    process: Process,
    parser: Parser,
    calls: mpsc::UnboundedReceiver<McpCall>,
    matcher: Matcher,
    _server: McpServer,
    resumed: bool,
}

/// Starts a run with Rovik's tools, for `prompt`.
async fn start_live(
    turn: &Turn,
    settings: &Settings,
    session: &Session,
    prompt: String,
) -> Result<Live, String> {
    let program = resolve_program(&settings.program)?;
    // As in `run_turn`: the agent's network, held to the allowed hosts.
    let chat = turn.dialog_id.as_deref().unwrap_or_default();
    let held = crate::allowed_hosts::hold_program(chat)?;
    let network = held.network.clone();
    let (server, calls) = McpServer::start(&turn.tools, MCP_INSTRUCTIONS, network.as_ref())
        .await
        .map_err(|e| format!("Rovik's tools couldn't be served to Claude Code: {e}"))?;
    let config = TempFile::write("mcp", "json", &server.config_json())?;
    let system = prompt_file(turn)?;
    let args = args_for(
        settings,
        turn,
        session,
        system.as_ref().map(TempFile::path),
        Some(config.path()),
        None,
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
        Some((&held, chat)),
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
    let waiting = LIVE.take(&dialog);
    let mut live = match waiting {
        Some(mut live) if !turn.continue_reply && live.matcher.takes(turn.answers.as_ref()) => {
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
            live.matcher.await_answers(made);
            LIVE.keep(dialog, live);
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
    let args = ["auth", "status", "--json"].map(String::from);
    let out = harness::output(program, &args, env, "", AUTH_STATUS_WITHIN).await?;
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
            let version = harness::version(&program, &env).await?;
            let mut probe = ConnectorProbe {
                program: program.clone(),
                version,
                models: models(),
                request: None,
                notes: Vec::new(),
                auth: check_auth(&program, &settings, &env, &args.api_key)
                    .await
                    .ok(),
                // The newest version is the connector module's to add.
                update: None,
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

    /// `claude` on the PATH, or where an installer puts it.
    fn installed(&self) -> bool {
        resolve_program("").is_ok()
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
            // Its own updater: the native installation's, or the package
            // manager's (which on a server needs the administrator).
            SetupAction::Update => {
                SetupCommand::program(resolve_program(&settings.program)?, &["update"])
                    .with_env(env_for(&settings, &args.api_key))
            }
        }))
    }

    /// Its `initialize` request, as the Agent SDK sends it (stream-json
    /// input): the commands it takes, with their descriptions and hints,
    /// answered without a turn of the model, so for no tokens.
    fn commands<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
        cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Vec<ConnectorCommand>, String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&args.settings);
            let program = resolve_program(&settings.program)?;
            let env = env_for(&settings, &args.api_key);
            let mut flags: Vec<String> = [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ]
            .map(String::from)
            .into();
            if settings.bare {
                flags.push("--bare".into());
            }
            flags.extend(split_args(&settings.extra_args));
            let request = json!({
                "type": "control_request",
                "request_id": COMMANDS_REQUEST,
                "request": { "subtype": "initialize" },
            });
            let input = format!("{request}\n");
            harness::answer(
                &program,
                &flags,
                &env,
                cwd,
                input,
                COMMANDS_WITHIN,
                commands_of,
            )
            .await?
        })
    }

    fn latest_version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async {
            let response = crate::net::client().await.get(LATEST).send().await.ok()?;
            latest_of(&response.error_for_status().ok()?.text().await.ok()?)
        })
    }

    fn end_live(&self, dialog_id: &str) {
        LIVE.end(dialog_id);
    }

    fn end_all(&self) {
        LIVE.end_all();
    }

    fn run<'a>(
        &'a self,
        turn: Turn,
        cancel: &'a mut watch::Receiver<bool>,
        on_event: &'a mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&turn.settings);
            if turn.rovik_tools {
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

    fn step_calls(&self, step: &HarnessStep) -> Vec<HarnessCall> {
        harness_call(&step.name, &step.input).into_iter().collect()
    }
}

/// A small request, to see that the program is signed in and answers:
/// what it cost, and the model that answered.
async fn test_request(
    program: &str,
    settings: &Settings,
    env: &[(String, String)],
) -> Result<ProbeRequest, String> {
    let mut args: Vec<String> = [
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
    ]
    .map(String::from)
    .into();
    if settings.bare {
        args.push("--bare".into());
    }
    args.extend(split_args(&settings.extra_args));
    let started = Instant::now();
    let prompt = "Reply with the single word: pong";
    let out = harness::output(program, &args, env, prompt, Duration::from_secs(180)).await?;
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
    use crate::connectors::{Answers, Connector};

    fn turn(prompt: &str) -> Turn {
        Turn {
            settings: json!({}),
            api_key: String::new(),
            model: "sonnet".into(),
            system_prompt: "Be brief.".into(),
            prompt: prompt.into(),
            cwd: None,
            isolated: false,
            rovik_tools: false,
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
            asks: None,
            commands: None,
            denied: Default::default(),
            allowed_hosts_on: false,
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
                ChatStreamEvent::HarnessStep { step } => match &step.thinking {
                    Some(text) => format!(
                        "thought:{}:{text}:{}",
                        step.id,
                        if step.ms.is_some() { "done" } else { "…" }
                    ),
                    None => format!(
                        "step:{}:{}:{}",
                        step.name,
                        step.result.clone().unwrap_or_default(),
                        step.ok.map(|ok| ok.to_string()).unwrap_or_default()
                    ),
                },
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
        let args = args_for(
            &settings,
            &t,
            &Session::New("s-1".into()),
            Some(file),
            None,
            None,
        );
        let joined = args.join(" ");
        assert!(joined.starts_with("-p --output-format stream-json --verbose --include-partial-messages --permission-prompts none"), "{joined}");
        // Its own tools: its own prompt, whatever file there is.
        assert!(
            joined.contains(
                "--model sonnet --effort high --thinking-display summarized --system-prompt-snapshot off"
            ),
            "{joined}"
        );
        assert!(!joined.contains("system-prompt-file"), "{joined}");
        // A stored choice of the prompt, from before there was none, is
        // read as nothing.
        let stored = Settings::from_value(&json!({ "systemPrompt": "append" }));
        let joined_stored = args_for(
            &stored,
            &t,
            &Session::New("s-1".into()),
            Some(file),
            None,
            None,
        )
        .join(" ");
        assert!(
            !joined_stored.contains("system-prompt-file"),
            "{joined_stored}"
        );
        assert!(joined.contains("--permission-mode acceptEdits --allowedTools Bash(npm test *) Read --max-turns 30 --max-budget-usd 2.5 --session-id s-1"), "{joined}");
        assert_eq!(
            &args[args.len() - 3..],
            ["--add-dir", "../lib dir", "--debug"]
        );
        // Continuing the chat's session; Rovik's tools, so Rovik's prompt in
        // place of Claude Code's; no model override.
        let settings = Settings::from_value(&json!({ "permissionMode": "" }));
        let mut t = turn("Go on");
        t.rovik_tools = true;
        t.model = "default".into();
        t.session = Some("s-0".into());
        let joined = args_for(
            &settings,
            &t,
            &Session::Resume("s-0".into()),
            Some(file),
            None,
            None,
        )
        .join(" ");
        assert!(!joined.contains("--model"), "{joined}");
        assert!(
            joined.contains("--system-prompt-file /tmp/prompt.md"),
            "{joined}"
        );
        // No prompt: no prompt's flag; the snapshot is still rebuilt.
        let none = args_for(
            &settings,
            &t,
            &Session::Resume("s-0".into()),
            None,
            None,
            None,
        )
        .join(" ");
        assert!(!none.contains("system-prompt-file"), "{none}");
        assert!(none.contains("--system-prompt-snapshot off"), "{none}");
        assert!(!joined.contains("--permission-mode"), "{joined}");
        assert!(joined.ends_with("--resume s-0"), "{joined}");
    }

    #[test]
    fn in_a_chats_copy_its_own_tools_are_denied_git_that_changes_the_repository() {
        let session = Session::New("s-1".into());
        let mut t = turn("Fix it");
        t.isolated = true;
        let disallowed = |args: &[String]| -> Vec<String> {
            let at = args.iter().position(|a| a == "--disallowedTools");
            at.map(|i| {
                args[i + 1..]
                    .iter()
                    .take_while(|a| !a.starts_with("--"))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
        };
        // Whatever the mode, with the settings' own rules first, in one flag.
        for mode in ["bypassPermissions", "acceptEdits", "auto", "rovik"] {
            let settings = Settings::from_value(&json!({
                "permissionMode": mode,
                "disallowedTools": "WebFetch",
            }));
            // Rovik's approvals server is there only when Rovik decides.
            let config = (mode == "rovik").then(|| Path::new("/tmp/mcp.json"));
            let args = args_for(&settings, &t, &session, None, config, None);
            let rules = disallowed(&args);
            assert_eq!(rules[0], "WebFetch", "{mode}");
            for rule in [
                "Bash(git checkout *)",
                "Bash(git push *)",
                "Bash(git branch -D *)",
                "Bash(git stash)",
            ] {
                assert!(
                    rules.iter().any(|r| r == rule),
                    "{mode}: {rule} in {rules:?}"
                );
            }
            assert_eq!(args.iter().filter(|a| *a == "--disallowedTools").count(), 1);
        }
        // In the project itself, only the settings' rules.
        t.isolated = false;
        let settings = Settings::from_value(&json!({ "permissionMode": "bypassPermissions" }));
        let args = args_for(&settings, &t, &session, None, None, None);
        assert!(disallowed(&args).is_empty(), "{args:?}");
    }

    #[test]
    fn its_own_tools_are_denied_the_projects_restricted_files() {
        let session = Session::New("s-1".into());
        let mut t = turn("Fix it");
        t.denied = crate::restricted_files::Denied {
            read: vec![".env".into(), "secrets/".into(), "odd[1]*.key".into()],
            write: vec![".env".into()],
        };
        for mode in ["bypassPermissions", "rovik"] {
            let settings = Settings::from_value(&json!({ "permissionMode": mode }));
            let config = (mode == "rovik").then(|| Path::new("/tmp/mcp.json"));
            let args = args_for(&settings, &t, &session, None, config, None);
            let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
            assert_eq!(
                args[at + 1..at + 5],
                [
                    "Read(./.env)",
                    "Read(./secrets/**)",
                    "Read(./odd\\[1\\]\\*.key)",
                    "Edit(./.env)",
                ],
                "{mode}"
            );
        }
        // With Rovik's tools, none of its own: nothing to deny.
        t.rovik_tools = true;
        let settings = Settings::default();
        let args = args_for(
            &settings,
            &t,
            &session,
            None,
            Some(Path::new("/tmp/mcp.json")),
            None,
        );
        assert!(!args.iter().any(|a| a == "--disallowedTools"), "{args:?}");
    }

    /// Its web search runs on Anthropic's servers, where the user's allowed
    /// hosts can't reach: off while they're on, in one flag with the rest.
    #[test]
    fn its_web_search_is_off_while_the_allowed_hosts_are_on() {
        let session = Session::New("s-1".into());
        let mut t = turn("Look it up");
        let disallowed = |t: &Turn, mode: &str| -> Vec<String> {
            let settings = Settings::from_value(&json!({
                "permissionMode": mode,
                "disallowedTools": "WebFetch",
            }));
            let config = (mode == "rovik").then(|| Path::new("/tmp/mcp.json"));
            let args = args_for(&settings, t, &session, None, config, None);
            assert!(args.iter().filter(|a| *a == "--disallowedTools").count() <= 1);
            let at = args.iter().position(|a| a == "--disallowedTools");
            at.map(|i| {
                args[i + 1..]
                    .iter()
                    .take_while(|a| !a.starts_with("--"))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
        };
        for mode in ["bypassPermissions", "rovik"] {
            assert_eq!(disallowed(&t, mode), ["WebFetch"], "{mode}");
        }
        t.allowed_hosts_on = true;
        for mode in ["bypassPermissions", "rovik"] {
            assert_eq!(disallowed(&t, mode), ["WebFetch", "WebSearch"], "{mode}");
        }
    }

    #[test]
    fn each_git_deny_rule_names_a_command_rovik_refuses_and_reading_stays_free() {
        let rules = git_deny_rules();
        let command = |rule: &str| {
            rule.trim_start_matches("Bash(")
                .trim_end_matches(')')
                .trim_end_matches(" *")
                .to_string()
        };
        for rule in &rules {
            let words: Vec<String> = command(rule).split(' ').map(str::to_string).collect();
            assert!(
                crate::overlay::chats::git_changes_repo(&words).is_some(),
                "{rule} isn't refused by Rovik's own check"
            );
        }
        // Reading the repository matches none of them.
        for read in [
            "git status",
            "git log --oneline",
            "git stash list",
            "git branch",
            "git diff",
        ] {
            assert!(
                !rules.iter().any(|r| {
                    let c = command(r);
                    if r.ends_with(" *)") {
                        read == c || read.starts_with(&format!("{c} "))
                    } else {
                        read == c
                    }
                }),
                "{read} is denied"
            );
        }
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
        let logout = step(SetupAction::Logout, settings.clone())
            .unwrap()
            .unwrap();
        assert_eq!(logout.args, ["auth", "logout"]);
        let update = step(SetupAction::Update, settings).unwrap().unwrap();
        assert_eq!(update.args, ["update"]);
        let plain = json!({ "program": program.to_string_lossy() });
        let login = step(SetupAction::Login, plain).unwrap().unwrap();
        assert_eq!(login.args, ["auth", "login"]);
    }

    #[test]
    fn rovik_decides_what_claude_code_would_ask_about() {
        let settings = Settings::from_value(&json!({
            "allowedTools": "Bash(npm test *)",
            "disallowedTools": "WebFetch",
        }));
        assert_eq!(settings.permission_mode, ROVIK_PERMISSIONS, "the default");
        let t = turn("Fix it");
        let config = Path::new("/tmp/mcp.json");
        let args = args_for(
            &settings,
            &t,
            &Session::New("s".into()),
            None,
            Some(config),
            None,
        );
        let joined = args.join(" ");
        assert!(joined.contains("--permission-prompts host"), "{joined}");
        assert!(
            joined.contains(
                "--permission-mode default --mcp-config /tmp/mcp.json \
                 --permission-prompt-tool mcp__rovik__approve --disallowedTools WebFetch"
            ),
            "{joined}"
        );
        // An allowed-tools rule would pass Rovik by; Claude Code's own
        // tools stay, and the user's own MCP servers with them.
        assert!(!joined.contains("--allowedTools"), "{joined}");
        assert!(
            !joined.contains("--strict-mcp-config") && !joined.contains("--tools"),
            "{joined}"
        );

        // Nobody to ask (a request outside a run): what would ask is denied.
        let args = args_for(&settings, &t, &Session::New("s".into()), None, None, None);
        let joined = args.join(" ");
        assert!(joined.contains("--permission-prompts none"), "{joined}");
        assert!(joined.contains("--permission-mode default"), "{joined}");
        assert!(!joined.contains("--permission-prompt-tool"), "{joined}");

        // Claude Code's own modes are as before.
        let own = Settings::from_value(&json!({ "permissionMode": "acceptEdits" }));
        let joined = args_for(&own, &t, &Session::New("s".into()), None, None, None).join(" ");
        assert!(joined.contains("--permission-mode acceptEdits"), "{joined}");
        assert!(joined.contains("--permission-prompts none"), "{joined}");
    }

    #[test]
    fn claude_codes_calls_are_asked_as_rovik_s_of_the_same_kind() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.as_path();
        let file = root.join("src.txt");
        std::fs::write(&file, "one two one\n").unwrap();
        let path = file.to_string_lossy().into_owned();

        let bash = asked_call(
            "Bash",
            &json!({ "command": "npm i", "timeout": 5 }),
            Some(root),
        );
        assert_eq!(
            (bash.tool.as_str(), bash.arguments.clone()),
            ("bash", json!({ "command": "npm i" }))
        );

        // An edit is the whole file it makes, at its path in the project.
        let edit = json!({ "file_path": path, "old_string": "one", "new_string": "1" });
        let asked = asked_call("Edit", &edit, Some(root));
        assert_eq!(asked.tool, "write");
        assert_eq!(
            asked.arguments,
            json!({ "path": "src.txt", "content": "1 two one\n" })
        );
        assert_eq!(asked.current.as_deref(), Some("one two one\n"));
        let all = json!({ "file_path": path, "old_string": "one", "new_string": "1", "replace_all": true });
        assert_eq!(
            asked_call("Edit", &all, Some(root)).arguments["content"],
            "1 two 1\n"
        );
        let multi = json!({ "file_path": path, "edits": [
            { "old_string": "two", "new_string": "2" },
            { "old_string": "one", "new_string": "1", "replace_all": true },
        ] });
        assert_eq!(
            asked_call("MultiEdit", &multi, Some(root)).arguments["content"],
            "1 2 1\n"
        );
        let write = json!({ "file_path": root.join("new.txt"), "content": "hi" });
        assert_eq!(
            asked_call("Write", &write, Some(root)).arguments,
            json!({ "path": "new.txt", "content": "hi" })
        );
        // One that won't apply is asked about as an edit of its file; one
        // outside the project keeps its path, which Rovik refuses.
        let stale = json!({ "file_path": path, "old_string": "gone", "new_string": "x" });
        assert_eq!(
            asked_call("Edit", &stale, Some(root)).arguments,
            json!({ "path": "src.txt" })
        );
        let outside = json!({ "file_path": "/etc/hosts", "content": "x" });
        assert_eq!(
            asked_call("Write", &outside, Some(root)).arguments["path"],
            "/etc/hosts"
        );
        // Any other tool by its own name.
        let fetch = json!({ "url": "https://example.com" });
        assert_eq!(asked_call("WebFetch", &fetch, Some(root)).tool, "WebFetch");
    }

    /// What a step of Claude Code's own did is the Rovik call that does the
    /// same, as its check before it runs has it: the run records the
    /// changes of those that succeeded as its own.
    #[test]
    fn claude_codes_steps_are_the_rovik_calls_that_do_the_same() {
        let step = |name: &str, input: Value| HarnessStep {
            id: "toolu_1".into(),
            name: name.into(),
            input,
            ..HarnessStep::default()
        };
        let write = |args: Value| HarnessCall::Tool {
            name: "write".into(),
            args: args.as_object().unwrap().clone(),
        };
        let calls = |name: &str, input: Value| ClaudeCode.step_calls(&step(name, input));
        assert_eq!(
            calls(
                "Bash",
                json!({ "command": "npm i", "description": "install" })
            ),
            [HarnessCall::Command("npm i".into())]
        );
        assert_eq!(
            calls(
                "Edit",
                json!({ "file_path": "src/a.ts", "old_string": "a", "new_string": "b" })
            ),
            [write(json!({ "path": "src/a.ts", "old_text": "" }))]
        );
        assert_eq!(
            calls("Write", json!({ "file_path": "b.ts", "content": "x" })),
            [write(json!({ "path": "b.ts", "content": "x" }))]
        );
        assert_eq!(
            calls(
                "NotebookEdit",
                json!({ "notebook_path": "n.ipynb", "new_source": "x" })
            ),
            [write(json!({ "path": "n.ipynb", "old_text": "" }))]
        );
        // What changes no file is none of the run's changes.
        assert!(calls("Glob", json!({ "pattern": "**/*.ts" })).is_empty());
        assert!(calls("WebFetch", json!({ "url": "https://example.com" })).is_empty());
        let thought = HarnessStep {
            id: "thinking-1".into(),
            thinking: Some("Hmm.".into()),
            ..HarnessStep::default()
        };
        assert!(ClaudeCode.step_calls(&thought).is_empty());
    }

    #[test]
    fn rovik_s_answer_is_the_permission_tool_s_result() {
        let edit = json!({ "file_path": "/p/a.txt", "old_string": "a", "new_string": "b" });
        let asked = AskedCall {
            tool: "write".into(),
            arguments: json!({ "path": "a.txt", "content": "b\nc\n" }),
            current: Some("a\nc\n".into()),
        };
        let deny = ToolAnswer::Deny {
            reason: "Not allowed.".into(),
        };
        assert_eq!(
            permission_result("Edit", &edit, &asked, deny),
            json!({ "behavior": "deny", "message": "Not allowed." })
        );
        let allow = ToolAnswer::Allow { arguments: None };
        assert_eq!(
            permission_result("Edit", &edit, &asked, allow),
            json!({ "behavior": "allow", "updatedInput": edit })
        );
        // Some of the changes accepted: the file as it is, replaced whole.
        let partial = ToolAnswer::Allow {
            arguments: Some(json!({ "path": "a.txt", "content": "a\nc\nd\n" }).to_string()),
        };
        assert_eq!(
            permission_result("Edit", &edit, &asked, partial.clone())["updatedInput"],
            json!({ "file_path": "/p/a.txt", "old_string": "a\nc\n", "new_string": "a\nc\nd\n", "replace_all": false })
        );
        let write = json!({ "file_path": "/p/a.txt", "content": "b\nc\n" });
        assert_eq!(
            permission_result("Write", &write, &asked, partial)["updatedInput"]["content"],
            "a\nc\nd\n"
        );
    }

    /// The real `claude` with its own tools under Rovik's permissions: its
    /// Bash call comes to the run as `bash`, and runs or not as answered.
    ///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored
    #[tokio::test]
    #[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
    async fn real_claude_asks_rovik_before_its_own_tools() {
        if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
            return;
        }
        for allow in [false, true] {
            let dir = crate::testing::temp_dir();
            let mut t = turn("Run exactly this shell command with your Bash tool: echo hi > out.txt. Then reply done.");
            t.model = "haiku".into();
            t.cwd = Some(dir.0.clone());
            let (asks, mut asked) = mpsc::unbounded_channel();
            t.asks = Some(asks);
            let answering = tokio::spawn(async move {
                let ask: ToolAsk = asked.recv().await.expect("Claude Code asks");
                let seen = (ask.tool.clone(), ask.arguments.clone());
                let answer = if allow {
                    ToolAnswer::Allow { arguments: None }
                } else {
                    ToolAnswer::Deny {
                        reason: "The user said no.".into(),
                    }
                };
                ask.reply.send(answer).unwrap();
                seen
            });
            let (_tx, mut cancel) = watch::channel(false);
            let mut events = Vec::new();
            ClaudeCode
                .run(t, &mut cancel, &mut |e| events.push(e))
                .await
                .unwrap();
            let (tool, arguments) = answering.await.unwrap();
            assert_eq!(tool, "bash");
            assert!(arguments.contains("out.txt"), "{arguments}");
            assert_eq!(dir.0.join("out.txt").exists(), allow, "{events:#?}");
        }
    }

    #[test]
    fn the_newest_version_is_the_release_bucket_s_answer() {
        assert_eq!(latest_of("2.1.310\n").as_deref(), Some("2.1.310"));
        assert_eq!(latest_of(" "), None);
        assert_eq!(latest_of("<html><body>Not found</body></html>"), None);
        assert_eq!(latest_of("v2"), None);
    }

    /// The real release bucket answers with a version.
    ///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored
    #[tokio::test]
    #[ignore = "asks downloads.claude.ai: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
    async fn real_claude_latest_version_is_published() {
        if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
            return;
        }
        let latest = ClaudeCode.latest_version().await.expect("a version");
        assert!(crate::connectors::is_newer(&latest, "0.0.1"), "{latest}");
    }

    #[test]
    fn claude_code_s_own_prompt_is_left_as_it_is() {
        let settings = Settings::from_value(&json!({}));
        let t = turn("Fix it");
        // No file to read, and none of the system prompt flags.
        assert!(prompt_file(&t).unwrap().is_none());
        let file = Path::new("/tmp/prompt.md");
        let joined = args_for(
            &settings,
            &t,
            &Session::New("s".into()),
            Some(file),
            None,
            None,
        )
        .join(" ");
        assert!(!joined.contains("system-prompt-file"), "{joined}");
        // A session that began with Rovik's prompt takes its own from now on.
        assert!(joined.contains("--system-prompt-snapshot off"), "{joined}");
        // With Rovik's tools, Claude Code has none for its prompt to describe:
        // Rovik's goes in its place.
        let mut rovik = turn("Fix it");
        rovik.rovik_tools = true;
        assert!(sends_rovik_prompt(&rovik));
        let joined = args_for(
            &settings,
            &rovik,
            &Session::New("s".into()),
            Some(file),
            None,
            None,
        )
        .join(" ");
        assert!(
            joined.contains("--system-prompt-file /tmp/prompt.md"),
            "{joined}"
        );

        // A one-off request's system prompt is the request (a title's
        // instructions): it's sent in place of Claude Code's.
        let mut one = turn("Name this chat");
        one.one_shot = true;
        let written = prompt_file(&one).unwrap().expect("a file");
        let joined = args_for(
            &settings,
            &one,
            &Session::New("s".into()),
            Some(written.path()),
            None,
            None,
        )
        .join(" ");
        assert!(joined.contains("--system-prompt-file"), "{joined}");
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

    /// The usage of the run's `Finished` event.
    fn finished_usage(events: &[ChatStreamEvent]) -> Value {
        events
            .iter()
            .find_map(|e| match e {
                ChatStreamEvent::Finished { usage, .. } => usage.clone(),
                _ => None,
            })
            .expect("a Finished event")
    }

    #[test]
    fn a_reply_keeps_the_context_it_left() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let lines = [
            json!({"type":"autocompact_state","value":{"enabled":true,"effective_window":980000,"threshold":784000}}),
            json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-haiku-5-5"}),
            json!({"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"ok"}]},"parent_tool_use_id":null}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"ok","num_turns":1,"usage":{"input_tokens":2,"cache_creation_input_tokens":9202,"cache_read_input_tokens":25989,"output_tokens":4,"iterations":[{"input_tokens":2,"cache_creation_input_tokens":9202,"cache_read_input_tokens":25989,"output_tokens":4}]},"modelUsage":{"claude-haiku-5-5":{"contextWindow":1000000}},"session_id":"s-1"}),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut emit);
        }
        // The last request's tokens, of the model's window; where it compacts.
        assert_eq!(
            finished_usage(&events)["connector"]["context"],
            json!({ "used": 35197, "window": 1000000, "compactsAt": 784000 })
        );
    }

    #[test]
    fn a_compaction_says_what_it_did_and_what_it_left() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let lines = [
            json!({"type":"autocompact_state","value":{"effective_window":980000,"threshold":784000}}),
            json!({"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":35312,"post_tokens":5422}}),
            json!({"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted </local-command-stdout>"}}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"","num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{},"session_id":"s-1"}),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut emit);
        }
        assert!(
            names(&events)
                .contains(&"delta:_Compacted the conversation: 35.3k → 5.4k tokens._".to_string()),
            "{:?}",
            names(&events)
        );
        assert_eq!(
            finished_usage(&events)["connector"]["context"],
            json!({ "used": 5422, "window": 980000, "compactsAt": 784000 })
        );

        // A command that made no request and compacted nothing (`/context`):
        // its output is the reply, and the context is the last one known.
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let result = json!({"type":"result","subtype":"success","is_error":false,"result":"## Context Usage","num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"session_id":"s-1"});
        p.line(&result.to_string(), &mut emit);
        assert!(names(&events).contains(&"delta:## Context Usage".to_string()));
        assert!(finished_usage(&events)["connector"]
            .get("context")
            .is_none());
    }

    #[test]
    fn the_commands_are_the_initialize_answer_s_without_those_rovik_keeps() {
        let answer = json!({"type":"control_response","response":{"subtype":"success","request_id":"rovik-commands","response":{"commands":[
            {"name":"hello","description":"Says hello to a name (project)","argumentHint":"<name>"},
            {"name":"clear","description":"Start a new session","argumentHint":"[name]","aliases":["reset","new"],"builtin":true},
            {"name":"compact","description":"Free up context","builtin":true},
            {"name":"  ","description":"nameless"},
            {"name":"model","description":"Set the AI model for Claude Code","builtin":true},
            {"name":"fewer-permission-prompts","description":"Add an allowlist","builtin":true},
            {"name":"__remote-workflow","description":"internal","builtin":true},
            {"name":"model","description":"Pick our model (project)"}
        ]}}});
        let commands = commands_of(&answer.to_string()).unwrap().unwrap();
        assert_eq!(
            commands,
            vec![
                ConnectorCommand {
                    name: "hello".into(),
                    description: "Says hello to a name (project)".into(),
                    argument_hint: Some("<name>".into()),
                    aliases: Vec::new(),
                    builtin: false,
                },
                ConnectorCommand {
                    name: "compact".into(),
                    description: "Free up context".into(),
                    argument_hint: None,
                    aliases: Vec::new(),
                    builtin: true,
                },
                // A project's own `/model` is the project's to run.
                ConnectorCommand {
                    name: "model".into(),
                    description: "Pick our model (project)".into(),
                    argument_hint: None,
                    aliases: Vec::new(),
                    builtin: false,
                },
            ]
        );
        // Other lines aren't its answer; a refusal is an error.
        assert!(commands_of(r#"{"type":"system","subtype":"init"}"#).is_none());
        let other = json!({"type":"control_response","response":{"subtype":"success","request_id":"other","response":{}}});
        assert!(commands_of(&other.to_string()).is_none());
        let refused = json!({"type":"control_response","response":{"subtype":"error","request_id":"rovik-commands","error":"Unsupported"}});
        assert!(commands_of(&refused.to_string())
            .unwrap()
            .unwrap_err()
            .contains("Unsupported"));
    }

    /// The real `claude` lists its commands, the project's own among them,
    /// without a turn of the model.
    ///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored
    #[tokio::test]
    #[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
    async fn real_claude_lists_its_commands() {
        if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
            return;
        }
        let dir = crate::testing::temp_dir();
        let commands = dir.0.join(".claude/commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(
            commands.join("hello.md"),
            "---\ndescription: Says hello to a name\nargument-hint: <name>\n---\nReply with: hello $ARGUMENTS\n",
        )
        .unwrap();
        let args = ProbeRequestArgs {
            kind: ConnectorKind::ClaudeCode,
            settings: json!({}),
            api_key: String::new(),
            request: false,
        };
        let listed = ClaudeCode.commands(&args, &dir.0).await.unwrap();
        let hello = listed
            .iter()
            .find(|c| c.name == "hello")
            .expect("the project's command");
        assert_eq!(hello.argument_hint.as_deref(), Some("<name>"));
        assert!(hello.description.starts_with("Says hello"), "{hello:?}");
        assert!(listed.iter().any(|c| c.name == "compact" && c.builtin));
        // Those that would change what Rovik keeps aren't offered.
        for hidden in ["clear", "model", "effort", "config"] {
            assert!(
                listed.iter().all(|c| c.name != hidden),
                "{hidden} is offered"
            );
        }
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
                // Its thinking, a step; its text ends it.
                "thought:thinking-1:hmm:…",
                "thought:thinking-1:hmm:done",
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
        } = &events[8]
        else {
            panic!()
        };
        assert_eq!(usage["cost"], 0.01);
        assert_eq!(usage["connector"]["numTurns"], 2);
        assert_eq!(usage["connector"]["model"], "claude-sonnet-5");
        assert_eq!(usage["connector"]["version"], "2.1.309");
        assert_eq!(
            usage["connector"]["steps"], 2,
            "its tool calls, not its thinking"
        );
        let ChatStreamEvent::HarnessStep { step } = &events[6] else {
            panic!()
        };
        assert_eq!(step.parent.as_deref(), Some("t9"));
        assert!(p.finished && p.failure.is_none());
    }

    /// With its own tools, each stretch of its thinking is a step of its
    /// own, in its place among its tool calls, timed once it's over;
    /// thinking that came back empty (its text not asked for) is none.
    #[test]
    fn with_its_own_tools_each_stretch_of_thinking_is_a_step_in_its_place() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let event = |message: &str, e: Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null,"api_message_id":message});
        let think = |index: u64, text: &str| json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}});
        let lines = [
            event("m1", json!({"type":"message_start","message":{"id":"m1"}})),
            event(
                "m1",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            event(
                "m1",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"s"}}),
            ),
            event("m1", json!({"type":"content_block_stop","index":0})),
            event(
                "m1",
                json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}),
            ),
            event("m1", think(1, "Read the file")),
            event("m1", think(1, " first.")),
            event("m1", json!({"type":"content_block_stop","index":1})),
            event(
                "m1",
                json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"Read","input":{}}}),
            ),
            event("m1", json!({"type":"content_block_stop","index":2})),
            event("m1", json!({"type":"message_stop"})),
            // The whole message repeats its thinking: not shown again.
            json!({"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"Read the file first."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]},"parent_tool_use_id":null}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"42","is_error":false}]},"parent_tool_use_id":null}),
            event("m2", json!({"type":"message_start","message":{"id":"m2"}})),
            event(
                "m2",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            event("m2", think(0, "It returns 42.")),
            event("m2", json!({"type":"content_block_stop","index":0})),
            event(
                "m2",
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            ),
            event(
                "m2",
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"It's 42."}}),
            ),
            event("m2", json!({"type":"content_block_stop","index":1})),
            event("m2", json!({"type":"message_stop"})),
            json!({"type":"result","subtype":"success","is_error":false,"result":"It's 42.","usage":{},"num_turns":2}),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut emit);
        }
        assert_eq!(
            names(&events),
            [
                "thought:thinking-1:Read the file:…",
                "thought:thinking-1:Read the file first.:…",
                "thought:thinking-1:Read the file first.:done",
                "step:Read::",
                "step:Read:42:true",
                "thought:thinking-2:It returns 42.:…",
                "thought:thinking-2:It returns 42.:done",
                "delta:It's 42.",
                "finished:success:0",
                "done",
            ]
        );
        let ChatStreamEvent::HarnessStep { step } = &events[2] else {
            panic!()
        };
        assert!(step.name.is_empty() && step.input.is_null(), "{step:?}");
    }

    /// The paths its steps name are relative to its folder, as Rovik's own
    /// tools take them; one outside it stays as it is.
    #[test]
    fn the_paths_its_steps_name_are_relative_to_its_folder() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.clone();
        let mut p = Parser {
            root: Some(root.clone()),
            ..Parser::default()
        };
        let mut events = Vec::new();
        let message = json!({"type":"assistant","message":{"id":"m1","content":[
            {"type":"tool_use","id":"t1","name":"Read","input":{"file_path": root.join("src").join("a.rs")}},
            {"type":"tool_use","id":"t2","name":"Grep","input":{"pattern":"x","path": root}},
            {"type":"tool_use","id":"t3","name":"Read","input":{"file_path":"/etc/hosts"}},
        ]},"parent_tool_use_id":null});
        p.line(&message.to_string(), &mut |e| events.push(e));
        let inputs: Vec<Value> = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::HarnessStep { step } => Some(step.input.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(inputs[0]["file_path"], "src/a.rs");
        assert_eq!(inputs[1]["path"], ".");
        assert_eq!(inputs[2]["file_path"], "/etc/hosts");
    }

    #[test]
    fn a_message_not_streamed_shows_its_thinking_whole_before_its_tool_calls() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let message = json!({"type":"assistant","message":{"id":"m1","content":[
            {"type":"thinking","thinking":""},
            {"type":"thinking","thinking":"Check a.rs."},
            {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}},
        ]},"parent_tool_use_id":null});
        p.line(&message.to_string(), &mut |e| events.push(e));
        assert_eq!(
            names(&events),
            ["thought:thinking-1:Check a.rs.:…", "step:Read::"]
        );
    }

    /// With Rovik's tools, a reply is one message of Claude Code's: its
    /// thinking is the reply's reasoning, as any model's is.
    #[test]
    fn with_rovik_tools_thinking_is_the_replys_reasoning() {
        let mut p = Parser::with_rovik_tools();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let event = |e: Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null,"api_message_id":"m1"});
        for line in [
            event(json!({"type":"message_start","message":{"id":"m1"}})),
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Look first."}}),
            ),
            event(json!({"type":"content_block_stop","index":0})),
            json!({"type":"assistant","message":{"id":"m2","content":[{"type":"thinking","thinking":"Then answer."}]},"parent_tool_use_id":null}),
        ] {
            p.line(&line.to_string(), &mut emit);
        }
        assert_eq!(
            names(&events),
            ["reasoning:Look first.", "reasoning:Then answer."]
        );
    }

    /// Its thinking is asked for on a chat's turns, whichever tools it
    /// runs, for the chat to show; a one-off request's isn't shown.
    #[test]
    fn its_thinking_is_asked_for_on_a_chats_turns_not_one_off_requests() {
        let session = Session::New("s-1".into());
        let asks = |rovik_tools: bool, one_shot: bool| {
            let mut t = turn("Hi");
            t.rovik_tools = rovik_tools;
            t.one_shot = one_shot;
            args_for(&Settings::default(), &t, &session, None, None, None)
                .join(" ")
                .contains("--thinking-display summarized")
        };
        assert!(asks(false, false), "its own tools");
        assert!(asks(true, false), "Rovik's tools");
        assert!(!asks(false, true), "a chat's title");
    }

    /// A skill the user runs (`/name`) that works in a forked context prints
    /// nothing of its work until it's done: it's a `Skill` step from the
    /// moment its task starts, done when the task ends, and what it did
    /// comes after, as its subagent's steps and the reply's text.
    #[test]
    fn a_skill_the_user_runs_in_a_forked_context_is_a_step_while_it_works() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        // Claude Code 2.1.311's lines for `/slow-census`, a skill with
        // `context: fork` (cut short): its task starts, then a command of
        // its own and the task itself end; the rest comes all at once.
        let fork = "forked-command-slow-census";
        let started = json!({"type":"system","subtype":"task_started","task_id":"a904","description":"/slow-census","subagent_type":"general-purpose","is_backgrounded":false,"task_type":"local_agent","prompt":"Base directory for this skill: …","skip_transcript":true,"ambient":true});
        p.line(&started.to_string(), &mut |e| events.push(e));
        assert_eq!(names(&events), ["step:Skill::"], "shown as it starts");
        let lines = [
            json!({"type":"system","subtype":"task_started","task_id":"bwhh","owned_by_subagent":true,"tool_use_id":"toolu_1","description":"Pause for 3 seconds","parent_task_id":"a904","task_type":"local_bash"}),
            json!({"type":"system","subtype":"task_notification","task_id":"bwhh","tool_use_id":"toolu_1","status":"completed","summary":"Pause for 3 seconds"}),
            json!({"type":"system","subtype":"task_notification","task_id":"a904","status":"completed","summary":"/slow-census","skip_transcript":true,"ambient":true}),
            json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-haiku-5-5"}),
            json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"Base directory for this skill: …"}]},"parent_tool_use_id":fork,"isSynthetic":true}),
            json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"sleep 3"}}]},"parent_tool_use_id":fork}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"(Bash completed with no output)"}]},"parent_tool_use_id":fork}),
            json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"a.txt says alpha."}]},"parent_tool_use_id":null}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"a.txt says alpha.","num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"total_cost_usd":0.0025}),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut |e| events.push(e));
        }
        assert_eq!(
            names(&events),
            [
                "step:Skill::",
                "step:Skill:The skill completed.:true",
                "session:s-1:claude-haiku-5-5",
                "step:Bash::",
                "step:Bash:(Bash completed with no output):true",
                "delta:a.txt says alpha.",
                "finished:success:0",
                "done",
            ]
        );
        let steps: Vec<&HarnessStep> = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::HarnessStep { step } => Some(step),
                _ => None,
            })
            .collect();
        assert_eq!(steps[0].id, "a904");
        assert_eq!(steps[0].input, json!({ "skill": "slow-census" }));
        assert_eq!(steps[0].parent, None, "the reply's own step");
        assert_eq!(steps[2].parent.as_deref(), Some(fork), "its subagent's");
        assert!(p.finished && p.failure.is_none());
    }

    /// The skill's arguments are its step's; a task that fails or is
    /// stopped says so. Tasks of a tool call's (whose step shows them) or
    /// not run as a command are no step of their own.
    #[test]
    fn a_forked_skill_s_step_has_its_arguments_and_how_it_ended() {
        let mut p = Parser::default();
        let mut events = Vec::new();
        let mut emit = |e| events.push(e);
        let started = |id: &str, description: &str| json!({"type":"system","subtype":"task_started","task_id":id,"description":description,"task_type":"local_agent"});
        let ended = |id: &str, status: &str| json!({"type":"system","subtype":"task_notification","task_id":id,"status":status,"summary":"x"});
        let lines = [
            started("t1", "/review-pr 123 --fast"),
            ended("t1", "failed"),
            started("t2", "/file-census"),
            ended("t2", "stopped"),
            // A subagent's task, which its tool call's step shows.
            json!({"type":"system","subtype":"task_started","task_id":"t3","tool_use_id":"toolu_9","description":"/file-census","task_type":"local_agent"}),
            ended("t3", "completed"),
            // Not the user's command.
            started("t4", "Summarize the session"),
            ended("t4", "completed"),
        ];
        for line in lines {
            p.line(&line.to_string(), &mut emit);
        }
        assert_eq!(
            names(&events),
            [
                "step:Skill::",
                "step:Skill:The skill failed.:false",
                "step:Skill::",
                "step:Skill:The skill was stopped.:false",
            ]
        );
        let ChatStreamEvent::HarnessStep { step } = &events[0] else {
            panic!()
        };
        assert_eq!(
            step.input,
            json!({ "skill": "review-pr", "args": "123 --fast" })
        );
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
        assert_eq!(
            system_text(&t, false),
            "You are Rovik's agent.\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n## This project\nRust."
        );
        // Rovik's tools named only when there are some.
        assert!(!system_text(&t, true).contains("mcp__rovik__"));
        t.tools = vec![read_tool()];
        let text = system_text(&t, true);
        assert!(text.contains("`mcp__rovik__read`"), "{text}");
        assert!(text.find("## Your tools").unwrap() < text.find(PROMPT_BOUNDARY).unwrap());
    }

    #[test]
    fn rovik_tools_mode_offers_its_server_alone() {
        let mut t = turn("Fix it");
        t.rovik_tools = true;
        // Whatever the permission mode, Rovik's (the default) included:
        // Rovik runs each call of its tools under its own permissions.
        for mode in ["bypassPermissions", ROVIK_PERMISSIONS] {
            let settings = Settings::from_value(&json!({
                "permissionMode": mode,
                "allowedTools": "Bash",
                "maxTurns": 40,
            }));
            let args = args_for(
                &settings,
                &t,
                &Session::New("s".into()),
                Some(Path::new("/tmp/p.md")),
                Some(Path::new("/tmp/mcp.json")),
                None,
            );
            let joined = args.join(" ");
            assert!(joined.contains("--permission-prompts none"), "{joined}");
            assert!(joined.contains("--tools  --mcp-config /tmp/mcp.json --strict-mcp-config --permission-mode dontAsk --allowedTools mcp__rovik --max-turns 40 --session-id s"), "{joined}");
            assert!(
                !joined.contains("bypassPermissions")
                    && !joined.contains("Bash")
                    && !joined.contains("--permission-prompt-tool"),
                "the settings' own permissions aren't used: {joined}"
            );
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn rovik_tools_mode_yields_rovik_runs_the_call_and_the_run_goes_on() {
        let dir = crate::testing::temp_dir();
        let Some(program) =
            crate::connectors::fake_claude_with_mcp(&dir.0, "read", &json!({ "path": "a.rs" }))
        else {
            eprintln!("no python3: skipped");
            return;
        };
        let dialog = format!("d-{}", uuid::Uuid::new_v4());
        let mut first = turn("Read a.rs");
        first.settings = json!({ "program": program });
        first.rovik_tools = true;
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
        assert!(LIVE.has(&dialog), "it waits for Rovik's result");
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
        second.settings = json!({ "program": program });
        second.rovik_tools = true;
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
        assert!(!LIVE.has(&dialog), "its run is over");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_waiting_run_ends_with_rovik_run() {
        let dir = crate::testing::temp_dir();
        let Some(program) =
            crate::connectors::fake_claude_with_mcp(&dir.0, "read", &json!({ "path": "a.rs" }))
        else {
            return;
        };
        let dialog = format!("d-{}", uuid::Uuid::new_v4());
        let mut first = turn("Read a.rs");
        first.settings = json!({ "program": program });
        first.rovik_tools = true;
        first.dialog_id = Some(dialog.clone());
        first.tools = vec![read_tool()];
        let (_tx, mut cancel) = watch::channel(false);
        ClaudeCode
            .run(first, &mut cancel, &mut |_| {})
            .await
            .unwrap();
        assert!(LIVE.has(&dialog));
        crate::connectors::end_live(&dialog);
        assert!(!LIVE.has(&dialog));
        // A turn that doesn't answer the waiting calls (another reply came
        // between) starts afresh instead of going on.
        let mut other = turn("Something else");
        other.settings = json!({ "program": program });
        other.rovik_tools = true;
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
        let settings = json!({});
        let mut first = turn(
            "Read the file notes.txt with your read tool, then reply with only the secret word it contains.",
        );
        first.model = "haiku".into();
        first.settings = settings.clone();
        first.rovik_tools = true;
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
        assert!(LIVE.has(&dialog), "waiting for Rovik's result");
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
        second.rovik_tools = true;
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
        assert!(!LIVE.has(&dialog));
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

    /// Rovik's hooks, around its own tools: before each call of its shell
    /// and of its tools that read, change or search files, and after a
    /// command fails, posted to this turn's server with its token, an hour
    /// to wait; and the settings file they're in goes before the
    /// connector's own arguments, so a `--settings` of theirs wins.
    #[tokio::test]
    async fn rovik_hooks_wrap_claude_codes_own_tools() {
        let (server, _calls) = McpServer::start(&[], "test", None).await.unwrap();
        let hooks: Value = serde_json::from_str(&hook_settings(&server)).unwrap();
        for (event, matcher) in [
            ("PreToolUse", CHECKED_TOOLS),
            ("PostToolUseFailure", "Bash"),
        ] {
            let entry = &hooks["hooks"][event][0];
            assert_eq!(entry["matcher"], matcher, "{event}");
            let hook = &entry["hooks"][0];
            assert_eq!(hook["type"], "http");
            assert_eq!(hook["url"], server.hooks_url());
            assert_eq!(hook["timeout"], 3600);
            assert_eq!(hook["headers"]["Authorization"], server.authorization());
        }
        let settings = Settings::from_value(&json!({ "extraArgs": "--settings mine.json" }));
        let t = turn("hi");
        let joined = args_for(
            &settings,
            &t,
            &Session::New("s".into()),
            None,
            None,
            Some(Path::new("/tmp/hooks.json")),
        )
        .join(" ");
        assert!(
            joined.ends_with("--settings /tmp/hooks.json --settings mine.json"),
            "{joined}"
        );
    }

    /// Its own file tools are checked as the Rovik calls that do the same
    /// with files: `Read` a read, `Edit` a partial write, `Grep` for lines
    /// the `rg` it runs, quoted as a shell reads it back.
    #[test]
    fn claude_codes_file_tools_are_checked_as_the_rovik_calls_that_do_the_same() {
        let tool = |name: &str, args: Value| {
            Some(HarnessCall::Tool {
                name: name.into(),
                args: args.as_object().cloned().unwrap(),
            })
        };
        assert_eq!(
            harness_call("Bash", &json!({ "command": "cat .env" })),
            Some(HarnessCall::Command("cat .env".into()))
        );
        assert_eq!(harness_call("Bash", &json!({ "command": " " })), None);
        assert_eq!(
            harness_call("Read", &json!({ "file_path": "/p/.env", "limit": 5 })),
            tool("read", json!({ "path": "/p/.env" }))
        );
        assert_eq!(
            harness_call("Write", &json!({ "file_path": "/p/a", "content": "x" })),
            tool("write", json!({ "path": "/p/a", "content": "x" }))
        );
        let edit = json!({ "file_path": "/p/a", "old_string": "x", "new_string": "y" });
        assert_eq!(
            harness_call("Edit", &edit),
            tool("write", json!({ "path": "/p/a", "old_text": "" }))
        );
        assert_eq!(
            harness_call("NotebookEdit", &json!({ "notebook_path": "/p/n.ipynb" })),
            tool("write", json!({ "path": "/p/n.ipynb", "old_text": "" }))
        );
        let grep = json!({
            "pattern": "it's KEY", "path": "/p/src", "glob": "*.ts",
            "output_mode": "content", "-i": true,
        });
        let Some(HarnessCall::Tool { name, args }) = harness_call("Grep", &grep) else {
            panic!("a search for lines is checked");
        };
        assert_eq!(name, "bash");
        let command = args["command"].as_str().unwrap();
        assert_eq!(
            crate::agent_run::permissions::parse_command(command).segments,
            [["rg", "--hidden", "-i", "--glob", "*.ts", "-e", "it's KEY", "/p/src"]]
        );
        // Multiline, as Claude Code runs it: `.` across lines too.
        let across = json!({ "pattern": "A.*B", "output_mode": "content", "multiline": true });
        let Some(HarnessCall::Tool { args, .. }) = harness_call("Grep", &across) else {
            panic!("a multiline search is checked");
        };
        let command = args["command"].as_str().unwrap();
        assert_eq!(
            crate::agent_run::permissions::parse_command(command).segments,
            [[
                "rg",
                "--hidden",
                "-U",
                "--multiline-dotall",
                "-e",
                "A.*B",
                "."
            ]]
        );
        // Names or counts, and finding files, read no file's text.
        assert_eq!(harness_call("Grep", &json!({ "pattern": "KEY" })), None);
        assert_eq!(harness_call("Glob", &json!({ "pattern": "**/*" })), None);
    }

    /// A call before which Rovik can't be asked doesn't run: what it'd do
    /// with the restricted files is unknown. After a command failed,
    /// there's nothing to refuse.
    #[tokio::test]
    async fn a_call_the_run_cant_check_doesnt_run() {
        let ask = |event: &str, checks: &CommandChecks| {
            let (reply, answer) = oneshot::channel();
            let call = McpCall {
                tool_use_id: Some("toolu_1".into()),
                name: HOOK.into(),
                arguments: json!({
                    "hook_event_name": event,
                    "tool_name": "Bash",
                    "tool_input": { "command": "cat notes.txt" },
                    "error": "Exit code 1",
                }),
                reply,
            };
            answer_hook(call, checks, None, PathBuf::from("."));
            answer
        };
        // The run isn't there to ask.
        let (checks, gone) = tokio::sync::mpsc::unbounded_channel();
        drop(gone);
        let denied: Value =
            serde_json::from_str(&ask("PreToolUse", &checks).await.unwrap().text).unwrap();
        assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecisionReason"],
            UNCHECKED
        );
        let after = ask("PostToolUseFailure", &checks).await.unwrap();
        assert_eq!(after.text, "{}");
        // The run drops the question unanswered.
        let (checks, mut asked) = tokio::sync::mpsc::unbounded_channel::<CommandCheck>();
        let answer = ask("PreToolUse", &checks);
        drop(asked.recv().await.unwrap());
        let denied: Value = serde_json::from_str(&answer.await.unwrap().text).unwrap();
        assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    /// Claude Code's `error` for a failed command: the exit code on its
    /// first line where it exited, the output after it.
    #[test]
    fn a_failed_commands_exit_code_is_read_from_its_error() {
        assert_eq!(
            exit_and_output("Exit code 127\n/bin/bash: line 2: node: command not found"),
            (
                Some(127),
                "/bin/bash: line 2: node: command not found".to_string()
            )
        );
        assert_eq!(
            exit_and_output("Command timed out after 2m"),
            (None, "Command timed out after 2m".to_string())
        );
    }

    #[test]
    fn a_missing_program_says_how_to_install_it() {
        let err = resolve_program("rovik-no-such-harness-xyz").unwrap_err();
        assert!(err.contains("isn't installed"), "{err}");
        let err = resolve_program("/no/such/dir/claude").unwrap_err();
        assert!(err.contains("no such program"), "{err}");
    }

    #[cfg(unix)]
    use crate::connectors::fake_claude;

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
        // Its own tools: its own prompt; Rovik sends none.
        assert!(
            !args.contains("system-prompt-file") && args.contains("--system-prompt-snapshot off"),
            "{args}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("prompt.txt")).unwrap(),
            "Say hi"
        );

        // A one-off request sends Rovik's prompt in Claude Code's place, in
        // a file that's gone with the run.
        let mut one = turn("Name this chat");
        one.settings = json!({ "program": program, "auth": "token" });
        one.api_key = "tok".into();
        one.one_shot = true;
        one.system_prompt = "Name chats in a few words.".into();
        ClaudeCode.run(one, &mut cancel, &mut |_| {}).await.unwrap();
        let args = std::fs::read_to_string(dir.0.join("args.txt")).unwrap();
        let file = args
            .split_whitespace()
            .skip_while(|a| *a != "--system-prompt-file")
            .nth(1)
            .unwrap_or_else(|| panic!("{args}"));
        assert!(!Path::new(file).exists(), "{file}");
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

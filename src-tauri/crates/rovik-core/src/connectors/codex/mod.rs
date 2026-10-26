//! Codex as a connector (connectors/mod.rs): OpenAI's coding agent, run as
//! its app server (`codex app-server`, developers.openai.com/codex/app-server)
//! and spoken to over its stdio (codex/app_server.rs).
//!
//! One turn is one app server in the project folder. It starts a thread
//! for the chat (`thread/start`), or continues the chat's (`thread/resume`),
//! so Codex keeps its own context between turns, and runs one turn of it
//! (`turn/start`) with the chat's message, the model and the effort. Its
//! own system prompt goes with its own tools; with Rovik's tools, and in a
//! one-off request, Rovik's replaces it (`baseInstructions`).
//!
//! It runs unattended. Under Rovik's permissions (the default) Codex asks
//! before every command and patch, and Rovik's hooks (codex/hooks.rs)
//! answer as Rovik's permissions and approvals say, a patch as the file
//! edits it makes; Codex's own modes are its sandbox and approval presets,
//! where what would ask is declined. In Plan mode it plans in its own plan
//! mode, in a read-only sandbox, its plan the reply. Around each command
//! of its shell, Rovik's hooks refuse what would touch the project's
//! restricted files, and have the run make ready what the command needs,
//! before it runs and after it failed (agent_run/environment.rs); on a
//! cloud host its shell reads the host's toolchains as they're installed
//! (`BASH_ENV`).
//!
//! What it does comes back as the IDE shows a reply (codex/events.rs). With
//! Rovik's tools, Codex is the model behind Rovik's agent loop
//! (codex/rovik_tools.rs). A run on a server finds the program as the
//! user's shell would (PATH), or where a server installs it for everyone
//! (docs/cloud-hosts.md, "Codex").
//!
//! Authentication is the program's own: the login of the computer's user
//! (`codex login`, with ChatGPT), an access token (`CODEX_ACCESS_TOKEN`), or
//! an OpenAI API key, which goes to the app server over its input and is
//! kept in its memory (never a command line, nor a file:
//! `account/login/start`). The token or key travels as the profile's
//! `api_key`, kept where providers' keys are.

mod app_server;
mod events;
mod hooks;
mod patch;
mod rovik_tools;

use super::harness::{
    self, expand_home, pump, split_args, Process, Stop, TempFile, MIN_FIRST_LINE,
};
use super::mcp::{McpCall, McpServer, HOOK};
use super::{
    Connector, ConnectorAuth, ConnectorCommand, ConnectorKind, ConnectorModel, ConnectorProbe,
    ProbeRequest, ProbeRequestArgs, SetupAction, SetupCommand, Turn,
};
use crate::agent::{ChatStreamEvent, ReasoningEffort};
use crate::chat_request::ChatError;
use app_server::{call, config, decline, Message, Rpc, Unanswered};
use events::Parser;
use futures_util::future::BoxFuture;
use hooks::{Event, Hooks};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// OpenAI's installer (developers.openai.com/codex/cli): `codex` in the
/// user's `~/.local/bin` (on Windows, their programs folder), with no
/// admin rights; told not to stop for questions.
const INSTALL: &str = if cfg!(windows) {
    "$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex"
} else {
    "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh"
};

/// Where the newest version is published: the npm package's `latest` tag,
/// which every installation follows.
const LATEST: &str = "https://registry.npmjs.org/@openai/codex/latest";

/// How a session the chat keeps starts when its thread has Rovik's tools:
/// a thread is kept for the tools it was started with.
const ROVIK_TOOLS_SESSION: &str = "rovik:";

/// How long a question with no model turn may take: the app server starts,
/// reads its settings, and answers (the model list, the skills).
const ASK_WITHIN: Duration = Duration::from_secs(30);
/// How long `codex login status` may take: it reads files, asks nothing.
const AUTH_STATUS_WITHIN: Duration = Duration::from_secs(20);
/// How long the probe's test request may take.
const TEST_WITHIN: Duration = Duration::from_secs(180);

/// The MCP servers' tokens, as Codex reads them from its environment
/// (`bearer_token_env_var`): never on a command line.
const TOOLS_TOKEN: &str = "ROVIK_MCP_TOKEN";
const HOOKS_TOKEN: &str = "ROVIK_HOOKS_TOKEN";

/// The connector.
pub struct Codex;

/// How the program signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Auth {
    /// The login of this computer's user (`codex login`, with ChatGPT).
    #[default]
    Login,
    /// An access token, as `CODEX_ACCESS_TOKEN`.
    Token,
    /// An OpenAI API key, given to the app server, kept in its memory.
    ApiKey,
}

/// What Codex's own tools may do without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Rovik's permissions and approvals decide on each command and patch.
    #[default]
    Rovik,
    /// Codex's Auto: it works in the project's folder, and asks its own
    /// reviewer before anything more.
    Auto,
    /// In the project's folder, without network, never asking.
    Workspace,
    /// It reads, and changes nothing.
    ReadOnly,
    /// Anything, without a sandbox or asking.
    FullAccess,
}

/// Whose tools a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolsMode {
    /// Codex's own (its shell, its patches…), under the permission mode of
    /// the settings: Rovik shows what it did.
    #[default]
    Own,
    /// Rovik's, over MCP: Codex is the model behind Rovik's agent loop,
    /// and each call is a tool call of the reply, which Rovik runs.
    Rovik,
}

/// The connector's settings, as the frontend keeps them with its entry
/// (src/lib/codex.ts has the same fields and defaults).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The program: `codex`, found on the PATH, or a path to it.
    pub program: String,
    pub auth: Auth,
    pub permission_mode: PermissionMode,
    /// `CODEX_HOME`: another login and settings folder, such as one shared
    /// on a server. Empty: the program's own (`~/.codex`).
    pub config_dir: String,
    /// More arguments for `codex app-server`, as typed on a command line
    /// (`-c key=value`): after Rovik's, so theirs win.
    pub extra_args: String,
    pub tools: ToolsMode,
}

impl Settings {
    /// The settings a profile carries; absent fields take their defaults.
    pub fn from_value(value: &Value) -> Settings {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

/// Where the program is looked for besides the PATH: the installer's
/// place, the package managers', and a server's shared copy.
fn well_known_places() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return harness::unix_places("codex");
    }
    // Where OpenAI's installer puts it.
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    local
        .map(|local| local.join(r"Programs\OpenAI\Codex\bin\codex.exe"))
        .into_iter()
        .collect()
}

/// The program to run: `program` as given when it names a path, else the
/// first `codex` on the PATH the IDE's programs get, else one of the places
/// it's installed to. An error says how to install it.
pub fn resolve_program(program: &str) -> Result<String, String> {
    let install = format!("Install Codex ({INSTALL}, or `npm install -g @openai/codex`)");
    harness::resolve_program(program, "codex", &well_known_places(), &install)
}

/// The environment a run gets besides the IDE's: where its settings are,
/// and its token in that mode.
pub fn env_for(settings: &Settings, api_key: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let key = api_key.trim();
    if settings.auth == Auth::Token && !key.is_empty() {
        env.push(("CODEX_ACCESS_TOKEN".into(), key.into()));
    }
    let config_dir = settings.config_dir.trim();
    if !config_dir.is_empty() {
        env.push((
            "CODEX_HOME".into(),
            expand_home(config_dir).to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Whether a run signs in with the API key of the settings: one is set.
fn signs_in_with_key(settings: &Settings, api_key: &str) -> bool {
    settings.auth == Auth::ApiKey && !api_key.trim().is_empty()
}

/// Whether Rovik's permissions decide on Codex's own tools.
fn follows_rovik(settings: &Settings) -> bool {
    settings.tools == ToolsMode::Own && settings.permission_mode == PermissionMode::Rovik
}

/// The effort as Codex takes it: the levels every model it lists has.
fn effort_arg(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None | ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// The model the thread is told: none for Codex's default.
fn model_of(turn: &Turn) -> Option<String> {
    let model = turn.model.trim();
    (!model.is_empty() && model != "default").then(|| model.to_string())
}

/// What a run lets Codex do, as its thread is set: when it asks
/// (`approvalPolicy`), what its sandbox lets through, and who reviews
/// what it asks, when not the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Policy {
    approval: &'static str,
    sandbox: &'static str,
    reviewer: Option<&'static str>,
}

/// A turn's policy. With Rovik's tools, and in a one-off request, Codex's
/// own tools have nothing to do: read-only, never asking. In Plan mode its
/// sandbox keeps everything as it is, and nothing is asked (Codex refuses
/// what would need it). Under Rovik's permissions it asks before every
/// command and patch, Rovik's hooks answering, and runs what Rovik allows
/// without a sandbox of its own, as Claude Code runs its commands.
fn policy(settings: &Settings, turn: &Turn) -> Policy {
    let policy = |approval, sandbox| Policy {
        approval,
        sandbox,
        reviewer: None,
    };
    if turn.one_shot || settings.tools == ToolsMode::Rovik || turn.plan_mode {
        return policy("never", "read-only");
    }
    match settings.permission_mode {
        PermissionMode::Rovik => policy("untrusted", "danger-full-access"),
        PermissionMode::Auto => Policy {
            reviewer: Some("auto_review"),
            ..policy("on-request", "workspace-write")
        },
        PermissionMode::Workspace => policy("never", "workspace-write"),
        PermissionMode::ReadOnly => policy("never", "read-only"),
        PermissionMode::FullAccess => policy("never", "danger-full-access"),
    }
}

/// An MCP server of the run's, as Codex's settings name it: its name, its
/// address, the variable its token is in, and whether the model sees its
/// tools.
struct Served<'a> {
    name: &'a str,
    server: &'a McpServer,
    token_var: &'a str,
    hidden: bool,
}

/// The arguments of the app server: Rovik's settings for the run, then the
/// connector's own (`extra_args`), which win.
fn args_for(
    settings: &Settings,
    turn: &Turn,
    servers: &[Served<'_>],
    hooks: &[Event],
    api_key: &str,
) -> Vec<String> {
    let mut args = vec!["app-server".to_string()];
    let mut set = |key: &str, value: Value| args.extend(config(key, &value));
    // The key goes in over the input (`account/login/start`), and stays in
    // the server's memory: never written to the user's login.
    if signs_in_with_key(settings, api_key) {
        set("cli_auth_credentials_store", json!("ephemeral"));
    }
    for served in servers {
        let mut server = json!({
            "url": served.server.url(),
            "bearer_token_env_var": served.token_var,
            // A call waits for Rovik's answer: a day, for an approval the
            // user gives late.
            "tool_timeout_sec": 86_400.0,
            "default_tools_approval_mode": "approve",
            "required": true,
        });
        if served.hidden {
            server["omit_tools_from"] = json!(["code_mode", "deferred", "direct"]);
        }
        set(&format!("mcp_servers.{}", served.name), server);
    }
    let own_tools_off = turn.one_shot || settings.tools == ToolsMode::Rovik;
    if own_tools_off {
        // Codex's own tools and what its prompt says of them, and the
        // project's instructions, which Rovik's prompt carries.
        for feature in [
            "shell_tool",
            "unified_exec",
            "view_image",
            "image_generation",
            "multi_agent",
            "goals",
            "sleep_tool",
            "apps",
            "plugins",
            "browser_use",
            "computer_use",
        ] {
            set(&format!("features.{feature}"), json!(false));
        }
        set("web_search", json!("disabled"));
        set("include_permissions_instructions", json!(false));
        set("include_apps_instructions", json!(false));
        set("project_doc_max_bytes", json!(0));
    }
    if settings.tools == ToolsMode::Rovik && !turn.one_shot {
        // Rovik's tools called as functions of their own, not from code.
        let namespace = format!("mcp__{}", super::mcp::SERVER_NAME);
        set(
            "features.code_mode",
            json!({ "enabled": false, "direct_only_tool_namespaces": [namespace] }),
        );
    }
    if !hooks.is_empty() {
        args.extend(hooks::args(hooks));
    }
    args.extend(split_args(&settings.extra_args));
    args
}

/// The hook events a turn's run has Rovik's hooks for: the checks around
/// its own tools in a chat's turn, Codex's asks under Rovik's permissions,
/// and with Rovik's tools, the refusal of its own.
fn hook_events(settings: &Settings, turn: &Turn) -> Vec<Event> {
    if turn.one_shot {
        return Vec::new();
    }
    if settings.tools == ToolsMode::Rovik {
        return vec![Event::PreToolUse];
    }
    if turn.commands.is_none() && turn.asks.is_none() {
        return Vec::new();
    }
    let mut events = vec![Event::PreToolUse];
    if follows_rovik(settings) && !turn.plan_mode && turn.asks.is_some() {
        events.push(Event::PermissionRequest);
    }
    events.push(Event::PostToolUse);
    events
}

/// The thread a session the chat keeps names, for the tools of the turn:
/// none when it was kept for the other tools (a new thread is started).
fn thread_of(session: Option<&str>, rovik_tools: bool) -> Option<String> {
    let session = session?;
    match session.strip_prefix(ROVIK_TOOLS_SESSION) {
        Some(thread) if rovik_tools => Some(thread.to_string()),
        None if !rovik_tools => Some(session.to_string()),
        _ => None,
    }
}

/// The session the chat keeps for a thread.
fn session_of(thread: &str, rovik_tools: bool) -> String {
    match rovik_tools {
        true => format!("{ROVIK_TOOLS_SESSION}{thread}"),
        false => thread.to_string(),
    }
}

/// The folder a run works in: the turn's, or the system's temporary one.
fn cwd_of(turn: &Turn) -> PathBuf {
    turn.cwd
        .clone()
        .filter(|c| c.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// A started app server, with the thread its turn runs in.
struct Thread {
    process: Process,
    rpc: Rpc,
    id: String,
    model: Option<String>,
    version: Option<String>,
}

/// Why a thread didn't start.
enum Halt {
    /// The thread to continue is gone: the caller starts a new one.
    Gone(Process),
    /// The run's error, to report.
    Failed(ChatError),
}

/// What a thread is started or continued with.
struct Opening<'a> {
    policy: Policy,
    /// The thread to continue; none starts one.
    resume: Option<&'a str>,
    /// Codex's system prompt replaced, and the project's part apart.
    base: Option<String>,
    developer: Option<String>,
    /// The hooks to trust (their events), when the run has them.
    hooks: &'a [Event],
    /// Nothing kept: a one-off request.
    ephemeral: bool,
    /// Plan mode's collaboration modes, an experimental part of the API.
    experimental: bool,
}

/// Starts the app server's conversation: its handshake, the key it signs
/// in with, its hooks trusted, and the thread started or continued.
async fn open(
    mut process: Process,
    turn: &Turn,
    settings: &Settings,
    opening: Opening<'_>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Thread, Halt> {
    let Some(input) = process.input() else {
        return Err(Halt::Failed(ChatError::new(
            crate::chat_request::ErrorKind::Provider,
            "Codex's input couldn't be written.",
        )));
    };
    let rpc = Rpc::new(input);
    let within = turn.first_byte_timeout.max(MIN_FIRST_LINE);
    let answerer = rpc.clone();
    let mut other = move |message: Message| {
        if let Message::Request { id, method, .. } = message {
            decline(&answerer, &id, &method);
        }
    };
    macro_rules! ask {
        ($method:expr, $params:expr) => {
            match call(
                &mut process,
                &rpc,
                $method,
                $params,
                within,
                cancel,
                &mut other,
            )
            .await
            {
                Ok(result) => Ok(result),
                Err(Unanswered::Gone(stop)) => {
                    let (code, stderr) = process.finish().await;
                    let kind = ConnectorKind::Codex;
                    return Err(Halt::Failed(harness::ended_error(
                        kind, &process, stop, code, &stderr, false,
                    )));
                }
                Err(Unanswered::Refused(error)) => Err(error),
            }
        };
    }
    let refused = |what: &str, error: app_server::RpcError| {
        Halt::Failed(super::harness_error(
            ConnectorKind::Codex,
            format!("{what}: {}", error.message),
            None,
        ))
    };
    let hello = json!({
        "clientInfo": { "name": "rovik", "title": "Rovik IDE", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "experimentalApi": opening.experimental },
    });
    ask!("initialize", hello).map_err(|e| refused("Codex refused to start", e))?;
    rpc.notify("initialized");
    if signs_in_with_key(settings, &turn.api_key) {
        let login = json!({ "type": "apiKey", "apiKey": turn.api_key.trim() });
        ask!("account/login/start", login)
            .map_err(|e| refused("Codex didn't take the API key", e))?;
    }
    let cwd = cwd_of(turn);
    let mut config = json!({});
    if !opening.hooks.is_empty() {
        let listed = ask!("hooks/list", json!({ "cwds": [cwd] }))
            .map_err(|e| refused("Codex couldn't list its hooks", e))?;
        config = hooks::trust(&listed, opening.hooks)
            .map_err(|why| Halt::Failed(super::harness_error(ConnectorKind::Codex, why, None)))?;
    }
    let mut params = json!({
        "cwd": cwd,
        "approvalPolicy": opening.policy.approval,
        "sandbox": opening.policy.sandbox,
        "config": config,
    });
    if let Some(reviewer) = opening.policy.reviewer {
        params["approvalsReviewer"] = json!(reviewer);
    }
    if let Some(model) = model_of(turn) {
        params["model"] = json!(model);
    }
    if let Some(base) = opening.base {
        params["baseInstructions"] = json!(base);
    }
    if let Some(developer) = opening.developer {
        params["developerInstructions"] = json!(developer);
    }
    let started = match opening.resume {
        Some(thread) => {
            params["threadId"] = json!(thread);
            params["excludeTurns"] = json!(true);
            match ask!("thread/resume", params) {
                Ok(started) => started,
                Err(error) if is_gone(&error.message) => return Err(Halt::Gone(process)),
                Err(error) => {
                    return Err(refused("Codex couldn't continue the chat's thread", error))
                }
            }
        }
        None => {
            if opening.ephemeral {
                params["ephemeral"] = json!(true);
            }
            ask!("thread/start", params).map_err(|e| refused("Codex couldn't start a thread", e))?
        }
    };
    let field = |key: &str| {
        started
            .pointer(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(id) = field("/thread/id") else {
        return Err(Halt::Failed(super::harness_error(
            ConnectorKind::Codex,
            "Codex started a thread without naming it.",
            None,
        )));
    };
    Ok(Thread {
        model: field("/model").or_else(|| field("/thread/model")),
        version: field("/thread/cliVersion"),
        process,
        rpc,
        id,
    })
}

/// Whether `thread/resume` said the thread isn't there (another computer,
/// a cleared folder): the chat goes on in a new one.
fn is_gone(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("no rollout found")
        || lower.contains("invalid session id")
        || lower.contains("thread not found")
}

/// A turn's message as Codex takes it: text, or one of the commands the
/// chat's picker offers (`compact`, `review`, a skill), picked as
/// `/name arguments`.
#[derive(Debug, Clone, PartialEq)]
enum Ask {
    Text(String),
    Compact,
    Review(String),
    Skill {
        name: String,
        path: String,
        args: String,
    },
}

/// The command a picked message names (it starts with `/`; a typed one
/// starts with a space, `connectors::prompt_for`): its name and the rest.
fn picked(prompt: &str) -> Option<(&str, &str)> {
    let command = prompt.strip_prefix('/')?;
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    (!name.is_empty()).then_some((name, args.trim()))
}

/// The request a turn's prompt makes, its skill found in `skills` (the
/// answer to `skills/list`).
fn ask_of(prompt: &str, skills: &[Value]) -> Ask {
    let Some((name, args)) = picked(prompt) else {
        return Ask::Text(prompt.to_string());
    };
    match name {
        "compact" => Ask::Compact,
        "review" => Ask::Review(args.to_string()),
        _ => skills
            .iter()
            .find(|s| s.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|s| s.get("path").and_then(Value::as_str))
            .map(|path| Ask::Skill {
                name: name.to_string(),
                path: path.to_string(),
                args: args.to_string(),
            })
            .unwrap_or_else(|| Ask::Text(prompt.to_string())),
    }
}

/// The enabled skills `skills/list` answered with, for `cwd`.
fn skills_of(listed: &Value) -> Vec<Value> {
    listed
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("skills").and_then(Value::as_array))
        .flatten()
        .filter(|s| s.get("enabled").and_then(Value::as_bool) != Some(false))
        .cloned()
        .collect()
}

/// Codex's own commands the picker offers, besides its skills.
fn builtin_commands() -> Vec<ConnectorCommand> {
    let command = |name: &str, description: &str, hint: Option<&str>| ConnectorCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: hint.map(str::to_string),
        aliases: Vec::new(),
        builtin: true,
    };
    vec![
        command(
            "compact",
            "Summarize the conversation so far, to free up its context",
            None,
        ),
        command(
            "review",
            "Review the uncommitted changes, or what you name, and report what's wrong",
            Some("[what to review]"),
        ),
    ]
}

/// The picker's commands: Codex's own, then the skills it finds in the
/// project and the user's folders.
fn commands_from(listed: &Value) -> Vec<ConnectorCommand> {
    let mut commands = builtin_commands();
    for skill in skills_of(listed) {
        let text = |key: &str| {
            skill
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        let name = text("name");
        if name.is_empty() || commands.iter().any(|c| c.name == name) {
            continue;
        }
        let short = text("shortDescription");
        commands.push(ConnectorCommand {
            name,
            description: if short.is_empty() {
                text("description")
            } else {
                short
            },
            argument_hint: None,
            aliases: Vec::new(),
            builtin: false,
        });
    }
    commands
}

/// The request that runs `ask` in `thread`, its method and parameters.
fn request_for(ask: &Ask, thread: &str, turn: &Turn, model: Option<&str>) -> (&'static str, Value) {
    let text = |text: &str| json!({ "type": "text", "text": text, "text_elements": [] });
    let input = match ask {
        Ask::Compact => return ("thread/compact/start", json!({ "threadId": thread })),
        Ask::Review(what) => {
            let target = match what.is_empty() {
                true => json!({ "type": "uncommittedChanges" }),
                false => json!({ "type": "custom", "instructions": what }),
            };
            return (
                "review/start",
                json!({ "threadId": thread, "target": target }),
            );
        }
        Ask::Text(prompt) => vec![text(prompt)],
        Ask::Skill { name, path, args } => {
            let said = match args.is_empty() {
                true => format!("${name}"),
                false => format!("${name} {args}"),
            };
            vec![
                json!({ "type": "skill", "name": name, "path": path }),
                text(&said),
            ]
        }
    };
    let mut params = json!({
        "threadId": thread,
        "input": input,
        // Its reasoning summarized, for the chat to show; a one-off
        // request's isn't shown.
        "summary": if turn.one_shot { "none" } else { "auto" },
    });
    let effort = turn.effort.map(effort_arg);
    if let Some(effort) = effort {
        params["effort"] = json!(effort);
    }
    if turn.plan_mode && !turn.one_shot {
        params["collaborationMode"] = json!({
            "mode": "plan",
            "settings": { "model": model.unwrap_or_default(), "reasoning_effort": effort, "developer_instructions": null },
        });
    }
    ("turn/start", params)
}

/// How a run ended.
enum Outcome {
    /// Its events said how (`Done` or `Error`).
    Done,
    /// The thread to continue is gone: the caller starts a new one.
    SessionGone,
}

/// The toolchains' script its shell reads before each command, on a cloud
/// host whose shell has none of its own (`BASH_ENV`): written again after
/// each command's hook (codex/hooks.rs).
fn env_file(cwd: &Path) -> Result<Option<TempFile>, String> {
    if crate::process::child_var("BASH_ENV").is_some_and(|v| !v.is_empty()) {
        return Ok(None);
    }
    let script = crate::toolchains::env_script(cwd);
    if script.is_empty() {
        return Ok(None);
    }
    TempFile::write("env", "sh", &script).map(Some)
}

/// Runs one turn with Codex's own tools (or a one-off request), reporting
/// it through `emit`.
async fn run_turn(
    turn: &Turn,
    settings: &Settings,
    thread: Option<&str>,
    prompt: &str,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<Outcome, String> {
    let program = resolve_program(&settings.program)?;
    let cwd = cwd_of(turn);
    let events = hook_events(settings, turn);
    // On a team server, the user's private network: Codex runs in it, and
    // the servers it starts take their ports there.
    let network = crate::overlay::confine::network::current();
    let (sender, mut calls) = mpsc::unbounded_channel::<McpCall>();
    let server = match events.is_empty() {
        true => None,
        false => Some(
            McpServer::start_sending(&[hook_tool()], HOOKS_INSTRUCTIONS, network.as_ref(), sender)
                .await
                .map_err(|e| format!("Couldn't start Rovik's hooks for Codex: {e}"))?,
        ),
    };
    let env_file = match server.is_some() {
        true => env_file(&cwd)?,
        false => None,
    };
    let served: Vec<Served<'_>> = server
        .iter()
        .map(|server| Served {
            name: hooks::SERVER,
            server,
            token_var: HOOKS_TOKEN,
            hidden: true,
        })
        .collect();
    let args = args_for(settings, turn, &served, &events, &turn.api_key);
    let mut env = env_for(settings, &turn.api_key);
    if let Some(server) = &server {
        env.push((HOOKS_TOKEN.into(), server.token().into()));
        env.extend(super::mcp::loopback_env());
    }
    if let Some(file) = &env_file {
        env.push((
            "BASH_ENV".into(),
            file.path().to_string_lossy().into_owned(),
        ));
    }
    let env_path = env_file.as_ref().map(|file| file.path().to_path_buf());
    let chat = turn.dialog_id.as_deref().unwrap_or_default();
    let process = Process::spawn_with_input(
        &program,
        &args,
        &env,
        Some(&cwd),
        network.as_deref().map(|network| (network, chat)),
        turn.first_byte_timeout.max(MIN_FIRST_LINE),
        env_file.into_iter().collect(),
    )?;
    let plan = turn.plan_mode && !turn.one_shot;
    let opening = Opening {
        policy: policy(settings, turn),
        resume: thread,
        base: turn
            .one_shot
            .then(|| turn.system_prompt.trim().to_string())
            .filter(|p| !p.is_empty()),
        developer: None,
        hooks: &events,
        ephemeral: turn.one_shot,
        experimental: plan,
    };
    let opened = match open(process, turn, settings, opening, cancel).await {
        Ok(opened) => opened,
        Err(Halt::Gone(mut process)) => {
            process.finish().await;
            return Ok(Outcome::SessionGone);
        }
        Err(Halt::Failed(error)) => {
            emit(ChatStreamEvent::Error { error });
            return Ok(Outcome::Done);
        }
    };
    let Thread {
        mut process,
        rpc,
        id,
        model,
        version,
    } = opened;
    let ledger = hooks::Shared::default();
    let mut parser = Parser::new(rpc.clone(), &id, &session_of(&id, false))
        .in_folder(cwd.clone())
        .running(model.clone(), version);
    if server.is_some() {
        parser = parser.with_ledger(ledger.clone());
    }
    if !turn.one_shot {
        parser.announce(emit);
    }
    // The skills a picked command may name.
    let skills = match picked(prompt) {
        Some(_) if !turn.one_shot => {
            let mut other = |message: Message| {
                if let Message::Request { id, method, .. } = message {
                    decline(&rpc, &id, &method);
                }
            };
            let listed = call(
                &mut process,
                &rpc,
                "skills/list",
                json!({ "cwds": [cwd] }),
                ASK_WITHIN,
                cancel,
                &mut other,
            )
            .await;
            match listed {
                Ok(listed) => skills_of(&listed),
                // The message goes as typed, which Codex reads as text; a
                // run that ended or was stopped ends as the turn does.
                Err(why) => {
                    ::log::warn!("codex: its skills couldn't be listed: {why:?}");
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    let ask = match turn.one_shot {
        true => Ask::Text(prompt.to_string()),
        false => ask_of(prompt, &skills),
    };
    let (method, params) = request_for(&ask, &id, turn, model.as_deref());
    parser.begin(rpc.request(method, params));
    let rovik_decides = (follows_rovik(settings) || plan) && !turn.one_shot;
    let hooks = Arc::new(Hooks {
        checks: turn.commands.clone().filter(|_| !turn.one_shot),
        asks: turn.asks.clone().filter(|_| rovik_decides),
        ask_commands: rovik_decides && !plan,
        ask_patches: rovik_decides,
        rovik_tools: false,
        root: cwd.clone(),
        env_file: env_path,
        ledger,
    });
    let mut on_call = |call: McpCall| match call.name == HOOK {
        true => hooks.answer(call),
        false => drop(call),
    };
    let calls = server.as_ref().map(|_| &mut calls);
    let stop = pump(&mut process, &mut parser, calls, &mut on_call, cancel, emit).await;
    conclude(&mut process, &parser, stop, emit).await;
    drop(server);
    Ok(Outcome::Done)
}

/// A run's turn ended: as its events said, or with the error its stderr or
/// the way it ended gives.
async fn conclude(
    process: &mut Process,
    parser: &Parser,
    stop: Stop,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) {
    let (code, stderr) = process.finish().await;
    if parser.finished {
        return;
    }
    let partial = parser.shown();
    let error = harness::ended_error(ConnectorKind::Codex, process, stop, code, &stderr, partial);
    emit(ChatStreamEvent::Error { error });
}

/// What the hooks server says of itself, for Codex.
const HOOKS_INSTRUCTIONS: &str = "Rovik IDE's hooks around Codex's tool calls.";

/// The hooks server's one tool, which Codex's hooks call (its model never
/// sees it).
fn hook_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": HOOK,
            "description": "Rovik IDE's answer to one of Codex's hook events.",
            "parameters": { "type": "object", "additionalProperties": true },
        },
    })
}

/// Asks a question of an app server that needs no thread or model turn:
/// its models, the skills it finds in `cwd`.
async fn ask_server(
    program: &str,
    settings: &Settings,
    env: &[(String, String)],
    cwd: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut args = vec!["app-server".to_string()];
    args.extend(split_args(&settings.extra_args));
    let mut process =
        Process::spawn_with_input(program, &args, env, Some(cwd), None, ASK_WITHIN, Vec::new())?;
    let Some(input) = process.input() else {
        return Err("Codex's input couldn't be written.".into());
    };
    let rpc = Rpc::new(input);
    let (_stop, mut cancel) = watch::channel(false);
    let answerer = rpc.clone();
    let mut other = move |message: Message| {
        if let Message::Request { id, method, .. } = message {
            decline(&answerer, &id, &method);
        }
    };
    let hello = json!({ "clientInfo": { "name": "rovik", "title": "Rovik IDE", "version": env!("CARGO_PKG_VERSION") } });
    let asked = async {
        call(
            &mut process,
            &rpc,
            "initialize",
            hello,
            ASK_WITHIN,
            &mut cancel,
            &mut other,
        )
        .await?;
        rpc.notify("initialized");
        call(
            &mut process,
            &rpc,
            method,
            params,
            ASK_WITHIN,
            &mut cancel,
            &mut other,
        )
        .await
    }
    .await;
    let answer = match asked {
        Ok(answer) => Ok(answer),
        Err(Unanswered::Refused(error)) => Err(format!("Codex: {}", error.message)),
        Err(Unanswered::Gone(Stop::Stalled)) => Err(format!(
            "Codex didn't answer in {} s.",
            ASK_WITHIN.as_secs()
        )),
        Err(Unanswered::Gone(_)) => {
            let (_, stderr) = process.finish().await;
            let why = stderr
                .lines()
                .last()
                .unwrap_or("it ended without answering")
                .to_string();
            return Err(format!("Codex: {why}"));
        }
    };
    process.finish().await;
    answer
}

/// The models `model/list` answered with, as the connector offers them:
/// those it shows, its default first.
fn models_of(listed: &Value) -> Vec<ConnectorModel> {
    let mut models: Vec<(bool, ConnectorModel)> = listed
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("model"))?
                .as_str()?
                .to_string();
            let text = |key: &str| m.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
            let description = match (text("displayName"), text("description")) {
                (Some(name), Some(about)) => Some(format!("{name}: {about}")),
                (name, about) => name.or(about).map(str::to_string),
            };
            let model = ConnectorModel {
                id,
                owned_by: Some("openai".into()),
                context_length: None,
                description,
            };
            Some((
                m.get("isDefault").and_then(Value::as_bool) == Some(true),
                model,
            ))
        })
        .collect();
    models.sort_by_key(|(default, _)| !default);
    models.into_iter().map(|(_, model)| model).collect()
}

/// What `codex login status` said (on stderr, exit 0 when signed in, 1
/// when not), as the connector's settings would see it. `Err` when it said
/// nothing usable, which isn't "signed out".
pub fn parse_auth_status(
    stdout: &str,
    stderr: &str,
    settings: &Settings,
    api_key: &str,
) -> Result<ConnectorAuth, String> {
    let said = [stderr, stdout]
        .iter()
        .flat_map(|t| t.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let method = if let Some(how) = said.strip_prefix("Logged in using ") {
        Some(match how.trim().to_lowercase().as_str() {
            "chatgpt" => "chatgpt".to_string(),
            "access token" => "access_token".to_string(),
            "personal access token" => "personal_access_token".to_string(),
            other => other.replace(' ', "_"),
        })
    } else if said.starts_with("API key configured") {
        Some("api_key".to_string())
    } else if said.starts_with("Not logged in") {
        None
    } else if let Some(why) = said.strip_prefix("Error checking login status:") {
        return Err(format!("`codex login status`: {}", why.trim()));
    } else if said.is_empty() {
        return Err("`codex login status` said nothing.".into());
    } else {
        return Err(format!("`codex login status`: {said}"));
    };
    let config_dir = match settings.config_dir.trim() {
        "" => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".codex")),
        dir => Some(expand_home(dir)),
    };
    let mut auth = ConnectorAuth {
        logged_in: method.is_some(),
        method,
        config_dir: config_dir.map(|d| d.to_string_lossy().into_owned()),
        detail: None,
    };
    let has_key = !api_key.trim().is_empty();
    let detail = match (settings.auth, has_key, auth.logged_in) {
        // The key goes to each run's app server; nothing of it is stored.
        (Auth::ApiKey, true, _) => {
            auth.logged_in = true;
            auth.method = Some("api_key".into());
            Some("The API key is taken as good until a request says otherwise.")
        }
        (Auth::Token, true, true) => {
            Some("The token is taken as good until a request says otherwise.")
        }
        (Auth::Token, true, false) => Some(
            "Codex doesn't take the token in the connector's settings: check that it's a Codex access token.",
        ),
        (Auth::Token, false, true) => Some("No token is set, so runs use this computer's login."),
        (Auth::ApiKey, false, true) => {
            Some("No API key is set, so runs use this computer's login.")
        }
        (Auth::Login, _, true) => None,
        (Auth::Login, _, false) => Some(
            "Not signed in: use Set up → Sign in (`codex login`), or choose a token or an API key.",
        ),
        (Auth::Token, false, false) => {
            Some("Not signed in: paste an access token in the connector's settings.")
        }
        (Auth::ApiKey, false, false) => {
            Some("Not signed in: set an OpenAI API key in the connector's settings.")
        }
    };
    auth.detail = detail.map(str::to_string);
    Ok(auth)
}

/// `codex login status`, with the turn's environment: the sign-in a chat's
/// run would have.
async fn check_auth(
    program: &str,
    settings: &Settings,
    env: &[(String, String)],
    api_key: &str,
) -> Result<ConnectorAuth, String> {
    let args = ["login", "status"].map(String::from);
    let out = harness::output(program, &args, env, "", AUTH_STATUS_WITHIN).await?;
    parse_auth_status(
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
        settings,
        api_key,
    )
}

/// The version npm's registry answered with: numbers and dots, as a
/// version is; anything else (a page of an error) isn't one.
fn latest_of(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let version = value.get("version")?.as_str()?.trim();
    let is_version = !version.is_empty()
        && version.len() <= 32
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    is_version.then(|| version.to_string())
}

/// Whether the sign-in page should show a code to enter, rather than
/// open in a browser here: a computer with no screen (a cloud host), where
/// the browser the user signs in with can't reach `codex login`'s local
/// callback.
fn headless() -> bool {
    cfg!(target_os = "linux")
        && ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .all(|key| crate::process::child_var(key).is_none_or(|v| v.is_empty()))
}

/// A small request, to see that the program is signed in and answers: the
/// model that answered, its tokens and how long it took.
async fn test_request(args: &ProbeRequestArgs) -> Result<ProbeRequest, String> {
    let turn = Turn {
        settings: args.settings.clone(),
        api_key: args.api_key.clone(),
        model: String::new(),
        system_prompt: String::new(),
        system_context: None,
        prompt: "Reply with the single word: pong".into(),
        cwd: None,
        isolated: false,
        plan_mode: false,
        session: None,
        recap: None,
        one_shot: true,
        effort: None,
        first_byte_timeout: TEST_WITHIN,
        dialog_id: None,
        tools: Vec::new(),
        answers: None,
        continue_reply: false,
        asks: None,
        commands: None,
        denied: Default::default(),
    };
    let settings = Settings::from_value(&args.settings);
    let started = Instant::now();
    let mut usage = None;
    let mut model = None;
    let mut failure = None;
    let mut record = |event: ChatStreamEvent| match event {
        ChatStreamEvent::Finished { usage: Some(u), .. } => usage = Some(u),
        ChatStreamEvent::HarnessSession { model: m, .. } => model = m,
        ChatStreamEvent::Error { error } => failure = Some(error.message),
        _ => {}
    };
    let (_stop, mut cancel) = watch::channel(false);
    let prompt = turn.prompt.clone();
    let run = run_turn(&turn, &settings, None, &prompt, &mut cancel, &mut record);
    tokio::time::timeout(TEST_WITHIN, run).await.map_err(|_| {
        format!(
            "The test request gave no answer in {} s.",
            TEST_WITHIN.as_secs()
        )
    })??;
    if let Some(why) = failure {
        return Err(format!("Codex answered with an error: {why}"));
    }
    let usage = usage.ok_or("The test request gave no answer.")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Ok(ProbeRequest {
        model: model
            .or_else(|| {
                usage
                    .pointer("/connector/model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default(),
        input_tokens: count("prompt_tokens"),
        output_tokens: count("completion_tokens"),
        cost_usd: None,
        ms: started.elapsed().as_millis() as u64,
    })
}

impl Connector for Codex {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Codex
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
                models: Vec::new(),
                request: None,
                notes: Vec::new(),
                auth: check_auth(&program, &settings, &env, &args.api_key)
                    .await
                    .ok(),
                // The newest version is the connector module's to add.
                update: None,
            };
            let cwd = std::env::temp_dir();
            match ask_server(&program, &settings, &env, &cwd, "model/list", json!({})).await {
                Ok(listed) => probe.models = models_of(&listed),
                Err(why) => probe
                    .notes
                    .push(format!("Codex's models couldn't be listed: {why}")),
            }
            match settings.auth {
                Auth::Login => probe.notes.push(
                    "Signs in as this computer's user: Set up → Sign in (`codex login`), or choose a token or an API key."
                        .into(),
                ),
                Auth::Token if args.api_key.trim().is_empty() => {
                    probe.notes.push("No access token yet.".into())
                }
                Auth::ApiKey if args.api_key.trim().is_empty() => {
                    probe.notes.push("No API key yet.".into())
                }
                _ => {}
            }
            if args.request {
                probe.request = Some(test_request(args).await?);
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

    /// `codex` on the PATH, or where an installer puts it.
    fn installed(&self) -> bool {
        resolve_program("").is_ok()
    }

    /// OpenAI's installer; `codex login` and `logout` with a run's
    /// settings folder. The login opens the browser where it runs; on a
    /// computer with no screen it prints a page and a code to enter there
    /// (`--device-auth`). A token or an API key isn't signed in with: it's
    /// pasted in the connector's settings.
    fn setup_command(
        &self,
        action: SetupAction,
        args: &ProbeRequestArgs,
    ) -> Result<Option<SetupCommand>, String> {
        let settings = Settings::from_value(&args.settings);
        let run = |verb: &str| -> Result<SetupCommand, String> {
            let program = resolve_program(&settings.program)?;
            Ok(SetupCommand::program(program, &[verb]).with_env(env_for(&settings, &args.api_key)))
        };
        Ok(Some(match action {
            SetupAction::Install => SetupCommand::shell(INSTALL),
            SetupAction::Login if settings.auth != Auth::Login => {
                return Err(match settings.auth {
                    Auth::Token => {
                        "This connector signs in with an access token: paste it in its settings."
                    }
                    _ => {
                        "This connector signs in with an OpenAI API key: paste it in its settings."
                    }
                }
                .into())
            }
            SetupAction::Login if headless() => {
                run("login")?.arg("--device-auth").with_device_code()
            }
            SetupAction::Login => run("login")?,
            SetupAction::Logout => run("logout")?,
            // Its own updater: the installer's, or the package manager's.
            SetupAction::Update => run("update")?,
        }))
    }

    /// `skills/list` of an app server in `cwd`: Codex's own commands and
    /// the skills it finds, with no turn of the model.
    fn commands<'a>(
        &'a self,
        args: &'a ProbeRequestArgs,
        cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Vec<ConnectorCommand>, String>> {
        Box::pin(async move {
            let settings = Settings::from_value(&args.settings);
            let program = resolve_program(&settings.program)?;
            let env = env_for(&settings, &args.api_key);
            let params = json!({ "cwds": [cwd] });
            let listed = ask_server(&program, &settings, &env, cwd, "skills/list", params).await?;
            Ok(commands_from(&listed))
        })
    }

    /// Its collaboration mode `plan`, in a read-only sandbox: its plan the
    /// reply.
    fn plan_mode(&self) -> bool {
        true
    }

    fn latest_version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async {
            let response = crate::net::client().await.get(LATEST).send().await.ok()?;
            latest_of(&response.error_for_status().ok()?.text().await.ok()?)
        })
    }

    fn end_live(&self, dialog_id: &str) {
        rovik_tools::LIVE.end(dialog_id);
    }

    fn end_all(&self) {
        rovik_tools::LIVE.end_all();
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
                return rovik_tools::run(turn, settings, cancel, on_event).await;
            }
            // A session kept for Rovik's tools isn't continued with Codex's
            // own: a new thread, told what the chat was about.
            let thread = thread_of(turn.session.as_deref(), false);
            let fresh_prompt = turn.recap.clone().unwrap_or_else(|| turn.prompt.clone());
            let prompt = match (&turn.session, &thread) {
                (Some(_), None) => fresh_prompt.clone(),
                _ => turn.prompt.clone(),
            };
            match run_turn(
                &turn,
                &settings,
                thread.as_deref(),
                &prompt,
                cancel,
                on_event,
            )
            .await?
            {
                Outcome::Done => Ok(()),
                Outcome::SessionGone => {
                    run_turn(&turn, &settings, None, &fresh_prompt, cancel, on_event)
                        .await
                        .map(|_| ())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;

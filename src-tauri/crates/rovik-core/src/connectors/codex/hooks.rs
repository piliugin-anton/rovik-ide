//! Rovik's hooks around Codex's own tools (codex/mod.rs). Codex runs a
//! hook as a call of a tool of an MCP server's (an `mcp_tool` hook):
//! Rovik's hooks server for the turn (connectors/mcp.rs), which the model
//! doesn't see, gets each event as a call of its `hook` tool, and the
//! answer is the hook's output, in the schema Claude Code's hooks have.
//!
//! - Before each call (`PreToolUse`): a command of its shell, the files a
//!   patch changes and an image it looks at go to the run's checks
//!   (`CommandCheck`): what would read or change one of the project's
//!   restricted files is refused, and what a command needs is made ready.
//!   Under Rovik's permissions, a command, and a patch file by file as
//!   Rovik's own `write` of it, then go to Rovik's permissions and
//!   approvals (`ToolAsk`): refused with why, or allowed; a patch of which
//!   the user accepted some changes is applied as accepted.
//! - When Codex would ask before a call runs (`PermissionRequest`, under
//!   Rovik's permissions): a command or patch Rovik allowed before it was
//!   to run is allowed; a call of another MCP server's tool is put to
//!   Rovik's permissions; anything else is refused.
//! - After a command (`PostToolUse`): one that failed goes to the run,
//!   which makes ready what it didn't find and tells the model.
//!
//! Codex lets a call run when its hook fails, so Rovik doesn't rely on
//! that alone: under Rovik's permissions Codex asks before every command
//! and patch (`untrusted`), and only what Rovik allowed passes; and a turn
//! whose command or patch starts unchecked is stopped (`Ledger::checked`,
//! codex/events.rs).
//!
//! Hooks on the command line are the session's, which Codex runs once
//! they're trusted: the run lists them (`hooks/list`) and trusts its own
//! for the thread it starts ([`trust`]).

use super::app_server::config;
use super::patch::{self, Hunk, Patch};
use crate::connectors::mcp::{McpCall, McpReply, HOOK, TOOL_PREFIX};
use crate::connectors::steps::relative_path;
use crate::connectors::{
    hook_answer, CommandAnswer, CommandCheck, CommandChecks, HarnessCall, HarnessStep, ToolAnswer,
    ToolAsk, ToolAsks, UNCHECKED,
};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::oneshot;

/// The hooks server's name in Codex's configuration.
pub(super) const SERVER: &str = "rovik_hooks";
/// How long Codex waits on a hook: a day, for an approval the user gives
/// late, or a toolchain that takes minutes to install.
const TIMEOUT_SECS: u64 = 86_400;
/// How long the hook after a command waits for the command's end to be
/// read (its exit code is in Codex's event, which may come just after).
#[cfg(not(any(test, feature = "testing")))]
const EXIT_WITHIN: Duration = Duration::from_secs(10);
#[cfg(any(test, feature = "testing"))]
const EXIT_WITHIN: Duration = Duration::from_millis(200);

/// Why a call Rovik didn't see before it was to run doesn't run.
const NOT_SEEN: &str = "Error: Rovik IDE didn't see this call before it was to run, so it can't allow it. Try it again.";
/// Why a request Rovik has no rule for is refused.
const NOT_ALLOWED: &str = "Error: Rovik IDE decides what Codex may do in this project, and this isn't something it allows: do without it, or tell the user what you need.";
/// Why Codex's own tools are refused in Rovik's tools mode.
const ROVIK_TOOLS_ONLY: &str =
    "Error: in Rovik IDE your tools are Rovik's (`mcp__rovik__…`): use those.";

/// An event Rovik's hooks are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::PreToolUse => "PreToolUse",
            Event::PermissionRequest => "PermissionRequest",
            Event::PostToolUse => "PostToolUse",
        }
    }

    /// The event as `hooks/list` names it.
    fn listed(self) -> &'static str {
        match self {
            Event::PreToolUse => "preToolUse",
            Event::PermissionRequest => "permissionRequest",
            Event::PostToolUse => "postToolUse",
        }
    }

    /// The hook's call: the event's fields Rovik reads, which Codex puts
    /// in for each `${field}`, its JSON as it is. A field the event
    /// doesn't have would fail the hook: `PermissionRequest` names no
    /// tool use.
    fn input(self) -> Value {
        let mut input = json!({
            "hook_event_name": "${hook_event_name}",
            "tool_name": "${tool_name}",
            "tool_input": "${tool_input}",
            "cwd": "${cwd}",
        });
        if self != Event::PermissionRequest {
            input["tool_use_id"] = json!("${tool_use_id}");
        }
        if self == Event::PostToolUse {
            input["tool_response"] = json!("${tool_response}");
        }
        input
    }
}

/// The command line's settings for Rovik's hooks on `events`: hooks on,
/// and for each event one hook, for every tool (Codex's matchers have no
/// "all but"), calling the hooks server.
pub(super) fn args(events: &[Event]) -> Vec<String> {
    let mut args: Vec<String> = config("features.hooks", &json!(true)).into();
    for event in events {
        let hook = json!([{
            "hooks": [{
                "type": "mcp_tool",
                "server": SERVER,
                "tool": HOOK,
                "timeout": TIMEOUT_SECS,
                "input": event.input(),
            }],
        }]);
        args.extend(config(&format!("hooks.{}", event.name()), &hook));
    }
    args
}

/// The thread's settings that trust Rovik's hooks, from what `hooks/list`
/// answered: each hook's key and the hash of what it is now. An error when
/// one of `events` isn't among them, or is off (a Codex too old for them,
/// settings that turn hooks off): its tool calls would go unchecked.
pub(super) fn trust(listed: &Value, events: &[Event]) -> Result<Value, String> {
    let ours: Vec<&Value> = listed
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter(|hook| hook.get("server").and_then(Value::as_str) == Some(SERVER))
        .collect();
    let mut state = Map::new();
    for event in events {
        let hook = ours
            .iter()
            .find(|hook| hook.get("eventName").and_then(Value::as_str) == Some(event.listed()))
            .filter(|hook| hook.get("enabled").and_then(Value::as_bool) != Some(false));
        let found = hook.and_then(|hook| {
            let key = hook.get("key")?.as_str()?;
            let hash = hook.get("currentHash")?.as_str()?;
            Some((key, hash))
        });
        let Some((key, hash)) = found else {
            let said = listed
                .pointer("/data/0/errors")
                .or_else(|| listed.pointer("/data/0/warnings"))
                .and_then(Value::as_array)
                .and_then(|said| said.first())
                .map(|said| format!(" (Codex said: {said})"))
                .unwrap_or_default();
            return Err(format!(
                "Codex didn't take Rovik IDE's {} hook{said}, which checks its tool calls against the project's restricted files and permissions. Update Codex, and check that its settings don't turn hooks off.",
                event.name()
            ));
        };
        state.insert(key.to_string(), json!({ "trusted_hash": hash }));
    }
    Ok(json!({ "hooks.state": state }))
}

/// What a run's hooks and its parser share: which calls the hooks saw,
/// what they allowed, how commands ended, and what they refused before it
/// could start.
#[derive(Default)]
pub(super) struct Ledger {
    /// The tool uses Rovik's `PreToolUse` hook was called for.
    checked: HashSet<String>,
    /// What Rovik allowed before it was to run: tool, and its command
    /// (a patch's text) as it runs.
    allowed: Vec<(String, String)>,
    /// Commands that ended, by tool use: their exit code.
    ended: HashMap<String, Option<i32>>,
    /// Hooks after a command waiting for it to have ended.
    waiting: HashMap<String, Vec<oneshot::Sender<Option<i32>>>>,
    /// Calls refused before they started, as the chat shows them: they
    /// have no event of Codex's of their own.
    refused: Vec<HarnessStep>,
}

/// The ledger, shared by a run's hooks and its parser.
pub(super) type Shared = Arc<Mutex<Ledger>>;

/// The ledger, locked.
pub(super) fn lock(shared: &Shared) -> MutexGuard<'_, Ledger> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ledger {
    /// Whether Rovik's hook saw the call `id` before it was to run.
    pub(super) fn checked(&self, id: &str) -> bool {
        self.checked.contains(id)
    }

    /// The command `id` ended with `code` (Codex's event of its end).
    pub(super) fn ended(&mut self, id: &str, code: Option<i32>) {
        for waiter in self.waiting.remove(id).unwrap_or_default() {
            let _ = waiter.send(code);
        }
        self.ended.insert(id.to_string(), code);
    }

    /// Keeps the step of a call refused before it started, to show.
    pub(super) fn refuse(&mut self, step: HarnessStep) {
        self.refused.push(step);
    }

    /// The steps of the calls refused since the last time, to show.
    pub(super) fn take_refused(&mut self) -> Vec<HarnessStep> {
        std::mem::take(&mut self.refused)
    }
}

/// What answers a run's hooks: the run's checks and asks, how much of
/// Codex's own Rovik decides on, and the project's folder.
pub(super) struct Hooks {
    pub(super) checks: Option<CommandChecks>,
    pub(super) asks: Option<ToolAsks>,
    /// Rovik's permissions decide on its commands (not in Plan mode, where
    /// its read-only sandbox keeps them from changing anything).
    pub(super) ask_commands: bool,
    /// They decide on its patches (in Plan mode too, which refuses them).
    pub(super) ask_patches: bool,
    /// Rovik's tools mode: its own tools are refused, Rovik's pass.
    pub(super) rovik_tools: bool,
    /// The folder the run works in.
    pub(super) root: PathBuf,
    /// Its shell's `BASH_ENV` (a cloud host's toolchains), written again
    /// after each command's hook so the next command finds what was
    /// installed meanwhile.
    pub(super) env_file: Option<PathBuf>,
    pub(super) ledger: Shared,
}

/// One file of a patch, as Rovik asks about it.
struct PatchFile {
    /// Its hunk in the patch.
    hunk: usize,
    /// The path as the patch names it, and relative to the project.
    path: String,
    shown: String,
    kind: &'static str,
    /// Its content now, and the content the patch gives it, when known.
    current: Option<String>,
    new: Option<String>,
}

impl Hooks {
    /// Answers one call of the hooks server, the work in a task of its own:
    /// an approval can take as long as the user does.
    pub(super) fn answer(self: &Arc<Self>, call: McpCall) {
        let event = call
            .arguments
            .get("hook_event_name")
            .and_then(Value::as_str);
        if event == Some(Event::PreToolUse.name()) {
            if let Some(id) = call.arguments.get("tool_use_id").and_then(Value::as_str) {
                lock(&self.ledger).checked.insert(id.to_string());
            }
        }
        let hooks = self.clone();
        tokio::spawn(async move {
            let event = &call.arguments;
            let answer = match event.get("hook_event_name").and_then(Value::as_str) {
                Some("PreToolUse") => hooks.before(event).await,
                Some("PermissionRequest") => hooks.permission(event).await,
                Some("PostToolUse") => hooks.after(event).await,
                _ => json!({}),
            };
            hooks.write_env_file();
            let _ = call.reply.send(McpReply::text(answer.to_string()));
        });
    }

    /// `PreToolUse`: the call checked, and asked about where Rovik decides.
    async fn before(&self, event: &Value) -> Value {
        let tool = text(event, "tool_name");
        let id = text(event, "tool_use_id");
        let input = event.get("tool_input").cloned().unwrap_or(Value::Null);
        if self.rovik_tools {
            return match tool.starts_with(TOOL_PREFIX) {
                true => json!({}),
                false => deny(Event::PreToolUse, ROVIK_TOOLS_ONLY),
            };
        }
        match tool {
            "Bash" => self.before_command(id, text(&input, "command")).await,
            "apply_patch" => self.before_patch(id, text(&input, "command")).await,
            "view_image" => {
                let args = json!({ "path": text(&input, "path") });
                match self.check(id, tool_call("read", args), None).await {
                    CommandAnswer::Deny(reason) => deny(Event::PreToolUse, &reason),
                    CommandAnswer::Note(_) => json!({}),
                }
            }
            _ => json!({}),
        }
    }

    async fn before_command(&self, id: &str, command: &str) -> Value {
        let refused = |reason: &str| {
            let step = refused_step(id, "shell", json!({ "command": command }), reason);
            lock(&self.ledger).refuse(step);
            deny(Event::PreToolUse, reason)
        };
        let call = HarnessCall::Command(command.to_string());
        let note = match self.check(id, call, None).await {
            CommandAnswer::Deny(reason) => return refused(&reason),
            CommandAnswer::Note(note) => note,
        };
        let mut runs = command.to_string();
        if self.ask_commands {
            match self.ask(id, "bash", json!({ "command": command })).await {
                ToolAnswer::Deny { reason } => return refused(&reason),
                ToolAnswer::Allow { arguments } => {
                    if let Some(changed) = arguments
                        .as_deref()
                        .and_then(|a| serde_json::from_str::<Value>(a).ok())
                        .and_then(|a| a.get("command").and_then(Value::as_str).map(str::to_string))
                    {
                        runs = changed;
                    }
                }
            }
        }
        self.allow("Bash", &runs);
        let changed = (runs != command).then(|| json!({ "command": runs }));
        allow(changed, note)
    }

    async fn before_patch(&self, id: &str, text: &str) -> Value {
        let patch = match patch::parse(text) {
            Ok(patch) => patch,
            Err(why) => {
                let reason = format!(
                    "Error: Rovik IDE couldn't read this patch ({why}), so it didn't apply. Send it again in apply_patch's format."
                );
                let input = json!({ "patch": crate::connectors::trimmed_text(text, 400) });
                let step = refused_step(id, "apply_patch", input, &reason);
                lock(&self.ledger).refuse(step);
                return deny(Event::PreToolUse, &reason);
            }
        };
        let files = self.patch_files(&patch);
        let refused = |reason: &str| {
            let mut ledger = lock(&self.ledger);
            for (n, file) in files.iter().enumerate() {
                let input = json!({ "path": file.shown, "kind": file.kind });
                let step = refused_step(&edit_step_id(id, n), "edit", input, reason);
                ledger.refuse(step);
            }
            drop(ledger);
            deny(Event::PreToolUse, reason)
        };
        for file in &files {
            let args = match file.kind {
                "add" => json!({ "path": file.path, "content": file.new }),
                "update" => json!({ "path": file.path, "old_text": "" }),
                _ => json!({ "path": file.path, "content": "" }),
            };
            if let CommandAnswer::Deny(reason) =
                self.check(id, tool_call("write", args), None).await
            {
                return refused(&reason);
            }
        }
        let mut writes: Vec<(usize, String, String)> = Vec::new();
        if self.ask_patches {
            for (n, file) in files.iter().enumerate() {
                let asked = match &file.new {
                    Some(content) => json!({ "path": file.shown, "content": content }),
                    // A change whose result isn't known (a file that
                    // isn't there to change): asked as an edit of it.
                    None => json!({ "path": file.shown }),
                };
                let accepted = match self.ask(&edit_step_id(id, n), "write", asked).await {
                    ToolAnswer::Deny { reason } => return refused(&reason),
                    ToolAnswer::Allow { arguments } => arguments
                        .and_then(|a| serde_json::from_str::<Value>(&a).ok())
                        .and_then(|a| a.get("content").and_then(Value::as_str).map(str::to_string)),
                };
                if let Some(content) = accepted.filter(|c| Some(c) != file.new.as_ref()) {
                    writes.push((file.hunk, file.current.clone().unwrap_or_default(), content));
                }
            }
        }
        let runs = match writes.is_empty() {
            true => text.to_string(),
            false => patch.rewritten(&writes),
        };
        self.allow("apply_patch", &runs);
        let changed = (!writes.is_empty()).then(|| json!({ "command": runs }));
        allow(changed, None)
    }

    /// The files a patch changes, each with what it has now and will have.
    fn patch_files(&self, patch: &Patch) -> Vec<PatchFile> {
        let mut files = Vec::new();
        for (hunk, change) in patch.hunks.iter().enumerate() {
            let file = |path: &str, kind, current: Option<String>, new| PatchFile {
                hunk,
                path: path.to_string(),
                shown: relative_path(&self.resolve(path).to_string_lossy(), &self.root),
                kind,
                current,
                new,
            };
            let read = |path: &str| std::fs::read_to_string(self.resolve(path)).ok();
            match change {
                Hunk::Add { path, contents } => {
                    files.push(file(path, "add", None, Some(contents.clone())))
                }
                Hunk::Delete { path } => {
                    files.push(file(path, "delete", read(path), Some(String::new())))
                }
                Hunk::Update {
                    path,
                    move_path,
                    chunks,
                } => {
                    let current = read(path);
                    let new = current
                        .as_deref()
                        .and_then(|now| patch::new_contents(now, chunks).ok());
                    match move_path {
                        // A move: the new file, and the old one gone.
                        Some(to) => {
                            files.push(file(to, "add", current.clone(), new));
                            files.push(file(path, "delete", current, Some(String::new())));
                        }
                        None => files.push(file(path, "update", current, new)),
                    }
                }
            }
        }
        files
    }

    /// A path of the patch's, where it is: in the project unless absolute.
    fn resolve(&self, path: &str) -> PathBuf {
        match Path::new(path).is_absolute() {
            true => PathBuf::from(path),
            false => self.root.join(path),
        }
    }

    /// `PermissionRequest`: what Codex would ask about, answered.
    async fn permission(&self, event: &Value) -> Value {
        let tool = text(event, "tool_name");
        let input = event.get("tool_input").cloned().unwrap_or(Value::Null);
        match tool {
            "Bash" | "apply_patch" => {
                let command = text(&input, "command");
                let allowed = lock(&self.ledger)
                    .allowed
                    .iter()
                    .any(|(t, c)| t == tool && c == command);
                if allowed {
                    return decide(None);
                }
                if tool == "Bash" {
                    let id = format!("refused-{}", uuid::Uuid::new_v4().simple());
                    let input = json!({ "command": command });
                    let step = refused_step(&id, "shell", input, NOT_SEEN);
                    lock(&self.ledger).refuse(step);
                }
                decide(Some(NOT_SEEN.into()))
            }
            // Another MCP server's tool: as Rovik's other tools are asked.
            tool if tool.starts_with("mcp__") => {
                let id = uuid::Uuid::new_v4().to_string();
                match self.ask(&id, tool, input).await {
                    ToolAnswer::Allow { .. } => decide(None),
                    ToolAnswer::Deny { reason } => decide(Some(reason)),
                }
            }
            _ => decide(Some(NOT_ALLOWED.into())),
        }
    }

    /// `PostToolUse`: a command that failed, put to the run.
    async fn after(&self, event: &Value) -> Value {
        if text(event, "tool_name") != "Bash" {
            return json!({});
        }
        let id = text(event, "tool_use_id");
        let Some(code) = self.exit_code(id).await.filter(|code| *code != 0) else {
            return json!({});
        };
        let command = event
            .pointer("/tool_input/command")
            .and_then(Value::as_str)
            .unwrap_or("");
        let output = match event.get("tool_response") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let call = HarnessCall::Command(command.to_string());
        let failed = Some((Some(code), output));
        let said = self.check_after(id, call, failed).await;
        hook_answer(Event::PostToolUse.name(), said)
    }

    /// How the command `id` ended, once its end is read: its exit code
    /// (none when Codex's event has none, or it isn't read in time).
    async fn exit_code(&self, id: &str) -> Option<i32> {
        let waiting = {
            let mut ledger = lock(&self.ledger);
            if let Some(code) = ledger.ended.get(id) {
                return *code;
            }
            let (sender, receiver) = oneshot::channel();
            ledger
                .waiting
                .entry(id.to_string())
                .or_default()
                .push(sender);
            receiver
        };
        tokio::time::timeout(EXIT_WITHIN, waiting)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
    }

    /// The run's check of a call before it runs: refused when the run
    /// can't be asked (it ended), never let through unchecked.
    async fn check(&self, id: &str, call: HarnessCall, cwd: Option<String>) -> CommandAnswer {
        let Some(checks) = &self.checks else {
            return CommandAnswer::Note(None);
        };
        let (reply, said) = oneshot::channel();
        let check = CommandCheck {
            call_id: id.to_string(),
            call,
            cwd,
            failed: None,
            reply,
        };
        match checks.send(check) {
            Ok(()) => said
                .await
                .unwrap_or_else(|_| CommandAnswer::Deny(UNCHECKED.into())),
            Err(_) => CommandAnswer::Deny(UNCHECKED.into()),
        }
    }

    /// The run's word on a command that failed: nothing when it can't be
    /// asked.
    async fn check_after(
        &self,
        id: &str,
        call: HarnessCall,
        failed: Option<(Option<i32>, String)>,
    ) -> CommandAnswer {
        let Some(checks) = &self.checks else {
            return CommandAnswer::Note(None);
        };
        let (reply, said) = oneshot::channel();
        let check = CommandCheck {
            call_id: id.to_string(),
            call,
            cwd: None,
            failed,
            reply,
        };
        match checks.send(check) {
            Ok(()) => said.await.unwrap_or(CommandAnswer::Note(None)),
            Err(_) => CommandAnswer::Note(None),
        }
    }

    /// Rovik's permissions on a call, as Rovik's `tool` with `arguments`:
    /// refused when nobody can decide (the run ended).
    async fn ask(&self, id: &str, tool: &str, arguments: Value) -> ToolAnswer {
        let gone = || ToolAnswer::Deny {
            reason: "Rovik couldn't decide on this call: its run has ended.".into(),
        };
        let Some(asks) = &self.asks else {
            return gone();
        };
        let (reply, answer) = oneshot::channel();
        let ask = ToolAsk {
            call_id: id.to_string(),
            tool: tool.to_string(),
            arguments: arguments.to_string(),
            reply,
        };
        match asks.send(ask) {
            Ok(()) => answer.await.unwrap_or_else(|_| gone()),
            Err(_) => gone(),
        }
    }

    /// Remembers what Rovik allowed, for Codex's ask that follows.
    fn allow(&self, tool: &str, command: &str) {
        lock(&self.ledger)
            .allowed
            .push((tool.to_string(), command.to_string()));
    }

    /// Writes the toolchains' script again, while the turn's file is there.
    fn write_env_file(&self) {
        let Some(file) = self.env_file.as_ref().filter(|file| file.exists()) else {
            return;
        };
        let script = crate::toolchains::env_script(&self.root);
        if let Err(e) = crate::atomic_file::write_atomic_private(file, script.as_bytes()) {
            ::log::warn!("codex: its environment file {}: {e}", file.display());
        }
    }
}

/// A string field of an event's.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A call of Codex's file tools as the Rovik call that does the same.
fn tool_call(name: &str, args: Value) -> HarnessCall {
    HarnessCall::Tool {
        name: name.to_string(),
        args: args.as_object().cloned().unwrap_or_default(),
    }
}

/// The step id of a patch's `n`th file: its tool use's, and after the
/// first, numbered (`codex/events.rs` names its steps the same).
pub(super) fn edit_step_id(id: &str, n: usize) -> String {
    match n {
        0 => id.to_string(),
        n => format!("{id}#{n}"),
    }
}

/// A call refused before it started, as a step of the reply.
fn refused_step(id: &str, name: &str, input: Value, reason: &str) -> HarnessStep {
    HarnessStep {
        id: id.to_string(),
        name: name.to_string(),
        input,
        result: Some(crate::connectors::trimmed_text(reason, 1_200)),
        ok: Some(false),
        ..HarnessStep::default()
    }
}

/// `PreToolUse`'s refusal.
fn deny(event: Event, reason: &str) -> Value {
    hook_answer(event.name(), CommandAnswer::Deny(reason.to_string()))
}

/// `PreToolUse`'s answer that lets a call run: with the input it runs
/// with, when Rovik changed it, and what the model should be told.
fn allow(changed: Option<Value>, note: Option<String>) -> Value {
    let Some(input) = changed else {
        return hook_answer(Event::PreToolUse.name(), CommandAnswer::Note(note));
    };
    let mut output = json!({
        "hookEventName": Event::PreToolUse.name(),
        "permissionDecision": "allow",
        "updatedInput": input,
    });
    if let Some(note) = note {
        output["additionalContext"] = json!(note);
    }
    json!({ "hookSpecificOutput": output })
}

/// `PermissionRequest`'s decision: allowed, or refused with why.
fn decide(refused: Option<String>) -> Value {
    let decision = match refused {
        None => json!({ "behavior": "allow" }),
        Some(message) => json!({ "behavior": "deny", "message": message }),
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": Event::PermissionRequest.name(),
            "decision": decision,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn hooks_are_declared_for_every_tool_with_the_fields_each_event_has() {
        let args = args(&[Event::PreToolUse, Event::PermissionRequest]);
        assert_eq!(args[..2], ["-c", "features.hooks=true"]);
        assert_eq!(args[2], "-c");
        assert!(
            args[3].starts_with("hooks.PreToolUse=[{hooks=[{"),
            "{}",
            args[3]
        );
        for field in [
            r#"type="mcp_tool""#,
            r#"server="rovik_hooks""#,
            r#"tool="hook""#,
            "timeout=86400",
            r#"tool_use_id="${tool_use_id}""#,
            r#"tool_input="${tool_input}""#,
        ] {
            assert!(args[3].contains(field), "{field}: {}", args[3]);
        }
        assert!(args[5].starts_with("hooks.PermissionRequest="));
        // Its event names no tool use, which would fail the hook.
        assert!(!args[5].contains("tool_use_id"), "{}", args[5]);
        assert!(!args[3].contains("matcher"));
        assert!(Event::PostToolUse.input()["tool_response"] == "${tool_response}");
    }

    #[test]
    fn only_rovik_s_own_hooks_are_trusted_and_a_missing_one_is_an_error() {
        let listed = json!({ "data": [{ "hooks": [
            { "key": "/<session-flags>/config.toml:pre_tool_use:0:0", "eventName": "preToolUse",
              "server": SERVER, "currentHash": "sha256:aa", "enabled": true },
            { "key": "/home/u/.codex/config.toml:pre_tool_use:0:0", "eventName": "preToolUse",
              "server": "theirs", "currentHash": "sha256:bb", "enabled": true },
        ], "warnings": [], "errors": [] }] });
        assert_eq!(
            trust(&listed, &[Event::PreToolUse]).unwrap(),
            json!({ "hooks.state": {
                "/<session-flags>/config.toml:pre_tool_use:0:0": { "trusted_hash": "sha256:aa" },
            } })
        );
        let missing = trust(&listed, &[Event::PreToolUse, Event::PostToolUse]).unwrap_err();
        assert!(missing.contains("PostToolUse"), "{missing}");
        let off = json!({ "data": [{ "hooks": [
            { "key": "k", "eventName": "preToolUse", "server": SERVER, "currentHash": "h", "enabled": false },
        ], "errors": ["hooks are disabled by policy"] }] });
        let error = trust(&off, &[Event::PreToolUse]).unwrap_err();
        assert!(error.contains("disabled by policy"), "{error}");
    }

    /// A check the run was given: the call, and how it failed, when it did.
    type Checked = (HarnessCall, Option<(Option<i32>, String)>);

    /// Hooks with the run's checks and asks answered as `checks` and
    /// `asks` say, and what they were given.
    struct Run {
        hooks: Arc<Hooks>,
        checked: Arc<Mutex<Vec<Checked>>>,
        asked: Arc<Mutex<Vec<(String, String, Value)>>>,
    }

    fn run(
        root: &Path,
        ask: bool,
        check: impl Fn(&HarnessCall) -> CommandAnswer + Send + 'static,
        decide: impl Fn(&str, &Value) -> ToolAnswer + Send + 'static,
    ) -> Run {
        let (checks, mut checked_rx) = mpsc::unbounded_channel::<CommandCheck>();
        let (asks, mut asked_rx) = mpsc::unbounded_channel::<ToolAsk>();
        let checked = Arc::new(Mutex::new(Vec::new()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = checked.clone();
        tokio::spawn(async move {
            while let Some(c) = checked_rx.recv().await {
                seen.lock()
                    .unwrap()
                    .push((c.call.clone(), c.failed.clone()));
                let _ = c.reply.send(check(&c.call));
            }
        });
        let seen = asked.clone();
        tokio::spawn(async move {
            while let Some(a) = asked_rx.recv().await {
                let args: Value = serde_json::from_str(&a.arguments).unwrap();
                seen.lock()
                    .unwrap()
                    .push((a.call_id.clone(), a.tool.clone(), args.clone()));
                let _ = a.reply.send(decide(&a.tool, &args));
            }
        });
        let hooks = Arc::new(Hooks {
            checks: Some(checks),
            asks: Some(asks),
            ask_commands: ask,
            ask_patches: ask,
            rovik_tools: false,
            root: root.to_path_buf(),
            env_file: None,
            ledger: Shared::default(),
        });
        Run {
            hooks,
            checked,
            asked,
        }
    }

    async fn hook(hooks: &Arc<Hooks>, event: Value) -> Value {
        let (reply, answer) = oneshot::channel();
        hooks.answer(McpCall {
            tool_use_id: None,
            name: HOOK.into(),
            arguments: event,
            reply,
        });
        serde_json::from_str(&answer.await.unwrap().text).unwrap()
    }

    fn pre(tool: &str, input: Value, id: &str) -> Value {
        json!({ "hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input, "tool_use_id": id, "cwd": "/p" })
    }

    fn permission(tool: &str, input: Value) -> Value {
        json!({ "hook_event_name": "PermissionRequest", "tool_name": tool, "tool_input": input, "cwd": "/p" })
    }

    fn allow_all(_: &str, _: &Value) -> ToolAnswer {
        ToolAnswer::Allow { arguments: None }
    }

    #[tokio::test]
    async fn a_command_is_checked_then_asked_and_codex_s_ask_after_passes_only_what_rovik_allowed()
    {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            true,
            |_| {
                CommandAnswer::Note(Some(
                    "Rovik IDE, before this command ran: installed Node 22.".into(),
                ))
            },
            allow_all,
        );
        let answer = hook(
            &run.hooks,
            pre("Bash", json!({ "command": "npm test" }), "exec-1"),
        )
        .await;
        assert_eq!(
            answer["hookSpecificOutput"]["additionalContext"],
            "Rovik IDE, before this command ran: installed Node 22."
        );
        assert!(lock(&run.hooks.ledger).checked("exec-1"));
        assert_eq!(
            run.checked.lock().unwrap()[0].0,
            HarnessCall::Command("npm test".into())
        );
        let asked = run.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![(
                "exec-1".to_string(),
                "bash".to_string(),
                json!({ "command": "npm test" })
            )]
        );
        // Codex's ask before it runs: what was allowed passes, anything
        // else is refused, and shown as refused.
        let passes = hook(
            &run.hooks,
            permission("Bash", json!({ "command": "npm test" })),
        )
        .await;
        assert_eq!(
            passes["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        let unseen = hook(
            &run.hooks,
            permission("Bash", json!({ "command": "rm -rf /" })),
        )
        .await;
        assert_eq!(unseen["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            unseen["hookSpecificOutput"]["decision"]["message"],
            NOT_SEEN
        );
        let refused = lock(&run.hooks.ledger).take_refused();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].input["command"], "rm -rf /");
        assert_eq!(refused[0].ok, Some(false));
    }

    #[tokio::test]
    async fn a_command_rovik_refuses_never_runs_and_its_step_says_why() {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            true,
            |_| CommandAnswer::Note(None),
            |_, _| ToolAnswer::Deny {
                reason: "The user rejected this.".into(),
            },
        );
        let answer = hook(
            &run.hooks,
            pre("Bash", json!({ "command": "git push" }), "exec-2"),
        )
        .await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecisionReason"],
            "The user rejected this."
        );
        let step = lock(&run.hooks.ledger).take_refused().remove(0);
        assert_eq!((step.id.as_str(), step.name.as_str()), ("exec-2", "shell"));
        assert_eq!(step.result.as_deref(), Some("The user rejected this."));
        // Nor does Codex's ask after let it through.
        let after = hook(
            &run.hooks,
            permission("Bash", json!({ "command": "git push" })),
        )
        .await;
        assert_eq!(after["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }

    #[tokio::test]
    async fn a_restricted_file_is_refused_whoever_decides_and_unasked() {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            false,
            |_| CommandAnswer::Deny("Error: .env is a restricted file.".into()),
            allow_all,
        );
        let answer = hook(
            &run.hooks,
            pre("Bash", json!({ "command": "cat .env" }), "exec-3"),
        )
        .await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(run.asked.lock().unwrap().is_empty());
        let image = hook(
            &run.hooks,
            pre("view_image", json!({ "path": ".env" }), "v-1"),
        )
        .await;
        assert_eq!(image["hookSpecificOutput"]["permissionDecision"], "deny");
        let HarnessCall::Tool { name, args } = run.checked.lock().unwrap()[1].0.clone() else {
            panic!("a tool call");
        };
        assert_eq!(
            (name.as_str(), args["path"].as_str()),
            ("read", Some(".env"))
        );
    }

    #[tokio::test]
    async fn a_run_that_ended_leaves_nothing_unchecked() {
        let dir = crate::testing::temp_dir();
        let (checks, gone) = mpsc::unbounded_channel::<CommandCheck>();
        drop(gone);
        let hooks = Arc::new(Hooks {
            checks: Some(checks),
            asks: None,
            ask_commands: false,
            ask_patches: false,
            rovik_tools: false,
            root: dir.0.clone(),
            env_file: None,
            ledger: Shared::default(),
        });
        let answer = hook(&hooks, pre("Bash", json!({ "command": "ls" }), "exec-4")).await;
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecisionReason"],
            UNCHECKED
        );
    }

    #[tokio::test]
    async fn each_file_of_a_patch_is_asked_as_rovik_s_write_of_the_content_it_makes() {
        let dir = crate::testing::temp_dir();
        std::fs::write(dir.0.join("a.txt"), "one\ntwo\n").unwrap();
        let run = run(&dir.0, true, |_| CommandAnswer::Note(None), allow_all);
        let text = "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+uno\n*** Add File: b.txt\n+bee\n*** End Patch";
        let answer = hook(
            &run.hooks,
            pre("apply_patch", json!({ "command": text }), "exec-5"),
        )
        .await;
        assert_eq!(answer, json!({}), "allowed as it is");
        let asked = run.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![
                (
                    "exec-5".into(),
                    "write".into(),
                    json!({ "path": "a.txt", "content": "uno\ntwo\n" })
                ),
                (
                    "exec-5#1".into(),
                    "write".into(),
                    json!({ "path": "b.txt", "content": "bee\n" })
                ),
            ]
        );
        // The restricted files' check: a change of part of a file reads it.
        let checked = run.checked.lock().unwrap().clone();
        let HarnessCall::Tool { args, .. } = &checked[0].0 else {
            panic!("a tool call");
        };
        assert_eq!(args.get("old_text"), Some(&json!("")));
        let passes = hook(
            &run.hooks,
            permission("apply_patch", json!({ "command": text })),
        )
        .await;
        assert_eq!(
            passes["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
    }

    #[tokio::test]
    async fn a_patch_partly_accepted_is_applied_as_accepted() {
        let dir = crate::testing::temp_dir();
        std::fs::write(dir.0.join("a.txt"), "one\ntwo\n").unwrap();
        let run = run(
            &dir.0,
            true,
            |_| CommandAnswer::Note(None),
            |_, args| ToolAnswer::Allow {
                arguments: Some(
                    json!({ "path": args["path"], "content": "uno\ntwo\n" }).to_string(),
                ),
            },
        );
        let text =
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+uno\n-two\n+dos\n*** End Patch";
        let answer = hook(
            &run.hooks,
            pre("apply_patch", json!({ "command": text }), "exec-6"),
        )
        .await;
        let output = &answer["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "allow");
        let runs = output["updatedInput"]["command"].as_str().unwrap();
        let rewritten = patch::parse(runs).unwrap();
        let Hunk::Update { chunks, .. } = &rewritten.hunks[0] else {
            panic!("an update");
        };
        assert_eq!(
            patch::new_contents("one\ntwo\n", chunks).unwrap(),
            "uno\ntwo\n"
        );
        // Codex asks about the patch it will apply: the rewritten one.
        let passes = hook(
            &run.hooks,
            permission("apply_patch", json!({ "command": runs })),
        )
        .await;
        assert_eq!(
            passes["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        let original = hook(
            &run.hooks,
            permission("apply_patch", json!({ "command": text })),
        )
        .await;
        assert_eq!(
            original["hookSpecificOutput"]["decision"]["behavior"],
            "deny"
        );
    }

    #[tokio::test]
    async fn a_refused_file_refuses_the_whole_patch_each_file_shown_refused() {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            true,
            |_| CommandAnswer::Note(None),
            |_, args| match args["path"] == "secret.txt" {
                true => ToolAnswer::Deny {
                    reason: "Not that one.".into(),
                },
                false => ToolAnswer::Allow { arguments: None },
            },
        );
        let text = "*** Begin Patch\n*** Add File: ok.txt\n+fine\n*** Add File: secret.txt\n+no\n*** End Patch";
        let answer = hook(
            &run.hooks,
            pre("apply_patch", json!({ "command": text }), "exec-7"),
        )
        .await;
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecisionReason"],
            "Not that one."
        );
        let steps = lock(&run.hooks.ledger).take_refused();
        let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["exec-7", "exec-7#1"]);
        assert_eq!(steps[1].input["path"], "secret.txt");
    }

    #[tokio::test]
    async fn a_patch_rovik_can_t_read_doesn_t_apply() {
        let dir = crate::testing::temp_dir();
        let run = run(&dir.0, false, |_| CommandAnswer::Note(None), allow_all);
        let answer = hook(
            &run.hooks,
            pre("apply_patch", json!({ "command": "garbage" }), "exec-8"),
        )
        .await;
        let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("couldn't read this patch"), "{reason}");
    }

    #[tokio::test]
    async fn another_servers_tool_is_asked_by_its_name_and_other_requests_refused() {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            true,
            |_| CommandAnswer::Note(None),
            |tool, _| match tool {
                "mcp__github__create_issue" => ToolAnswer::Deny {
                    reason: "No issues today.".into(),
                },
                _ => ToolAnswer::Allow { arguments: None },
            },
        );
        let mcp = hook(
            &run.hooks,
            permission("mcp__github__create_issue", json!({ "title": "x" })),
        )
        .await;
        assert_eq!(
            mcp["hookSpecificOutput"]["decision"]["message"],
            "No issues today."
        );
        assert_eq!(run.asked.lock().unwrap()[0].2, json!({ "title": "x" }));
        let more = hook(&run.hooks, permission("request_permissions", json!({}))).await;
        assert_eq!(
            more["hookSpecificOutput"]["decision"]["message"],
            NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn a_failed_command_goes_to_the_run_with_its_exit_code_once_its_end_is_read() {
        let dir = crate::testing::temp_dir();
        let run = run(
            &dir.0,
            false,
            |call| match call {
                HarnessCall::Command(_) => {
                    CommandAnswer::Note(Some("Rovik IDE: installed vite; run it again.".into()))
                }
                _ => CommandAnswer::Note(None),
            },
            allow_all,
        );
        let post = |id: &str| {
            json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": { "command": "npm run dev" },
                    "tool_use_id": id, "tool_response": "vite: not found\n", "cwd": "/p" })
        };
        // Its end read after the hook came: the hook waits for it.
        let hooks = run.hooks.clone();
        let answer = tokio::spawn(async move { hook(&hooks, post("exec-9")).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        lock(&run.hooks.ledger).ended("exec-9", Some(127));
        let answer = answer.await.unwrap();
        assert_eq!(
            answer["hookSpecificOutput"]["additionalContext"],
            "Rovik IDE: installed vite; run it again."
        );
        assert_eq!(
            run.checked.lock().unwrap()[0].1,
            Some((Some(127), "vite: not found\n".to_string()))
        );
        // One that succeeded, or whose end never comes, isn't the run's.
        lock(&run.hooks.ledger).ended("exec-10", Some(0));
        assert_eq!(hook(&run.hooks, post("exec-10")).await, json!({}));
        assert_eq!(hook(&run.hooks, post("exec-11")).await, json!({}));
        assert_eq!(run.checked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn with_rovik_s_tools_codex_s_own_are_refused() {
        let dir = crate::testing::temp_dir();
        let hooks = Arc::new(Hooks {
            checks: None,
            asks: None,
            ask_commands: false,
            ask_patches: false,
            rovik_tools: true,
            root: dir.0.clone(),
            env_file: None,
            ledger: Shared::default(),
        });
        let own = hook(&hooks, pre("apply_patch", json!({ "command": "x" }), "c1")).await;
        assert_eq!(
            own["hookSpecificOutput"]["permissionDecisionReason"],
            ROVIK_TOOLS_ONLY
        );
        let rovik = hook(
            &hooks,
            pre("mcp__rovik__read", json!({ "path": "a" }), "c2"),
        )
        .await;
        assert_eq!(rovik, json!({}));
    }
}

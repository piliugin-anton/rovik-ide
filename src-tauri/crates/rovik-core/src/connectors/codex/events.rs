//! What Codex's app server says while a turn runs, as the chat shows a
//! reply (codex/mod.rs): its messages' text as it streams, its plan in
//! Plan mode, its reasoning summaries (steps of their own with its own
//! tools, the reply's reasoning with Rovik's), each tool call of its own as
//! a step with its result, and at the end its tokens and its context.
//!
//! Only the thread the run started is followed: a subagent's work is in
//! threads of its own. What Codex asks of the run is answered at once
//! (`app_server::decline`): Rovik's permissions are its hooks' to give.

use super::app_server::{decline, Message, Rpc};
use super::hooks::{self, edit_step_id};
use crate::agent::ChatStreamEvent;
use crate::chat_request::{ChatError, ErrorKind};
use crate::connectors::harness::Lines;
use crate::connectors::mcp::SERVER_NAME;
use crate::connectors::steps::{relative_path, relative_paths, thought_id, thousands, Thought};
use crate::connectors::{
    harness_error, trimmed_text, trimmed_value, usage_value, ConnectorKind, HarnessStep,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Where a step's input is cut, per field, and its result.
const STEP_INPUT_CHARS: usize = 400;
const STEP_RESULT_CHARS: usize = 1_200;

/// Why a turn is stopped whose call started without Rovik's check.
const UNCHECKED_CALL: &str = "a tool call of its own started without Rovik IDE's hook checking it against the project's restricted files and permissions, so Rovik stopped the turn. Update Codex; if it happens again, turn the connector's tools to Rovik's";

/// What a run's lines meant, kept as they come.
pub(super) struct Parser {
    rpc: Rpc,
    /// The thread the run follows.
    thread: String,
    /// The request that started the turn: its error is the turn's.
    started_by: Option<u64>,
    /// The turn being read, once an event names it.
    turn: Option<String>,
    /// Rovik's tools mode: calls to them are the reply's tool calls.
    rovik_tools: bool,
    /// The folder the run works in, which its steps' paths are relative to.
    root: Option<PathBuf>,
    /// Shared with the run's hooks (codex/hooks.rs), when it has them.
    ledger: Option<hooks::Shared>,
    /// The session as the chat keeps it, its model and Codex's version.
    session: String,
    model: Option<String>,
    version: Option<String>,
    text_shown: bool,
    /// A new message's text follows the last one's after a blank line.
    needs_break: bool,
    /// Messages and plans whose text streamed: their end repeats it.
    streamed: HashSet<String>,
    /// With its own tools, the reasoning being streamed: its item, the
    /// summary part it's in, and its step.
    thought: Option<(String, i64, Thought)>,
    thoughts: usize,
    /// Reasoning items whose summary streamed.
    reasoned: HashSet<String>,
    steps: HashMap<String, HarnessStep>,
    order: Vec<String>,
    /// This turn's tokens: input, cache writes, cache reads, output.
    tokens: [u64; 4],
    /// The context after the last request: tokens used, the model's window.
    context: Option<(u64, u64)>,
    /// Retries Codex announced this turn.
    retries: u32,
    compacted: bool,
    /// The turn is over: it completed, failed, or was stopped.
    pub(super) finished: bool,
    /// How it ended, when it failed.
    pub(super) failure: Option<ChatError>,
    /// Rovik's tools mode: the calls Rovik's turn ended with (id, name,
    /// arguments).
    pub(super) yielded: Option<Vec<(String, String, String)>>,
}

impl Parser {
    /// A parser for a turn of `thread` (the chat keeps it as `session`).
    pub(super) fn new(rpc: Rpc, thread: &str, session: &str) -> Parser {
        Parser {
            rpc,
            thread: thread.to_string(),
            started_by: None,
            turn: None,
            rovik_tools: false,
            root: None,
            ledger: None,
            session: session.to_string(),
            model: None,
            version: None,
            text_shown: false,
            needs_break: false,
            streamed: HashSet::new(),
            thought: None,
            thoughts: 0,
            reasoned: HashSet::new(),
            steps: HashMap::new(),
            order: Vec::new(),
            tokens: [0; 4],
            context: None,
            retries: 0,
            compacted: false,
            finished: false,
            failure: None,
            yielded: None,
        }
    }

    /// With Rovik's tools.
    pub(super) fn with_rovik_tools(mut self) -> Parser {
        self.rovik_tools = true;
        self
    }

    /// Its steps' paths relative to `root`.
    pub(super) fn in_folder(mut self, root: PathBuf) -> Parser {
        self.root = Some(root);
        self
    }

    /// Sharing the run's hooks' ledger, whose checks it holds calls to.
    pub(super) fn with_ledger(mut self, ledger: hooks::Shared) -> Parser {
        self.ledger = Some(ledger);
        self
    }

    /// The model and Codex's version, as the thread's start said them.
    pub(super) fn running(mut self, model: Option<String>, version: Option<String>) -> Parser {
        self.model = model;
        self.version = version;
        self
    }

    /// A turn starts with `request`: a new reply, its own usage.
    pub(super) fn begin(&mut self, request: u64) {
        self.started_by = Some(request);
        self.turn = None;
        self.retries = 0;
        self.finished = false;
        self.failure = None;
        self.next_reply();
    }

    /// Rovik's tools mode: Rovik's next turn goes on with the same turn
    /// of Codex's, given its results: a new reply, its own usage.
    pub(super) fn next_reply(&mut self) {
        self.text_shown = false;
        self.needs_break = false;
        self.tokens = [0; 4];
        self.yielded = None;
    }

    /// Says the session, as a turn on it begins.
    pub(super) fn announce(&self, emit: &mut dyn FnMut(ChatStreamEvent)) {
        emit(ChatStreamEvent::HarnessSession {
            kind: ConnectorKind::Codex.id().into(),
            session: self.session.clone(),
            model: self.model.clone(),
            version: self.version.clone(),
            rovik_tools: self.rovik_tools,
        });
    }

    /// Whether any of the reply was shown (for an error's `partial`).
    pub(super) fn shown(&self) -> bool {
        self.text_shown || !self.order.is_empty()
    }

    /// One line of the server's.
    pub(super) fn line(&mut self, line: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        // What the hooks refused before it could start: shown first.
        if let Some(ledger) = &self.ledger {
            let refused = hooks::lock(ledger).take_refused();
            for step in refused {
                self.show(step, emit);
            }
        }
        match super::app_server::parse(line) {
            Some(Message::Notification { method, params }) => self.event(&method, &params, emit),
            Some(Message::Request { id, method, .. }) => decline(&self.rpc, &id, &method),
            Some(Message::Response {
                id,
                result: Err(error),
            }) if Some(id) == self.started_by => {
                let error = harness_error(ConnectorKind::Codex, error.message, None);
                self.fail(error, emit);
            }
            _ => {}
        }
    }

    fn event(&mut self, method: &str, params: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        // Another thread's (a subagent's): its work isn't the reply's.
        if let Some(thread) = params.get("threadId").and_then(Value::as_str) {
            if thread != self.thread {
                return;
            }
        }
        if let Some(turn) = params
            .get("turnId")
            .or_else(|| params.pointer("/turn/id"))
            .and_then(Value::as_str)
        {
            self.turn = Some(turn.to_string());
        }
        let item = params.get("item").unwrap_or(&Value::Null);
        let delta = params.get("delta").and_then(Value::as_str).unwrap_or("");
        let item_id = params.get("itemId").and_then(Value::as_str).unwrap_or("");
        match method {
            "item/started" => self.started(item, emit),
            "item/completed" => self.completed(item, emit),
            "item/agentMessage/delta" | "item/plan/delta" => {
                self.streamed.insert(item_id.to_string());
                self.text(delta, emit);
            }
            "item/reasoning/summaryTextDelta" => {
                let part = params
                    .get("summaryIndex")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                self.thinking(item_id, part, delta, emit);
            }
            "turn/plan/updated" if !self.rovik_tools => {
                let turn = params.get("turnId").and_then(Value::as_str).unwrap_or("");
                let mut input = json!({ "plan": params.get("plan").cloned().unwrap_or(json!([])) });
                if let Some(why) = params.get("explanation").filter(|e| e.is_string()) {
                    input["explanation"] = why.clone();
                }
                // Its plan as it stands: a record, done as it shows, not a
                // tool at work (whose step has no result until it's done).
                let step = HarnessStep {
                    id: format!("plan-{turn}"),
                    name: "update_plan".into(),
                    input,
                    ok: Some(true),
                    ..HarnessStep::default()
                };
                self.show(step, emit);
            }
            "thread/tokenUsage/updated" => self.usage(params.get("tokenUsage")),
            "error" => {
                if params.get("willRetry").and_then(Value::as_bool) == Some(true) {
                    self.retries += 1;
                    let message = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("error");
                    emit(ChatStreamEvent::Retrying {
                        attempt: self.retries,
                        max_attempts: self.retries,
                        delay_ms: 0,
                        reason: ChatError::new(ErrorKind::Provider, format!("Codex: {message}")),
                    });
                }
            }
            "turn/completed" => self.turn_completed(params.get("turn"), emit),
            "configWarning" => {
                let said = params.get("summary").or_else(|| params.get("message"));
                ::log::warn!("codex: {}", said.cloned().unwrap_or_default());
            }
            _ => {}
        }
    }

    fn started(&mut self, item: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let id = text_of(item, "id");
        match text_of(item, "type") {
            "agentMessage" | "plan" => {
                if self.text_shown {
                    self.needs_break = true;
                }
            }
            "reasoning" if !self.rovik_tools => self.end_thought(emit),
            "commandExecution" | "fileChange" if !self.guarded(item, emit) => {}
            "commandExecution" if !self.rovik_tools => {
                let step = command_step(item, self.root.as_deref());
                self.show(step, emit);
            }
            "fileChange" => {
                for step in edit_steps(item, self.root.as_deref()) {
                    self.show(step, emit);
                }
            }
            "mcpToolCall" if self.rovik_tools && text_of(item, "server") == SERVER_NAME => {
                self.yield_call(id, item, emit)
            }
            "mcpToolCall" => {
                let name = format!(
                    "mcp__{}__{}",
                    text_of(item, "server"),
                    text_of(item, "tool")
                );
                let input = relative_paths(
                    item.get("arguments").unwrap_or(&Value::Null),
                    self.root.as_deref(),
                );
                self.show(step(id, &name, input), emit);
            }
            "webSearch" if !self.rovik_tools => {
                let input = json!({ "query": text_of(item, "query") });
                self.show(step(id, "web_search", input), emit);
            }
            "imageView" if !self.rovik_tools => {
                let path = self.relative(text_of(item, "path"));
                self.show(step(id, "view_image", json!({ "path": path })), emit);
            }
            "collabAgentToolCall" => {
                let input = json!({
                    "tool": text_of(item, "tool"),
                    "prompt": item.get("prompt").cloned().unwrap_or(Value::Null),
                });
                self.show(step(id, "agent", input), emit);
            }
            _ => {}
        }
    }

    fn completed(&mut self, item: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let id = text_of(item, "id");
        let status = text_of(item, "status");
        match text_of(item, "type") {
            // Text that didn't stream (a review's): shown whole.
            "agentMessage" | "plan" if !self.streamed.contains(id) => {
                self.text(text_of(item, "text"), emit)
            }
            "reasoning" => self.reasoned(id, item, emit),
            "commandExecution" => {
                let code = item
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                if let Some(ledger) = &self.ledger {
                    hooks::lock(ledger).ended(id, code);
                }
                let output = text_of(item, "aggregatedOutput");
                let ok = status == "completed" && code.unwrap_or(0) == 0;
                self.finish_step(id, output, ok, emit);
            }
            "fileChange" => {
                let said = match status {
                    "completed" => "",
                    "declined" => "Declined.",
                    _ => "It didn't apply.",
                };
                let mut n = 0;
                while self.steps.contains_key(&edit_step_id(id, n)) {
                    self.finish_step(&edit_step_id(id, n), said, status == "completed", emit);
                    n += 1;
                }
            }
            "mcpToolCall" if !(self.rovik_tools && text_of(item, "server") == SERVER_NAME) => {
                let result = match item.pointer("/error/message").and_then(Value::as_str) {
                    Some(error) => error.to_string(),
                    None => content_text(item.pointer("/result/content")),
                };
                self.finish_step(id, &result, status == "completed", emit);
            }
            "webSearch" | "imageView" | "collabAgentToolCall" => {
                self.finish_step(id, "", status != "failed", emit)
            }
            "contextCompaction" => self.compacted = true,
            _ => {}
        }
    }

    /// The safety net under the hooks: a command or patch of Codex's own
    /// that starts without Rovik's hook having seen it stops the turn.
    /// False when it did.
    fn guarded(&mut self, item: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) -> bool {
        let Some(ledger) = &self.ledger else {
            return true;
        };
        // A command's input to one already running, or one the user ran,
        // isn't a call of the model's.
        if matches!(
            text_of(item, "source"),
            "userShell" | "unifiedExecInteraction"
        ) {
            return true;
        }
        if hooks::lock(ledger).checked(text_of(item, "id")) {
            return true;
        }
        // Codex ends what it started; the run then ends Codex.
        if let Some(turn) = &self.turn {
            let params = json!({ "threadId": self.thread, "turnId": turn });
            self.rpc.request("turn/interrupt", params);
        }
        let mut error = harness_error(ConnectorKind::Codex, UNCHECKED_CALL, None);
        error.partial = self.shown();
        self.fail(error, emit);
        false
    }

    /// Rovik's tools mode: a call of Rovik's ends Rovik's turn with it.
    fn yield_call(&mut self, id: &str, item: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let name = text_of(item, "tool").to_string();
        let arguments = match item.get("arguments") {
            Some(Value::String(s)) => s.clone(),
            Some(value) if !value.is_null() => value.to_string(),
            _ => "{}".to_string(),
        };
        emit(ChatStreamEvent::ToolCallStart {
            id: id.to_string(),
            name: name.clone(),
        });
        emit(ChatStreamEvent::ToolCallComplete {
            id: id.to_string(),
            name: name.clone(),
            arguments: arguments.clone(),
            extra_content: None,
        });
        emit(ChatStreamEvent::Finished {
            finish_reason: Some("tool_calls".into()),
            usage: Some(self.usage_value(json!({}))),
        });
        emit(ChatStreamEvent::Done);
        self.yielded = Some(vec![(id.to_string(), name, arguments)]);
    }

    fn text(&mut self, text: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if text.is_empty() {
            return;
        }
        self.end_thought(emit);
        let mut content = String::new();
        if std::mem::take(&mut self.needs_break) {
            content.push_str("\n\n");
        }
        content.push_str(text);
        self.text_shown = true;
        emit(ChatStreamEvent::Delta { content });
    }

    /// A reasoning summary streamed: the reply's reasoning with Rovik's
    /// tools; with its own, the step of the thought it's part of, a blank
    /// line between its parts.
    fn thinking(
        &mut self,
        item: &str,
        part: i64,
        text: &str,
        emit: &mut dyn FnMut(ChatStreamEvent),
    ) {
        if text.is_empty() {
            return;
        }
        self.reasoned.insert(item.to_string());
        if self.rovik_tools {
            emit(ChatStreamEvent::Reasoning {
                content: text.into(),
            });
            return;
        }
        if self.thought.as_ref().is_some_and(|(id, ..)| id != item) {
            self.end_thought(emit);
        }
        let (_, last_part, thought) = self
            .thought
            .get_or_insert_with(|| (item.to_string(), part, Thought::new()));
        let so_far = thought.step.thinking.get_or_insert_with(String::new);
        if part != *last_part && !so_far.is_empty() {
            so_far.push_str("\n\n");
        }
        *last_part = part;
        so_far.push_str(text);
        if thought.step.id.is_empty() {
            self.thoughts += 1;
            thought.step.id = thought_id(self.thoughts);
        }
        emit(ChatStreamEvent::HarnessStep {
            step: thought.step.clone(),
        });
    }

    /// A reasoning item ended: its thought over, or, when its summary
    /// didn't stream, shown whole.
    fn reasoned(&mut self, id: &str, item: &Value, emit: &mut dyn FnMut(ChatStreamEvent)) {
        if self.thought.as_ref().is_some_and(|(item, ..)| item == id) {
            self.end_thought(emit);
            return;
        }
        if self.reasoned.contains(id) {
            return;
        }
        let summary: Vec<&str> = item
            .get("summary")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if summary.is_empty() {
            return;
        }
        let text = summary.join("\n\n");
        if self.rovik_tools {
            emit(ChatStreamEvent::Reasoning { content: text });
            return;
        }
        self.thoughts += 1;
        let step = HarnessStep {
            id: thought_id(self.thoughts),
            thinking: Some(text),
            ..HarnessStep::default()
        };
        emit(ChatStreamEvent::HarnessStep { step });
    }

    /// The thought being streamed is over: shown with its time.
    fn end_thought(&mut self, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let Some((_, _, Thought { mut step, started })) = self.thought.take() else {
            return;
        };
        step.ms = Some(started.elapsed().as_millis() as u64);
        emit(ChatStreamEvent::HarnessStep { step });
    }

    /// A step of a call, shown and kept for its result.
    fn show(&mut self, step: HarnessStep, emit: &mut dyn FnMut(ChatStreamEvent)) {
        self.end_thought(emit);
        if !self.steps.contains_key(&step.id) {
            self.order.push(step.id.clone());
        }
        self.steps.insert(step.id.clone(), step.clone());
        emit(ChatStreamEvent::HarnessStep { step });
    }

    fn finish_step(
        &mut self,
        id: &str,
        result: &str,
        ok: bool,
        emit: &mut dyn FnMut(ChatStreamEvent),
    ) {
        let Some(step) = self.steps.get_mut(id) else {
            return;
        };
        step.result = Some(trimmed_text(result.trim_end(), STEP_RESULT_CHARS));
        step.ok = Some(ok);
        emit(ChatStreamEvent::HarnessStep { step: step.clone() });
    }

    fn relative(&self, path: &str) -> String {
        match &self.root {
            Some(root) => relative_path(path, root),
            None => path.to_string(),
        }
    }

    /// A request's tokens, as `thread/tokenUsage/updated` gives them: the
    /// last request's added to the turn's, and the context it left.
    fn usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        let count = |key: &str| {
            usage
                .pointer(&format!("/last/{key}"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let (input, cached, written) = (
            count("inputTokens"),
            count("cachedInputTokens"),
            count("cacheWriteInputTokens"),
        );
        let request = [
            input.saturating_sub(cached + written),
            written,
            cached,
            count("outputTokens"),
        ];
        for (total, n) in self.tokens.iter_mut().zip(request) {
            *total += n;
        }
        if let Some(window) = usage.get("modelContextWindow").and_then(Value::as_u64) {
            self.context = Some((count("totalTokens"), window));
        }
    }

    /// This turn's usage, as providers report theirs.
    fn usage_value(&self, mut extra: Value) -> Value {
        extra["model"] = json!(self.model);
        extra["version"] = json!(self.version);
        extra["steps"] = json!(self.order.len());
        if let Some((used, window)) = self.context {
            extra["context"] = json!({ "used": used, "window": window });
        }
        let [input, written, cached, output] = self.tokens;
        usage_value(
            ConnectorKind::Codex,
            input,
            written,
            cached,
            output,
            None,
            extra,
        )
    }

    fn turn_completed(&mut self, turn: Option<&Value>, emit: &mut dyn FnMut(ChatStreamEvent)) {
        let turn = turn.unwrap_or(&Value::Null);
        self.end_thought(emit);
        let status = text_of(turn, "status");
        if status == "failed" {
            let error = turn.get("error").unwrap_or(&Value::Null);
            let mut failure = turn_error(error);
            failure.partial = self.shown();
            self.fail(failure, emit);
            return;
        }
        // A compaction says nothing of its own (`/compact`): what it did.
        if self.compacted && !self.text_shown {
            let left = self
                .context
                .map(|(used, _)| format!(": {} tokens now", thousands(used)))
                .unwrap_or_default();
            self.text(&format!("_Compacted the conversation{left}._"), emit);
        }
        let usage = self.usage_value(json!({ "status": status }));
        emit(ChatStreamEvent::Finished {
            finish_reason: Some(
                if status == "interrupted" {
                    "interrupted"
                } else {
                    "stop"
                }
                .into(),
            ),
            usage: Some(usage),
        });
        emit(ChatStreamEvent::Done);
        self.finished = true;
    }

    /// The turn failed: its usage, then its error.
    fn fail(&mut self, error: ChatError, emit: &mut dyn FnMut(ChatStreamEvent)) {
        emit(ChatStreamEvent::Finished {
            finish_reason: Some("error".into()),
            usage: Some(self.usage_value(json!({ "status": "failed" }))),
        });
        emit(ChatStreamEvent::Error {
            error: error.clone(),
        });
        self.failure = Some(error);
        self.finished = true;
    }
}

impl Lines for Parser {
    fn on_line(&mut self, line: &str, emit: &mut dyn FnMut(ChatStreamEvent)) {
        self.line(line, emit);
    }

    fn yielded(&self) -> bool {
        self.yielded.is_some()
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

/// The error of a failed turn, of the kind its `codexErrorInfo` says.
pub(super) fn turn_error(error: &Value) -> ChatError {
    let message = match text_of(error, "message") {
        "" => "the turn failed",
        message => message,
    };
    let details = error
        .get("additionalDetails")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_string);
    let mut failure = harness_error(ConnectorKind::Codex, message, details);
    let info = error.get("codexErrorInfo");
    let kind = info
        .and_then(Value::as_str)
        .or_else(|| info?.as_object()?.keys().next().map(String::as_str));
    if let Some(kind) = kind.and_then(|kind| match kind {
        "unauthorized" => Some(ErrorKind::Auth),
        "usageLimitExceeded" | "rateLimitExceeded" | "sessionBudgetExceeded" => {
            Some(ErrorKind::RateLimited)
        }
        "contextWindowExceeded" => Some(ErrorKind::ContextLength),
        "serverOverloaded" | "internalServerError" => Some(ErrorKind::ServerError),
        "badRequest" => Some(ErrorKind::BadRequest),
        "httpConnectionFailed"
        | "responseStreamConnectionFailed"
        | "responseStreamDisconnected" => Some(ErrorKind::Network),
        _ => None,
    }) {
        failure.kind = kind;
    }
    failure
}

/// A string field of an item's.
fn text_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A step of a call of Codex's own tools.
fn step(id: &str, name: &str, input: Value) -> HarnessStep {
    HarnessStep {
        id: id.to_string(),
        name: name.to_string(),
        input: trimmed_value(&input, STEP_INPUT_CHARS),
        ..HarnessStep::default()
    }
}

/// A command of its shell: the command as the model wrote it, the folder
/// it ran in when that isn't the project's, and what Codex read it as
/// doing (a read of a file, a search), its paths the project's.
fn command_step(item: &Value, root: Option<&std::path::Path>) -> HarnessStep {
    let relative = |path: &str| match root {
        Some(root) => relative_path(path, root),
        None => path.to_string(),
    };
    let mut input = json!({ "command": unwrapped(text_of(item, "command")) });
    let cwd = relative(text_of(item, "cwd"));
    if !cwd.is_empty() && cwd != "." {
        input["cwd"] = json!(cwd);
    }
    let actions: Vec<Value> = item
        .get("commandActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|action| {
            let mut action = action.clone();
            if let Some(Value::String(path)) = action.get_mut("path") {
                *path = relative(path);
            }
            action
        })
        .collect();
    if !actions.is_empty() {
        input["actions"] = json!(actions);
    }
    step(text_of(item, "id"), "shell", input)
}

/// A patch's steps, one per file it changes.
fn edit_steps(item: &Value, root: Option<&std::path::Path>) -> Vec<HarnessStep> {
    let id = text_of(item, "id");
    let relative = |path: &str| match root {
        Some(root) => relative_path(path, root),
        None => path.to_string(),
    };
    item.get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(n, change)| {
            let kind = change
                .pointer("/kind/type")
                .and_then(Value::as_str)
                .unwrap_or("update");
            let mut input = json!({
                "path": relative(text_of(change, "path")),
                "kind": kind,
                "diff": text_of(change, "diff"),
            });
            if let Some(to) = change.pointer("/kind/move_path").and_then(Value::as_str) {
                input["movePath"] = json!(relative(to));
            }
            step(&edit_step_id(id, n), "edit", input)
        })
        .collect()
}

/// A command as the model wrote it, out of the shell Codex runs it in
/// (`/bin/bash -lc 'npm test'`); one in another shape stays as it is.
fn unwrapped(command: &str) -> String {
    for flag in [" -lc ", " -c "] {
        let Some(at) = command.find(flag) else {
            continue;
        };
        let (shell, quoted) = (&command[..at], &command[at + flag.len()..]);
        if shell.contains(' ') || !shell.rsplit('/').next().is_some_and(|s| s.ends_with("sh")) {
            continue;
        }
        if let Some(inner) = single_quoted(quoted) {
            return inner;
        }
    }
    command.to_string()
}

/// The text of a POSIX single-quoted word (`'it'\''s'`), when `word` is
/// one whole.
fn single_quoted(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    loop {
        let inner = rest.strip_prefix('\'')?;
        let end = inner.find('\'')?;
        out.push_str(&inner[..end]);
        rest = &inner[end + 1..];
        if rest.is_empty() {
            return Some(out);
        }
        rest = rest.strip_prefix("\\'")?;
        out.push('\'');
        if rest.is_empty() {
            return Some(out);
        }
    }
}

/// The text of an MCP result's content blocks.
fn content_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// A parser, what it writes to the server, and the server's input,
    /// which the test holds open as the run's process does.
    fn parser() -> (
        Parser,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedSender<String>,
    ) {
        let (input, sent) = mpsc::unbounded_channel();
        let parser = Parser::new(Rpc::new(input.clone()), "t1", "t1")
            .running(Some("gpt-6.1-sol".into()), Some("0.164.0".into()));
        (parser, sent, input)
    }

    fn feed(parser: &mut Parser, lines: &[Value]) -> Vec<ChatStreamEvent> {
        let mut events = Vec::new();
        for line in lines {
            parser.line(&line.to_string(), &mut |e| events.push(e));
        }
        events
    }

    fn note(method: &str, params: Value) -> Value {
        let mut params = params;
        params["threadId"] = json!("t1");
        json!({ "method": method, "params": params })
    }

    fn texts(events: &[ChatStreamEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Delta { content } => Some(content.as_str()),
                _ => None,
            })
            .collect()
    }

    fn last_steps(events: &[ChatStreamEvent]) -> HashMap<String, HarnessStep> {
        let mut steps = HashMap::new();
        for e in events {
            if let ChatStreamEvent::HarnessStep { step } = e {
                steps.insert(step.id.clone(), step.clone());
            }
        }
        steps
    }

    fn usage_line(input: u64, cached: u64, output: u64, total: u64) -> Value {
        note(
            "thread/tokenUsage/updated",
            json!({ "tokenUsage": {
                "last": { "inputTokens": input, "cachedInputTokens": cached, "cacheWriteInputTokens": 0,
                          "outputTokens": output, "totalTokens": total },
                "modelContextWindow": 258400,
            } }),
        )
    }

    #[test]
    fn a_turn_becomes_text_steps_and_usage() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.clone();
        let (parser, _, _input) = parser();
        let mut parser = parser.in_folder(root.clone());
        parser.begin(3);
        let file = root.join("a.txt").to_string_lossy().into_owned();
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/started",
                    json!({ "item": { "type": "agentMessage", "id": "m1", "text": "" } }),
                ),
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": "m1", "delta": "Reading it." }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "agentMessage", "id": "m1", "text": "Reading it." } }),
                ),
                note(
                    "item/started",
                    json!({ "item": {
                    "type": "commandExecution", "id": "exec-1", "command": "/bin/bash -lc 'cat a.txt'",
                    "cwd": root, "source": "unifiedExecStartup", "status": "inProgress",
                    "commandActions": [{ "type": "read", "command": "cat a.txt", "name": "a.txt", "path": file }],
                } }),
                ),
                note(
                    "item/completed",
                    json!({ "item": {
                    "type": "commandExecution", "id": "exec-1", "status": "completed",
                    "aggregatedOutput": "hello\n", "exitCode": 0,
                } }),
                ),
                usage_line(1000, 800, 20, 1020),
                note(
                    "item/started",
                    json!({ "item": { "type": "agentMessage", "id": "m2", "text": "" } }),
                ),
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": "m2", "delta": "It says hello." }),
                ),
                usage_line(1100, 1000, 5, 1105),
                note(
                    "turn/completed",
                    json!({ "turn": { "id": "u1", "status": "completed" } }),
                ),
            ],
        );
        assert_eq!(texts(&events), "Reading it.\n\nIt says hello.");
        let steps = last_steps(&events);
        let shell = &steps["exec-1"];
        assert_eq!(shell.name, "shell");
        assert_eq!(shell.input["command"], "cat a.txt");
        assert_eq!(shell.input["actions"][0]["path"], "a.txt");
        assert!(shell.input.get("cwd").is_none(), "the project's own folder");
        assert_eq!(
            (shell.result.as_deref(), shell.ok),
            (Some("hello"), Some(true))
        );
        let usage = events
            .iter()
            .find_map(|e| match e {
                ChatStreamEvent::Finished { usage, .. } => usage.clone(),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage["prompt_tokens"], 2100);
        assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 1800);
        assert_eq!(usage["completion_tokens"], 25);
        assert_eq!(usage["connector"]["kind"], "codex");
        assert_eq!(
            usage["connector"]["context"],
            json!({ "used": 1105, "window": 258400 })
        );
        assert!(matches!(events.last(), Some(ChatStreamEvent::Done)));
        assert!(parser.finished);
    }

    #[test]
    fn a_failed_command_and_a_patch_show_as_steps_with_their_results() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/started",
                    json!({ "item": { "type": "commandExecution", "id": "exec-2",
                    "command": "/bin/bash -lc 'ls nope'", "cwd": "/elsewhere", "source": "unifiedExecStartup" } }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "commandExecution", "id": "exec-2",
                    "status": "failed", "aggregatedOutput": "ls: nope: No such file\n", "exitCode": 2 } }),
                ),
                note(
                    "item/started",
                    json!({ "item": { "type": "fileChange", "id": "exec-3", "status": "inProgress", "changes": [
                    { "path": "/p/a.txt", "kind": { "type": "update", "move_path": null }, "diff": "@@ -1 +1 @@\n-a\n+b\n" },
                    { "path": "/p/new.txt", "kind": { "type": "add" }, "diff": "new\n" },
                ] } }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "fileChange", "id": "exec-3", "status": "declined", "changes": [] } }),
                ),
            ],
        );
        let steps = last_steps(&events);
        assert_eq!(steps["exec-2"].ok, Some(false));
        assert_eq!(steps["exec-2"].input["cwd"], "/elsewhere");
        assert_eq!(steps["exec-3"].input["kind"], "update");
        assert_eq!(steps["exec-3"].input["diff"], "@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(steps["exec-3#1"].input["kind"], "add");
        assert_eq!(steps["exec-3#1"].result.as_deref(), Some("Declined."));
        assert_eq!(steps["exec-3#1"].ok, Some(false));
    }

    #[test]
    fn with_its_own_tools_reasoning_is_a_step_in_its_place() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/started",
                    json!({ "item": { "type": "reasoning", "id": "r1" } }),
                ),
                note(
                    "item/reasoning/summaryTextDelta",
                    json!({ "itemId": "r1", "delta": "**Look**", "summaryIndex": 0 }),
                ),
                note(
                    "item/reasoning/summaryTextDelta",
                    json!({ "itemId": "r1", "delta": "Then act", "summaryIndex": 1 }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "reasoning", "id": "r1", "summary": ["**Look**", "Then act"] } }),
                ),
                // One whose summary didn't stream: shown whole.
                note(
                    "item/completed",
                    json!({ "item": { "type": "reasoning", "id": "r2", "summary": ["Quietly"] } }),
                ),
            ],
        );
        let steps = last_steps(&events);
        assert_eq!(
            steps["thinking-1"].thinking.as_deref(),
            Some("**Look**\n\nThen act")
        );
        assert!(steps["thinking-1"].ms.is_some());
        assert_eq!(steps["thinking-2"].thinking.as_deref(), Some("Quietly"));
    }

    /// Its plan is a step done as it shows: no tool is at work on it, so
    /// the chat doesn't wait on it (run.rs `harness_step_running`).
    #[test]
    fn a_plan_update_is_a_step_done_as_it_shows() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let plan = json!([{ "step": "Read a.rs", "status": "inProgress" }]);
        let events = feed(
            &mut parser,
            &[note(
                "turn/plan/updated",
                json!({ "turnId": "u1", "plan": plan, "explanation": "Start small" }),
            )],
        );
        let steps = last_steps(&events);
        let step = &steps["plan-u1"];
        assert_eq!(step.name, "update_plan");
        assert_eq!(
            step.input,
            json!({ "plan": plan, "explanation": "Start small" })
        );
        assert_eq!(step.ok, Some(true));
    }

    #[test]
    fn a_plan_and_text_that_did_not_stream_are_the_reply() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": "m1", "delta": "I looked around.\n" }),
                ),
                note(
                    "item/started",
                    json!({ "item": { "type": "plan", "id": "p1", "text": "" } }),
                ),
                note(
                    "item/plan/delta",
                    json!({ "itemId": "p1", "delta": "# Plan\n1. Do A\n" }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "plan", "id": "p1", "text": "# Plan\n1. Do A\n" } }),
                ),
                note(
                    "item/started",
                    json!({ "item": { "type": "agentMessage", "id": "m9", "text": "" } }),
                ),
                note(
                    "item/completed",
                    json!({ "item": { "type": "agentMessage", "id": "m9", "text": "Review: fine." } }),
                ),
            ],
        );
        assert_eq!(
            texts(&events),
            "I looked around.\n\n\n# Plan\n1. Do A\n\n\nReview: fine."
        );
    }

    #[test]
    fn a_failed_turn_is_the_harnesss_error_of_its_kind() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[note(
                "turn/completed",
                json!({ "turn": { "status": "failed", "error": {
                "message": "Your access token could not be refreshed.", "codexErrorInfo": "unauthorized",
                "additionalDetails": null,
            } } }),
            )],
        );
        let error = parser.failure.clone().unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert!(error.message.starts_with("Codex: "));
        assert!(matches!(events.last(), Some(ChatStreamEvent::Error { .. })));
        assert!(parser.finished);
        let window = turn_error(
            &json!({ "message": "too long", "codexErrorInfo": "contextWindowExceeded" }),
        );
        assert_eq!(window.kind, ErrorKind::ContextLength);
        let network = turn_error(
            &json!({ "message": "x", "codexErrorInfo": { "responseStreamDisconnected": { "httpStatusCode": null } } }),
        );
        assert_eq!(network.kind, ErrorKind::Network);
        // A turn its request couldn't start fails the same way.
        let (mut parser, _, _input) = self::parser();
        parser.begin(5);
        feed(
            &mut parser,
            &[json!({ "id": 5, "error": { "code": -32600, "message": "bad input" } })],
        );
        assert!(parser.failure.unwrap().message.contains("bad input"));
    }

    #[test]
    fn retries_show_as_retrying_and_other_threads_are_not_the_reply() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let mut other = json!({ "method": "item/agentMessage/delta", "params": { "itemId": "x", "delta": "subagent says" } });
        other["params"]["threadId"] = json!("t2");
        let events = feed(
            &mut parser,
            &[
                note(
                    "error",
                    json!({ "error": { "message": "stream disconnected" }, "willRetry": true }),
                ),
                other,
            ],
        );
        assert!(matches!(
            &events[0],
            ChatStreamEvent::Retrying { attempt: 1, .. }
        ));
        assert_eq!(texts(&events), "");
    }

    #[test]
    fn what_codex_asks_is_answered_at_once() {
        let (mut parser, mut sent, _input) = parser();
        feed(
            &mut parser,
            &[
                json!({ "id": 9, "method": "item/commandExecution/requestApproval", "params": { "threadId": "t1" } }),
            ],
        );
        let answer: Value = serde_json::from_str(sent.try_recv().unwrap().trim()).unwrap();
        assert_eq!(
            answer,
            json!({ "id": 9, "result": { "decision": "decline" } })
        );
    }

    #[test]
    fn with_rovik_s_tools_a_call_of_theirs_ends_rovik_s_turn() {
        let (parser, _, _input) = parser();
        let mut parser = parser.with_rovik_tools();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/reasoning/summaryTextDelta",
                    json!({ "itemId": "r1", "delta": "Need the file", "summaryIndex": 0 }),
                ),
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": "m1", "delta": "Let me read it." }),
                ),
                note(
                    "item/started",
                    json!({ "item": { "type": "mcpToolCall", "id": "call_1", "server": "rovik",
                    "tool": "read", "arguments": { "path": "a.txt" }, "status": "inProgress" } }),
                ),
            ],
        );
        assert!(
            matches!(&events[0], ChatStreamEvent::Reasoning { content } if content == "Need the file")
        );
        assert_eq!(
            parser.yielded,
            Some(vec![(
                "call_1".into(),
                "read".into(),
                r#"{"path":"a.txt"}"#.into()
            )])
        );
        let names: Vec<&str> = events
            .iter()
            .map(|e| match e {
                ChatStreamEvent::ToolCallStart { .. } => "start",
                ChatStreamEvent::ToolCallComplete { .. } => "complete",
                ChatStreamEvent::Finished { .. } => "finished",
                ChatStreamEvent::Done => "done",
                _ => "other",
            })
            .collect();
        assert_eq!(
            names,
            ["other", "other", "start", "complete", "finished", "done"]
        );
    }

    #[test]
    fn a_call_that_starts_unchecked_stops_the_turn_and_codex_s_turn_too() {
        let (parser, mut sent, _input) = parser();
        let ledger: hooks::Shared = Arc::new(Mutex::new(hooks::Ledger::default()));
        let mut parser = parser.with_ledger(ledger.clone());
        parser.begin(1);
        let command = |id: &str| {
            note(
                "item/started",
                json!({ "item": { "type": "commandExecution", "id": id,
                "command": "/bin/bash -lc 'rm -rf x'", "cwd": "/p", "source": "unifiedExecStartup" } }),
            )
        };
        // Input to a command already running isn't a call of its own.
        let interaction = note(
            "item/started",
            json!({ "item": { "type": "commandExecution", "id": "i1",
            "command": "x", "cwd": "/p", "source": "unifiedExecInteraction" } }),
        );
        let turn = note("turn/started", json!({ "turn": { "id": "u7" } }));
        let events = feed(&mut parser, &[turn, interaction, command("exec-9")]);
        let error = parser.failure.clone().expect("stopped");
        assert!(
            error.message.contains("without Rovik IDE's hook"),
            "{}",
            error.message
        );
        assert!(parser.finished);
        assert!(
            !last_steps(&events).contains_key("exec-9"),
            "never shown as running"
        );
        let interrupt: Value = serde_json::from_str(sent.try_recv().unwrap().trim()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(
            interrupt["params"],
            json!({ "threadId": "t1", "turnId": "u7" })
        );
    }

    #[test]
    fn calls_the_hooks_refused_show_as_steps_at_the_next_line() {
        let (parser, _, _input) = parser();
        let ledger: hooks::Shared = Arc::new(Mutex::new(hooks::Ledger::default()));
        let mut parser = parser.with_ledger(ledger.clone());
        hooks::lock(&ledger).refuse(HarnessStep {
            id: "exec-4".into(),
            name: "shell".into(),
            input: json!({ "command": "git push" }),
            result: Some("Not allowed.".into()),
            ok: Some(false),
            ..HarnessStep::default()
        });
        let events = feed(&mut parser, &[note("hook/completed", json!({}))]);
        let steps = last_steps(&events);
        assert_eq!(steps["exec-4"].result.as_deref(), Some("Not allowed."));
        assert_eq!(steps["exec-4"].ok, Some(false));
    }

    #[test]
    fn a_compaction_says_what_it_left() {
        let (mut parser, _, _input) = parser();
        parser.begin(1);
        let events = feed(
            &mut parser,
            &[
                note(
                    "item/completed",
                    json!({ "item": { "type": "contextCompaction", "id": "c1" } }),
                ),
                usage_line(30_000, 0, 1_200, 31_200),
                note(
                    "turn/completed",
                    json!({ "turn": { "status": "completed" } }),
                ),
            ],
        );
        assert_eq!(
            texts(&events),
            "_Compacted the conversation: 31.2k tokens now._"
        );
    }

    #[test]
    fn a_command_reads_as_the_model_wrote_it() {
        assert_eq!(unwrapped("/bin/bash -lc 'npm test'"), "npm test");
        assert_eq!(unwrapped("/bin/zsh -lc 'echo '\\''hi'\\'''"), "echo 'hi'");
        assert_eq!(unwrapped("bash -c 'a && b'"), "a && b");
        assert_eq!(
            unwrapped("powershell.exe -Command 'dir'"),
            "powershell.exe -Command 'dir'"
        );
        assert_eq!(
            unwrapped("/bin/bash -lc 'a' extra"),
            "/bin/bash -lc 'a' extra"
        );
    }
}

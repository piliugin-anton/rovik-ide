//! A subagent's run (subagent.rs has the kinds, briefs and reports).
//!
//! The lead's `task` calls become child chats run through the same `Run`
//! as any chat, with `Role::Sub`: their own fresh context and history (the
//! brief is the first message), the parent's root and project section, the
//! kind's tools and prompt. A run of `task` calls in one turn starts
//! together, at most the settings' number at once; each child runs as a task
//! of its own, so Stop on the lead reaches it through `AgentState` and it
//! still ends cleanly, with a `stopped` report.
//!
//! The child's loop is simpler than the lead's (`drive_sub`): model turns
//! and tool calls until it replies without calling a tool. No checklist, no
//! checks, no CLM, no checkpoints (nobody is there to press Continue).
//! Instead:
//!   - a budget of tool calls and wall time (time waiting for an approval
//!     doesn't count); past it, or stuck in a loop, one turn without tools
//!     to write the report from what it found: `partial`;
//!   - a model that fails for good hands over to the kind's next model, on
//!     the same stored history; with none left: `failed`, with what the
//!     subagent read before;
//!   - a report that doesn't follow the format gets one repair turn, then
//!     goes to the lead as written: `unformatted`.

use super::events::{emit, AgentEvent};
use super::guard::FinishedCall;
use super::run::{new_message, unanswered_tool_calls, Role, Run, IDE_NOTE};
use super::subagent::{
    self, Brief, Budget, ChildWrite, Kind, SubResult, SubStatus, SubagentConfig,
};
use super::worker::{self, WriteScope};
use super::{failure, log, permissions, tools, ModelProfile};
use crate::agent::OpenAIChatOptions;
use crate::chat_db::store::{self, NewChildDialog, ToolCall};
use crate::chat_request::{ChatError, ErrorKind};
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Runtime;

/// What a subagent's run knows about itself.
pub(super) struct SubCtx {
    pub kind: Kind,
    /// The lead's chat and the `task` call this subagent answers.
    pub parent_dialog_id: String,
    pub parent_call_id: String,
    /// The user's request the lead works on: the AI reviewer judges a
    /// subagent's actions by it, not by the brief (K06).
    pub parent_request: String,
    /// Its registration in `AgentState`.
    pub run_id: String,
    pub budget: Budget,
    /// The models to try, in order, and the one in use.
    pub models: Vec<ModelProfile>,
    pub model_index: usize,
    pub models_tried: Vec<String>,
    /// The kind's reasoning effort; None: the chat's, for the chat's model.
    pub options: Option<OpenAIChatOptions>,
    kind_options: Option<OpenAIChatOptions>,
    chat_profile_id: String,
    chat_options: Option<OpenAIChatOptions>,
    /// Time spent waiting for the user's approval.
    waited: Mutex<Duration>,
    /// The history was trimmed once after a reply didn't fit (S10).
    pub trimmed: bool,
    /// The models that failed it, by id, with the error kind (S12).
    pub failures: Vec<(String, String)>,
    /// Tests only: tools beyond the kind's (`KindConfig::extra_tools`).
    pub extra_tools: Vec<String>,
    /// Tokens the provider reported, when it did (S09).
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub usage_seen: bool,
    pub cost: Option<f64>,
    /// A writing kind: where it may write (worker.rs, D7).
    pub scope: Option<WriteScope>,
    /// A step worker: its plan step.
    pub step: Option<usize>,
    /// The test writer: it writes only test files, and can't read or
    /// search these (the files the change touches, K11).
    pub tests_only: bool,
    pub hidden: Vec<String>,
    /// Only light commands: no builds or test runs (E02, F05).
    pub light_bash: bool,
}

impl SubCtx {
    /// Whether it may call `tool`: its kind's tools only.
    pub fn allows(&self, tool: &str) -> bool {
        self.kind.spec().allows(tool) || self.extra_tools.iter().any(|t| t == tool)
    }

    pub fn add_waited(&self, d: Duration) {
        *self.waited.lock().unwrap_or_else(|e| e.into_inner()) += d;
    }

    fn waited(&self) -> Duration {
        *self.waited.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds a reply's usage (OpenAI's `prompt_tokens`/`completion_tokens`,
    /// or `input_tokens`/`output_tokens`).
    pub fn note_usage(&mut self, usage: Option<&Value>) {
        let Some(u) = usage else { return };
        let n = |keys: &[&str]| keys.iter().find_map(|k| u.get(*k)?.as_u64());
        if let Some(i) = n(&["prompt_tokens", "input_tokens"]) {
            self.tokens_in += i;
            self.usage_seen = true;
        }
        if let Some(o) = n(&["completion_tokens", "output_tokens"]) {
            self.tokens_out += o;
            self.usage_seen = true;
        }
        // OpenRouter and some others say what the request cost.
        if let Some(c) = u.get("cost").and_then(|c| c.as_f64()) {
            *self.cost.get_or_insert(0.0) += c;
        }
    }

    /// The effort a model is asked with: the kind's, or the chat's for the
    /// chat's own model.
    fn options_for(&self, profile: &ModelProfile) -> Option<OpenAIChatOptions> {
        self.kind_options.clone().or_else(|| {
            (profile.id == self.chat_profile_id)
                .then(|| self.chat_options.clone())
                .flatten()
        })
    }
}

/// A `task` call, ready to run or already answered.
enum Job<R: Runtime> {
    Run(Box<Run<R>>),
    Answered(String),
}

type SubFuture = Pin<Box<dyn Future<Output = Option<SubResult>> + Send>>;

/// A subagent's whole run as a boxed future: a run's future can't contain
/// itself (a child's tool calls go through the same code as the lead's).
fn go_sub<R: Runtime>(run: Run<R>) -> SubFuture {
    Box::pin(async move {
        let mut run = run;
        Some(run.go_sub().await)
    })
}

impl<R: Runtime> Run<R> {
    /*
     * -----------------------------------------------------------------------
     * The lead's side: `task` calls
     * -----------------------------------------------------------------------
     */

    /// A run of `task` calls: each checked and started (or refused), run at
    /// most `max_at_once` at a time, and answered in the calls' order.
    /// `resumed`: the calls never got their results (the app closed): a
    /// child chat they had already started goes on where it was (S14).
    pub(super) async fn execute_tasks(
        &mut self,
        calls: &[ToolCall],
        resumed: bool,
    ) -> Result<(), String> {
        let config = self.state.config().subagents;
        let mut jobs: Vec<Job<R>> = Vec::with_capacity(calls.len());
        for tc in calls {
            if self.stopped() {
                jobs.push(Job::Answered(
                    "Error: stopped by the user before this ran.".into(),
                ));
                continue;
            }
            jobs.push(match self.start_task(tc, &config, resumed).await {
                Ok(child) => Job::Run(Box::new(child)),
                Err(text) => Job::Answered(text),
            });
        }
        let started = Instant::now();
        let running = jobs.iter().filter(|j| matches!(j, Job::Run(_))).count();
        let jobs_write = jobs.iter().any(|j| match j {
            Job::Run(child) => child.sub().is_some_and(|s| s.kind.writes()),
            Job::Answered(_) => false,
        });
        let throttled = jobs.iter().any(|j| match j {
            Job::Run(child) => self.ledger.is_throttled(&child.profile.id),
            Job::Answered(_) => false,
        });
        #[cfg(test)]
        super::parallel::BATCHES.lock().unwrap().push((
            self.dialog_id.clone(),
            calls.iter().map(|c| c.id.clone()).collect(),
        ));
        let mut answered: Vec<Option<String>> = Vec::with_capacity(jobs.len());
        let futures: Vec<SubFuture> = jobs
            .into_iter()
            .map(|job| match job {
                Job::Answered(text) => {
                    answered.push(Some(text));
                    Box::pin(async { None }) as SubFuture
                }
                Job::Run(child) => {
                    answered.push(None);
                    // Started when a place is free (`buffered` polls it
                    // then), as a task of its own: dropping this future
                    // never cancels a child halfway; Stop ends it cleanly.
                    Box::pin(async move {
                        tauri::async_runtime::spawn(go_sub(*child))
                            .await
                            .ok()
                            .flatten()
                    }) as SubFuture
                }
            })
            .collect();
        // A model a rate limit stopped runs one subagent at a time (S12);
        // so does a batch with a step worker: it writes the lead's files.
        let writes = jobs_write;
        let limit = if throttled || writes {
            1
        } else {
            config.max_at_once()
        };
        let mut results = stream::iter(futures).buffered(limit);
        for (tc, refusal) in calls.iter().zip(answered) {
            let result = results.next().await.flatten();
            let (content, meta): (String, Option<Value>) = match (refusal, result) {
                (Some(text), _) => (text, None),
                (None, Some(mut result)) => {
                    self.absorb_child_writes(&result.writes);
                    if result.kind == Kind::TestWriter {
                        for w in &result.writes {
                            if !self.test_writer_files.contains(&w.path) {
                                self.test_writer_files.push(w.path.clone());
                            }
                        }
                    }
                    self.note_models(&result);
                    let update = self.after_worker(&mut result).await;
                    let mut meta = json!({ "subagent": result.meta() });
                    if let Some(items) = update {
                        meta["checklistUpdate"] = json!(items);
                    }
                    (result.for_lead(), Some(meta))
                }
                (None, None) => (
                    "Error: the subagent ended without a report (its run failed).".into(),
                    None,
                ),
            };
            let mut content = content;
            let verdict = self.guard.record(FinishedCall {
                name: &tc.name,
                args_json: &tc.arguments,
                result: &content,
                write: None,
                rejected: false,
            });
            if let Some(note) = verdict.note() {
                content.push_str(note);
            }
            self.add_tool_result(&tc.id, content, meta, Vec::new())
                .await?;
        }
        if running > 0 {
            log::write(
                &self.app,
                json!({
                    "event": "subagentBatch",
                    "dialogId": self.dialog_id,
                    "calls": calls.len(),
                    "started": running,
                    "ms": started.elapsed().as_millis() as u64,
                }),
            );
        }
        Ok(())
    }

    /// Checks a `task` call and prepares its child run; the error is the
    /// call's result.
    async fn start_task(
        &mut self,
        tc: &ToolCall,
        config: &SubagentConfig,
        resumed: bool,
    ) -> Result<Run<R>, String> {
        let kinds = self.subagent_kinds();
        if kinds.is_empty() {
            return Err(
                "Error: subagents are turned off in the settings. Do the work yourself.".into(),
            );
        }
        let args = permissions::parse_args(&tc.arguments);
        if let Some(handle) = args
            .get("continue")
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            return self
                .start_follow_up(tc, config, &kinds, handle, &args)
                .await;
        }
        let brief = subagent::parse_brief(&args, &kinds)?;
        let kind = brief.kind;
        if kind == Kind::Worker {
            return self.start_worker(tc, config, brief, resumed).await;
        }
        if kind == Kind::TestWriter {
            return self.start_test_writer(tc, config, brief, resumed).await;
        }

        // Started before the app closed: it goes on in its own chat.
        let existing = if resumed {
            let (parent, call) = (self.dialog_id.clone(), tc.id.clone());
            self.db
                .read(move |c| store::child_for_call(c, &parent, &call))
                .await?
        } else {
            None
        };
        let child_id = match &existing {
            Some(child) => child.id.clone(),
            None => {
                if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
                    return Err(why);
                }
                self.ledger.admit(&brief, config.max_per_run())?;
                uuid::Uuid::new_v4().to_string()
            }
        };

        let budget = brief.budget(config.budget(kind));
        let sub = self.new_sub(kind, &tc.id, budget, config).await?;
        match &existing {
            Some(_) => self.close_child_calls(&child_id).await?,
            None => {
                self.store_child(&child_id, &tc.id, kind, &brief.goal, brief.message())
                    .await?
            }
        }
        self.launch_child(child_id, &tc.id, &brief.goal, sub)
    }

    /// A04: a follow-up question to a finished subagent, in its own chat
    /// and context. Refused for one that's unknown here, still working, or
    /// of a kind that's off.
    async fn start_follow_up(
        &mut self,
        tc: &ToolCall,
        config: &SubagentConfig,
        kinds: &[Kind],
        handle: &str,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Run<R>, String> {
        let goal = args
            .get("goal")
            .and_then(|g| g.as_str())
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .ok_or("Error: \"goal\" is required: the follow-up question.")?
            .to_string();
        let (parent, key) = (self.dialog_id.clone(), handle.to_string());
        let Some(child) = self
            .db
            .read(move |c| store::child_by_handle(c, &parent, &key))
            .await?
        else {
            return Err(format!(
                "Error: no subagent \"{handle}\" in this chat. Use the handle from its report."
            ));
        };
        let kind = Kind::parse(&child.kind)
            .filter(|k| kinds.contains(k))
            .ok_or_else(|| {
                format!(
                    "Error: {} subagents are turned off in the settings.",
                    child.kind
                )
            })?;
        if self.state.is_running(&child.id) {
            return Err(
                "Error: that subagent is still working: wait for its report before asking it more."
                    .into(),
            );
        }
        if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
            return Err(why);
        }
        // A worker goes on with its step's scope (A04, F07).
        let step = if kind == Kind::Worker {
            Some(self.worker_step_of(&child.id).await.ok_or(
                "Error: that worker's plan step can't be found any more (was the plan changed?). Do the rest yourself.",
            )?)
        } else {
            None
        };
        let brief = Brief {
            kind,
            goal: format!("follow-up to {handle}: {goal}"),
            context: None,
            paths: Vec::new(),
            max_tool_calls: None,
            step: step.as_ref().map(|s| s.number),
        };
        match &step {
            Some(_) => {
                let plan_steps = match self.implementing_plan().await {
                    Some((_, plan)) => super::plan::steps(&plan).len(),
                    None => 0,
                };
                self.ledger.admit_worker(&brief, plan_steps)?
            }
            None => self.ledger.admit(&brief, config.max_per_run())?,
        }
        let mut sub = self
            .new_sub(kind, &tc.id, config.budget(kind), config)
            .await?;
        if let Some(step) = &step {
            sub.scope = Some(WriteScope::for_step(step));
            sub.step = Some(step.number);
        }
        self.close_child_calls(&child.id).await?;
        let mut message = new_message(
            "user",
            &format!(
                "# Follow-up from the lead agent\n\n{goal}\n\nAnswer it in the same report format."
            ),
        );
        message.meta = Some(json!({
            "subagentBrief": { "kind": kind.id(), "parentCallId": tc.id, "followUp": true }
        }));
        let id = child.id.clone();
        self.db
            .write(move |c| store::append_messages(c, &id, &[message]).map(|_| ()))
            .await?;
        self.launch_child(child.id, &tc.id, &goal, sub)
    }

    /// A subagent's context, before it has a run. Err: none of its models
    /// can be used (they kept failing in this run, S12).
    async fn new_sub(
        &self,
        kind: Kind,
        call_id: &str,
        budget: Budget,
        config: &SubagentConfig,
    ) -> Result<SubCtx, String> {
        if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
            return Err(why);
        }
        let all = config.models(kind, &self.profile);
        let models: Vec<ModelProfile> = all
            .iter()
            .filter(|m| !self.ledger.is_open(&m.id))
            .cloned()
            .collect();
        if models.is_empty() {
            let names: Vec<&str> = all.iter().map(|m| m.label.as_str()).collect();
            return Err(format!(
                "Error: subagents are unavailable for the rest of this run: their models keep failing ({}). Do the work yourself.",
                names.join(", ")
            ));
        }
        let kind_options = config.kinds.get(kind.id()).and_then(|k| k.options.clone());
        let mut sub = SubCtx {
            kind,
            parent_dialog_id: self.dialog_id.clone(),
            parent_call_id: call_id.to_string(),
            parent_request: self.latest_user_request().await,
            run_id: String::new(),
            budget,
            models_tried: vec![models[0].label.clone()],
            models,
            model_index: 0,
            options: None,
            kind_options,
            chat_profile_id: self.profile.id.clone(),
            chat_options: self.options.clone(),
            waited: Mutex::new(Duration::ZERO),
            trimmed: false,
            failures: Vec::new(),
            extra_tools: Vec::new(),
            tokens_in: 0,
            tokens_out: 0,
            usage_seen: false,
            cost: None,
            scope: None,
            step: None,
            tests_only: false,
            hidden: Vec::new(),
            light_bash: false,
        };
        #[cfg(test)]
        if let Some(k) = config.kinds.get(kind.id()) {
            sub.extra_tools = k.extra_tools.clone();
        }
        sub.options = sub.options_for(&sub.models[0]);
        Ok(sub)
    }

    /// Registers a child under this run and prepares its run; the chat
    /// hears it started.
    fn launch_child(
        &self,
        child_id: String,
        call_id: &str,
        goal: &str,
        mut sub: SubCtx,
    ) -> Result<Run<R>, String> {
        let Some(controls) = self.state.register_child(
            &child_id,
            &self.dialog_id,
            call_id,
            &sub.models[0].id,
            sub.options.clone(),
        ) else {
            return Err("Error: that subagent is already running.".into());
        };
        sub.run_id = controls.run_id.clone();
        let focus: Vec<String> = sub
            .scope
            .as_ref()
            .map(|s| s.files.iter().chain(&s.folders).cloned().collect())
            .unwrap_or_default();
        let map = self.kind_map(sub.kind, &focus);
        let writes = sub.kind.writes();
        emit(
            &self.app,
            AgentEvent::SubagentStarted {
                dialog_id: self.dialog_id.clone(),
                tool_call_id: call_id.to_string(),
                child_dialog_id: child_id.clone(),
                kind: sub.kind.id().into(),
                model: sub.models[0].label.clone(),
                goal: goal.to_string(),
            },
        );
        let mut child = Run::prepare_child(self, child_id, sub, controls);
        // D06: what the files were before its first write to each, so a
        // worker that fails or is stopped can be rolled back (R02's code).
        if writes {
            child.checkpoint = Some(super::repair::Checkpoint::default());
        }
        if let Some((map, chars)) = map {
            child.repo_map = map;
            child.repo_map_chars = chars;
            child.rebuild_project_context();
        }
        Ok(child)
    }

    /// S18: a kind's own repository map, when it isn't the lead's: around
    /// the files this run changed, smaller. None: the lead's map.
    fn kind_map(&self, kind: Kind, focus_files: &[String]) -> Option<(Option<String>, usize)> {
        let subagent::MapScope::Changed { fraction_percent } = kind.map_scope() else {
            return None;
        };
        let root = self.root.clone()?;
        let chars = (self.repo_map_chars * fraction_percent / 100).max(500);
        let mut touched: Vec<String> = self.touched_keys().into_iter().collect();
        touched.extend(focus_files.iter().cloned());
        let root_path = std::path::Path::new(&root);
        let focus = crate::repo_map::focus_from(root_path, "", &touched);
        let options = crate::repo_map::Options {
            max_chars: chars,
            ..Default::default()
        };
        Some((
            crate::repo_map::repo_map(root_path, &focus, &options),
            chars,
        ))
    }

    /// A subagent the IDE starts itself (a reviewer), for `call_id` (the
    /// record that shows it): its brief is `message`, written by the
    /// harness. Runs to its report as a task of its own, so Stop on this
    /// run ends it cleanly. Err: it couldn't start.
    pub(super) async fn run_ide_subagent(
        &mut self,
        kind: Kind,
        call_id: &str,
        goal: &str,
        message: String,
    ) -> Result<(String, Option<SubResult>), String> {
        let config = self.state.config().subagents;
        let sub = self
            .new_sub(kind, call_id, config.budget(kind), &config)
            .await?;
        let child_id = uuid::Uuid::new_v4().to_string();
        self.store_child(&child_id, call_id, kind, goal, message)
            .await?;
        let child = self.launch_child(child_id.clone(), call_id, goal, sub)?;
        let result = tauri::async_runtime::spawn(go_sub(child))
            .await
            .ok()
            .flatten();
        if let Some(r) = &result {
            self.absorb_child_writes(&r.writes);
            self.note_models(r);
        }
        Ok((child_id, result))
    }

    /// The child chat, with the brief as its first message.
    async fn store_child(
        &self,
        child_id: &str,
        call_id: &str,
        kind: Kind,
        goal: &str,
        message: String,
    ) -> Result<(), String> {
        let dialog = NewChildDialog {
            id: child_id.to_string(),
            parent_id: self.dialog_id.clone(),
            parent_call_id: call_id.to_string(),
            kind: kind.id().into(),
            title: format!("{}: {}", kind.spec().label, store::char_prefix(goal, 80)),
            created_at: store::now_ms(),
        };
        let mut first = new_message("user", &message);
        first.meta =
            Some(json!({ "subagentBrief": { "kind": kind.id(), "parentCallId": call_id } }));
        self.db
            .write(move |c| {
                store::create_child_dialog(c, &dialog)?;
                store::append_messages(c, &dialog.id, &[first])?;
                Ok(())
            })
            .await
    }

    /// A resumed child's own tool calls that never ran get a result, so its
    /// history is whole again before it goes on.
    async fn close_child_calls(&self, child_id: &str) -> Result<(), String> {
        let id = child_id.to_string();
        let recent = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await?
            .messages;
        let results: Vec<_> = unanswered_tool_calls(&recent)
            .into_iter()
            .map(|call| {
                let mut m = new_message(
                    "tool",
                    "Error: interrupted: the app closed before this ran. Run it again if you still need it.",
                );
                m.tool_call_id = Some(call.id);
                m
            })
            .collect();
        if results.is_empty() {
            return Ok(());
        }
        let id = child_id.to_string();
        self.db
            .write(move |c| store::append_messages(c, &id, &results).map(|_| ()))
            .await
    }

    /*
     * -----------------------------------------------------------------------
     * The subagent's side
     * -----------------------------------------------------------------------
     */

    /// Tells the lead's chat how far the subagent got (its block shows it).
    pub(super) fn note_sub_progress(&self, current_tool: &str) {
        let Some(sub) = self.sub() else { return };
        emit(
            &self.app,
            AgentEvent::SubagentProgress {
                dialog_id: sub.parent_dialog_id.clone(),
                tool_call_id: sub.parent_call_id.clone(),
                tool_calls: self.guard.tool_calls,
                current_tool: Some(current_tool.to_string()).filter(|t| !t.is_empty()),
                model: self.profile.label.clone(),
            },
        );
    }

    /// Runs the subagent to its report.
    async fn go_sub(&mut self) -> SubResult {
        // Its time starts now, not when it was prepared (it may have waited
        // for a free place).
        self.started_at = Instant::now();
        let (status, text, error) = match self.drive_sub().await {
            Ok(end) => end,
            Err(e) => {
                eprintln!("agent: subagent in {} failed: {e}", self.dialog_id);
                (SubStatus::Failed, String::new(), Some(e))
            }
        };
        let files_read =
            if matches!(status, SubStatus::Failed | SubStatus::Stopped) || text.trim().is_empty() {
                self.files_read().await
            } else {
                Vec::new()
            };
        let (notes, worker) = self.writer_end(status).await;
        let writes = self.child_writes();
        let Role::Sub(sub) = &self.role else {
            unreachable!("go_sub runs subagents only")
        };
        self.state.unregister(&self.dialog_id, &sub.run_id);
        let ms = self.started_at.elapsed().as_millis() as u64;
        let result = SubResult {
            kind: sub.kind,
            status,
            text,
            child_dialog_id: Some(self.dialog_id.clone()),
            model: Some(self.profile.label.clone()),
            models_tried: sub.models_tried.clone(),
            tool_calls: self.guard.tool_calls,
            ms,
            error,
            files_read,
            writes,
            failed_models: sub.failures.clone(),
            usage: sub.usage_seen.then_some(subagent::Usage {
                input_tokens: sub.tokens_in,
                output_tokens: sub.tokens_out,
                cost: sub.cost,
            }),
            answered_by: (!matches!(status, SubStatus::Failed | SubStatus::Stopped))
                .then(|| self.profile.id.clone()),
            notes,
            worker,
        };
        emit(
            &self.app,
            AgentEvent::SubagentEnded {
                dialog_id: sub.parent_dialog_id.clone(),
                tool_call_id: sub.parent_call_id.clone(),
                child_dialog_id: self.dialog_id.clone(),
                status: status.id().into(),
            },
        );
        log::write(
            &self.app,
            subagent_log_line(sub, &self.dialog_id, &self.profile.model, &result),
        );
        result
    }

    /// The files this subagent changed, with what each was before it.
    fn child_writes(&self) -> Vec<ChildWrite> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (path, original) in &self.baseline {
            let now = std::fs::read(tools::to_absolute_path(path, &root))
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned());
            if now != *original {
                out.push(ChildWrite {
                    path: path.clone(),
                    original: original.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// S03b: a subagent's changes count as this run's (D15, K01, K02): they
    /// join its changed files and what the checks and the reviewer look
    /// at, the repository map's counter, and what the file guard counts as
    /// seen, so this run can write them without "changed since you read it".
    pub(super) fn absorb_child_writes(&mut self, writes: &[ChildWrite]) {
        let Some(root) = self.root.clone() else {
            return;
        };
        for w in writes {
            if !self
                .changed_files
                .iter()
                .any(|c| super::repair::rel_key(&root, c) == w.path)
            {
                self.changed_files.push(w.path.clone());
            }
            self.baseline
                .entry(w.path.clone())
                .or_insert_with(|| w.original.clone());
            if let Some(cp) = self.checkpoint.as_mut().filter(|c| !c.too_big) {
                cp.files
                    .entry(w.path.clone())
                    .or_insert_with(|| w.original.clone().map(String::into_bytes));
            }
            let abs = tools::to_absolute_path(&w.path, &root);
            if let Ok(now) = std::fs::read(&abs) {
                self.seen.record(std::path::Path::new(&abs), &now);
            }
            self.writes_since_verify = true;
            self.writes_since_review = true;
            self.map_writes_since_refresh += 1;
        }
    }

    /// S12: what a subagent's end says about its models, for the next ones.
    fn note_models(&mut self, result: &SubResult) {
        for model in self.ledger.note(result) {
            log::write(
                &self.app,
                json!({
                    "event": "subagentCircuit",
                    "dialogId": self.dialog_id,
                    "model": model,
                }),
            );
        }
    }

    /// The paths its `read` calls named, for a report that has little else.
    async fn files_read(&self) -> Vec<String> {
        let id = self.dialog_id.clone();
        let Ok(page) = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await
        else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for call in page
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
        {
            if call.name != "read" {
                continue;
            }
            if let Some(p) = permissions::str_arg(&call.arguments, "path") {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Budget used up: the reason, for the result turn.
    fn over_budget(&self) -> Option<String> {
        let sub = self.sub()?;
        if self.guard.tool_calls >= sub.budget.tool_calls {
            return Some(format!(
                "You've used your budget of {} tool calls",
                sub.budget.tool_calls
            ));
        }
        let worked = self.started_at.elapsed().saturating_sub(sub.waited());
        if worked >= sub.budget.wall {
            return Some(format!(
                "You've used your time budget of about {} seconds",
                sub.budget.wall.as_secs()
            ));
        }
        None
    }

    /// The subagent's loop: how it ended, its report, and an error kind.
    async fn drive_sub(&mut self) -> Result<(SubStatus, String, Option<String>), String> {
        let mut last_text = String::new();
        loop {
            if self.stopped() {
                return Ok((SubStatus::Stopped, last_text, None));
            }
            if let Some(why) = self.over_budget() {
                return self.result_turn(SubStatus::Partial, &why, last_text).await;
            }
            self.note_sub_progress("");
            let turn = self.model_turn().await?;

            if let Some(error) = turn.failure {
                self.store_failed_turn(&turn.message_id, &turn.text, turn.reasoning, &error)
                    .await?;
                if !turn.text.trim().is_empty() {
                    last_text = turn.text.clone();
                }
                if error.kind == ErrorKind::Cancelled || self.stopped() {
                    return Ok((SubStatus::Stopped, last_text, None));
                }
                // The request didn't fit the model's window: the same model
                // again, once, with the history trimmed to three quarters.
                if error.kind == ErrorKind::ContextLength && self.trim_once() {
                    continue;
                }
                // Anything else after chat_request's own retries (a rate
                // limit too long to wait for included): the next model.
                // A window too small isn't the provider's fault (S12).
                if error.kind != ErrorKind::ContextLength {
                    let failed = (self.profile.id.clone(), error_kind(&error));
                    if let Some(sub) = self.sub_mut() {
                        sub.failures.push(failed);
                    }
                }
                if self.next_model(&error) {
                    continue;
                }
                return Ok((SubStatus::Failed, last_text, Some(error_kind(&error))));
            }

            // A reply that started repeating itself: what came before it is
            // its report (the format check below decides how it's taken).
            let (text, calls) = match turn.repetition_cut {
                Some(cut) => (
                    turn.text[..cut.min(turn.text.len())].trim_end().to_string(),
                    Vec::new(),
                ),
                None => (turn.text.clone(), turn.tool_calls.clone()),
            };
            let meta = turn.reasoning.map(|r| json!({ "reasoning": r }));
            self.finalize(&turn.message_id, text.clone(), calls.clone(), meta, None)
                .await?;
            if !text.trim().is_empty() {
                last_text = text.clone();
            }
            if self.stopped() {
                return Ok((SubStatus::Stopped, last_text, None));
            }
            if calls.is_empty() {
                return self.finish_report(text).await;
            }
            self.execute_tool_calls(&calls, None).await?;
            if self.stopped() {
                return Ok((SubStatus::Stopped, last_text, None));
            }
            // No checkpoints for a subagent; a loop it was already warned
            // about ends it, with a report (K03).
            if let Some(pause) = self.guard.pause() {
                let why = format!(
                    "You seem to be stuck: {}",
                    pause
                        .reason
                        .as_deref()
                        .unwrap_or("the same calls keep repeating")
                );
                return self.result_turn(SubStatus::Partial, &why, last_text).await;
            }
        }
    }

    /// A failed turn, stored as the lead's are: what arrived is kept, else
    /// the error shows (and the history leaves it out).
    async fn store_failed_turn(
        &self,
        message_id: &str,
        text: &str,
        reasoning: Option<Value>,
        error: &ChatError,
    ) -> Result<(), String> {
        let stored = failure::stored(error);
        if !text.trim().is_empty() {
            let mut meta = reasoning
                .map(|r| json!({ "reasoning": r }))
                .unwrap_or_else(|| json!({}));
            meta["interrupted"] = stored;
            self.finalize(message_id, text.to_string(), Vec::new(), Some(meta), None)
                .await
        } else {
            self.finalize(
                message_id,
                String::new(),
                Vec::new(),
                Some(json!({ "failure": stored })),
                Some(failure::describe(error)),
            )
            .await
        }
    }

    /// Once per subagent: its requests are fitted to three quarters of the
    /// window they were (S10). False when that was done already.
    fn trim_once(&mut self) -> bool {
        let Some(sub) = self.sub_mut() else {
            return false;
        };
        if sub.trimmed {
            return false;
        }
        sub.trimmed = true;
        let window = self
            .profile
            .context_tokens
            .filter(|t| *t > 0)
            .unwrap_or(32_000);
        self.profile.context_tokens = Some(window * 3 / 4);
        log::write(
            &self.app,
            json!({
                "event": "subagentTrim",
                "dialogId": self.dialog_id,
                "model": self.profile.label,
                "window": window,
            }),
        );
        true
    }

    /// The kind's next model takes over, on the same stored history (S10).
    /// False when there is none left.
    fn next_model(&mut self, error: &ChatError) -> bool {
        let dialog_id = self.dialog_id.clone();
        let failed_model = self.profile.label.clone();
        let Some(sub) = self.sub_mut() else {
            return false;
        };
        if sub.model_index + 1 >= sub.models.len() {
            return false;
        }
        sub.model_index += 1;
        let next = sub.models[sub.model_index].clone();
        sub.models_tried.push(next.label.clone());
        let options = sub.options_for(&next);
        let kind = sub.kind;
        self.options = options;
        self.profile = next;
        log::write(
            &self.app,
            json!({
                "event": "subagentFallback",
                "dialogId": dialog_id,
                "kind": kind.id(),
                "from": failed_model,
                "to": self.profile.label,
                "error": error_kind(error),
            }),
        );
        true
    }

    /// The final reply: checked against the kind's format, with one repair
    /// turn when it doesn't follow it (S13).
    async fn finish_report(
        &mut self,
        text: String,
    ) -> Result<(SubStatus, String, Option<String>), String> {
        let spec = self.sub().map(|s| s.kind.spec()).expect("a subagent");
        let Some(problem) = subagent::format_problem(spec, &text) else {
            return Ok((status_of(&text, SubStatus::Done), text, None));
        };
        let sections = spec
            .sections
            .iter()
            .map(|s| format!("\"## {s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        self.add_harness(
            format!(
                "{IDE_NOTE}Your report didn't follow the format: {problem}. Don't call any tools. Write the report again, with the headings {sections}."
            ),
            "subagentFormat",
        )
        .await?;
        let again = self.words_only_turn().await?;
        if subagent::format_problem(spec, &again).is_none() {
            return Ok((status_of(&again, SubStatus::Done), again, None));
        }
        let raw = if again.trim().is_empty() { text } else { again };
        Ok((SubStatus::Unformatted, raw, None))
    }

    /// One turn without tools for the report, after the budget ran out or a
    /// loop: `status` unless it says it's blocked.
    async fn result_turn(
        &mut self,
        status: SubStatus,
        why: &str,
        last_text: String,
    ) -> Result<(SubStatus, String, Option<String>), String> {
        self.add_harness(
            format!(
                "{IDE_NOTE}{why}. Don't call any tools. Write your report now, in the required format, from what you found so far, and say under \"## Open questions\" what you didn't get to."
            ),
            "subagentBudget",
        )
        .await?;
        let text = self.words_only_turn().await?;
        if self.stopped() {
            return Ok((SubStatus::Stopped, text, None));
        }
        let text = if text.trim().is_empty() {
            last_text
        } else {
            text
        };
        Ok((status_of(&text, status), text, None))
    }

    /// A turn whose calls aren't run: its text, stored (empty when it failed).
    async fn words_only_turn(&mut self) -> Result<String, String> {
        self.words_only = true;
        let turn = self.model_turn().await?;
        if let Some(error) = &turn.failure {
            self.store_failed_turn(&turn.message_id, &turn.text, turn.reasoning.clone(), error)
                .await?;
            return Ok(turn.text);
        }
        let text = match turn.repetition_cut {
            Some(cut) => turn.text[..cut.min(turn.text.len())].trim_end().to_string(),
            None => turn.text,
        };
        let meta = turn.reasoning.map(|r| json!({ "reasoning": r }));
        // Calls a provider sent anyway aren't run, and aren't kept either.
        self.finalize(&turn.message_id, text.clone(), Vec::new(), meta, None)
            .await?;
        Ok(text)
    }
}

impl<R: Runtime> Run<R> {
    /*
     * -----------------------------------------------------------------------
     * Step workers (worker.rs)
     * -----------------------------------------------------------------------
     */

    /// The plan this run implements: its path and text.
    async fn implementing_plan(&self) -> Option<(String, String)> {
        let recent = self.recent().await.ok()?;
        let request = recent
            .iter()
            .rev()
            .find(|m| m.role == "user" && !super::plan::is_harness(m))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let base = self.project_root.as_deref().or(self.root.as_deref());
        let path = worker::plan_file_named(&request, base)
            .or_else(|| super::run::chat_plan_file(&recent))?;
        let text = std::fs::read_to_string(&path).ok()?;
        let shown = super::plan::relative_plan_path(&path, base);
        Some((shown, text))
    }

    /// D03: a `task{kind:"worker", step}` call: the step from the plan
    /// being implemented, its scope, and the brief the harness writes.
    async fn start_worker(
        &mut self,
        tc: &ToolCall,
        config: &SubagentConfig,
        brief: Brief,
        resumed: bool,
    ) -> Result<Run<R>, String> {
        let n = brief.step.unwrap_or(0);
        let Some((plan_path, plan)) = self.implementing_plan().await else {
            return Err("Error: workers carry out steps of a saved plan, and this run isn't implementing one. Do the work yourself.".into());
        };
        let steps = super::plan::steps(&plan);
        let Some(step) = steps.iter().find(|s| s.number == n).cloned() else {
            return Err(format!(
                "Error: the plan {plan_path} has no step {n}; its steps are {}.",
                steps
                    .iter()
                    .map(|s| s.number.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let existing = if resumed {
            let (parent, call) = (self.dialog_id.clone(), tc.id.clone());
            self.db
                .read(move |c| store::child_for_call(c, &parent, &call))
                .await?
        } else {
            None
        };
        let child_id = match &existing {
            Some(child) => child.id.clone(),
            None => {
                if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
                    return Err(why);
                }
                self.ledger.admit_worker(&brief, steps.len())?;
                uuid::Uuid::new_v4().to_string()
            }
        };
        let scope = WriteScope::for_step(&step);
        let root = self.root.clone().unwrap_or_default();
        let changes: Vec<(String, Option<String>, Option<String>)> = step
            .files
            .iter()
            .filter(|f| !f.path.ends_with('/'))
            .filter_map(|f| {
                let key = super::repair::rel_key(&root, &f.path);
                let before = self.original_of(&key)?;
                let after = std::fs::read(tools::to_absolute_path(&key, &root))
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).into_owned());
                Some((key, before, after))
            })
            .collect();
        let diff = worker::files_diff(&changes);
        let lead_note = (!brief.goal.starts_with("Carry out step "))
            .then(|| brief.goal.clone())
            .into_iter()
            .chain(brief.context.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let message = worker::brief(&worker::BriefParts {
            plan_path: &plan_path,
            plan: &plan,
            step: &step,
            done: &self.worker_steps,
            diff: &diff,
            scope: &scope,
            note: Some(lead_note.as_str()).filter(|n| !n.is_empty()),
        });
        let budget = brief.budget(config.budget(Kind::Worker));
        let mut sub = self.new_sub(Kind::Worker, &tc.id, budget, config).await?;
        sub.scope = Some(scope);
        sub.step = Some(n);
        let goal = format!("Step {n}: {}", step.title);
        match &existing {
            Some(_) => self.close_child_calls(&child_id).await?,
            None => {
                self.store_child(&child_id, &tc.id, Kind::Worker, &goal, message)
                    .await?
            }
        }
        self.launch_child(child_id, &tc.id, &goal, sub)
    }

    /// E02: a `task{kind:"testWriter"}` call: the brief has the request,
    /// the plan, and the outlines of the files the change touches, which it
    /// can't read or search.
    async fn start_test_writer(
        &mut self,
        tc: &ToolCall,
        config: &SubagentConfig,
        brief: Brief,
        resumed: bool,
    ) -> Result<Run<R>, String> {
        let plan = self.implementing_plan().await;
        let root = self.root.clone().unwrap_or_default();
        let mut touched: Vec<String> = Vec::new();
        if let Some((_, text)) = &plan {
            for step in super::plan::steps(text) {
                for f in step.files {
                    let key = super::repair::rel_key(&root, &f.path);
                    if !f.path.ends_with('/') && !touched.contains(&key) {
                        touched.push(key);
                    }
                }
            }
        }
        for key in self.touched_keys() {
            if !touched.contains(&key) {
                touched.push(key);
            }
        }
        touched.retain(|k| !super::project::is_test_path(k));
        let outlines: Vec<(String, Option<String>)> = touched
            .iter()
            .map(|k| {
                let exists = std::path::Path::new(&tools::to_absolute_path(k, &root)).is_file();
                let outline = exists
                    .then(|| crate::agent_tools::tool_outline(root.clone(), k.clone()).ok())
                    .flatten();
                (k.clone(), outline)
            })
            .collect();
        let existing = if resumed {
            let (parent, call) = (self.dialog_id.clone(), tc.id.clone());
            self.db
                .read(move |c| store::child_for_call(c, &parent, &call))
                .await?
        } else {
            None
        };
        let child_id = match &existing {
            Some(child) => child.id.clone(),
            None => {
                if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
                    return Err(why);
                }
                self.ledger.admit(&brief, config.max_per_run())?;
                uuid::Uuid::new_v4().to_string()
            }
        };
        let request = self.latest_user_request().await;
        let test_check = self
            .checks
            .iter()
            .find(|c| c.kind == "test" && c.enabled)
            .map(|c| c.command.clone());
        let note = (brief.goal != "Write tests for the change")
            .then(|| brief.goal.clone())
            .into_iter()
            .chain(brief.context.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let message = super::test_writer::brief(&super::test_writer::BriefParts {
            request: &request,
            plan: plan.as_ref().map(|(p, t)| (p.as_str(), t.as_str())),
            outlines: &outlines,
            note: Some(note.as_str()).filter(|n| !n.is_empty()),
            test_check: test_check.as_deref(),
        });
        let budget = brief.budget(config.budget(Kind::TestWriter));
        let mut sub = self
            .new_sub(Kind::TestWriter, &tc.id, budget, config)
            .await?;
        sub.tests_only = true;
        sub.hidden = touched;
        sub.light_bash = true;
        let goal = brief.goal.clone();
        match &existing {
            Some(_) => self.close_child_calls(&child_id).await?,
            None => {
                self.store_child(&child_id, &tc.id, Kind::TestWriter, &goal, message)
                    .await?
            }
        }
        self.launch_child(child_id, &tc.id, &goal, sub)
    }

    /// A worker's step when it's continued (A04): from its brief's heading.
    async fn worker_step_of(&self, child_id: &str) -> Option<super::plan::PlanStep> {
        let id = child_id.to_string();
        let first = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 1))
            .await
            .ok()?
            .messages
            .into_iter()
            .next()?;
        let rest = first.content.strip_prefix("# Your step: ")?;
        let n: usize = rest[..rest.find('.')?].trim().parse().ok()?;
        let (_, plan) = self.implementing_plan().await?;
        super::plan::steps(&plan)
            .into_iter()
            .find(|s| s.number == n)
    }

    /// D04: after a worker, on the lead's side: its report against the
    /// files it really changed, the step it finished, and the checklist
    /// with that step done (returned, for the result's meta).
    async fn after_worker(&mut self, result: &mut SubResult) -> Option<Vec<super::plan::TodoItem>> {
        if result.kind != Kind::Worker {
            return None;
        }
        let actual: Vec<String> = result.writes.iter().map(|w| w.path.clone()).collect();
        if matches!(
            result.status,
            SubStatus::Done | SubStatus::Partial | SubStatus::Unformatted
        ) {
            if let Some(note) =
                worker::mismatch_note(&worker::reported_files(&result.text), &actual)
            {
                result.notes.push(note);
            }
        }
        let n = result.worker.as_ref()?.get("step")?.as_u64()? as usize;
        if result.status != SubStatus::Done {
            return None;
        }
        let (_, plan) = self.implementing_plan().await?;
        let steps = super::plan::steps(&plan);
        let step = steps.iter().find(|s| s.number == n)?;
        let summary = subagent::sections(&result.text)
            .get("Summary")
            .map(|s| s.lines().next().unwrap_or("").trim().to_string())
            .unwrap_or_default();
        self.worker_steps.retain(|d| d.number != n);
        self.worker_steps.push(worker::DoneStep {
            number: n,
            title: step.title.clone(),
            summary: crate::chat_db::store::char_prefix(&summary, 300).to_string(),
        });
        self.worker_steps.sort_by_key(|d| d.number);
        let recent = self.recent().await.ok()?;
        let current = super::plan::current_checklist(&recent)?;
        let items = worker::mark_step_done(&current.items, step, steps.len())?;
        emit(
            &self.app,
            AgentEvent::Checklist {
                dialog_id: self.dialog_id.clone(),
                items: items.clone(),
            },
        );
        Some(items)
    }

    /// D06, on the worker's side, before its result: a failed or stopped
    /// worker's changes are rolled back; a partial one keeps them, listed.
    /// Returns the notes for the lead and the facts for its block.
    async fn writer_end(&mut self, status: SubStatus) -> (Vec<String>, Option<Value>) {
        let Some(sub) = self.sub() else {
            return (Vec::new(), None);
        };
        if !sub.kind.writes() {
            return (Vec::new(), None);
        }
        let (step, grants, scope) = (
            sub.step,
            sub.scope
                .as_ref()
                .map(|s| s.grants.clone())
                .unwrap_or_default(),
            sub.scope.as_ref().map(|s| s.describe()),
        );
        let mut notes = Vec::new();
        let mut rolled_back = None;
        if matches!(status, SubStatus::Failed | SubStatus::Stopped) {
            if let Some(rb) = self.roll_back(Vec::new()).await {
                let mut note = String::from("Its changes were undone");
                if rb.restored.is_empty() && rb.skipped.is_empty() {
                    note = "It changed no files.".into();
                } else {
                    if !rb.restored.is_empty() {
                        note.push_str(&format!(
                            ": {} put back as they were",
                            rb.restored.join(", ")
                        ));
                    }
                    if !rb.skipped.is_empty() {
                        note.push_str(&format!(
                            "{}left as they are, because they changed outside it meanwhile: {}",
                            if rb.restored.is_empty() {
                                ", except: "
                            } else {
                                "; "
                            },
                            rb.skipped.join(", ")
                        ));
                    }
                    note.push('.');
                }
                notes.push(note);
                rolled_back = Some(rb);
            }
        }
        let kept: Vec<String> = self.child_writes().iter().map(|w| w.path.clone()).collect();
        if status == SubStatus::Partial && !kept.is_empty() {
            notes.push(format!(
                "Its changes are kept: {}. Finish the step (continue it, or do the rest yourself), or undo them.",
                kept.join(", ")
            ));
        }
        if !grants.is_empty() {
            notes.push(format!(
                "It was granted, beyond its step: {}.",
                grants.join(", ")
            ));
        }
        let facts = json!({
            "step": step,
            "scope": scope,
            "grants": grants,
            "rolledBack": rolled_back,
            "changed": kept,
        });
        (notes, Some(facts))
    }
}

/// `Blocked` when the report says so, else `otherwise`.
fn status_of(text: &str, otherwise: SubStatus) -> SubStatus {
    if subagent::is_blocked(text) {
        SubStatus::Blocked
    } else {
        otherwise
    }
}

fn error_kind(error: &ChatError) -> String {
    serde_json::to_value(error.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{:?}", error.kind))
}

/// The run log's one line per subagent (S09): kind, model, calls, tokens
/// when reported, time, status.
fn subagent_log_line(sub: &SubCtx, dialog_id: &str, model: &str, result: &SubResult) -> Value {
    let mut line = json!({
        "event": "subagent",
        "role": "sub",
        "dialogId": dialog_id,
        "parentDialogId": sub.parent_dialog_id,
        "kind": sub.kind.id(),
        "model": model,
        "modelsTried": sub.models_tried,
        "toolCalls": result.tool_calls,
        "ms": result.ms,
        "status": result.status.id(),
    });
    if sub.usage_seen {
        line["inputTokens"] = json!(sub.tokens_in);
        line["outputTokens"] = json!(sub.tokens_out);
    }
    if let Some(error) = &result.error {
        line["error"] = json!(error);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SubCtx {
        let profile: ModelProfile = serde_json::from_value(json!({
            "id": "p::m", "label": "Test/m", "model": "m", "baseUrl": "http://x",
        }))
        .unwrap();
        SubCtx {
            kind: Kind::Scout,
            parent_dialog_id: "lead".into(),
            parent_call_id: "c1".into(),
            parent_request: "Fix it".into(),
            run_id: "r".into(),
            budget: Kind::Scout.spec().default_budget,
            models: vec![profile],
            model_index: 0,
            models_tried: vec!["Test/m".into()],
            options: None,
            kind_options: None,
            chat_profile_id: "p::m".into(),
            chat_options: None,
            waited: Mutex::new(Duration::ZERO),
            trimmed: false,
            failures: Vec::new(),
            extra_tools: Vec::new(),
            tokens_in: 0,
            tokens_out: 0,
            usage_seen: false,
            cost: None,
            scope: None,
            step: None,
            tests_only: false,
            hidden: Vec::new(),
            light_bash: false,
        }
    }

    #[test]
    fn the_log_line_has_the_kind_and_tokens_when_reported() {
        let mut sub = ctx();
        let result = SubResult {
            kind: Kind::Scout,
            status: SubStatus::Done,
            text: String::new(),
            child_dialog_id: Some("k".into()),
            model: Some("Test/m".into()),
            models_tried: vec!["Test/m".into()],
            tool_calls: 4,
            ms: 1200,
            error: None,
            files_read: vec![],
            writes: vec![],
            failed_models: vec![],
            answered_by: None,
            usage: None,
            notes: vec![],
            worker: None,
        };
        let line = subagent_log_line(&sub, "k", "m", &result);
        assert_eq!(line["event"], "subagent");
        assert_eq!(line["kind"], "scout");
        assert_eq!(line["status"], "done");
        assert_eq!(line["toolCalls"], 4);
        assert!(line.get("inputTokens").is_none(), "no usage reported");

        sub.note_usage(Some(
            &json!({ "prompt_tokens": 100, "completion_tokens": 20 }),
        ));
        sub.note_usage(Some(&json!({ "input_tokens": 5, "output_tokens": 1 })));
        sub.note_usage(None);
        let line = subagent_log_line(&sub, "k", "m", &result);
        assert_eq!(line["inputTokens"], 105);
        assert_eq!(line["outputTokens"], 21);
    }

    #[test]
    fn waiting_for_approval_is_counted_apart() {
        let sub = ctx();
        sub.add_waited(Duration::from_secs(600));
        sub.add_waited(Duration::from_secs(5));
        assert_eq!(sub.waited(), Duration::from_secs(605));
    }
}

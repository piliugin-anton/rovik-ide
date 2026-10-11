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
    /// A worker in a parallel group (F04): what the group's other steps own.
    pub group: Option<super::group::GroupCtx>,
    /// The test writer: it writes only test files, and can't read or
    /// search these (the files the change touches, K11).
    pub tests_only: bool,
    pub hidden: Vec<String>,
    /// Only light commands: no builds or test runs (E02, F05).
    pub light_bash: bool,
    /// Resumed after the app closed: the files it wrote before then. They
    /// count as its changes, but aren't rolled back (what they were before
    /// it isn't known any more).
    pub resumed_writes: Vec<String>,
    /// Its registration, from `launch_child` on (P1.1): dropping a child
    /// that never started (or panicked) unregisters it.
    pub registered: Option<super::Registered>,
    /// A group worker continued in the copy it kept (a conflict, or a
    /// landing that waits or failed): what it changes there lands when it
    /// ends (P1.2, P1.3).
    pub kept_copy: bool,
    /// A step worker outside a group: the plan's other steps' scopes,
    /// which extend_scope doesn't grant (P2.3).
    pub other_steps: Vec<(usize, WriteScope)>,
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
    /// Answered without a child of its own: a refusal, or a whole group of
    /// parallel workers (group.rs), with the result's meta.
    Answered(String, Option<Value>),
}

pub(super) type SubFuture = Pin<Box<dyn Future<Output = Option<SubResult>> + Send>>;

/// A subagent's whole run as a boxed future: a run's future can't contain
/// itself (a child's tool calls go through the same code as the lead's).
/// Tests: the subagent answering this `task` call panics as it starts.
#[cfg(test)]
pub static PANIC_SUB: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Tests: the subagent for this `task` call can't be launched.
#[cfg(test)]
pub static FAIL_LAUNCH: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(super) fn go_sub<R: Runtime>(run: Run<R>) -> SubFuture {
    // Unregistered however it ends, by the guard in its `SubCtx`: a child
    // that panicked would otherwise stay "running" and its chat couldn't be
    // deleted.
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
        // Writing subagents run one at a time anyway; each is started only
        // once the one before it ended, so its brief (the steps done, the
        // files as they are) includes that one's work.
        let mut any_writes = false;
        if calls.len() > 1 {
            for tc in calls {
                if self.task_writes(tc).await {
                    any_writes = true;
                    break;
                }
            }
        }
        if any_writes {
            for tc in calls {
                Box::pin(self.execute_tasks(std::slice::from_ref(tc), resumed)).await?;
            }
            return Ok(());
        }
        let config = self.state.config().subagents;
        let mut jobs: Vec<Job<R>> = Vec::with_capacity(calls.len());
        for tc in calls {
            if self.stopped() {
                jobs.push(Job::Answered(
                    "Error: stopped by the user before this ran.".into(),
                    None,
                ));
                continue;
            }
            // F04: a plan group, run as parallel workers and joined here.
            if let Some(group) = super::group::group_call(&tc.arguments) {
                let (text, meta) = self.run_group(tc, &group, &config).await;
                jobs.push(Job::Answered(text, meta));
                continue;
            }
            jobs.push(match self.start_task(tc, &config, resumed).await {
                Ok(child) => Job::Run(Box::new(child)),
                Err(text) => Job::Answered(text, None),
            });
        }
        let started = Instant::now();
        let running = jobs.iter().filter(|j| matches!(j, Job::Run(_))).count();
        let jobs_write = jobs.iter().any(|j| match j {
            Job::Run(child) => child.sub().is_some_and(|s| s.kind.writes()),
            Job::Answered(..) => false,
        });
        let throttled = jobs.iter().any(|j| match j {
            Job::Run(child) => self.ledger.is_throttled(&child.profile.id),
            Job::Answered(..) => false,
        });
        #[cfg(test)]
        super::parallel::BATCHES.lock().unwrap().push((
            self.dialog_id.clone(),
            calls.iter().map(|c| c.id.clone()).collect(),
        ));
        // Workers continued in a kept copy: it lands when they end.
        let kept: Vec<Option<String>> = jobs
            .iter()
            .map(|j| match j {
                Job::Run(child) => child
                    .sub()
                    .is_some_and(|s| s.kept_copy)
                    .then(|| child.dialog_id.clone()),
                Job::Answered(..) => None,
            })
            .collect();
        let mut answered: Vec<Option<(String, Option<Value>)>> = Vec::with_capacity(jobs.len());
        let futures: Vec<SubFuture> = jobs
            .into_iter()
            .map(|job| match job {
                Job::Answered(text, meta) => {
                    answered.push(Some((text, meta)));
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
        for ((tc, refusal), kept) in calls.iter().zip(answered).zip(kept) {
            let result = results.next().await.flatten();
            let (content, meta): (String, Option<Value>) = match (refusal, result) {
                (Some((text, meta)), _) => (text, meta),
                (None, Some(mut result)) => {
                    if let Some(child) = kept {
                        self.land_kept(&child, &mut result).await;
                    }
                    self.absorb_child_writes(&result.writes);
                    if result.kind == Kind::TestWriter {
                        for w in &result.writes {
                            if !self.test_writer_files.contains(&w.path) {
                                self.test_writer_files.push(w.path.clone());
                            }
                        }
                        // E04: tests that pass on the code before the change.
                        if config.red_check {
                            if let Some(note) = self.red_check(&result).await {
                                result.notes.push(note);
                            }
                        }
                    }
                    self.note_models(&result);
                    let update = self.after_worker(&mut result, None).await;
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
            if let Err(e) = self
                .add_tool_result(&tc.id, content, meta, Vec::new())
                .await
            {
                // P1.1: the run ends in an error; its subagents end first,
                // stopped, so none is left running (or registered).
                self.state.stop_children(&self.app, &self.dialog_id);
                while results.next().await.is_some() {}
                return Err(e);
            }
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
                .start_follow_up(tc, config, &kinds, handle, &args, resumed)
                .await;
        }
        if let Some(kind) = args
            .get("kind")
            .and_then(|k| k.as_str())
            .and_then(Kind::parse)
            .filter(|k| self.plan_mode && k.writes())
        {
            return Err(plan_mode_refusal(kind));
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
                uuid::Uuid::new_v4().to_string()
            }
        };

        let budget = brief.budget(config.budget(kind));
        let sub = self.new_sub(kind, &tc.id, budget, config).await?;
        // Counted once it can start (P2.7): no models left isn't a start.
        if existing.is_none() {
            self.ledger.admit(&brief, config.max_per_run())?;
        }
        let message = existing.is_none().then(|| brief.message());
        let admitted = existing.is_none().then_some(&brief);
        self.store_and_launch(&child_id, &tc.id, kind, &brief.goal, message, sub, admitted)
            .await
    }

    /// Whether a `task` call starts or continues a writing subagent; a
    /// continued one by the kind its chat was started with, however it's
    /// named (a handle or the call that started it, P2.8).
    async fn task_writes(&self, tc: &ToolCall) -> bool {
        if writes_task(tc) {
            return true;
        }
        let args = permissions::parse_args(&tc.arguments);
        let Some(handle) = args
            .get("continue")
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return false;
        };
        let (parent, key) = (self.dialog_id.clone(), handle.to_string());
        self.db
            .read(move |c| store::child_by_handle(c, &parent, &key))
            .await
            .ok()
            .flatten()
            .and_then(|child| Kind::parse(&child.kind))
            .is_some_and(|k| k.writes())
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
        resumed: bool,
    ) -> Result<Run<R>, String> {
        let goal = args
            .get("goal")
            .and_then(|g| g.as_str())
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .ok_or("Error: \"goal\" is required: the follow-up question.")?
            .to_string();
        // The same limits as a new subagent's brief (P2.8).
        if goal.chars().count() > subagent::MAX_GOAL_CHARS {
            return Err(format!(
                "Error: \"goal\" is too long ({} characters, at most {}). Ask one focused question.",
                goal.chars().count(),
                subagent::MAX_GOAL_CHARS
            ));
        }
        let max_tool_calls = args
            .get("max_tool_calls")
            .and_then(|n| n.as_f64())
            .filter(|n| n.is_finite() && *n >= 1.0)
            .map(|n| n.floor() as u32);
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
        if let Some(kind) = Kind::parse(&child.kind).filter(|k| self.plan_mode && k.writes()) {
            return Err(plan_mode_refusal(kind));
        }
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
            max_tool_calls,
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
            .new_sub(kind, &tc.id, brief.budget(config.budget(kind)), config)
            .await?;
        if let Some(step) = &step {
            let mut scope = WriteScope::for_step(step);
            // What it was granted before goes on (P2.3).
            scope.grants = self.grants_of(&child.id).await;
            sub.scope = Some(scope);
            sub.step = Some(step.number);
            if let Some((_, plan)) = self.implementing_plan().await {
                sub.other_steps = other_steps(&super::plan::steps(&plan), step.number);
            }
        }
        // A test writer asked more keeps its limits.
        if kind == Kind::TestWriter {
            let hidden = self.test_writer_hidden().await;
            self.limit_test_writer(&mut sub, hidden);
        }
        // A group worker whose copy was kept goes on in it (P1.2, P1.3),
        // opened again if the app closed since (round 2).
        let kept_view = if kind == Kind::Worker {
            let (parent, id) = (self.dialog_id.clone(), child.id.clone());
            tauri::async_runtime::spawn_blocking(move || {
                crate::overlay::chats::reopen_child(&parent, &id).map(|o| o.view.clone())
            })
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        if kept_view.is_some() {
            sub.kept_copy = true;
            sub.light_bash = true;
        }
        self.close_child_calls(&child.id).await?;
        // Resumed after the app closed: the question may be in its chat
        // already, and it goes on from there.
        let (id, call) = (child.id.clone(), tc.id.clone());
        let asked = resumed
            && self
                .db
                .read(move |c| {
                    Ok(store::get_messages(c, &id, None, 500)?
                        .messages
                        .iter()
                        .any(|m| {
                            m.meta.as_ref().is_some_and(|meta| {
                                meta["subagentBrief"]["parentCallId"] == call.as_str()
                            })
                        }))
                })
                .await?;
        if !asked {
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
        }
        let mut run = self.launch_child(child.id, &tc.id, &goal, sub)?;
        if let Some(view) = kept_view {
            run.root = Some(view.to_string_lossy().into_owned());
            run.checkpoint = None;
        }
        Ok(run)
    }

    /// A subagent's context, before it has a run. Err: none of its models
    /// can be used (they kept failing in this run, S12).
    pub(super) async fn new_sub(
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
            group: None,
            tests_only: false,
            hidden: Vec::new(),
            light_bash: false,
            resumed_writes: Vec::new(),
            registered: None,
            kept_copy: false,
            other_steps: Vec::new(),
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
    pub(super) fn launch_child(
        &self,
        child_id: String,
        call_id: &str,
        goal: &str,
        mut sub: SubCtx,
    ) -> Result<Run<R>, String> {
        #[cfg(test)]
        if FAIL_LAUNCH.lock().unwrap().as_deref() == Some(call_id) {
            return Err("Error: that subagent is already running.".into());
        }
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
        sub.registered = Some(self.state.registered(&child_id, &controls.run_id));
        let focus: Vec<String> = sub
            .scope
            .as_ref()
            .map(|s| s.files.iter().chain(&s.folders).cloned().collect())
            .unwrap_or_default();
        let map = self.kind_map(sub.kind, &focus);
        let writes = sub.kind.writes();
        self.state.describe_child(
            &child_id,
            super::ChildInfo {
                kind: sub.kind.id().into(),
                goal: goal.to_string(),
                model: sub.models[0].label.clone(),
                started_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
                tool_calls: 0,
            },
        );
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
    pub(super) async fn store_child(
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
    pub(super) async fn close_child_calls(&self, child_id: &str) -> Result<(), String> {
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
        self.state
            .note_child_progress(&self.dialog_id, self.guard.tool_calls, &self.profile.label);
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
        #[cfg(test)]
        if self.sub().is_some_and(|s| {
            PANIC_SUB
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref()
                == Some(s.parent_call_id.as_str())
        }) {
            panic!("a test's subagent panics");
        }
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
            let now = std::fs::read(tools::to_absolute_path(path, &root)).ok();
            if now != original.bytes {
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
            let abs = tools::to_absolute_path(&w.path, &root);
            if let Some(cp) = self.checkpoint.as_mut() {
                note_round_start(cp, w);
            }
            if let Ok(now) = std::fs::read(&abs) {
                self.seen.record(std::path::Path::new(&abs), &now);
            }
            self.writes_since_verify = true;
            self.writes_since_review = true;
            self.map_writes_since_refresh += 1;
        }
    }

    /// S12: what a subagent's end says about its models, for the next ones.
    pub(super) fn note_models_of(&mut self, result: &SubResult) {
        self.note_models(result)
    }

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
    /// A subagent's working time left (time waiting for the user aside);
    /// None for the lead.
    pub(super) fn wall_left(&self) -> Option<Duration> {
        let sub = self.sub()?;
        let worked = self.started_at.elapsed().saturating_sub(sub.waited());
        Some(sub.budget.wall.saturating_sub(worked))
    }

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

            // Cut for repeating itself: the stream was cancelled to cut it,
            // which isn't a failure or the user's Stop (handled below).
            let repeated = turn.repetition_cut.is_some() && !self.stopped();
            if let Some(error) = turn.failure.filter(|_| !repeated) {
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
            // The budget holds within a reply too: calls past it aren't run,
            // and each gets a result saying so.
            let left = self.sub().map_or(usize::MAX, |s| {
                s.budget.tool_calls.saturating_sub(self.guard.tool_calls) as usize
            });
            let (run_now, over) = calls.split_at(left.min(calls.len()));
            if !run_now.is_empty() {
                self.execute_tool_calls(run_now, None).await?;
            }
            for tc in over {
                let content = format!(
                    "Error: not run: it's past your budget of {} tool calls. Write your report now with what you have.",
                    self.sub().map_or(0, |s| s.budget.tool_calls)
                );
                self.add_tool_result(&tc.id, content, None, Vec::new())
                    .await?;
            }
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
        // Its own window: the trim the model before used isn't this one's.
        sub.trimmed = false;
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
    pub(super) async fn implementing_plan(&self) -> Option<(String, String)> {
        let recent = self.recent().await.ok()?;
        let base = self.project_root.as_deref().or(self.root.as_deref());
        // The latest request that named a plan (a follow-up in the chat
        // still works on it), else the chat's own plan file.
        let path = recent
            .iter()
            .rev()
            .filter(|m| m.role == "user" && !super::plan::is_harness(m))
            .find_map(|m| worker::plan_file_named(&m.content, base))
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
                uuid::Uuid::new_v4().to_string()
            }
        };
        let scope = WriteScope::for_step(&step);
        let diff = self.step_diff(&step);
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
        if existing.is_none() {
            self.ledger.admit_worker(&brief, steps.len())?;
        }
        sub.scope = Some(scope);
        sub.step = Some(n);
        sub.other_steps = other_steps(&steps, n);
        let goal = format!("Step {n}: {}", step.title);
        let message = existing.is_none().then_some(message);
        let admitted = existing.is_none().then_some(&brief);
        let mut child = self
            .store_and_launch(
                &child_id,
                &tc.id,
                Kind::Worker,
                &goal,
                message,
                sub,
                admitted,
            )
            .await?;
        if existing.is_some() {
            self.resume_child_writes(&mut child, &child_id).await;
        }
        Ok(child)
    }

    /// Stores a new child's chat (`message`: its brief) or reopens a
    /// resumed one's (None), and launches it. A new one that can't start
    /// gives back the place `admitted` took (round 2, P1.1).
    #[allow(clippy::too_many_arguments)]
    async fn store_and_launch(
        &mut self,
        child_id: &str,
        call_id: &str,
        kind: Kind,
        goal: &str,
        message: Option<String>,
        sub: SubCtx,
        admitted: Option<&Brief>,
    ) -> Result<Run<R>, String> {
        let resumed = message.is_none();
        let started = async {
            match message {
                None => self.close_child_calls(child_id).await?,
                Some(m) => self.store_child(child_id, call_id, kind, goal, m).await?,
            }
            let launched = self.launch_child(child_id.to_string(), call_id, goal, sub);
            self.launched(launched, child_id, resumed).await
        }
        .await;
        if started.is_err() {
            if let Some(brief) = admitted {
                self.ledger.release(brief);
            }
        }
        started
    }

    /// A child just launched (P2.7): one resumed after a restart goes on
    /// with the tool calls it already made counted; one that couldn't
    /// launch leaves no chat behind.
    async fn launched(
        &mut self,
        launched: Result<Run<R>, String>,
        child_id: &str,
        resumed: bool,
    ) -> Result<Run<R>, String> {
        match launched {
            Ok(mut child) => {
                if resumed {
                    let id = child_id.to_string();
                    let made = self
                        .db
                        .read(move |c| store::get_messages(c, &id, None, 500))
                        .await
                        .map(|page| {
                            page.messages
                                .iter()
                                .map(|m| m.tool_calls.as_ref().map_or(0, Vec::len))
                                .sum::<usize>()
                        })
                        .unwrap_or(0);
                    child.guard.tool_calls = made as u32;
                }
                Ok(child)
            }
            Err(e) => {
                if !resumed {
                    let id = child_id.to_string();
                    let _ = self
                        .db
                        .write(move |c| store::delete_dialog(c, &id))
                        .await;
                }
                Err(e)
            }
        }
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
        let touched = self.test_writer_hidden().await;
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
        if existing.is_none() {
            self.ledger.admit(&brief, config.max_per_run())?;
        }
        self.limit_test_writer(&mut sub, touched);
        let goal = brief.goal.clone();
        let message = existing.is_none().then_some(message);
        let admitted = existing.is_none().then_some(&brief);
        let mut child = self
            .store_and_launch(
                &child_id,
                &tc.id,
                Kind::TestWriter,
                &goal,
                message,
                sub,
                admitted,
            )
            .await?;
        if existing.is_some() {
            self.resume_child_writes(&mut child, &child_id).await;
        }
        Ok(child)
    }

    /// A writing subagent resumed after the app closed: the files it wrote
    /// before then, from its chat. Each counts as its change (and the
    /// lead's) from what it was before this run, where that's known (the
    /// project's copy, in an isolated chat). They're never rolled back:
    /// what they were just before it isn't known any more.
    async fn resume_child_writes(&mut self, child: &mut Run<R>, child_id: &str) {
        let id = child_id.to_string();
        let Ok(page) = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await
        else {
            return;
        };
        let root = self.root.clone().unwrap_or_default();
        let failed = |call: &str| {
            page.messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(call) && m.content.starts_with("Error"))
        };
        let mut paths: Vec<String> = Vec::new();
        for m in &page.messages {
            for tc in m.tool_calls.iter().flatten() {
                if tc.name != "write" || failed(&tc.id) {
                    continue;
                }
                let args = permissions::parse_args(&tc.arguments);
                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                    let key = super::repair::rel_key(&root, path);
                    if !paths.contains(&key) {
                        paths.push(key);
                    }
                }
            }
        }
        for key in &paths {
            if let Some(original) = self.original_bytes_of(key) {
                child.baseline.entry(key.clone()).or_insert(original);
            }
            // The lead's checks and reviewer look at it either way.
            if !self
                .changed_files
                .iter()
                .any(|c| super::repair::rel_key(&root, c) == *key)
            {
                self.changed_files.push(key.clone());
            }
            self.writes_since_verify = true;
            self.writes_since_review = true;
        }
        if let Some(sub) = child.sub_mut() {
            sub.resumed_writes = paths;
        }
    }

    /// The files the test writer may not read (E02): those the plan names
    /// and those this run changed, test files aside.
    async fn test_writer_hidden(&self) -> Vec<String> {
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
        touched
    }

    /// The test writer's limits (E02): test files only, the change's code
    /// hidden, checks only with bash. Every run of it, follow-ups included.
    fn limit_test_writer(&self, sub: &mut SubCtx, hidden: Vec<String>) {
        sub.tests_only = true;
        sub.hidden = hidden;
        sub.light_bash = true;
    }

    /// The step's files as they changed in this run so far, for its brief.
    pub(super) fn step_diff(&self, step: &super::plan::PlanStep) -> String {
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
        worker::files_diff(&changes)
    }

    /// What a worker was granted (extend_scope results' `meta.granted`).
    pub(super) async fn grants_of(&self, child_id: &str) -> Vec<String> {
        let id = child_id.to_string();
        let Ok(page) = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await
        else {
            return Vec::new();
        };
        let mut grants: Vec<String> = Vec::new();
        for m in &page.messages {
            let granted = m.meta.as_ref().and_then(|meta| meta.get("granted"));
            for g in granted.and_then(|g| g.as_array()).into_iter().flatten() {
                if let Some(g) = g.as_str().filter(|g| !grants.iter().any(|x| x == g)) {
                    grants.push(g.to_string());
                }
            }
        }
        grants
    }

    /// A worker's step when it's continued (A04): from its brief's heading.
    pub(super) async fn worker_step_of(&self, child_id: &str) -> Option<super::plan::PlanStep> {
        let id = child_id.to_string();
        // Its brief: the first message (a page is the latest ones, so the
        // brief is found by its heading).
        let first = self
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await
            .ok()?
            .messages
            .into_iter()
            .find(|m| m.role == "user" && m.content.starts_with("# Your step: "))?;
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
    pub(super) async fn after_worker(
        &mut self,
        result: &mut SubResult,
        base: Option<&[super::plan::TodoItem]>,
    ) -> Option<Vec<super::plan::TodoItem>> {
        if result.kind != Kind::Worker {
            return None;
        }
        let actual: Vec<String> = result.writes.iter().map(|w| w.path.clone()).collect();
        let mut mismatch = false;
        if matches!(
            result.status,
            SubStatus::Done | SubStatus::Partial | SubStatus::Unformatted
        ) {
            if let Some(note) =
                worker::mismatch_note(&worker::reported_files(&result.text), &actual)
            {
                result.notes.push(note);
                mismatch = true;
            }
        }
        let n = result.worker.as_ref()?.get("step")?.as_u64()? as usize;
        // P2.9: "done" with nothing written, or not what it says it wrote,
        // isn't a step done.
        if result.status == SubStatus::Done && (actual.is_empty() || mismatch) {
            result.status = SubStatus::Partial;
            result.notes.push(format!(
                "Step {n} isn't marked done: {}. Check it, then continue the worker or finish the step yourself.",
                if actual.is_empty() {
                    "it said it was done but changed no files"
                } else {
                    "what it changed isn't what it reported"
                }
            ));
        }
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
        // The list so far: a group's earlier joins, else the chat's.
        let current = match base {
            Some(items) => items.to_vec(),
            None => {
                let recent = self.recent().await.ok()?;
                super::plan::current_checklist(&recent)?.items
            }
        };
        let items = worker::mark_step_done(&current, step, steps.len())?;
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
        let resumed = self
            .sub()
            .map(|s| s.resumed_writes.clone())
            .unwrap_or_default();
        if matches!(status, SubStatus::Failed | SubStatus::Stopped) {
            if let Some(rb) = self.roll_back(Vec::new()).await {
                let mut note = String::from("Its changes were undone");
                if rb.restored.is_empty() && rb.skipped.is_empty() {
                    note = if resumed.is_empty() {
                        "It changed no files.".into()
                    } else {
                        "Nothing it changed after the app closed needed undoing.".into()
                    };
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
        if matches!(status, SubStatus::Failed | SubStatus::Stopped) && !resumed.is_empty() {
            notes.push(format!(
                "What it wrote before the app closed is kept, not undone: {}. Check it, or undo it yourself.",
                resumed.join(", ")
            ));
        }
        let kept: Vec<String> = self.child_writes().iter().map(|w| w.path.clone()).collect();
        // P2.9: a blocked or unformatted worker's half step is kept too.
        if matches!(
            status,
            SubStatus::Partial | SubStatus::Blocked | SubStatus::Unformatted
        ) && !kept.is_empty()
        {
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

/// Whether a `task` call starts (or continues) a subagent that writes, as
/// its arguments say (see `Run::task_writes` for a continued one).
fn writes_task(tc: &ToolCall) -> bool {
    let args = permissions::parse_args(&tc.arguments);
    if super::group::group_call(&tc.arguments).is_some() {
        return true;
    }
    let kind = |id: &str| Kind::parse(id).is_some_and(|k| k.writes());
    if args.get("kind").and_then(|k| k.as_str()).is_some_and(kind) {
        return true;
    }
    // A handle is "<kind>-<id>".
    args.get("continue")
        .and_then(|c| c.as_str())
        .and_then(|h| h.rsplit_once('-'))
        .is_some_and(|(k, _)| kind(k))
}

/// A writing kind asked for in Plan mode, which changes no files.
fn plan_mode_refusal(kind: Kind) -> String {
    format!(
        "Error: Plan mode changes no files, so {} subagents can't run here; they run when the plan is implemented. Scouts can explore.",
        kind.spec().label.to_lowercase()
    )
}

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

/// The plan's steps other than `n`, with their scopes (P2.3).
pub(super) fn other_steps(steps: &[super::plan::PlanStep], n: usize) -> Vec<(usize, WriteScope)> {
    steps
        .iter()
        .filter(|s| s.number != n)
        .map(|s| (s.number, WriteScope::for_step(s)))
        .collect()
}

/// What the round started with, for a file a subagent changed. The
/// checkpoint holds every file the chat changed, so a path missing from it
/// was never changed: an older original (the project's copy, the run's
/// start) is still what the round started with, and the file as it is now
/// is the subagent's write.
fn note_round_start(cp: &mut super::repair::Checkpoint, w: &ChildWrite) {
    if !cp.too_big {
        cp.files
            .entry(w.path.clone())
            .or_insert_with(|| w.original.bytes.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 2, P0.3: a project-copy original goes into the checkpoint as
    /// it is, not the bytes the subagent wrote; a path the round already
    /// has keeps its entry.
    #[test]
    fn absorb_keeps_a_project_copy_original_in_the_checkpoint() {
        let mut cp = super::super::repair::Checkpoint::default();
        cp.files.insert("b.bin".into(), Some(b"round".to_vec()));
        let original = |bytes: &[u8], source| subagent::Original::new(Some(bytes.to_vec()), source);
        for (path, source) in [
            ("a.bin", subagent::Source::ProjectCopy),
            ("b.bin", subagent::Source::ProjectCopy),
            ("c.bin", subagent::Source::ViewAtRunStart),
        ] {
            let w = ChildWrite {
                path: path.into(),
                original: original(b"\xfe\x00old", source),
            };
            note_round_start(&mut cp, &w);
        }
        note_round_start(
            &mut cp,
            &ChildWrite {
                path: "new.rs".into(),
                original: subagent::Original::new(None, subagent::Source::ProjectCopy),
            },
        );
        assert_eq!(cp.files["a.bin"].as_deref(), Some(&b"\xfe\x00old"[..]));
        assert_eq!(cp.files["b.bin"].as_deref(), Some(&b"round"[..]));
        assert_eq!(cp.files["c.bin"].as_deref(), Some(&b"\xfe\x00old"[..]));
        assert_eq!(cp.files["new.rs"], None);
        // Over the limit: no rollback, so nothing is noted.
        let mut big = super::super::repair::Checkpoint {
            too_big: true,
            ..Default::default()
        };
        note_round_start(
            &mut big,
            &ChildWrite {
                path: "a.bin".into(),
                original: original(b"x", subagent::Source::ProjectCopy),
            },
        );
        assert!(big.files.is_empty());
    }

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
            group: None,
            tests_only: false,
            hidden: Vec::new(),
            light_bash: false,
            resumed_writes: Vec::new(),
            registered: None,
            kept_copy: false,
            other_steps: Vec::new(),
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

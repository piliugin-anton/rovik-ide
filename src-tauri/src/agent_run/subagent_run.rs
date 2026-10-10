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
use super::subagent::{self, Budget, Kind, SubResult, SubStatus, SubagentConfig};
use super::{failure, log, permissions, ModelProfile};
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
    /// Tokens the provider reported, when it did (S09).
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub usage_seen: bool,
}

impl SubCtx {
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
        let mut results = stream::iter(futures).buffered(config.max_at_once());
        for (tc, refusal) in calls.iter().zip(answered) {
            let result = results.next().await.flatten();
            let (content, meta): (String, Option<Value>) = match (refusal, result) {
                (Some(text), _) => (text, None),
                (None, Some(result)) => (
                    result.for_lead(),
                    Some(json!({ "subagent": result.meta() })),
                ),
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
        let brief = subagent::parse_brief(&args, &kinds)?;
        let kind = brief.kind;

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
                self.ledger.admit(&brief, config.max_per_run())?;
                uuid::Uuid::new_v4().to_string()
            }
        };

        let budget = brief.budget(config.budget(kind));
        let sub = self.new_sub(kind, &tc.id, budget, config).await;
        match &existing {
            Some(_) => self.close_child_calls(&child_id).await?,
            None => {
                self.store_child(&child_id, &tc.id, kind, &brief.goal, brief.message())
                    .await?
            }
        }
        self.launch_child(child_id, &tc.id, &brief.goal, sub)
    }

    /// A subagent's context, before it has a run.
    async fn new_sub(
        &self,
        kind: Kind,
        call_id: &str,
        budget: Budget,
        config: &SubagentConfig,
    ) -> SubCtx {
        let models = config.models(kind, &self.profile);
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
            tokens_in: 0,
            tokens_out: 0,
            usage_seen: false,
        };
        sub.options = sub.options_for(&sub.models[0]);
        sub
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
        Ok(Run::prepare_child(self, child_id, sub, controls))
    }

    /// A subagent the IDE starts itself (a reviewer), for `call_id` (the
    /// record that shows it): its brief is `message`, written by the
    /// harness. Runs to its report as a task of its own, so Stop on this
    /// run ends it cleanly. Err: it couldn't start.
    pub(super) async fn run_ide_subagent(
        &self,
        kind: Kind,
        call_id: &str,
        goal: &str,
        message: String,
    ) -> Result<(String, Option<SubResult>), String> {
        let config = self.state.config().subagents;
        let sub = self
            .new_sub(kind, call_id, config.budget(kind), &config)
            .await;
        let child_id = uuid::Uuid::new_v4().to_string();
        self.store_child(&child_id, call_id, kind, goal, message)
            .await?;
        let child = self.launch_child(child_id.clone(), call_id, goal, sub)?;
        let result = tauri::async_runtime::spawn(go_sub(child))
            .await
            .ok()
            .flatten();
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
            tokens_in: 0,
            tokens_out: 0,
            usage_seen: false,
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

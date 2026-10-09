//! One agent run: model turns until the model is done (and the checks
//! agree), the run pauses, fails, or is stopped.
//!
//! Each turn streams one reply into a stored assistant message, then runs
//! its tool calls; every result is stored before the next turn, which reads
//! the history from the database. When the model replies without tool
//! calls, the end-of-run checks decide whether it's really done: a plan
//! written in a reply instead of submitted (Plan mode), unfinished checklist
//! steps, and the verification gate (verify.rs).

use super::events::{emit, AgentEvent, OutMessage, Patch, Status};
use super::guard::Guard;
use super::project::{self, ProjectCheck};
use super::{failure, log, plan, repetition, tools, AgentState, Controls, EffortPick, ModelProfile, StartKind, StartRequest, StopSignal};
use crate::agent::{ChatParams, ChatStreamEvent, OpenAIChatOptions, ReasoningEffort, REASONING_FIELDS};
use crate::chat_db::store::{self, MessagePatch, MessageRow, NewMessage, ToolCall};
use crate::chat_db::DbState;
use crate::chat_request::{ChatError, ErrorKind};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::watch;

/// Wait this long without any output before saying so.
#[cfg(not(test))]
const STILL_WAITING: Duration = Duration::from_secs(15);
#[cfg(test)]
const STILL_WAITING: Duration = Duration::from_millis(400);
/// Mid-reply silence before "Still generating" shows.
#[cfg(not(test))]
const REPLY_IDLE: Duration = Duration::from_millis(2_500);
#[cfg(test)]
const REPLY_IDLE: Duration = Duration::from_millis(300);
/// After Stop, how long the request gets to end before it's dropped.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// A reply on screen is updated at most this often.
const UI_EVERY: Duration = Duration::from_millis(50);
/// And stored at most this often while it streams, so a crash loses little.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(2);
/// Recent messages a run reads for its bookkeeping (mode, plan, checklist).
const RECENT_MESSAGES: usize = 500;

pub(super) const RESUME_NOTE: &str =
    "Interrupted: the app was closed before this ran. Allow it to continue the task.";

const IDE_NOTE: &str = "[Automatic check by the IDE, not written by the user]\n";

pub struct Run<R: Runtime> {
    pub(super) app: AppHandle<R>,
    pub(super) state: AgentState,
    pub(super) db: DbState,
    pub(super) dialog_id: String,
    pub(super) profile: ModelProfile,
    pub(super) options: Option<OpenAIChatOptions>,
    /// The mode of the message that started the run (Plan or Agent).
    pub(super) plan_mode: bool,
    pub(super) context_mode: String,
    pub(super) root: Option<String>,
    /// The request's user message as sent (with editor context).
    pub(super) current_user_content: Option<String>,
    pub(super) local_date: String,
    pub(super) guard: Guard,
    /// ask_user calls so far (Plan mode caps them).
    pub(super) questions_asked: u32,
    /// The project's checks and the system-prompt section about it.
    pub(super) checks: Vec<ProjectCheck>,
    pub(super) project_context: String,
    /// The project section without the repository map: the map is rebuilt
    /// mid-run while this stays fixed, then recombined into project_context.
    pub(super) base_project_context: String,
    /// The current repository map text (the `## Repository map` section),
    /// if any. Updated mid-run as files change; the next model turn sends it.
    pub(super) repo_map: Option<String>,
    /// Map budget in chars (about 2% of the context window), fixed at prepare.
    pub(super) repo_map_chars: usize,
    /// Successful file writes since the map was last (re)built. Set in
    /// record(), consumed by the mid-run refresh: same-file rewrites count,
    /// so this is a counter, not changed_files' length.
    pub(super) map_writes_since_refresh: u32,
    /// When the map was last (re)built: bounds how stale external edits
    /// (the editor, git) can get before a catch-all refresh.
    pub(super) map_refreshed_at: Instant,
    /// Successful refreshes this run (for logs and tests).
    pub(super) map_refreshes: u32,
    /// Project-relative paths written in this run, in order.
    pub(super) changed_files: Vec<String>,
    /// Files changed since the gate last ran: there is something to check.
    pub(super) writes_since_verify: bool,
    pub(super) verify_rounds: u32,
    /// The last round failed and was sent back to the agent.
    pub(super) last_verify_failed: bool,
    /// Unfinished checklist steps were pointed out once.
    pub(super) checklist_nudged: bool,
    /// The next request may not call tools (a summary before a pause).
    pub(super) words_only: bool,
    /// Plan mode: the current plan, shown to the model so it revises it.
    pub(super) plan_context: String,
    /// A plan written in a reply instead of submitted was pointed out once.
    pub(super) plan_nudged: bool,
    /// Replies stopped for repeating themselves.
    pub(super) repetition_stops: u32,
    pub(super) started_at: Instant,
    pub(super) stop: Arc<StopSignal>,
    /// The reasoning effort the user picks while the run goes on.
    effort: watch::Receiver<EffortPick>,
    /// Reasoning forms the provider refused in this run: never sent again,
    /// whatever effort is picked.
    refused_formats: Vec<String>,
    /// The sent message waiting for its first answer.
    pub(super) settle_message: Option<String>,
    continue_reply: bool,
}

/// Where a run starts.
pub(super) enum Entry {
    /// With a model turn.
    Turn,
    /// With the checks of a record that waited for the user's go.
    Verify { record_id: String },
    /// With tool calls that never ran (the app closed meanwhile).
    Resume { calls: Vec<ToolCall> },
}

/// What one model turn produced.
struct TurnOutput {
    message_id: String,
    text: String,
    tool_calls: Vec<ToolCall>,
    failure: Option<ChatError>,
    repetition_cut: Option<usize>,
    /// `{ text, ms }`, when the model reasoned.
    reasoning: Option<Value>,
}

impl<R: Runtime> Run<R> {
    /// Everything a run needs before its first step; None when there's
    /// nothing to do.
    pub(super) async fn prepare(
        app: &AppHandle<R>,
        state: &AgentState,
        db: &DbState,
        request: StartRequest,
        controls: Controls,
    ) -> Result<Option<(Run<R>, Entry)>, String> {
        let dialog_id = request.dialog_id.clone();
        let recent = recent_messages(db, &dialog_id).await?;
        let config = state.config();
        let mode = run_mode_for(&recent).or(request.mode.clone()).unwrap_or_else(|| "agent".into());
        let plan_mode = mode == "plan";
        let root = request.root.clone().filter(|r| !r.trim().is_empty());

        let mut start_at = 0;
        let mut settle_message = None;
        let mut continue_reply = false;
        let mut changed_files = Vec::new();
        let mut verify_rounds = 0;
        let entry = match request.start {
            StartKind::Send { message } => {
                settle_message = Some(message.id);
                Entry::Turn
            }
            StartKind::Retry => Entry::Turn,
            StartKind::ContinueReply => {
                continue_reply = true;
                Entry::Turn
            }
            StartKind::ContinueRun => {
                // Counting goes on from where it paused; loop evidence starts fresh.
                start_at = recent
                    .last()
                    .and_then(|m| m.meta.as_ref()?.pointer("/paused/toolCalls")?.as_u64())
                    .unwrap_or(0) as u32;
                Entry::Turn
            }
            StartKind::RunChecks { record_id } => {
                let record = recent.iter().find(|m| m.id == record_id);
                let Some(pending) = record.and_then(|m| m.meta.as_ref()?.get("verification").cloned()) else {
                    return Ok(None);
                };
                if pending.get("status").and_then(|s| s.as_str()) != Some("needs-approval") {
                    return Ok(None);
                }
                changed_files = pending
                    .get("changedFiles")
                    .and_then(|f| f.as_array())
                    .map(|f| f.iter().filter_map(|p| p.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                verify_rounds = pending.get("round").and_then(|r| r.as_u64()).unwrap_or(0) as u32;
                Entry::Verify { record_id }
            }
            StartKind::Resume => {
                let calls = unanswered_tool_calls(&recent);
                if calls.is_empty() {
                    return Ok(None);
                }
                Entry::Resume { calls }
            }
        };

        let (checks, project_context) = match &root {
            Some(r) => {
                let (app, r, verify) = (app.clone(), r.clone(), config.permissions.verify_changes);
                tauri::async_runtime::spawn_blocking(move || project::load(&app, &r, verify))
                    .await
                    .unwrap_or_default()
            }
            None => Default::default(),
        };
        let plan_context = if plan_mode {
            plan::current_plan_context(chat_plan_file(&recent).as_deref(), root.as_deref())
        } else {
            String::new()
        };
        let current_user_content = request.current_user_content.or_else(|| last_user_wire_content(&recent));
        // The repository map: the declarations most relevant to the request.
        // Built once here, then refreshed mid-run as files change
        // (refresh_repo_map_if_needed): the base section stays fixed while
        // the map is rebuilt and recombined into project_context before the
        // next model turn.
        let repo_map_chars = repo_map_chars(request.profile.context_tokens);
        let base_project_context = project_context;
        let repo_map = match &root {
            Some(r) => {
                let (r, request_text) = (r.clone(), current_user_content.clone().unwrap_or_default());
                let touched = touched_paths(&recent);
                let options = crate::repo_map::Options {
                    max_chars: repo_map_chars,
                    ..Default::default()
                };
                tauri::async_runtime::spawn_blocking(move || {
                    let root = std::path::Path::new(&r);
                    let focus = crate::repo_map::focus_from(root, &request_text, &touched);
                    crate::repo_map::repo_map(root, &focus, &options)
                })
                .await
                .ok()
                .flatten()
            }
            None => None,
        };
        let project_context = join_project_context(&base_project_context, &repo_map);

        let run = Run {
            app: app.clone(),
            state: state.clone(),
            db: db.clone(),
            dialog_id,
            profile: request.profile,
            options: request.options,
            plan_mode,
            context_mode: request.context_mode.unwrap_or_else(|| crate::clm::MODE_TRIMMING.into()),
            root,
            current_user_content,
            local_date: request.local_date.unwrap_or_else(log::utc_date),
            guard: Guard::new(config.permissions.tool_call_checkpoint, start_at),
            questions_asked: 0,
            checks,
            project_context,
            base_project_context,
            repo_map,
            repo_map_chars,
            map_writes_since_refresh: 0,
            map_refreshed_at: Instant::now(),
            map_refreshes: 0,
            writes_since_verify: !changed_files.is_empty(),
            changed_files,
            verify_rounds,
            last_verify_failed: false,
            checklist_nudged: false,
            words_only: false,
            plan_context,
            plan_nudged: false,
            repetition_stops: 0,
            started_at: Instant::now(),
            stop: controls.stop,
            effort: controls.effort,
            refused_formats: Vec::new(),
            settle_message,
            continue_reply,
        };
        Ok(Some((run, entry)))
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.is_set()
    }

    /// Runs to the end and returns how it ended (for the log and the UI).
    pub(super) async fn go(&mut self, entry: Entry) -> String {
        let outcome = match self.drive(entry).await {
            Ok(outcome) => outcome,
            Err(e) => {
                eprintln!("agent: run in {} failed: {e}", self.dialog_id);
                emit(
                    &self.app,
                    AgentEvent::Notice {
                        kind: "error".into(),
                        title: "The agent stopped".into(),
                        message: format!("Something went wrong: {e}"),
                    },
                );
                "error".to_string()
            }
        };
        // Ended before the message got any answer.
        self.settle(false).await;
        log::write(
            &self.app,
            json!({
                "event": "run",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "outcome": outcome,
                "toolCalls": self.guard.tool_calls,
                "filesChanged": self.changed_files.len(),
                "verifyRounds": self.verify_rounds,
                "ms": self.started_at.elapsed().as_millis() as u64,
            }),
        );
        outcome
    }

    async fn drive(&mut self, entry: Entry) -> Result<String, String> {
        match entry {
            Entry::Turn => {}
            Entry::Verify { record_id } => {
                // The waiting record becomes the running one, then the results.
                let outcome = self.verify_at_end(true, Some(record_id)).await?;
                if outcome != "continue" {
                    return Ok(outcome);
                }
            }
            Entry::Resume { calls } => {
                // Each asks the user again: they weren't there to decide.
                self.execute_tool_calls(&calls, Some(RESUME_NOTE)).await?;
                if self.stopped() {
                    return Ok("stopped".into());
                }
            }
        }

        loop {
            if self.stopped() {
                return Ok("stopped".into());
            }
            let turn = self.model_turn().await?;

            // Stopped for repeating itself: keep the reply up to where it
            // started, tell the model once, and go on; a second time, the run ends.
            if let Some(cut) = turn.repetition_cut {
                self.repetition_stops += 1;
                let kept = format!(
                    "{}\n\n_[Stopped here: the reply kept repeating itself.]_",
                    turn.text[..cut.min(turn.text.len())].trim_end()
                );
                self.finalize(&turn.message_id, kept, Vec::new(), turn.reasoning.clone(), None).await?;
                self.settle(true).await;
                if self.repetition_stops >= 2 || self.stopped() {
                    emit(
                        &self.app,
                        AgentEvent::Notice {
                            kind: "warning".into(),
                            title: "The model kept repeating itself".into(),
                            message: "Its reply was stopped twice, so the run ended. Try again, or a different model."
                                .into(),
                        },
                    );
                    return Ok("repetition".into());
                }
                let next = if self.plan_mode {
                    "If the plan is ready, submit it once with submit_plan and reply in a sentence or two."
                } else {
                    "Continue with the task from where you were."
                };
                self.add_harness(
                    format!(
                        "{IDE_NOTE}Your last reply started repeating the same text over and over, so it was stopped. Don't repeat anything. {next}"
                    ),
                    "repetition",
                )
                .await?;
                continue;
            }

            if let Some(error) = turn.failure {
                let stored = failure::stored(&error);
                if !turn.text.trim().is_empty() {
                    // Part of the reply arrived: keep it (it stays in the
                    // history) and let the user continue. Incomplete tool
                    // calls are dropped.
                    let mut meta = turn.reasoning.map(|r| json!({ "reasoning": r })).unwrap_or_else(|| json!({}));
                    meta["interrupted"] = stored;
                    self.finalize(&turn.message_id, turn.text, Vec::new(), Some(meta), None).await?;
                    self.settle(true).await;
                } else {
                    let meta = json!({ "failure": stored });
                    let text = failure::describe(&error);
                    self.finalize(&turn.message_id, String::new(), Vec::new(), Some(meta), Some(text)).await?;
                    self.settle(false).await;
                }
                return Ok(if error.kind == ErrorKind::Cancelled { "stopped" } else { "error" }.into());
            }

            let meta = turn.reasoning.map(|r| json!({ "reasoning": r }));
            self.finalize(&turn.message_id, turn.text.clone(), turn.tool_calls.clone(), meta, None).await?;
            self.settle(true).await;
            if self.stopped() {
                return Ok("stopped".into());
            }

            // The model says it's done: the checks decide.
            if turn.tool_calls.is_empty() {
                if self.plan_mode {
                    if self.finish_plan_reply(&turn.message_id, &turn.text).await? {
                        continue;
                    }
                } else if self.check_checklist().await? {
                    continue;
                }
                let outcome = self.verify_at_end(false, None).await?;
                if outcome == "continue" {
                    continue;
                }
                return Ok(outcome);
            }

            self.execute_tool_calls(&turn.tool_calls, None).await?;
            if self.stopped() {
                return Ok("stopped".into());
            }
            // Files changed: the run-start map is stale. Rebuild before the
            // next turn so it sees the new declarations; a no-op when
            // nothing was written or the ranking didn't move.
            self.refresh_repo_map_if_needed().await?;

            // Every tool call has its result, so the chat can wait here as
            // long as it likes: Continue picks up with the next request.
            if let Some(pause) = self.guard.pause() {
                if let Some(reason) = pause.reason.as_deref() {
                    if self.summarize_before_pause(reason).await? {
                        return Ok("stopped".into());
                    }
                }
                let mut paused = new_message("assistant", "");
                // Empty, no tool calls: kept out of what the model is sent.
                paused.meta = Some(json!({ "paused": pause }));
                self.append(vec![paused]).await?;
                return Ok(format!("paused-{}", pause.kind));
            }
        }
    }

    /// Before a loop pause: one reply without tools, in which the model
    /// tells the user where it got to, so the pause doesn't leave them with
    /// a bare reason. Best effort: a failed reply just isn't there. True
    /// when the user stopped the run meanwhile.
    async fn summarize_before_pause(&mut self, reason: &str) -> Result<bool, String> {
        self.add_harness(
            format!(
                "{IDE_NOTE}The run is paused for the user, because {reason}. Don't call any tools. \
                 In a few short bullets, tell the user: what you were trying to do, what you tried and what \
                 happened, what is blocking you, and what would unblock it (a decision, information, or a \
                 different approach)."
            ),
            "pauseSummary",
        )
        .await?;
        self.words_only = true;
        let turn = self.model_turn().await?;
        let mut meta = turn.reasoning.map(|r| json!({ "reasoning": r })).unwrap_or_else(|| json!({}));
        let Some(error) = turn.failure else {
            // Calls the provider sent anyway aren't run: the run is pausing.
            self.finalize(&turn.message_id, turn.text, Vec::new(), Some(meta), None).await?;
            self.settle(true).await;
            return Ok(self.stopped());
        };
        // As a failed turn in the run: what arrived is kept, else the error shows.
        let stored = failure::stored(&error);
        if !turn.text.trim().is_empty() {
            meta["interrupted"] = stored;
            self.finalize(&turn.message_id, turn.text, Vec::new(), Some(meta), None).await?;
            self.settle(true).await;
        } else {
            let text = failure::describe(&error);
            self.finalize(&turn.message_id, String::new(), Vec::new(), Some(json!({ "failure": stored })), Some(text))
                .await?;
            self.settle(false).await;
        }
        Ok(error.kind == ErrorKind::Cancelled || self.stopped())
    }

    /*
     * -----------------------------------------------------------------------
     * One model turn
     * -----------------------------------------------------------------------
     */

    /// One request to the model, streamed into a new assistant message. The
    /// history is read from the database, so everything up to here must be
    /// stored. Failed attempts are retried in chat_request.rs, which reports
    /// each retry, then succeeds or fails for good.
    async fn model_turn(&mut self) -> Result<TurnOutput, String> {
        let mut started = Instant::now();
        let mut pending = new_message("assistant", "");
        pending.finalized = false;
        let message_id = pending.id.clone();
        // Unfinished placeholders are skipped when the history is built.
        self.append_pending(pending).await?;

        // An effort picked since the last request applies from this one.
        self.take_effort();
        let config = self.state.config();
        let clm = self.context_mode == crate::clm::MODE_CLM
            && crate::clm::availability(&self.app.state::<crate::vfs::VfsState>()).is_ok();
        let tools = tools::tools_for(
            self.root.is_some(),
            self.profile.vision,
            self.profile.no_tools,
            &config.permissions,
            self.plan_mode,
            clm,
        );
        let project_context = if self.plan_mode {
            [self.project_context.as_str(), plan::PLAN_MODE_INSTRUCTIONS, self.plan_context.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            self.project_context.clone()
        };
        let continue_reply = std::mem::take(&mut self.continue_reply);
        let no_tool_calls = std::mem::take(&mut self.words_only);
        // Shown when the request starts (asking again with another effort).
        let mut opening_status: Option<Status> = None;
        // One-time context notes (CLM) an attempt at this turn went out with:
        // asking again has to repeat them, they aren't produced twice.
        let mut carried_notes: Vec<String> = Vec::new();

        loop {
            let params = ChatParams {
                base_url: self.profile.base_url.clone(),
                api_key: self.profile.api_key.clone(),
                model: self.profile.model.clone(),
                messages: None,
                dialog_id: Some(self.dialog_id.clone()),
                context_tokens: self.profile.context_tokens.filter(|t| *t > 0),
                vision: Some(self.profile.vision),
                current_user_content: self.current_user_content.clone(),
                streaming: self.profile.stream,
                first_byte_timeout_ms: self.profile.first_byte_timeout_ms,
                continue_reply: Some(continue_reply),
                assistant_message_id: Some(message_id.clone()),
                tools: Some(tools.clone()),
                system_prompt: None,
                project_context: Some(project_context.clone()),
                options: self.options.clone(),
                context_mode: Some(self.context_mode.clone()),
                carried_notes: carried_notes.clone(),
                no_tool_calls,
                api_route: self.profile.api_route.clone(),
                chat_id: None,
            };

            let (cancel_tx, mut cancel_rx) = watch::channel(false);
            let cancel_tx = Arc::new(cancel_tx);
            let stream = Arc::new(Mutex::new(Stream::new(self, &message_id, cancel_tx.clone())));
            if let Some(status) = opening_status.take() {
                lock(&stream).set_status(Some(status));
            }
            let sink = stream.clone();
            let mut on_event = move |event: ChatStreamEvent| lock(&sink).on_event(event);

            let mut effort_rx = self.effort.clone();
            // Another effort was picked in this chat: once the request has
            // stopped answering (and none of the reply has arrived), ask again.
            let mut wants_again = false;
            let mut ask_again = false;
            let vfs = self.app.state::<crate::vfs::VfsState>();
            let result = {
                let request = crate::agent::run_chat(&self.db, &vfs, params, &mut cancel_rx, &mut on_event);
                tokio::pin!(request);
                let mut stop_rx = self.stop.subscribe();
                let mut ticker = tokio::time::interval(Duration::from_millis(100));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut stop_deadline: Option<Instant> = None;
                // Asking again: the cancelled request gets as long to end as on Stop.
                let mut again_deadline: Option<Instant> = None;
                let mut effort_open = true;
                loop {
                    tokio::select! {
                        r = &mut request => break Some(r),
                        _ = ticker.tick() => {
                            lock(&stream).tick(started, stop_deadline.is_some());
                            if wants_again && !ask_again && stop_deadline.is_none() && lock(&stream).may_ask_again() {
                                ask_again = true;
                                again_deadline = Some(Instant::now() + STOP_GRACE);
                                let _ = cancel_tx.send(true);
                            }
                            // The request didn't end after Stop: stop waiting for it
                            // (dropping it ends the connection).
                            if [stop_deadline, again_deadline].iter().flatten().any(|at| Instant::now() >= *at) {
                                break None;
                            }
                        }
                        _ = crate::chat_request::cancelled(&mut stop_rx), if stop_deadline.is_none() => {
                            let _ = cancel_tx.send(true);
                            stop_deadline = Some(Instant::now() + STOP_GRACE);
                            lock(&stream).set_status(Some(Status::text("Stopping…")));
                        }
                        // Picked in this chat while the request is waiting for a
                        // first answer past the usual, stalled, or between
                        // retries, with none of the reply shown: no need to wait
                        // it out, ask again with the new effort (the ticker does
                        // once it gets there). A request that is answering keeps
                        // going; the effort applies to the next one.
                        changed = effort_rx.changed(), if effort_open && !ask_again && stop_deadline.is_none() => {
                            match changed {
                                Err(_) => effort_open = false,
                                Ok(()) => {
                                    let pick = effort_rx.borrow_and_update().clone();
                                    // The same effort picked again changes nothing.
                                    wants_again = pick.here && !same_effort(&pick.options, &self.options);
                                    if wants_again && lock(&stream).may_ask_again() {
                                        ask_again = true;
                                        again_deadline = Some(Instant::now() + STOP_GRACE);
                                        let _ = cancel_tx.send(true);
                                    }
                                }
                            }
                        }
                    }
                }
            };

            let mut s = lock(&stream);
            let failure = match result {
                None => Some(failure::stopped()),
                // Only problems before the request (e.g. building the history) land here.
                Some(Err(e)) => Some(failure::before_request(e)),
                Some(Ok(())) => s.failure.take(),
            };
            let learned = s.learned.take();

            // Cancelled to ask again (not Stop, and it didn't finish first).
            let asked_again = ask_again
                && !self.stop.is_set()
                && failure.as_ref().is_some_and(|e| e.kind == ErrorKind::Cancelled);
            if asked_again {
                let shown = s.shown_anything();
                for note in std::mem::take(&mut s.notes) {
                    if !carried_notes.contains(&note) {
                        carried_notes.push(note);
                    }
                }
                drop(s);
                if let Some((_, rejected, unresolved)) = learned {
                    self.drop_reasoning_formats(&rejected, &unresolved);
                }
                self.take_effort();
                log::write(
                    &self.app,
                    json!({
                        "event": "reply",
                        "dialogId": self.dialog_id,
                        "model": self.profile.model,
                        "error": "asked-again",
                        "ms": started.elapsed().as_millis() as u64,
                    }),
                );
                // Whatever the dropped attempt showed (its reasoning) goes.
                if shown {
                    self.patch_ui(
                        &message_id,
                        Patch {
                            content: Some(String::new()),
                            tool_calls: Some(Vec::new()),
                            meta: Some(json!({})),
                            ..Default::default()
                        },
                    );
                    let id = message_id.clone();
                    let patch = MessagePatch {
                        content: Some(String::new()),
                        tool_calls: Some(Vec::new()),
                        ..Default::default()
                    };
                    self.db.write_detached(move |c| store::update_message(c, &id, &patch));
                }
                opening_status = Some(Status::text(format!("Asking again with {}…", effort_label(&self.options))));
                started = Instant::now();
                continue;
            }

            s.set_status(None);
            let reasoning = s.reasoning_meta(true);
            let output = TurnOutput {
                message_id,
                text: std::mem::take(&mut s.text),
                tool_calls: std::mem::take(&mut s.tool_calls),
                repetition_cut: s.repetition_cut,
                reasoning,
                failure,
            };
            let (finish_reason, usage) = (s.finish_reason.take(), s.usage.take());
            let (focus_note, clm_reminder) = (s.focus_note, s.clm_reminder.take());
            drop(s);

            // Reasoning forms the provider refused aren't sent again in this run.
            if let Some((_, rejected, unresolved)) = learned {
                self.drop_reasoning_formats(&rejected, &unresolved);
            }

            let mut entry = json!({
                "event": "reply",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "finishReason": finish_reason,
                "toolCalls": if output.failure.is_some() { 0 } else { output.tool_calls.len() },
                "textChars": output.text.chars().count(),
                "usage": usage,
                "focusNote": focus_note,
                "clmReminder": clm_reminder,
                "ms": started.elapsed().as_millis() as u64,
            });
            if let Some(error) = &output.failure {
                entry["error"] = json!(error.kind);
            }
            log::write(&self.app, entry);
            return Ok(output);
        }
    }

    /// Takes the effort the user picked since it was last looked at, if any.
    fn take_effort(&mut self) {
        if self.effort.has_changed().unwrap_or(false) {
            self.options = self.effort.borrow_and_update().options.clone();
            self.prune_formats();
        }
    }

    /// Reasoning forms the provider refused aren't sent again in this run.
    fn drop_reasoning_formats(&mut self, rejected: &[String], unresolved: &[String]) {
        for f in rejected.iter().chain(unresolved) {
            if !self.refused_formats.contains(f) {
                self.refused_formats.push(f.clone());
            }
        }
        self.prune_formats();
    }

    fn prune_formats(&mut self) {
        without_refused(&mut self.options, &self.refused_formats);
    }

    /*
     * -----------------------------------------------------------------------
     * End-of-run checks (Plan mode and the checklist)
     * -----------------------------------------------------------------------
     */

    /// The end of a Plan-mode reply. If a plan was submitted in this
    /// request, the reply gets its file (for "Implement"). If none was, but
    /// the reply reads like a plan, the model is asked once to submit it
    /// properly. True: it was asked, the run goes on.
    async fn finish_plan_reply(&mut self, message_id: &str, text: &str) -> Result<bool, String> {
        let recent = self.recent().await?;
        let mut submitted = None;
        for m in recent.iter().rev() {
            if m.role == "user" && !plan::is_harness(m) {
                break;
            }
            if m.role == "tool" {
                if let Some(file) = m.meta.as_ref().and_then(|meta| meta.get("planFile")).and_then(|f| f.as_str()) {
                    submitted = Some(file.to_string());
                    break;
                }
            }
        }
        if let Some(file) = submitted {
            let mut meta = recent
                .iter()
                .find(|m| m.id == message_id)
                .and_then(|m| m.meta.clone())
                .filter(|m| m.is_object())
                .unwrap_or_else(|| json!({}));
            meta["planFile"] = json!(file);
            self.update_meta(message_id, meta).await?;
            return Ok(false);
        }
        if !plan::looks_like_plan(text) || self.plan_nudged {
            return Ok(false);
        }
        self.plan_nudged = true;
        self.add_harness(
            format!(
                "{IDE_NOTE}You wrote the plan in your reply, so it wasn't saved. Submit it with submit_plan (the complete plan, \
                 once), then reply in a sentence or two. Don't write it out again in the reply."
            ),
            "plan",
        )
        .await?;
        Ok(true)
    }

    /// The checklist at the end of a run: unfinished steps are pointed out
    /// once, and the run goes on. True: the agent was told, continue.
    async fn check_checklist(&mut self) -> Result<bool, String> {
        if self.checklist_nudged {
            return Ok(false);
        }
        let recent = self.recent().await?;
        let open: Vec<plan::TodoItem> = plan::latest_checklist(&recent)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !plan::is_settled(i))
            .collect();
        if open.is_empty() {
            return Ok(false);
        }
        self.checklist_nudged = true;
        let steps = open.iter().map(|i| format!("\"{}\"", i.title)).collect::<Vec<_>>().join(", ");
        self.add_harness(
            format!(
                "{IDE_NOTE}Your checklist still has unfinished steps: {steps}. \
                 Finish them, or update the checklist marking each skipped or changed with the reason, then finish."
            ),
            "checklist",
        )
        .await?;
        Ok(true)
    }

    /// Recombines the fixed project section with the current map.
    fn rebuild_project_context(&mut self) {
        self.project_context = join_project_context(&self.base_project_context, &self.repo_map);
    }

    /// Rebuilds the repository map mid-run, so the next model turn sees the
    /// new declarations (not the run-start snapshot).
    ///
    /// Bullet-proofing, in order of cheap to expensive:
    /// - Skips when nothing was written since the last build, the map is
    ///   fresh, stopped, or there is no project root: the common no-op is a
    ///   counter check plus an Instant comparison.
    /// - Per-file caches were already invalidated in record(), so a rewrite
    ///   with the same mtime+size can't serve a stale parse; a failed rebuild
    ///   keeps the old map and never breaks the run.
    /// - A successful bash marks the map dirty too (git checkout, codegen):
    ///   mtime/size catches those on the next collect, since no same-tick
    ///   cache race is possible across processes.
    /// - Past MAP_MAX_AGE, rebuilds even with no writes: the catch-all for
    ///   edits outside the run (the editor, git). Incremental (only changed
    ///   files re-parse) and a no-op when the ranking didn't move.
    /// - Focus is recomputed from the latest chat (reads + writes) plus the
    ///   original request, so new files the run touched steer the ranking.
    /// - Identical output keeps the old string (no context churn, no log spam).
    /// Returns true when the map text changed.
    async fn refresh_repo_map_if_needed(&mut self) -> Result<bool, String> {
        let Some(root) = self.root.clone() else { return Ok(false) };
        if self.stopped() {
            return Ok(false);
        }
        let dirty = self.map_writes_since_refresh > 0;
        let aged = self.map_refreshed_at.elapsed() >= MAP_MAX_AGE;
        if !dirty && !aged {
            return Ok(false);
        }
        // Consume the counter first: a failed rebuild must not retry every turn.
        self.map_writes_since_refresh = 0;
        self.map_refreshed_at = Instant::now();
        let recent = self.recent().await?;
        let touched = touched_paths(&recent);
        let request_text = self.current_user_content.clone().unwrap_or_default();
        let max_chars = self.repo_map_chars;
        let new_map = tauri::async_runtime::spawn_blocking(move || {
            let root_path = std::path::Path::new(&root);
            let focus = crate::repo_map::focus_from(root_path, &request_text, &touched);
            let options = crate::repo_map::Options { max_chars, ..Default::default() };
            crate::repo_map::repo_map(root_path, &focus, &options)
        })
        .await
        .ok()
        .flatten();
        if new_map == self.repo_map {
            return Ok(false);
        }
        self.repo_map = new_map;
        self.rebuild_project_context();
        self.map_refreshes += 1;
        log::write(
            &self.app,
            json!({
                "event": "repoMapRefresh",
                "dialogId": self.dialog_id,
                "refreshes": self.map_refreshes,
                "mapChars": self.repo_map.as_ref().map(|m| m.chars().count()).unwrap_or(0),
            }),
        );
        Ok(true)
    }

    /*
     * -----------------------------------------------------------------------
     * Storing what the run produces
     * -----------------------------------------------------------------------
     */

    pub(super) async fn recent(&self) -> Result<Vec<MessageRow>, String> {
        recent_messages(&self.db, &self.dialog_id).await
    }

    /// Stores messages at the end of the chat and shows them.
    pub(super) async fn append(&self, messages: Vec<NewMessage>) -> Result<(), String> {
        self.append_marked(messages, false).await
    }

    async fn append_pending(&self, message: NewMessage) -> Result<(), String> {
        self.append_marked(vec![message], true).await
    }

    async fn append_marked(&self, messages: Vec<NewMessage>, pending: bool) -> Result<(), String> {
        let id = self.dialog_id.clone();
        let stored = messages.clone();
        let seqs = self.db.write(move |c| store::append_messages(c, &id, &stored)).await?;
        let out = messages
            .iter()
            .zip(seqs)
            .map(|(m, seq)| OutMessage::from_new(&self.dialog_id, seq, m, pending))
            .collect();
        emit(&self.app, AgentEvent::MessagesAdded { dialog_id: self.dialog_id.clone(), messages: out });
        Ok(())
    }

    /// A user-role message the IDE writes to the model (never shown as the
    /// user's, never taken for their request).
    pub(super) async fn add_harness(&self, content: String, kind: &str) -> Result<(), String> {
        log::write(
            &self.app,
            json!({
                "event": "harness",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "kind": kind,
                "toolCalls": self.guard.tool_calls,
            }),
        );
        let mut m = new_message("user", &content);
        m.meta = Some(json!({ "harness": kind }));
        self.append(vec![m]).await
    }

    /// The result of a tool call, stored before the next turn.
    pub(super) async fn add_tool_result(
        &self,
        tool_call_id: &str,
        content: String,
        meta: Option<Value>,
        attachments: Vec<String>,
    ) -> Result<(), String> {
        let mut m = new_message("tool", &content);
        m.tool_call_id = Some(tool_call_id.to_string());
        m.meta = meta;
        m.attachments = attachments;
        self.append(vec![m]).await
    }

    /// A reply's final content: stored (indexed for search), and shown.
    async fn finalize(
        &self,
        message_id: &str,
        content: String,
        tool_calls: Vec<ToolCall>,
        meta: Option<Value>,
        error: Option<String>,
    ) -> Result<(), String> {
        let id = message_id.to_string();
        let patch = MessagePatch {
            content: Some(content.clone()),
            tool_calls: Some(tool_calls.clone()),
            meta: meta.clone(),
            error: error.clone(),
            failed: None,
            finalize: true,
        };
        self.db.write(move |c| store::update_message(c, &id, &patch)).await?;
        self.patch_ui(
            message_id,
            Patch {
                content: Some(content),
                tool_calls: Some(tool_calls),
                meta,
                error,
                pending: Some(false),
                finalized: Some(true),
                ..Default::default()
            },
        );
        Ok(())
    }

    /// Replaces a stored message's meta and shows it (records that change
    /// after they're added, like the verification gate's rounds).
    pub(super) async fn update_meta(&self, message_id: &str, meta: Value) -> Result<(), String> {
        let id = message_id.to_string();
        let patch = MessagePatch { meta: Some(meta.clone()), ..Default::default() };
        self.db.write(move |c| store::update_message(c, &id, &patch)).await?;
        self.patch_ui(message_id, Patch { meta: Some(meta), ..Default::default() });
        Ok(())
    }

    pub(super) fn patch_ui(&self, message_id: &str, patch: Patch) {
        emit(
            &self.app,
            AgentEvent::MessagePatched {
                dialog_id: self.dialog_id.clone(),
                message_id: message_id.to_string(),
                patch,
            },
        );
    }

    /// The first answer to a sent message: on none, the message is marked
    /// failed (its draft comes back in the composer).
    async fn settle(&mut self, ok: bool) {
        let Some(message_id) = self.settle_message.take() else { return };
        if !ok {
            let id = message_id.clone();
            let patch = MessagePatch { failed: Some(true), ..Default::default() };
            match self.db.write(move |c| store::update_message(c, &id, &patch)).await {
                Ok(()) => self.patch_ui(&message_id, Patch { failed: Some(true), ..Default::default() }),
                Err(e) => eprintln!("agent: could not mark a message failed: {e}"),
            }
        }
        emit(
            &self.app,
            AgentEvent::UserMessageSettled { dialog_id: self.dialog_id.clone(), message_id, ok },
        );
    }

    /// The user's latest request in the chat, as sent (for the AI reviewer).
    pub(super) async fn latest_user_request(&self) -> String {
        let recent = self.recent().await.unwrap_or_default();
        recent
            .iter()
            .rev()
            .find(|m| m.role == "user" && !m.failed && !plan::is_harness(m))
            .map(|m| sent_content(m).unwrap_or_else(|| m.content.clone()))
            .unwrap_or_default()
    }
}

/*
 * ---------------------------------------------------------------------------
 * The stream of one reply
 * ---------------------------------------------------------------------------
 */

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A reply as it streams in: collected here, shown at most every
/// `UI_EVERY`, stored every `CHECKPOINT_EVERY`.
struct Stream<R: Runtime> {
    app: AppHandle<R>,
    db: DbState,
    dialog_id: String,
    message_id: String,
    profile_id: String,
    cancel: Arc<watch::Sender<bool>>,
    stop: Arc<StopSignal>,

    text: String,
    reasoning: String,
    /// The reasoning as it is sent back (`reasoning_details`; reasoning_echo.rs).
    reasoning_details: Vec<Value>,
    /// The model replying: reasoning is sent back to this model only.
    model: String,
    reasoning_started: Option<Instant>,
    reasoning_ended: Option<Instant>,
    reasoning_seen: bool,
    tool_calls: Vec<ToolCall>,
    failure: Option<ChatError>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    /// (accepted, rejected, unresolved) reasoning forms.
    learned: Option<(Vec<String>, Vec<String>, Vec<String>)>,

    got_output: bool,
    /// "Still waiting" no longer applies (a retry is shown instead).
    waiting_done: bool,
    /// Waiting to try again after a failed attempt (chat_request.rs).
    retrying: bool,
    /// When this attempt started.
    started: Instant,
    /// One-time context notes this request carries (CLM).
    notes: Vec<String>,
    /// The request ended with the focus note (Trimming mode), for the log.
    focus_note: bool,
    /// The CLM budget reminder the request carried, if any, for the log.
    clm_reminder: Option<String>,
    last_output: Instant,
    last_output_ms: i64,
    idle_shown: bool,
    status: Option<Status>,

    repetition_cut: Option<usize>,
    repetition_checked_at: usize,

    dirty_text: bool,
    dirty_tools: bool,
    dirty_reasoning: bool,
    unsaved: bool,
    last_shown: Instant,
    last_saved: Instant,
}

impl<R: Runtime> Stream<R> {
    fn new(run: &Run<R>, message_id: &str, cancel: Arc<watch::Sender<bool>>) -> Self {
        let now = Instant::now();
        Stream {
            app: run.app.clone(),
            db: run.db.clone(),
            dialog_id: run.dialog_id.clone(),
            message_id: message_id.to_string(),
            profile_id: run.profile.id.clone(),
            cancel,
            stop: run.stop.clone(),
            text: String::new(),
            reasoning: String::new(),
            reasoning_details: Vec::new(),
            model: run.profile.model.clone(),
            reasoning_started: None,
            reasoning_ended: None,
            reasoning_seen: false,
            tool_calls: Vec::new(),
            failure: None,
            finish_reason: None,
            usage: None,
            learned: None,
            got_output: false,
            waiting_done: false,
            retrying: false,
            started: now,
            notes: Vec::new(),
            focus_note: false,
            clm_reminder: None,
            last_output: now,
            last_output_ms: store::now_ms(),
            idle_shown: false,
            status: None,
            repetition_cut: None,
            repetition_checked_at: 0,
            dirty_text: false,
            dirty_tools: false,
            dirty_reasoning: false,
            unsaved: false,
            last_shown: now,
            last_saved: now,
        }
    }

    fn patch(&self, patch: Patch) {
        emit(
            &self.app,
            AgentEvent::MessagePatched {
                dialog_id: self.dialog_id.clone(),
                message_id: self.message_id.clone(),
                patch,
            },
        );
    }

    fn set_status(&mut self, status: Option<Status>) {
        if self.status == status {
            return;
        }
        self.status = status.clone();
        self.patch(Patch { status: Some(status), ..Default::default() });
    }

    /// Output arrived: the first time, the wait is over.
    fn mark_output(&mut self) {
        if self.got_output {
            return;
        }
        self.got_output = true;
        self.waiting_done = true;
        self.set_status(None);
    }

    fn note_output(&mut self) {
        self.retrying = false;
        self.last_output = Instant::now();
        self.last_output_ms = store::now_ms();
        if self.idle_shown {
            self.idle_shown = false;
            self.set_status(None);
        }
    }

    fn on_event(&mut self, event: ChatStreamEvent) {
        if matches!(
            event,
            ChatStreamEvent::Delta { .. }
                | ChatStreamEvent::Reasoning { .. }
                | ChatStreamEvent::ToolCallStart { .. }
                | ChatStreamEvent::ToolCallArgumentsDelta { .. }
                | ChatStreamEvent::ToolCallComplete { .. }
        ) {
            self.note_output();
        }
        match event {
            ChatStreamEvent::Reasoning { content } => {
                self.mark_output();
                if self.reasoning.is_empty() {
                    self.reasoning_started = Some(Instant::now());
                }
                self.reasoning.push_str(&content);
                if !self.reasoning_seen {
                    self.reasoning_seen = true;
                    emit(
                        &self.app,
                        AgentEvent::ReasoningLearned {
                            profile_id: self.profile_id.clone(),
                            accepted: Vec::new(),
                            rejected: Vec::new(),
                            unresolved: Vec::new(),
                            reasons: true,
                        },
                    );
                }
                self.dirty_reasoning = true;
            }
            ChatStreamEvent::ReasoningSupport { accepted, rejected, unresolved } => {
                emit(
                    &self.app,
                    AgentEvent::ReasoningLearned {
                        profile_id: self.profile_id.clone(),
                        accepted: accepted.clone(),
                        rejected: rejected.clone(),
                        unresolved: unresolved.clone(),
                        reasons: false,
                    },
                );
                self.learned = Some((accepted, rejected, unresolved));
            }
            ChatStreamEvent::Delta { content } => {
                self.mark_output();
                // The reply started: the thinking before it is over.
                if !self.reasoning.is_empty() && self.reasoning_ended.is_none() {
                    self.reasoning_ended = Some(Instant::now());
                }
                self.text.push_str(&content);
                self.dirty_text = true;
                // Every couple of kilobytes: is it stuck repeating itself?
                if self.repetition_cut.is_none() && self.text.len() - self.repetition_checked_at > 1500 {
                    self.repetition_checked_at = self.text.len();
                    if let Some(cut) = repetition::find_repetition(&self.text) {
                        self.repetition_cut = Some(cut);
                        let _ = self.cancel.send(true);
                    }
                }
            }
            ChatStreamEvent::Finished { finish_reason, usage } => {
                self.finish_reason = finish_reason;
                self.usage = usage;
            }
            ChatStreamEvent::ApiRoute { route } => {
                emit(&self.app, AgentEvent::ApiRouteLearned { profile_id: self.profile_id.clone(), route });
            }
            ChatStreamEvent::Done => {}
            ChatStreamEvent::Retrying { attempt, max_attempts, delay_ms, reason } => {
                self.waiting_done = true;
                self.retrying = true;
                if !self.stop.is_set() {
                    self.set_status(Some(Status {
                        text: format!(
                            "{} Retrying (attempt {attempt} of {max_attempts})",
                            failure::retry_reason(&reason)
                        ),
                        until: Some(store::now_ms() + delay_ms as i64),
                        since: None,
                    }));
                }
            }
            ChatStreamEvent::Error { error } => self.failure = Some(error),
            ChatStreamEvent::FocusNote => self.focus_note = true,
            ChatStreamEvent::ReasoningDetails { details } => self.reasoning_details = details,
            ChatStreamEvent::ContextNotes { notes } => {
                // A budget reminder (CLM), up to its numbers, for the reply's log.
                self.clm_reminder = notes
                    .iter()
                    .find(|n| n.starts_with("CONTEXT BUDGET"))
                    .map(|n| n.split(" tokens)").next().unwrap_or(n).to_string());
                self.notes = notes;
            }
            ChatStreamEvent::ContextUpdated { status } => {
                emit(&self.app, AgentEvent::ContextUpdated { dialog_id: self.dialog_id.clone(), status });
            }
            ChatStreamEvent::ToolCallStart { id, name } => {
                self.mark_output();
                let call = ToolCall { id: id.clone(), name, arguments: String::new(), extra_content: None };
                match self.tool_calls.iter_mut().find(|c| c.id == id) {
                    Some(existing) => *existing = call,
                    None => self.tool_calls.push(call),
                }
                self.dirty_tools = true;
            }
            ChatStreamEvent::ToolCallArgumentsDelta { id, arguments_delta } => {
                if let Some(call) = self.tool_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&arguments_delta);
                    self.dirty_tools = true;
                }
            }
            ChatStreamEvent::ToolCallComplete { id, name, arguments, extra_content } => {
                // Sent back unchanged in the next request (Gemini 3 requires it).
                let call = ToolCall { id: id.clone(), name, arguments, extra_content };
                match self.tool_calls.iter_mut().find(|c| c.id == id) {
                    Some(existing) => *existing = call,
                    None => self.tool_calls.push(call),
                }
                self.dirty_tools = true;
            }
        }
        if self.last_shown.elapsed() >= UI_EVERY {
            self.show();
        }
    }

    /// Part of the reply itself has arrived (text or a tool call), not just
    /// its reasoning.
    fn reply_started(&self) -> bool {
        !self.text.is_empty() || !self.tool_calls.is_empty()
    }

    /// The request stopped answering: no first output past the usual wait,
    /// silent since its last output, or between retries.
    fn stuck(&self) -> bool {
        self.retrying
            || (!self.got_output && self.started.elapsed() >= STILL_WAITING)
            || (self.got_output && self.last_output.elapsed() >= REPLY_IDLE)
    }

    /// Asking again loses nothing of the reply and spares a wait.
    fn may_ask_again(&self) -> bool {
        !self.reply_started() && self.stuck()
    }

    /// Anything of this reply was shown, its reasoning included.
    fn shown_anything(&self) -> bool {
        self.reply_started() || !self.reasoning.is_empty()
    }

    /// The reasoning so far, as stored with the reply.
    /// `{ text, ms }` as shown, plus what is sent back to the same model
    /// (`details`, `model`; chat_db::history::attach_reasoning).
    fn reasoning_meta(&self, with_time: bool) -> Option<Value> {
        if self.reasoning.is_empty() && self.reasoning_details.is_empty() {
            return None;
        }
        let mut r = json!({ "model": self.model });
        if !self.reasoning.is_empty() {
            r["text"] = json!(self.reasoning);
        }
        if !self.reasoning_details.is_empty() {
            r["details"] = json!(self.reasoning_details);
        }
        if with_time {
            if let Some(start) = self.reasoning_started {
                let end = self.reasoning_ended.unwrap_or_else(Instant::now);
                r["ms"] = json!(end.duration_since(start).as_millis() as u64);
            }
        }
        Some(r)
    }

    /// Shows what changed since the last update.
    fn show(&mut self) {
        if !(self.dirty_text || self.dirty_tools || self.dirty_reasoning) {
            return;
        }
        let mut patch = Patch::default();
        if std::mem::take(&mut self.dirty_text) {
            patch.content = Some(self.text.clone());
        }
        if std::mem::take(&mut self.dirty_tools) {
            patch.tool_calls = Some(self.tool_calls.clone());
        }
        if std::mem::take(&mut self.dirty_reasoning) {
            patch.meta = self.reasoning_meta(false).map(|r| json!({ "reasoning": r }));
        }
        self.patch(patch);
        self.last_shown = Instant::now();
        self.unsaved = true;
    }

    /// Every 100 ms while the request runs: the waiting and idle notes, the
    /// screen, and a checkpoint of the reply.
    fn tick(&mut self, started: Instant, stopping: bool) {
        if !stopping && !self.stop.is_set() {
            if !self.got_output && !self.waiting_done && started.elapsed() >= STILL_WAITING {
                self.waiting_done = true;
                self.set_status(Some(Status::text("Still waiting for a response…")));
            }
            // Output that stops mid-reply. Some providers send a tool call only
            // once it's complete, so while the model writes a whole file nothing
            // arrives: say it's still going, with the time since the last output.
            if self.got_output && !self.idle_shown && self.last_output.elapsed() >= REPLY_IDLE {
                self.idle_shown = true;
                self.set_status(Some(Status {
                    text: "Still generating".into(),
                    until: None,
                    since: Some(self.last_output_ms),
                }));
            }
        }
        self.show();
        if self.unsaved && self.last_saved.elapsed() >= CHECKPOINT_EVERY {
            self.unsaved = false;
            self.last_saved = Instant::now();
            let id = self.message_id.clone();
            let patch = MessagePatch {
                content: Some(self.text.clone()),
                tool_calls: (!self.tool_calls.is_empty()).then(|| self.tool_calls.clone()),
                ..Default::default()
            };
            self.db.write_detached(move |c| store::update_message(c, &id, &patch));
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * Reading the chat
 * ---------------------------------------------------------------------------
 */

/// The effort's forms without those the provider refused.
fn without_refused(options: &mut Option<OpenAIChatOptions>, refused: &[String]) {
    if refused.is_empty() {
        return;
    }
    let Some(options) = options.as_mut().filter(|o| o.reasoning_effort.is_some()) else { return };
    let formats = options
        .reasoning_formats
        .get_or_insert_with(|| vec!["reasoning_effort".to_string(), "chat_template_kwargs".to_string()]);
    formats.retain(|f| !refused.contains(f) && REASONING_FIELDS.contains(&f.as_str()));
}

/// The same effort level (the forms it's sent in follow what's learned).
fn same_effort(a: &Option<OpenAIChatOptions>, b: &Option<OpenAIChatOptions>) -> bool {
    let level = |o: &Option<OpenAIChatOptions>| serde_json::to_value(o.as_ref().and_then(|o| o.reasoning_effort)).ok();
    level(a) == level(b)
}

/// "low effort", as the chat's effort selector words it.
fn effort_label(options: &Option<OpenAIChatOptions>) -> &'static str {
    match options.as_ref().and_then(|o| o.reasoning_effort) {
        None => "the default effort",
        Some(ReasoningEffort::None) => "no thinking",
        Some(ReasoningEffort::Low) => "low effort",
        Some(ReasoningEffort::Medium) => "medium effort",
        Some(ReasoningEffort::High) => "high effort",
    }
}

pub(super) fn new_message(role: &str, content: &str) -> NewMessage {
    NewMessage {
        id: uuid::Uuid::new_v4().to_string(),
        role: role.to_string(),
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: Vec::new(),
        created_at: store::now_ms(),
        finalized: true,
    }
}

async fn recent_messages(db: &DbState, dialog_id: &str) -> Result<Vec<MessageRow>, String> {
    let id = dialog_id.to_string();
    db.read(move |c| store::get_messages(c, &id, None, RECENT_MESSAGES)).await.map(|page| page.messages)
}

fn sent_content(m: &MessageRow) -> Option<String> {
    m.meta.as_ref()?.get("sentContent")?.as_str().map(str::to_string)
}

/// The mode of the request a run belongs to: its starting message's.
fn run_mode_for(messages: &[MessageRow]) -> Option<String> {
    let m = messages.iter().rev().find(|m| m.role == "user" && !plan::is_harness(m))?;
    m.meta.as_ref()?.get("mode")?.as_str().map(str::to_string)
}

/// The chat's plan file: the latest one a plan was saved to.
pub(super) fn chat_plan_file(messages: &[MessageRow]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find_map(|m| m.meta.as_ref()?.get("planFile")?.as_str().map(str::to_string))
}

/// The repository map's size: about 2% of the model's context window,
/// within 1,500 and 6,000 characters.
fn repo_map_chars(context_tokens: Option<usize>) -> usize {
    const CHARS_PER_TOKEN: usize = 4;
    let window = context_tokens.filter(|t| *t > 0).unwrap_or(32_000);
    (window * CHARS_PER_TOKEN / 50).clamp(1_500, 6_000)
}

/// A map older than this is rebuilt on the next turn even with no writes:
/// the catch-all for edits outside the run (the editor, git). The rebuild
/// is incremental (only changed files re-parse) and a no-op when the
/// ranking didn't move, so idle long runs pay one cheap walk per interval.
const MAP_MAX_AGE: Duration = Duration::from_secs(120);

/// The system-prompt project section: the fixed base plus the current map.
fn join_project_context(base: &str, map: &Option<String>) -> String {
    match (base, map) {
        (base, Some(map)) if base.is_empty() => map.clone(),
        (base, Some(map)) => format!("{base}\n\n{map}"),
        (base, None) => base.to_string(),
    }
}

/// Files the chat read or changed, latest last.
fn touched_paths(messages: &[MessageRow]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for call in messages.iter().filter_map(|m| m.tool_calls.as_ref()).flatten() {
        if !matches!(call.name.as_str(), "read" | "write") {
            continue;
        }
        if let Some(path) = super::permissions::str_arg(&call.arguments, "path") {
            paths.retain(|p| p != &path);
            paths.push(path);
        }
    }
    let keep = paths.len().saturating_sub(MAX_TOUCHED_PATHS);
    paths.split_off(keep)
}

/// Files of the chat's history the map starts from, at most.
const MAX_TOUCHED_PATHS: usize = 20;

/// The latest delivered user message as it was sent to the model (with its
/// editor context and file references).
fn last_user_wire_content(messages: &[MessageRow]) -> Option<String> {
    let m = messages.iter().rev().find(|m| m.role == "user" && !m.failed && !plan::is_harness(m))?;
    sent_content(m).or_else(|| Some(m.content.clone()))
}

/// Tool calls of the chat's last agent step that have no result yet (the
/// app closed while they waited for approval, or mid-way through a turn).
pub(super) fn unanswered_tool_calls(messages: &[MessageRow]) -> Vec<ToolCall> {
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role == "tool" {
            continue;
        }
        let calls = match &m.tool_calls {
            Some(calls) if m.role == "assistant" && m.finalized && m.error.is_none() && !calls.is_empty() => calls,
            _ => return Vec::new(),
        };
        let answered: Vec<&str> =
            messages[i + 1..].iter().filter(|x| x.role == "tool").filter_map(|x| x.tool_call_id.as_deref()).collect();
        return calls.iter().filter(|c| !answered.contains(&c.id.as_str())).cloned().collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, meta: Option<Value>) -> MessageRow {
        MessageRow {
            id: uuid::Uuid::new_v4().to_string(),
            dialog_id: "d".into(),
            seq: 0,
            role: role.into(),
            content: String::new(),
            body_external: false,
            body_bytes: 0,
            tool_calls: None,
            tool_call_id: None,
            meta,
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 0,
        }
    }

    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.into(), name: "read".into(), arguments: "{}".into(), extra_content: None }
    }

    #[test]
    fn finds_unanswered_calls_of_the_last_step() {
        let mut step = row("assistant", None);
        step.tool_calls = Some(vec![call("a"), call("b")]);
        let mut answer = row("tool", None);
        answer.tool_call_id = Some("a".into());
        let messages = vec![row("user", None), step.clone(), answer];
        assert_eq!(unanswered_tool_calls(&messages), vec![call("b")]);
        let mut done = messages.clone();
        done.push(row("assistant", None));
        assert!(unanswered_tool_calls(&done).is_empty());
        step.finalized = false;
        assert!(unanswered_tool_calls(&[step]).is_empty(), "a reply cut off mid-stream isn't resumed");
    }

    #[test]
    fn efforts_compare_by_level_and_keep_refused_forms_out() {
        let options = |v: Value| serde_json::from_value::<Option<OpenAIChatOptions>>(v).unwrap();
        let high = options(json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort"] }));
        let high_more = options(json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort", "reasoning"] }));
        assert!(same_effort(&high, &high_more));
        assert!(!same_effort(&high, &options(json!({ "reasoning_effort": "low" }))));
        assert!(!same_effort(&high, &None));

        // A new pick still listing a form refused earlier in the run.
        let mut picked = options(json!({ "reasoning_effort": "low", "reasoning_formats": ["reasoning_effort", "chat_template_kwargs"] }));
        without_refused(&mut picked, &["chat_template_kwargs".to_string()]);
        assert_eq!(picked.unwrap().reasoning_formats.unwrap(), ["reasoning_effort"]);
        // No forms given: the default ones, minus the refused.
        let mut default_forms = options(json!({ "reasoning_effort": "low" }));
        without_refused(&mut default_forms, &["reasoning_effort".to_string()]);
        assert_eq!(default_forms.unwrap().reasoning_formats.unwrap(), ["chat_template_kwargs"]);
    }

    #[test]
    fn reads_mode_and_request_from_the_chat() {
        let user = row("user", Some(json!({ "mode": "plan", "sentContent": "wire" })));
        let harness = row("user", Some(json!({ "harness": "verification" })));
        let messages = vec![user, row("assistant", Some(json!({ "planFile": "/p/a.md" }))), harness];
        assert_eq!(run_mode_for(&messages).as_deref(), Some("plan"));
        assert_eq!(last_user_wire_content(&messages).as_deref(), Some("wire"));
        assert_eq!(chat_plan_file(&messages).as_deref(), Some("/p/a.md"));
    }

    #[test]
    fn the_project_section_recombines_with_the_current_map() {
        let map = Some("## Repository map\nbody".to_string());
        assert_eq!(join_project_context("## Project\nbase", &map), "## Project\nbase\n\n## Repository map\nbody");
        assert_eq!(join_project_context("", &map), "## Repository map\nbody");
        assert_eq!(join_project_context("## Project\nbase", &None), "## Project\nbase");
        assert_eq!(join_project_context("", &None), "");
    }
}

#[cfg(test)]
mod repo_map_tests {
    use super::*;
    use crate::chat_db::store::ToolCall;

    #[test]
    fn the_map_is_about_two_percent_of_the_window() {
        assert_eq!(repo_map_chars(Some(100_000)), 6_000, "at most");
        assert_eq!(repo_map_chars(Some(50_000)), 4_000);
        assert_eq!(repo_map_chars(Some(8_000)), 1_500, "at least");
        assert_eq!(repo_map_chars(None), 2_560, "an unknown window counts as 32k");
        assert_eq!(repo_map_chars(Some(0)), 2_560);
    }

    fn with_calls(calls: &[(&str, &str)]) -> MessageRow {
        MessageRow {
            id: String::new(),
            dialog_id: String::new(),
            seq: 0,
            role: "assistant".into(),
            content: String::new(),
            body_external: false,
            body_bytes: 0,
            tool_calls: Some(
                calls
                    .iter()
                    .map(|(name, args)| ToolCall { id: String::new(), name: name.to_string(), arguments: args.to_string(), extra_content: None })
                    .collect(),
            ),
            tool_call_id: None,
            meta: None,
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 0,
        }
    }

    #[test]
    fn touched_paths_are_reads_and_writes_latest_last_each_once() {
        let messages = vec![
            with_calls(&[("read", r#"{"path":"a.rs"}"#), ("search_code", r#"{"query":"x"}"#)]),
            with_calls(&[("write", r#"{"path":"b.rs","old_text":"x","content":"y"}"#), ("bash", r#"{"command":"ls"}"#)]),
            with_calls(&[("read", r#"{"path":"a.rs"}"#), ("write", "not json"), ("list_dir", r#"{"path":"src"}"#)]),
        ];
        assert_eq!(touched_paths(&messages), ["b.rs", "a.rs"]);
    }

    #[test]
    fn only_the_latest_touched_paths_are_kept() {
        let calls: Vec<String> = (0..30).map(|i| format!(r#"{{"path":"f{i}.rs"}}"#)).collect();
        let pairs: Vec<(&str, &str)> = calls.iter().map(|c| ("read", c.as_str())).collect();
        let paths = touched_paths(&[with_calls(&pairs)]);
        assert_eq!(paths.len(), MAX_TOUCHED_PATHS);
        assert_eq!(paths.first().unwrap(), "f10.rs");
        assert_eq!(paths.last().unwrap(), "f29.rs");
    }
}

//! One agent run: model turns until the model is done (and the checks
//! agree), the run pauses, fails, or is stopped.
//!
//! Each turn streams one reply into a stored assistant message, then runs
//! its tool calls; every result is stored before the next turn, which reads
//! the history from the database. When the model replies without tool
//! calls, the end-of-run checks decide whether it's really done: a plan
//! written in a reply instead of submitted (Plan mode), unfinished checklist
//! steps, and the verification gate (verify.rs).

use super::events::{AgentEvent, OutMessage, Patch, Status};
use super::guard::Guard;
use super::permissions::ChecklistMode;
use super::project::{self, ProjectCheck};
use super::subagent_run::SubCtx;
use super::{
    failure, log, plan, repair, repetition, subagent, tools, Controls, EffortPick, ModelProfile,
    StartKind, StartRequest, StopSignal,
};
use crate::agent::{
    ChatParams, ChatStreamEvent, OpenAIChatOptions, ReasoningEffort, REASONING_FIELDS,
};
use crate::chat_db::store::{self, MessagePatch, MessageRow, NewMessage, ToolCall};
use crate::chat_db::DbState;
use crate::chat_request::{ChatError, ErrorKind};
use crate::core::Core;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
/// A typed request shorter than this says nothing about the task on its own
/// ("continue"): the repository map and the focus note fall back to the
/// previous real request's text as well.
const SHORT_REQUEST_CHARS: usize = 80;

pub(super) const RESUME_NOTE: &str =
    "Interrupted: the app was closed before this ran. Allow it to continue the task.";

/// Marks a chat as running for its run's length (conflicts found for it go
/// to it), and releases that and its hold on writing commands when the run
/// ends, however it ends.
struct RunningGuard(String);

impl RunningGuard {
    fn new(dialog_id: &str, takes_conflicts: bool) -> Self {
        if takes_conflicts {
            crate::overlay::chats::set_running(dialog_id, true);
        }
        RunningGuard(dialog_id.to_string())
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        crate::overlay::chats::set_running(&self.0, false);
        crate::overlay::chats::release_direct_writes(&self.0);
    }
}

pub(super) const IDE_NOTE: &str = "[Automatic check by the IDE, not written by the user]\n";
/// End-of-run checklist reminders before the run ends as unfinished-checklist.
const MAX_CHECKLIST_NUDGES: u32 = 2;
/// Context percent at and above which a finished checklist step asks for a
/// context compaction (Phase D). Below it, only a TASK.md update is asked.
pub(super) const CLM_STEP_COMPACT_MIN: u32 = 35;

/// A step-compaction request waiting for its edit (Phase D asks, Phase E
/// escalates when it is ignored).
#[derive(Debug, Clone)]
pub(super) struct PendingStepCompaction {
    pub(super) tool_calls: u32,
}

/// When a compaction pressure started (Phase E): the tool-call count then,
/// and the run's turn then (for turns-to-edit). Any edit clears it.
#[derive(Debug, Clone)]
pub(super) struct ClmWait {
    pub(super) tool_calls: u32,
    pub(super) turn: u32,
}

/// Phase E pressure thresholds: a step-compaction request or the 75%
/// reminder escalates after this many tool calls with no edit (at 50%+);
/// 90%+ restricts at once until an edit lands.
pub(super) const CLM_ESCALATE_AFTER_CALLS: u32 = 4;
pub(super) const CLM_ESCALATE_MIN_PCT: u32 = 50;
pub(super) const CLM_RESTRICT_LIFT_BELOW: u32 = 75;
/// Restricted turns with no edit before the safety valve lifts it.
pub(super) const CLM_MAX_RESTRICTED_STALL: u32 = 2;

pub struct Run {
    /// The host: the chat database, the other runs, where events go.
    pub(super) core: Core,
    pub(super) dialog_id: String,
    pub(super) profile: ModelProfile,
    pub(super) options: Option<OpenAIChatOptions>,
    /// The mode of the message that started the run (Plan or Agent).
    pub(super) plan_mode: bool,
    pub(super) context_mode: String,
    pub(super) root: Option<String>,
    /// The project folder itself. Differs from `root` in an isolated chat,
    /// whose tools and commands work in its overlay's view.
    pub(super) project_root: Option<String>,
    /// An isolated chat's changes landed at the end of this run.
    pub(super) landed: bool,
    /// Files the project's newer changes were just merged into, for the
    /// failure report of the checks that follow (landing.rs).
    pub(super) merged_from_project: Option<Vec<(String, String)>>,
    /// Times the model was reminded of conflict markers it left.
    pub(super) marker_reminders: u32,
    /// What this run has read and written of each file, so a whole-file
    /// write over a change it hasn't seen is refused (file_guard.rs).
    pub(super) seen: Arc<crate::file_guard::Seen>,
    /// The request's user message as sent (with editor context).
    pub(super) current_user_content: Option<String>,
    pub(super) local_date: String,
    pub(super) guard: Guard,
    /// ask_user calls so far (Plan mode caps them).
    pub(super) questions_asked: u32,
    /// The project's checks and the system-prompt section about it. Fixed
    /// for the run: what changes (the repository map) is sent apart.
    pub(super) checks: Vec<ProjectCheck>,
    pub(super) project_context: String,
    /// The current repository map text (the `## Repository map` section),
    /// if any. Updated mid-run as files change; the next model turn sends
    /// it at the end of the request (`ChatParams::volatile_context`), never
    /// in the system prompt, so a refresh doesn't invalidate the provider's
    /// cache of the prefix.
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
    /// Unfinished checklist steps were pointed out; at most MAX_CHECKLIST_NUDGES.
    pub(super) checklist_nudges: u32,
    /// Force the `todo` tool on the next model turn (plan implementation
    /// first turn, second checklist nudge). Consumed by `model_turn`.
    pub(super) force_todo_next: bool,
    /// The checklist gate refused one call this run (small-task safety):
    /// everything after that is let through, repeats included.
    pub(super) checklist_gate_refused: bool,
    /// The run ended as unfinished-checklist with this many open steps
    /// (for the run's log line).
    pub(super) open_steps_at_end: Option<usize>,
    /// A step-compaction request is outstanding (Phase D, for Phase E
    /// escalation): the step that just finished, the context revision it
    /// was asked at, and how many tool calls had run then.
    pub(super) pending_step_compaction: Option<PendingStepCompaction>,
    /// Phase E: this turn offers only context tools (the model must compact
    /// before anything else). Set when a step-compaction request or the 75%
    /// reminder is ignored, or at 90%+ with no edit since the urgent
    /// reminder; lifted by an edit, by dropping below 75%, or by the safety
    /// valve (`clm_non_compliant`).
    pub(super) clm_restricted: bool,
    /// Restricted turns so far, and consecutive restricted turns with no
    /// edit (the safety valve lifts at CLM_MAX_RESTRICTED_STALL).
    pub(super) restricted_turns: u32,
    pub(super) restricted_stall: u32,
    /// The safety valve fired: weak models aren't restricted again.
    pub(super) clm_non_compliant: bool,
    /// CLM turns so far (for turns-to-edit), and the peak context percent.
    pub(super) clm_turns: u32,
    pub(super) clm_peak_pct: u32,
    /// Edits seen (new context revisions) and their token deltas
    /// (revision, tokens before, tokens after), oldest first.
    pub(super) clm_edits_seen: u32,
    pub(super) clm_edit_deltas: Vec<(u64, usize, usize)>,
    /// Redone work: recall-hint notes offered after a compaction removed
    /// an earlier result the model then asked for again.
    pub(super) clm_recall_hits: u32,
    /// Reminder tiers seen this run (25, 50, 75, 90), and turns-to-edit after
    /// each pressure started.
    pub(super) clm_reminders_fired: Vec<u32>,
    pub(super) step_asked_turn: Option<u32>,
    pub(super) step_turns_to_edit: Option<u32>,
    pub(super) tier75_turns_to_edit: Option<u32>,
    pub(super) urgent_turns_to_edit: Option<u32>,
    /// The 75% reminder and the 90%+ urgent state while no edit has landed
    /// since (revision, tool calls and turn when first seen).
    pub(super) tier75_since: Option<ClmWait>,
    pub(super) urgent_since: Option<ClmWait>,
    /// CLM tracking started (the first CLM state was seen).
    pub(super) clm_tracking_started: bool,
    /// A write to the context file (or a restore) succeeded since the last
    /// turn (set in execute.rs). The file is rewritten before every request,
    /// so a new revision alone doesn't mean the agent edited anything.
    pub(super) context_edited: bool,
    /// An edit seen last turn, waiting for its size: the context is only
    /// re-measured by the next request (tokens before the edit).
    pub(super) edit_delta_pending: Option<usize>,
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
    /// The chat's lead, or a subagent of a lead (subagent_run.rs).
    pub(super) role: Role,
    /// The subagents this run started (a lead's).
    pub(super) ledger: subagent::Ledger,
    /// Each verification round's failures, oldest first (repair.rs, R01):
    /// a rolled-back round isn't among them.
    pub(super) verify_history: Vec<repair::RoundFailures>,
    /// The last round's results, reported again after a rollback.
    pub(super) last_verify_results: Vec<super::verify::CheckResult>,
    /// The files as they were before the current fix round (R02).
    pub(super) checkpoint: Option<repair::Checkpoint>,
    /// Each file as it was when the run first wrote it (R05), by path
    /// relative to the root; None: it didn't exist.
    pub(super) baseline: std::collections::HashMap<String, super::subagent::Original>,
    /// Changes that weaken the checks, which the agent was asked about (R05).
    pub(super) integrity_open: Vec<String>,
    /// Files changed since the change reviewer last ran (change_review.rs).
    pub(super) writes_since_review: bool,
    /// Change reviews in this run, and since the checks last ran.
    pub(super) reviews_total: u32,
    /// How the last change review went: its status and why it was skipped.
    /// None when it didn't start.
    pub(super) review_outcome: Option<(String, Option<String>)>,
    /// The checks were started from `record` with no carried state for it
    /// (P1.7): its record says so, and the agent is told once.
    pub(super) carry_lost: Option<String>,
    /// Commands that may have changed files since the round's checkpoint
    /// (P3.5): a rollback can't undo them, and says so.
    pub(super) round_commands: Vec<String>,
    /// What files were before the run couldn't all be rebuilt after a
    /// restart (P3.9): checks that pass aren't "verified".
    pub(super) integrity_unchecked: bool,
    /// Rounds checked again at once after fixers (P3.10, `MAX_RECHECKS`).
    pub(super) rechecks: u32,
    pub(super) carry_lost_told: bool,
    pub(super) reviews_in_row: u32,
    /// The findings sent to the agent so far, and the note that sent the
    /// latest (its answers follow it).
    pub(super) review_findings: Vec<super::change_review::Finding>,
    pub(super) review_note_id: Option<String>,
    /// What the last failure triage found, for the next one (triage.rs).
    pub(super) last_triage: Option<super::triage::Triage>,
    /// Plan reviews in this run, the findings that held a plan back, and
    /// the `submit_plan` call they answered (plan_review.rs).
    /// The running parallel group's extend_scope grants.
    pub(super) group_grants: super::group::GroupGrants,
    /// P4.7: earlier runs' saved check output was removed.
    pub(super) cleaned_this_run: bool,
    pub(super) plan_reviews: u32,
    pub(super) plan_findings: Vec<super::change_review::Finding>,
    /// The plan as it was held with those findings (round 3): sent again
    /// unchanged, every repeated finding stands; revised, repeats may go.
    pub(super) plan_held: Option<String>,
    pub(super) plan_review_call: Option<String>,
    /// Plan steps step workers finished in this run (worker.rs, D03).
    pub(super) worker_steps: Vec<super::worker::DoneStep>,
    /// Files the test writer wrote, those the lead changed after it, and
    /// how many of those the final message listed already (E03).
    pub(super) test_writer_files: Vec<String>,
    pub(super) tests_changed: Vec<String>,
    pub(super) tests_changed_listed: usize,
    /// Parallel groups joined in this run, for the repair loop (R03).
    pub(super) groups: Vec<super::repair_run::GroupRecord>,
    /// Fix attempts rolled back in this run, for a gave-up record (R06).
    pub(super) rolled_back_attempts: Vec<super::repair::Rollback>,
    /// Fix rounds beyond MAX_VERIFY_ROUNDS a review after green granted (R08).
    pub(super) bonus_rounds: u32,
    /// The last round asked the agent about changes that weaken the checks
    /// (R05): the next round's new failures aren't a regression.
    pub(super) asked_integrity: bool,
}

/// Who a run works for.
pub(super) enum Role {
    /// The chat's own agent: talks to the user, owns the plan, the checklist
    /// and the checks.
    Lead,
    /// A subagent of a lead (subagent_run.rs).
    Sub(Box<SubCtx>),
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
pub(super) struct TurnOutput {
    pub(super) message_id: String,
    pub(super) text: String,
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) failure: Option<ChatError>,
    pub(super) repetition_cut: Option<usize>,
    /// `{ text, ms }`, when the model reasoned.
    pub(super) reasoning: Option<Value>,
}

/// A reply up to where it started repeating itself (`cut`; all of it
/// when it didn't).
pub(super) fn text_before_cut(text: &str, cut: Option<usize>) -> &str {
    match cut {
        Some(cut) => text[..cut.min(text.len())].trim_end(),
        None => text,
    }
}

/// A reply's meta from what the model reasoned, if it did.
pub(super) fn reasoning_meta(reasoning: Option<Value>) -> Option<Value> {
    reasoning.map(|r| json!({ "reasoning": r }))
}

impl Run {
    /// Everything a run needs before its first step; None when there's
    /// nothing to do.
    pub(super) async fn prepare(
        core: &Core,
        request: StartRequest,
        controls: Controls,
    ) -> Result<Option<(Run, Entry)>, String> {
        let dialog_id = request.dialog_id.clone();
        let mut recent = recent_messages(&core.db, &dialog_id).await?;
        let config = core.agents.config();
        let mode = run_mode_for(&recent)
            .or(request.mode.clone())
            .unwrap_or_else(|| "agent".into());
        let plan_mode = mode == "plan";
        let project_root = request.root.clone().filter(|r| !r.trim().is_empty());
        // An isolated chat works in its overlay: the project with its own
        // changes on top, mounted as a folder (src/overlay). Plan mode
        // changes nothing, so it reads the project itself, unless the chat
        // has changes that haven't landed: then it plans against those.
        let mut isolation_problem = None;
        let isolate = request
            .isolate
            .unwrap_or_else(crate::overlay::chats::enabled);
        let root = match &project_root {
            Some(r) if isolate && (!plan_mode || crate::overlay::chats::exists(&dialog_id)) => {
                let core_ = core.clone();
                let on_warn = Box::new(move |used: u64, limit: u64| {
                    core_.emit(AgentEvent::Notice {
                        kind: "warning".into(),
                        title: "This chat's changes are near their limit".into(),
                        message: format!(
                            "{} MB of {} MB. Past the limit, its writes fail until its changes land or are discarded.",
                            used >> 20,
                            limit >> 20
                        ),
                    })
                });
                match crate::overlay::chats::open(&dialog_id, std::path::Path::new(r), on_warn) {
                    Ok(o) => Some(o.view.to_string_lossy().into_owned()),
                    Err(e) => {
                        isolation_problem = Some(e);
                        project_root.clone()
                    }
                }
            }
            _ => project_root.clone(),
        };
        if let Some(problem) =
            isolation_problem.filter(|_| crate::overlay::chats::tell_not_isolated(&dialog_id))
        {
            core.emit(AgentEvent::Notice {
                kind: "warning".into(),
                title: "This chat isn't isolated".into(),
                message: if problem.contains("couldn't mount") {
                    format!(
                        "{problem}. It works in the project folder directly, and only one such chat at a time can run commands that change files. Installing the virtual-files driver (Settings → Virtual files) lets chats work in isolation."
                    )
                } else {
                    format!(
                        "{problem}. It works in the project folder directly, and only one such chat at a time can run commands that change files."
                    )
                },
            });
        }
        let is_send = matches!(request.start, StartKind::Send { .. });
        let is_continue_run = matches!(request.start, StartKind::ContinueRun);
        // Plan-implementation first turn forces the `todo` tool (named
        // `tool_choice`): "Implement this plan" in Agent mode. Explicit
        // `meta.implementsPlan` wins; the `Implement the plan…` line is the
        // fallback for messages sent before the frontend marked them.
        let implements_plan = match &request.start {
            StartKind::Send { message } => {
                message
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("implementsPlan"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    || message.content.starts_with("Implement the plan")
            }
            _ => false,
        };

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
                let Some(pending) =
                    record.and_then(|m| m.meta.as_ref()?.get("verification").cloned())
                else {
                    return Ok(None);
                };
                if pending.get("status").and_then(|s| s.as_str()) != Some("needs-approval") {
                    return Ok(None);
                }
                changed_files = pending
                    .get("changedFiles")
                    .and_then(|f| f.as_array())
                    .map(|f| {
                        f.iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
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

        // A new message with an unfinished checklist carried from earlier
        // work: the app says so itself, so a short "continue" after a
        // restart doesn't lose the checklist. An app message (harness), so
        // it never starts a new request itself.
        //
        // ContinueRun (the panel's Continue on an interrupted checklist)
        // gets the same treatment: the remaining steps as an app message,
        // with no user message created so nothing gets lost. Both also
        // emit the live Checklist event, so the panel is correct from the
        // run's first turn.
        async fn append_carry_note(
            core: &Core,
            dialog_id: &str,
            recent: &mut Vec<MessageRow>,
            content: String,
            meta: Value,
            items: Option<Vec<plan::TodoItem>>,
        ) {
            let mut m = new_message("user", &content);
            m.meta = Some(meta);
            let stored = vec![m.clone()];
            let id = dialog_id.to_string();
            if let Ok(seqs) = core
                .db
                .write(move |c| store::append_messages(c, &id, &stored))
                .await
            {
                let seq = seqs.into_iter().next().unwrap_or(0);
                core.emit(AgentEvent::MessagesAdded {
                    dialog_id: dialog_id.to_string(),
                    messages: vec![OutMessage::from_new(dialog_id, seq, &m, false)],
                });
                let seq_row = recent.last().map(|r| r.seq + 1).unwrap_or(0);
                let meta = m.meta.clone();
                let (mid, mcontent, mtool_calls, mtool_call_id, mcreated) =
                    (m.id, m.content, m.tool_calls, m.tool_call_id, m.created_at);
                recent.push(MessageRow {
                    id: mid,
                    dialog_id: dialog_id.to_string(),
                    seq: seq_row,
                    role: m.role,
                    content: mcontent,
                    body_external: false,
                    body_bytes: 0,
                    tool_calls: mtool_calls,
                    tool_call_id: mtool_call_id,
                    meta,
                    error: None,
                    failed: false,
                    finalized: true,
                    attachments: Vec::new(),
                    created_at: mcreated,
                });
            }
            if let Some(items) = items {
                core.emit(AgentEvent::Checklist {
                    dialog_id: dialog_id.to_string(),
                    items,
                });
            }
        }

        // Agent mode only: Plan mode has no `todo` tool, so a note asking
        // for it would only confuse the plan.
        if is_send && !plan_mode {
            if let Some(carried) = plan::current_checklist(&recent) {
                if carried.carried && !plan::open_steps(&carried.items).is_empty() {
                    append_carry_note(
                        core,
                        &dialog_id,
                        &mut recent,
                        plan::carry_note(&carried.items),
                        json!({ "harness": "checklistCarried", "fromMessageId": carried.from_message_id }),
                        Some(carried.items),
                    )
                    .await;
                }
            }
        }
        if is_continue_run && !plan_mode {
            if let Some(current) = plan::current_checklist(&recent) {
                if !plan::open_steps(&current.items).is_empty() {
                    append_carry_note(
                        core,
                        &dialog_id,
                        &mut recent,
                        plan::continue_note(&current.items),
                        json!({ "harness": "checklistContinued", "fromMessageId": current.from_message_id }),
                        Some(current.items),
                    )
                    .await;
                }
            }
        }

        // B07b: implementing a plan file, which the user may have edited
        // since it was checked. Its problems now go to the agent before it
        // starts, and show in the chat.
        if implements_plan && !plan_mode {
            if let Some((file, problems)) =
                plan_check_at_start(&recent, root.as_deref(), project_root.as_deref())
            {
                let shown = plan::relative_plan_path(&file, project_root.as_deref());
                append_carry_note(
                    core,
                    &dialog_id,
                    &mut recent,
                    plan::plan_check_note(&shown, &problems),
                    json!({ "harness": "planCheck", "planCheck": { "file": shown, "problems": problems } }),
                    None,
                )
                .await;
            }
        }

        let (checks, project_context) = match &root {
            Some(r) => {
                // Settings are kept by the project's folder, not the view's.
                let settings_root = project_root.clone().unwrap_or_else(|| r.clone());
                let config_dir = core.paths.config_dir.clone();
                let (r, verify) = (r.clone(), config.permissions.verify_changes);
                crate::rt::spawn_blocking(move || {
                    project::load(&config_dir, &r, &settings_root, verify)
                })
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
        let current_user_content = request
            .current_user_content
            .or_else(|| last_user_wire_content(&recent));
        // A short typed request ("continue") says nothing about the task:
        // the map is built from the previous real request's text as well,
        // so it still points at the work being continued.
        let map_request_text = {
            let current = current_user_content.clone().unwrap_or_default();
            if current.trim().chars().count() < SHORT_REQUEST_CHARS {
                if let Some(prev) = previous_user_wire_content(&recent) {
                    if prev.trim() != current.trim() {
                        format!("{prev}\n\n{current}")
                    } else {
                        current
                    }
                } else {
                    current
                }
            } else {
                current
            }
        };
        // The repository map: the declarations most relevant to the request.
        // Built once here, then refreshed mid-run as files change
        // (refresh_repo_map_if_needed). The next model turn sends it at the
        // end of the request, after the history (ChatParams::volatile_context).
        let repo_map_chars = repo_map_chars(request.profile.context_tokens);
        let repo_map = match &root {
            Some(r) => {
                let (r, request_text) = (r.clone(), map_request_text.clone());
                let touched = touched_paths(&recent);
                let options = crate::repo_map::Options {
                    max_chars: repo_map_chars,
                    ..Default::default()
                };
                crate::rt::spawn_blocking(move || {
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

        // Forcing `todo` on a plan implementation's first turn only makes
        // sense when no checklist exists yet (nothing to carry, nothing
        // made): otherwise the carry note / live list already governs.
        // "Always" (the checklist setting) forces it on the first turn of
        // every Agent-mode message with a project, unless the request already
        // has its own checklist. A carried one still gets a fresh `todo`
        // call: the model confirms or closes it before anything else.
        let checklist_mode = config.permissions.checklist;
        let has_own_checklist = plan::current_checklist(&recent).is_some_and(|c| !c.carried);
        let force_todo_next = !plan_mode
            && root.is_some()
            && !has_own_checklist
            && match checklist_mode {
                ChecklistMode::Always => is_send,
                _ => implements_plan && plan::current_checklist(&recent).is_none(),
            };

        let run = Run {
            plan_mode,
            context_mode: request
                .context_mode
                .unwrap_or_else(|| crate::clm::MODE_TRIMMING.into()),
            root,
            project_root,
            current_user_content,
            local_date: request.local_date.unwrap_or_else(log::utc_date),
            guard: Guard::new(config.permissions.tool_call_checkpoint, start_at),
            checks,
            project_context,
            repo_map,
            repo_map_chars,
            writes_since_verify: !changed_files.is_empty(),
            changed_files,
            verify_rounds,
            force_todo_next,
            plan_context,
            settle_message,
            continue_reply,
            ..Run::blank(
                core.clone(),
                crate::file_guard::Seen::new(dialog_id.clone()),
                dialog_id,
                request.profile,
                request.options,
                controls,
                Role::Lead,
            )
        };
        Ok(Some((run, entry)))
    }

    /// A run with nothing set yet: every counter at zero, nothing changed,
    /// no plan, no checks. `prepare` and `prepare_child` fill in the rest.
    #[allow(clippy::too_many_arguments)]
    fn blank(
        core: Core,
        seen: Arc<crate::file_guard::Seen>,
        dialog_id: String,
        profile: ModelProfile,
        options: Option<OpenAIChatOptions>,
        controls: Controls,
        role: Role,
    ) -> Run {
        Run {
            core,
            seen,
            dialog_id,
            profile,
            options,
            plan_mode: false,
            context_mode: crate::clm::MODE_TRIMMING.into(),
            root: None,
            project_root: None,
            merged_from_project: None,
            marker_reminders: 0,
            landed: false,
            current_user_content: None,
            local_date: String::new(),
            guard: Guard::new(None, 0),
            questions_asked: 0,
            checks: Vec::new(),
            project_context: String::new(),
            repo_map: None,
            repo_map_chars: 0,
            map_writes_since_refresh: 0,
            map_refreshed_at: Instant::now(),
            map_refreshes: 0,
            writes_since_verify: false,
            changed_files: Vec::new(),
            verify_rounds: 0,
            last_verify_failed: false,
            checklist_nudges: 0,
            force_todo_next: false,
            checklist_gate_refused: false,
            open_steps_at_end: None,
            pending_step_compaction: None,
            clm_restricted: false,
            restricted_turns: 0,
            restricted_stall: 0,
            clm_non_compliant: false,
            clm_turns: 0,
            clm_peak_pct: 0,
            clm_edits_seen: 0,
            clm_edit_deltas: Vec::new(),
            clm_recall_hits: 0,
            clm_reminders_fired: Vec::new(),
            step_asked_turn: None,
            step_turns_to_edit: None,
            tier75_turns_to_edit: None,
            urgent_turns_to_edit: None,
            tier75_since: None,
            urgent_since: None,
            clm_tracking_started: false,
            context_edited: false,
            edit_delta_pending: None,
            words_only: false,
            plan_context: String::new(),
            plan_nudged: false,
            repetition_stops: 0,
            started_at: Instant::now(),
            stop: controls.stop,
            effort: controls.effort,
            refused_formats: Vec::new(),
            settle_message: None,
            continue_reply: false,
            role,
            ledger: subagent::Ledger::default(),
            verify_history: Vec::new(),
            last_verify_results: Vec::new(),
            checkpoint: None,
            baseline: Default::default(),
            integrity_open: Vec::new(),
            writes_since_review: false,
            reviews_total: 0,
            review_outcome: None,
            carry_lost: None,
            round_commands: Vec::new(),
            integrity_unchecked: false,
            rechecks: 0,
            carry_lost_told: false,
            reviews_in_row: 0,
            review_findings: Vec::new(),
            review_note_id: None,
            last_triage: None,
            cleaned_this_run: false,
            group_grants: Default::default(),
            plan_reviews: 0,
            plan_findings: Vec::new(),
            plan_held: None,
            plan_review_call: None,
            worker_steps: Vec::new(),
            test_writer_files: Vec::new(),
            tests_changed: Vec::new(),
            tests_changed_listed: 0,
            groups: Vec::new(),
            rolled_back_attempts: Vec::new(),
            bonus_rounds: 0,
            asked_integrity: false,
        }
    }

    /// A subagent's run, in the child chat `dialog_id` (already stored, its
    /// brief its first message). It works in the parent's root (D5) and
    /// never opens an overlay of its own (K04); it shares the parent's
    /// project section and repository map, gets its kind's tools and
    /// prompt, and none of the lead's end-of-run machinery: no checklist,
    /// no checks, no CLM, no plan mode, no checkpoints (K03).
    pub(super) fn prepare_child(
        parent: &Run,
        dialog_id: String,
        sub: SubCtx,
        controls: Controls,
    ) -> Run {
        let profile = sub.models[0].clone();
        let options = sub.options.clone();
        // A writing kind works for the lead: the lead's record of what it
        // read and wrote, so its files aren't "changed by another chat"
        // and the lead can go on from what the worker wrote.
        let seen = if sub.kind.writes() {
            parent.seen.clone()
        } else {
            crate::file_guard::Seen::new(dialog_id.clone())
        };
        Run {
            root: parent.root.clone(),
            project_root: parent.project_root.clone(),
            local_date: parent.local_date.clone(),
            project_context: parent.project_context.clone(),
            repo_map: parent.repo_map.clone(),
            repo_map_chars: parent.repo_map_chars,
            ..Run::blank(
                parent.core.clone(),
                seen,
                dialog_id,
                profile,
                options,
                controls,
                Role::Sub(Box::new(sub)),
            )
        }
    }

    /// Who this run's requests are for, for ROVIK_API_LOG (S09).
    fn log_tags(&self) -> Value {
        match self.sub() {
            Some(sub) => json!({
                "role": "sub",
                "kind": sub.kind.id(),
                "dialogId": self.dialog_id,
                "parentDialogId": sub.parent_dialog_id,
            }),
            None => json!({ "role": "lead", "dialogId": self.dialog_id }),
        }
    }

    /// The subagent's context, when this run is one.
    pub(super) fn sub(&self) -> Option<&SubCtx> {
        match &self.role {
            Role::Sub(sub) => Some(sub),
            Role::Lead => None,
        }
    }

    pub(super) fn sub_mut(&mut self) -> Option<&mut SubCtx> {
        match &mut self.role {
            Role::Sub(sub) => Some(sub),
            Role::Lead => None,
        }
    }

    /// The kinds of subagent this lead may start now (none for a subagent:
    /// depth 1, D3). Read at each turn: settings changes apply at once.
    /// Plan mode changes nothing, so no kind that writes (step workers, the
    /// test writer) is offered there.
    pub(super) fn subagent_kinds(&self) -> Vec<subagent::Kind> {
        if self.sub().is_some() || self.root.is_none() || self.profile.no_tools {
            return Vec::new();
        }
        let mut kinds = self.core.agents.config().subagents.lead_kinds();
        if self.plan_mode {
            kinds.retain(|k| !k.writes());
        }
        kinds
    }

    /// Parallel groups can be started now: they can run, and this isn't
    /// Plan mode (which writes the plan's groups, not their code).
    pub(super) fn groups_now(&self) -> bool {
        self.parallel_on() && !self.plan_mode
    }

    /// Parallel groups can run (F): the setting is on, step workers are
    /// on, and the chat is isolated (child overlays need its view).
    pub(super) fn parallel_on(&self) -> bool {
        let config = self.core.agents.config().subagents;
        self.sub().is_none()
            && config.parallel_workers
            && config.kind(subagent::Kind::Worker).is_some()
            && crate::overlay::chats::get(&self.dialog_id).is_some()
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.is_set()
    }

    /// Runs to the end and returns how it ended (for the log and the UI).
    pub(super) async fn go(&mut self, entry: Entry) -> String {
        // While it runs, conflicts found for this chat go to it (not in plan
        // mode: it can't edit files to resolve them). The guard takes that
        // back, and any hold on writing commands, however the run ends,
        // a panic included.
        let _running = RunningGuard::new(&self.dialog_id, !self.plan_mode);
        let outcome = self.drive(entry).await;
        crate::overlay::chats::set_running(&self.dialog_id, false);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                eprintln!("agent: run in {} failed: {e}", self.dialog_id);
                self.core.emit(AgentEvent::Notice {
                    kind: "error".into(),
                    title: "The agent stopped".into(),
                    message: format!("Something went wrong: {e}"),
                });
                "error".to_string()
            }
        };
        // P1.8: a lead's run that leaves no checks waiting leaves no state
        // for them either (an older run's is stale now).
        if self.sub().is_none() && outcome != "verify-needs-approval" {
            self.core.agents.drop_carry(&self.dialog_id);
        }
        // An isolated chat's changes land now, or say why they don't.
        self.land_at_end(&outcome).await;
        // Ended before the message got any answer.
        self.settle(false).await;
        self.log_clm_compliance(&outcome).await;
        let mut entry = json!({
            "event": "run",
            "dialogId": self.dialog_id,
            "model": self.profile.model,
            "outcome": outcome,
            "toolCalls": self.guard.tool_calls,
            "filesChanged": self.changed_files.len(),
            "verifyRounds": self.verify_rounds,
            "ms": self.started_at.elapsed().as_millis() as u64,
        });
        if let Some(open) = self.open_steps_at_end {
            entry["openSteps"] = json!(open);
        }
        log::write(&self.core, entry);
        outcome
    }

    async fn drive(&mut self, entry: Entry) -> Result<String, String> {
        // P1.7: a new run makes the checks older runs left waiting stale:
        // their records say so (no "Run checks" any more).
        if !matches!(entry, Entry::Verify { .. }) && self.sub().is_none() {
            self.supersede_waiting_checks().await;
        }
        match entry {
            Entry::Turn => {}
            Entry::Verify { record_id } => {
                // What the last run's fix rounds knew (command approval on).
                self.restore_verify_state(&record_id);
                if self.carry_lost.is_some() {
                    self.rebuild_baseline(&record_id).await;
                }
                // The waiting record becomes the running one, then the results.
                let outcome = self.verify_at_end(true, Some(record_id)).await?;
                if outcome != "continue" {
                    // As at any other end: the project's newer changes to
                    // the same files are merged in and checked first, and
                    // conflict markers handed over meanwhile are mentioned.
                    let end = match self.take_project_changes(&outcome).await? {
                        Some(next) if next == "continue" => None,
                        Some(next) => Some(next),
                        None => Some(outcome),
                    };
                    if let Some(end) = end {
                        if !self.remind_of_markers().await? {
                            return Ok(end);
                        }
                    }
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
            if turn.repetition_cut.is_some() {
                self.repetition_stops += 1;
                let kept = format!(
                    "{}\n\n_[Stopped here: the reply kept repeating itself.]_",
                    text_before_cut(&turn.text, turn.repetition_cut)
                );
                self.finalize(
                    &turn.message_id,
                    kept,
                    Vec::new(),
                    turn.reasoning.clone().map(|r| json!({ "reasoning": r })),
                    None,
                )
                .await?;
                self.settle(true).await;
                if self.repetition_stops >= 2 || self.stopped() {
                    self.core.emit(AgentEvent::Notice {
                        kind: "warning".into(),
                        title: "The model kept repeating itself".into(),
                        message: "Its reply was stopped twice, so the run ended. Try again, or a different model."
                            .into(),
                    });
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
                let kept = self
                    .store_failed_turn(&turn.message_id, &turn.text, turn.reasoning, &error)
                    .await?;
                self.settle(kept).await;
                return Ok(if error.kind == ErrorKind::Cancelled {
                    "stopped"
                } else {
                    "error"
                }
                .into());
            }

            self.finalize(
                &turn.message_id,
                turn.text.clone(),
                turn.tool_calls.clone(),
                reasoning_meta(turn.reasoning),
                None,
            )
            .await?;
            self.settle(true).await;
            // Stopped meanwhile: a reply with tool calls goes on to
            // execute_tool_calls, which answers each with a "stopped" result
            // (a call left without one reads as interrupted, and the chat
            // would offer to resume it).
            if self.stopped() && turn.tool_calls.is_empty() {
                return Ok("stopped".into());
            }

            // The model says it's done: the checks decide.
            if turn.tool_calls.is_empty() {
                // E03: test-writer tests it changed are listed for the user.
                if self.sub().is_none() && self.tests_changed.len() > self.tests_changed_listed {
                    let line = super::test_writer::changed_tests_line(&self.tests_changed);
                    self.tests_changed_listed = self.tests_changed.len();
                    self.finalize(
                        &turn.message_id,
                        format!("{}{line}", turn.text.trim_end()),
                        Vec::new(),
                        None,
                        None,
                    )
                    .await?;
                }
                if self.plan_mode {
                    if self.finish_plan_reply(&turn.message_id, &turn.text).await? {
                        continue;
                    }
                } else if self.check_checklist().await? {
                    continue;
                }
                // Conflicts handed to it: not done while their markers are left.
                if self.remind_of_markers().await? {
                    continue;
                }
                // A fresh-context review of the change, before the checks.
                if self.review_change().await? {
                    continue;
                }
                if self.stopped() {
                    return Ok("stopped".into());
                }
                let outcome = self.verify_at_end(false, None).await?;
                if outcome == "continue" {
                    continue;
                }
                // Reminders exhausted with steps still open: the run ends as
                // unfinished-checklist so the UI can offer Continue. The
                // final message carries `meta.checklistOpen`.
                if !self.plan_mode && self.checklist_nudges >= MAX_CHECKLIST_NUDGES {
                    let recent = self.recent().await?;
                    let open = plan::current_checklist(&recent)
                        .map(|c| c.items.into_iter().filter(|i| !plan::is_settled(i)).count())
                        .unwrap_or(0);
                    if open > 0 {
                        let mut meta = recent
                            .iter()
                            .find(|m| m.id == turn.message_id)
                            .and_then(|m| m.meta.clone())
                            .filter(|m| m.is_object())
                            .unwrap_or_else(|| json!({}));
                        meta["checklistOpen"] = json!(open);
                        let _ = self.update_meta(&turn.message_id, meta).await;
                        // Logged with the run's one `run` line (go()).
                        self.open_steps_at_end = Some(open);
                        return Ok("unfinished-checklist".into());
                    }
                }
                // Isolated: the project's newer changes to the same files
                // are merged in and checked before the run ends.
                let end = match self.take_project_changes(&outcome).await? {
                    Some(next) if next == "continue" => continue,
                    Some(next) => next,
                    None => outcome,
                };
                // Markers handed to it while the checks ran: told now.
                if self.remind_of_markers().await? {
                    continue;
                }
                return Ok(end);
            }

            // Isolated: not while the project is mid-switch.
            self.wait_while_project_busy().await;
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
        let Some(error) = turn.failure else {
            // Calls the provider sent anyway aren't run: the run is pausing.
            let meta = reasoning_meta(turn.reasoning).unwrap_or_else(|| json!({}));
            self.finalize(&turn.message_id, turn.text, Vec::new(), Some(meta), None)
                .await?;
            self.settle(true).await;
            return Ok(self.stopped());
        };
        // As a failed turn in the run: what arrived is kept, else the error shows.
        let kept = self
            .store_failed_turn(&turn.message_id, &turn.text, turn.reasoning, &error)
            .await?;
        self.settle(kept).await;
        Ok(error.kind == ErrorKind::Cancelled || self.stopped())
    }

    /// A turn the provider failed, stored: what arrived is kept as an
    /// interrupted reply (true), else the reply shows the error (false).
    pub(super) async fn store_failed_turn(
        &self,
        message_id: &str,
        text: &str,
        reasoning: Option<Value>,
        error: &ChatError,
    ) -> Result<bool, String> {
        let stored = failure::stored(error);
        if !text.trim().is_empty() {
            let mut meta = reasoning_meta(reasoning).unwrap_or_else(|| json!({}));
            meta["interrupted"] = stored;
            self.finalize(message_id, text.to_string(), Vec::new(), Some(meta), None)
                .await?;
            Ok(true)
        } else {
            self.finalize(
                message_id,
                String::new(),
                Vec::new(),
                Some(json!({ "failure": stored })),
                Some(failure::describe(error)),
            )
            .await?;
            Ok(false)
        }
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
    pub(super) async fn model_turn(&mut self) -> Result<TurnOutput, String> {
        let mut started = Instant::now();
        let mut pending = new_message("assistant", "");
        pending.finalized = false;
        let message_id = pending.id.clone();
        // Unfinished placeholders are skipped when the history is built.
        self.append_pending(pending).await?;

        // An effort picked since the last request applies from this one.
        self.take_effort();
        let config = self.core.agents.config();
        let clm = self.context_mode == crate::clm::MODE_CLM
            && crate::clm::availability(&self.core.vfs).is_ok();
        // Phase E: ignored compaction pressure restricts this turn to
        // context tools (90%+ at once, step/75% after 4 calls with no edit).
        self.update_clm_pressure(clm).await;
        let context_only = clm && self.clm_restricted;
        let mut tools = tools::tools_for(
            self.root.is_some(),
            self.profile.vision,
            self.profile.no_tools,
            &config.permissions,
            self.plan_mode,
            clm,
            context_only,
        );
        // A subagent gets its kind's tools only; a lead with subagents on,
        // the `task` tool (not on a turn restricted to context tools).
        let kinds = self.subagent_kinds();
        let mut system_prompt = None;
        if let Some(sub) = self.sub() {
            let spec = sub.kind.spec();
            tools.retain(|t| {
                sub.allows(
                    t.pointer("/function/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(""),
                )
            });
            if sub.scope.is_some() {
                tools.push(super::worker::extend_scope_schema());
            }
            system_prompt = Some(format!(
                "{}\n\n{}",
                spec.prompt(&sub.budget),
                self.project_context
            ));
        } else if !kinds.is_empty() && !context_only && !tools.is_empty() {
            tools.push(subagent::task_schema(&kinds, self.groups_now()));
        }
        let subagent_section = if context_only {
            None
        } else {
            subagent::lead_prompt(
                &kinds,
                config.subagents.worker_min_steps(),
                self.groups_now(),
            )
        };
        let group_rules = subagent::plan_group_rules(self.parallel_on());
        let project_context = if self.plan_mode {
            [
                self.project_context.as_str(),
                plan::PLAN_MODE_INSTRUCTIONS,
                group_rules,
                self.plan_context.as_str(),
            ]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n")
        } else {
            self.project_context.clone()
        };
        let project_context = match subagent_section {
            Some(section) => format!("{project_context}\n\n{section}"),
            None => project_context,
        };
        let continue_reply = std::mem::take(&mut self.continue_reply);
        let no_tool_calls = std::mem::take(&mut self.words_only);
        // Forced `todo` (plan-implementation first turn, second checklist
        // nudge): consumed by this turn, carried across its ask-again
        // retries. Providers rejecting the named `tool_choice` fall back to
        // `auto` per request; the execute gate covers them regardless.
        let force_tool = if std::mem::take(&mut self.force_todo_next) {
            Some("todo".to_string())
        } else {
            None
        };
        // The checklist, repeated on every CLM request right after the task
        // card (Trimming mode keeps the end-of-request focus note instead).
        // A restricted turn also says why only context tools are offered.
        let checklist_note = if clm {
            let list = self
                .recent()
                .await
                .ok()
                .and_then(|recent| plan::current_checklist(&recent))
                .map(|c| plan::checklist_note(&c.items, c.carried));
            if context_only {
                let restriction = "[context: this turn offers only context tools (read/write/list_dir on \
                    @context/…, restore_context) and todo: compact your context file now, copying facts forward, \
                    before anything else. The restriction lifts as soon as an edit revision lands.]";
                Some(match list {
                    Some(n) => format!("{n}\n{restriction}"),
                    None => restriction.to_string(),
                })
            } else {
                list
            }
        } else {
            None
        };
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
                pdf: Some(self.profile.pdf),
                current_user_content: self.current_user_content.clone(),
                streaming: self.profile.stream,
                first_byte_timeout_ms: self.profile.first_byte_timeout_ms,
                continue_reply: Some(continue_reply),
                assistant_message_id: Some(message_id.clone()),
                tools: Some(tools.clone()),
                system_prompt: system_prompt.clone(),
                project_context: Some(project_context.clone()),
                volatile_context: self.repo_map.clone(),
                options: self.options.clone(),
                context_mode: Some(self.context_mode.clone()),
                carried_notes: carried_notes.clone(),
                no_tool_calls,
                api_route: self.profile.api_route.clone(),
                chat_id: None,
                force_tool: force_tool.clone(),
                checklist_note: checklist_note.clone(),
            };

            let (cancel_tx, mut cancel_rx) = watch::channel(false);
            let cancel_tx = Arc::new(cancel_tx);
            let stream = Arc::new(Mutex::new(Stream::new(
                self,
                &message_id,
                cancel_tx.clone(),
            )));
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
            let vfs = &self.core.vfs;
            let result = {
                // ROVIK_API_LOG writes who each request is for (S09).
                let request = crate::api_log::TAGS.scope(
                    self.log_tags(),
                    crate::agent::run_chat(
                        &self.core.db,
                        &vfs,
                        params,
                        &mut cancel_rx,
                        &mut on_event,
                    ),
                );
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
                && failure
                    .as_ref()
                    .is_some_and(|e| e.kind == ErrorKind::Cancelled);
            if asked_again {
                let shown = s.shown_anything();
                // P6.2: what the dropped request used, when it said.
                let usage = s.usage.take();
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
                if let Some(sub) = self.sub_mut() {
                    sub.note_usage(usage.as_ref());
                }
                let mut entry = json!({
                    "event": "reply",
                    "dialogId": self.dialog_id,
                    "model": self.profile.model,
                    "error": "asked-again",
                    "cachedTokens": usage.as_ref().and_then(crate::prompt_cache::cached_tokens),
                    "usage": usage,
                    "ms": started.elapsed().as_millis() as u64,
                });
                self.note_sub_fields(&mut entry);
                log::write(&self.core, entry);
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
                    self.core
                        .db
                        .write_detached(move |c| store::update_message(c, &id, &patch));
                }
                opening_status = Some(Status::text(format!(
                    "Asking again with {}…",
                    effort_label(&self.options)
                )));
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

            if let Some(sub) = self.sub_mut() {
                sub.note_usage(usage.as_ref());
            }
            let mut entry = json!({
                "event": "reply",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "finishReason": finish_reason,
                "toolCalls": if output.failure.is_some() { 0 } else { output.tool_calls.len() },
                "textChars": output.text.chars().count(),
                // Prompt tokens the provider served from its cache: the
                // number to watch when changing how requests are built.
                "cachedTokens": usage.as_ref().and_then(crate::prompt_cache::cached_tokens),
                "usage": usage,
                "focusNote": focus_note,
                "clmReminder": clm_reminder,
                "ms": started.elapsed().as_millis() as u64,
            });
            if let Some(error) = &output.failure {
                entry["error"] = json!(error.kind);
            }
            self.note_sub_fields(&mut entry);
            log::write(&self.core, entry);
            return Ok(output);
        }
    }

    /// A subagent's reply lines say whose they are (round 3: the asked-again
    /// line too, so the report keeps its usage before the `task` call ends).
    fn note_sub_fields(&self, entry: &mut Value) {
        if let Some(sub) = self.sub() {
            entry["role"] = json!("sub");
            entry["kind"] = json!(sub.kind.id());
            entry["parentDialogId"] = json!(sub.parent_dialog_id);
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
                if let Some(file) = m
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get("planFile"))
                    .and_then(|f| f.as_str())
                {
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
                 once), then reply in a sentence or two. Don't write it out again in the reply. (If the user only asked a \
                 question, answer it in words instead: no plan is needed.)"
            ),
            "plan",
        )
        .await?;
        Ok(true)
    }

    /// The checklist at the end of a run: unfinished steps are pointed out
    /// up to MAX_CHECKLIST_NUDGES times, and the run goes on. True: the
    /// agent was told, continue. The second nudge also forces the `todo`
    /// call on the next turn (when the provider supports a named
    /// `tool_choice`). A carried checklist counts only when this run worked
    /// on the task (changed files): a quick unrelated question after an
    /// unfinished run is answered, not pushed back into the old work. The
    /// carry note already told the model about it, and the idle panel
    /// still offers Continue.
    async fn check_checklist(&mut self) -> Result<bool, String> {
        if self.checklist_nudges >= MAX_CHECKLIST_NUDGES {
            return Ok(false);
        }
        let recent = self.recent().await?;
        let open: Vec<plan::TodoItem> = plan::current_checklist(&recent)
            .filter(|c| !c.carried || !self.changed_files.is_empty())
            .map(|c| c.items)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !plan::is_settled(i))
            .collect();
        if open.is_empty() {
            return Ok(false);
        }
        self.checklist_nudges += 1;
        let steps = open
            .iter()
            .map(|i| format!("\"{}\"", i.title))
            .collect::<Vec<_>>()
            .join(", ");
        if self.checklist_nudges >= MAX_CHECKLIST_NUDGES {
            // Last reminder: stronger, and the next turn must update the
            // checklist first.
            self.force_todo_next = true;
            self.add_harness(
                format!(
                    "{IDE_NOTE}Your checklist still has unfinished steps: {steps}. \
                     This is the last reminder: call todo now (marking each skipped or changed step with the reason), \
                     finish the remaining steps, and only then finish."
                ),
                "checklist",
            )
            .await?;
        } else {
            self.add_harness(
                format!(
                    "{IDE_NOTE}Your checklist still has unfinished steps: {steps}. \
                     Finish them, or update the checklist marking each skipped or changed with the reason, then finish."
                ),
                "checklist",
            )
            .await?;
        }
        Ok(true)
    }

    /*
     * -----------------------------------------------------------------------
     * CLM context pressure (Phase E)
     * -----------------------------------------------------------------------
     */

    /// Tracks the CLM context before this turn and decides whether it is a
    /// restricted turn (only context tools). Called at the start of every
    /// `model_turn` in CLM mode, before the tool list is built.
    ///
    /// - An edit (a new context revision) lifts everything and records
    ///   turns-to-edit for each pressure that was waiting for it.
    /// - At 90%+ with no edit since the urgent state was first seen, the
    ///   turn is restricted at once.
    /// - A step-compaction request (Phase D) or the 75% reminder escalates
    ///   after CLM_ESCALATE_AFTER_CALLS tool calls with no edit (at 50%+).
    /// - The restriction lifts below 75%, and the safety valve lifts it
    ///   for good after CLM_MAX_RESTRICTED_STALL restricted turns with no
    ///   edit (weak models must not get stuck).
    async fn update_clm_pressure(&mut self, clm: bool) {
        if !clm {
            return;
        }
        self.clm_turns += 1;
        let dialog_id = self.dialog_id.clone();
        let state = self
            .core
            .db
            .read(move |conn| crate::clm::store::load_state(conn, &dialog_id))
            .await
            .unwrap_or_default();
        let pct = crate::clm::prompt::percent(state.tokens, state.budget);
        self.clm_peak_pct = self.clm_peak_pct.max(pct);
        for t in state
            .nudged
            .iter()
            .copied()
            .chain((pct >= crate::clm::prompt::URGENT).then_some(90))
        {
            if !self.clm_reminders_fired.contains(&t) {
                self.clm_reminders_fired.push(t);
            }
        }
        self.clm_reminders_fired.sort_unstable();

        // An edit: a successful write to the context file, a restore, or a
        // command on the file's real path, flagged as it happens
        // (execute.rs). Not a new revision as such: the file is rewritten
        // before every request with the new messages, so its revision moves
        // every turn whether the agent edited it or not.
        let edited = std::mem::take(&mut self.context_edited);
        // Last turn's edit, now measured by the request that read it back.
        if let (Some(before), Some(r)) = (self.edit_delta_pending.take(), state.rendered_revision) {
            self.clm_edit_deltas.push((r, before, state.tokens));
        }
        if !self.clm_tracking_started {
            self.clm_tracking_started = true;
            if !edited {
                return;
            }
        }
        if edited {
            self.clm_edits_seen += 1;
            self.edit_delta_pending = Some(state.tokens);
            if self.pending_step_compaction.take().is_some() {
                if let Some(asked) = self.step_asked_turn.take() {
                    self.step_turns_to_edit
                        .get_or_insert(self.clm_turns.saturating_sub(asked));
                }
            }
            if let Some(w) = self.urgent_since.take() {
                self.urgent_turns_to_edit
                    .get_or_insert(self.clm_turns.saturating_sub(w.turn));
            }
            if let Some(w) = self.tier75_since.take() {
                self.tier75_turns_to_edit
                    .get_or_insert(self.clm_turns.saturating_sub(w.turn));
            }
            self.clm_restricted = false;
            self.restricted_stall = 0;
            return;
        }

        // Safety valve first: two restricted turns already, still no edit.
        if self.clm_restricted && self.restricted_stall >= CLM_MAX_RESTRICTED_STALL {
            self.clm_restricted = false;
            self.restricted_stall = 0;
            self.clm_non_compliant = true;
            log::write(
                &self.core,
                json!({
                    "event": "clmNonCompliant",
                    "dialogId": self.dialog_id,
                    "model": self.profile.model,
                    "turn": self.clm_turns,
                    "pct": pct,
                    "restrictedTurns": self.restricted_turns,
                }),
            );
            return;
        }
        // Below 75% the restriction no longer applies.
        if self.clm_restricted && pct < CLM_RESTRICT_LIFT_BELOW {
            self.clm_restricted = false;
            self.restricted_stall = 0;
        }

        // Remember when each pressure started waiting for its edit; an edit
        // clears it, and it re-arms if the pressure is still there.
        if state.nudged.contains(&75) && self.tier75_since.is_none() {
            self.tier75_since = Some(ClmWait {
                tool_calls: self.guard.tool_calls,
                turn: self.clm_turns,
            });
        }
        if pct >= crate::clm::prompt::URGENT && self.urgent_since.is_none() {
            self.urgent_since = Some(ClmWait {
                tool_calls: self.guard.tool_calls,
                turn: self.clm_turns,
            });
        }

        if self.clm_non_compliant || self.clm_restricted {
            if self.clm_restricted {
                self.restricted_turns += 1;
                self.restricted_stall += 1;
            }
            return;
        }
        let reason = restriction_reason(
            pct,
            self.urgent_since.is_some(),
            self.pending_step_compaction.as_ref().map(|p| p.tool_calls),
            self.tier75_since.as_ref().map(|w| w.tool_calls),
            self.guard.tool_calls,
        );
        if let Some(reason) = reason {
            self.clm_restricted = true;
            self.restricted_turns += 1;
            self.restricted_stall += 1;
            log::write(
                &self.core,
                json!({
                    "event": "clmRestricted",
                    "dialogId": self.dialog_id,
                    "model": self.profile.model,
                    "reason": reason,
                    "turn": self.clm_turns,
                    "pct": pct,
                    "toolCalls": self.guard.tool_calls,
                }),
            );
        }
    }

    /// The per-run CLM compliance log (Phase E, step 17): which pressures
    /// fired, turns-to-edit, edits with their token deltas, recall-hint
    /// hits (redone work), the peak context, restricted turns and the
    /// safety-valve fallback.
    async fn log_clm_compliance(&self, outcome: &str) {
        if self.context_mode != crate::clm::MODE_CLM || !self.clm_tracking_started {
            return;
        }
        let dialog_id = self.dialog_id.clone();
        let revisions = self
            .core
            .db
            .read(move |conn| crate::clm::store::revisions(conn, &dialog_id))
            .await
            .unwrap_or_default();
        let edit_revisions = revisions.iter().filter(|r| r.kind != "context").count();
        log::write(
            &self.core,
            json!({
                "event": "clm",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "outcome": outcome,
                "remindersFired": self.clm_reminders_fired,
                "turnsToEditStep": self.step_turns_to_edit,
                "turnsToEdit75": self.tier75_turns_to_edit,
                "turnsToEditUrgent": self.urgent_turns_to_edit,
                "edits": self.clm_edits_seen,
                // Chat-wide (every run so far), not this run only.
                "chatEditRevisions": edit_revisions,
                "editDeltas": self.clm_edit_deltas.iter().map(|(r, before, after)| json!({
                    "revision": r, "before": before, "after": after,
                })).collect::<Vec<_>>(),
                "recallHints": self.clm_recall_hits,
                "peakPct": self.clm_peak_pct,
                "restrictedTurns": self.restricted_turns,
                "nonCompliant": self.clm_non_compliant,
            }),
        );
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
        let Some(root) = self.root.clone() else {
            return Ok(false);
        };
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
        let rebuilt = crate::rt::spawn_blocking(move || {
            let root_path = std::path::Path::new(&root);
            let focus = crate::repo_map::focus_from(root_path, &request_text, &touched);
            let options = crate::repo_map::Options {
                max_chars,
                ..Default::default()
            };
            crate::repo_map::repo_map(root_path, &focus, &options)
        })
        .await;
        // A rebuild that failed (the task panicked) keeps the map the run
        // has, instead of leaving the model without one for the rest of the
        // run. One that found nothing is real (no declarations left): no map.
        let Ok(new_map) = rebuilt else {
            return Ok(false);
        };
        if new_map == self.repo_map {
            return Ok(false);
        }
        self.repo_map = new_map;
        self.map_refreshes += 1;
        log::write(
            &self.core,
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
        recent_messages(&self.core.db, &self.dialog_id).await
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
        let seqs = self
            .core
            .db
            .write(move |c| store::append_messages(c, &id, &stored))
            .await?;
        let out = messages
            .iter()
            .zip(seqs)
            .map(|(m, seq)| OutMessage::from_new(&self.dialog_id, seq, m, pending))
            .collect();
        self.core.emit(AgentEvent::MessagesAdded {
            dialog_id: self.dialog_id.clone(),
            messages: out,
        });
        Ok(())
    }

    /// A user-role message the IDE writes to the model (never shown as the
    /// user's, never taken for their request).
    pub(super) async fn add_harness(&self, content: String, kind: &str) -> Result<(), String> {
        log::write(
            &self.core,
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
        #[cfg(test)]
        if FAIL_TOOL_RESULT.lock().unwrap().contains(tool_call_id) {
            return Err("the database is gone (test)".into());
        }
        let mut m = new_message("tool", &content);
        m.tool_call_id = Some(tool_call_id.to_string());
        m.meta = meta;
        m.attachments = attachments;
        self.append(vec![m]).await
    }

    /// A reply's final content: stored (indexed for search), and shown.
    pub(super) async fn finalize(
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
        self.core
            .db
            .write(move |c| store::update_message(c, &id, &patch))
            .await?;
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
    /// Marks the chat's records still waiting for "Run checks" superseded.
    async fn supersede_waiting_checks(&self) {
        let Ok(recent) = self.recent().await else {
            return;
        };
        for m in recent {
            let Some(mut meta) = m.meta.clone() else {
                continue;
            };
            let waiting = meta
                .pointer("/verification/status")
                .and_then(|s| s.as_str())
                == Some("needs-approval");
            if waiting {
                meta["verification"]["status"] = json!("superseded");
                // Its own round limit stays (round 2): not this run's.
                let _ = self.store_meta(&m.id, meta).await;
            }
        }
    }

    pub(super) async fn update_meta(&self, message_id: &str, meta: Value) -> Result<(), String> {
        let mut meta = self.with_max_rounds(meta);
        // P1.7: the record whose checks found no carried state says so;
        // round 3: so does one whose integrity check couldn't be complete
        // (a spilled file lost), carry or not.
        if let Some(v) = meta.get_mut("verification").and_then(|v| v.as_object_mut()) {
            if self.carry_lost.as_deref() == Some(message_id) {
                v.insert("stateRestored".into(), json!(false));
            }
            if self.integrity_unchecked {
                v.insert("integrityChecked".into(), json!(false));
            }
        }
        self.store_meta(message_id, meta).await
    }

    /// Stores a message's meta as given and shows it.
    async fn store_meta(&self, message_id: &str, meta: Value) -> Result<(), String> {
        let id = message_id.to_string();
        let patch = MessagePatch {
            meta: Some(meta.clone()),
            ..Default::default()
        };
        self.core
            .db
            .write(move |c| store::update_message(c, &id, &patch))
            .await?;
        self.patch_ui(
            message_id,
            Patch {
                meta: Some(meta),
                ..Default::default()
            },
        );
        Ok(())
    }

    pub(super) fn patch_ui(&self, message_id: &str, patch: Patch) {
        self.core.emit(AgentEvent::MessagePatched {
            dialog_id: self.dialog_id.clone(),
            message_id: message_id.to_string(),
            patch,
        });
    }

    /// The first answer to a sent message: on none, the message is marked
    /// failed (its draft comes back in the composer).
    async fn settle(&mut self, ok: bool) {
        let Some(message_id) = self.settle_message.take() else {
            return;
        };
        if !ok {
            let id = message_id.clone();
            let patch = MessagePatch {
                failed: Some(true),
                ..Default::default()
            };
            match self
                .core
                .db
                .write(move |c| store::update_message(c, &id, &patch))
                .await
            {
                Ok(()) => self.patch_ui(
                    &message_id,
                    Patch {
                        failed: Some(true),
                        ..Default::default()
                    },
                ),
                Err(e) => eprintln!("agent: could not mark a message failed: {e}"),
            }
        }
        self.core.emit(AgentEvent::UserMessageSettled {
            dialog_id: self.dialog_id.clone(),
            message_id,
            ok,
        });
    }

    /// The user's latest request in the chat, as sent (for the AI reviewer).
    pub(super) async fn latest_user_request(&self) -> String {
        // A subagent acts for the lead's user request, not for the brief the
        // lead wrote (K06): the AI reviewer judges by the user's intent.
        if let Some(sub) = self.sub() {
            return sub.parent_request.clone();
        }
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
struct Stream {
    core: Core,
    dialog_id: String,
    message_id: String,
    profile_id: String,
    cancel: Arc<watch::Sender<bool>>,
    stop: Arc<StopSignal>,

    text: String,
    reasoning: String,
    /// The reasoning as it is sent back (`reasoning_details`; reasoning_echo.rs).
    reasoning_details: Vec<Value>,
    /// The reasoning came in the reply text, in this tag (`<think>`): it goes
    /// back in the same form.
    reasoning_inline: Option<String>,
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

impl Stream {
    fn new(run: &Run, message_id: &str, cancel: Arc<watch::Sender<bool>>) -> Self {
        let now = Instant::now();
        Stream {
            core: run.core.clone(),
            dialog_id: run.dialog_id.clone(),
            message_id: message_id.to_string(),
            profile_id: run.profile.id.clone(),
            cancel,
            stop: run.stop.clone(),
            text: String::new(),
            reasoning: String::new(),
            reasoning_details: Vec::new(),
            reasoning_inline: None,
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
        self.core.emit(AgentEvent::MessagePatched {
            dialog_id: self.dialog_id.clone(),
            message_id: self.message_id.clone(),
            patch,
        });
    }

    fn set_status(&mut self, status: Option<Status>) {
        if self.status == status {
            return;
        }
        self.status = status.clone();
        self.patch(Patch {
            status: Some(status),
            ..Default::default()
        });
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
                    self.core.emit(AgentEvent::ReasoningLearned {
                        profile_id: self.profile_id.clone(),
                        accepted: Vec::new(),
                        rejected: Vec::new(),
                        unresolved: Vec::new(),
                        reasons: true,
                    });
                }
                self.dirty_reasoning = true;
            }
            ChatStreamEvent::ReasoningSupport {
                accepted,
                rejected,
                unresolved,
            } => {
                self.core.emit(AgentEvent::ReasoningLearned {
                    profile_id: self.profile_id.clone(),
                    accepted: accepted.clone(),
                    rejected: rejected.clone(),
                    unresolved: unresolved.clone(),
                    reasons: false,
                });
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
                if self.repetition_cut.is_none()
                    && self.text.len() - self.repetition_checked_at > 1500
                {
                    self.repetition_checked_at = self.text.len();
                    if let Some(cut) = repetition::find_repetition(&self.text) {
                        self.repetition_cut = Some(cut);
                        let _ = self.cancel.send(true);
                    }
                }
            }
            ChatStreamEvent::Finished {
                finish_reason,
                usage,
            } => {
                self.finish_reason = finish_reason;
                self.usage = usage;
            }
            ChatStreamEvent::ApiRoute { route } => {
                self.core.emit(AgentEvent::ApiRouteLearned {
                    profile_id: self.profile_id.clone(),
                    route,
                });
            }
            ChatStreamEvent::Done => {}
            ChatStreamEvent::Retrying {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => {
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
            ChatStreamEvent::ReasoningInline { tag } => self.reasoning_inline = Some(tag),
            ChatStreamEvent::ContextNotes { notes } => {
                // A budget reminder (CLM), up to its numbers, for the reply's log.
                self.clm_reminder = notes
                    .iter()
                    .find(|n| n.starts_with("CONTEXT BUDGET"))
                    .map(|n| n.split(" tokens)").next().unwrap_or(n).to_string());
                self.notes = notes;
            }
            ChatStreamEvent::ContextUpdated { status } => {
                self.core.emit(AgentEvent::ContextUpdated {
                    dialog_id: self.dialog_id.clone(),
                    status,
                });
            }
            ChatStreamEvent::ToolCallStart { id, name } => {
                self.mark_output();
                let call = ToolCall {
                    id: id.clone(),
                    name,
                    arguments: String::new(),
                    extra_content: None,
                };
                match self.tool_calls.iter_mut().find(|c| c.id == id) {
                    Some(existing) => *existing = call,
                    None => self.tool_calls.push(call),
                }
                self.dirty_tools = true;
            }
            ChatStreamEvent::ToolCallArgumentsDelta {
                id,
                arguments_delta,
            } => {
                if let Some(call) = self.tool_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&arguments_delta);
                    self.dirty_tools = true;
                }
            }
            ChatStreamEvent::ToolCallComplete {
                id,
                name,
                arguments,
                extra_content,
            } => {
                // Sent back unchanged in the next request (Gemini 3 requires it).
                let call = ToolCall {
                    id: id.clone(),
                    name,
                    arguments,
                    extra_content,
                };
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
        if let Some(tag) = &self.reasoning_inline {
            r["inline"] = json!(tag);
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
            patch.meta = self
                .reasoning_meta(false)
                .map(|r| json!({ "reasoning": r }));
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
            self.core
                .db
                .write_detached(move |c| store::update_message(c, &id, &patch));
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
    let Some(options) = options.as_mut().filter(|o| o.reasoning_effort.is_some()) else {
        return;
    };
    let formats = options.reasoning_formats.get_or_insert_with(|| {
        vec![
            "reasoning_effort".to_string(),
            "chat_template_kwargs".to_string(),
        ]
    });
    formats.retain(|f| !refused.contains(f) && REASONING_FIELDS.contains(&f.as_str()));
}

/// The same effort level (the forms it's sent in follow what's learned).
fn same_effort(a: &Option<OpenAIChatOptions>, b: &Option<OpenAIChatOptions>) -> bool {
    let level = |o: &Option<OpenAIChatOptions>| {
        serde_json::to_value(o.as_ref().and_then(|o| o.reasoning_effort)).ok()
    };
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

/// Tests: storing this call's result fails, as with a broken database.
#[cfg(test)]
pub(crate) static FAIL_TOOL_RESULT: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

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
    db.read(move |c| store::get_messages(c, &id, None, RECENT_MESSAGES))
        .await
        .map(|page| page.messages)
}

fn sent_content(m: &MessageRow) -> Option<String> {
    m.meta
        .as_ref()?
        .get("sentContent")?
        .as_str()
        .map(str::to_string)
}

/// The mode of the request a run belongs to: its starting message's.
fn run_mode_for(messages: &[MessageRow]) -> Option<String> {
    let m = messages
        .iter()
        .rev()
        .find(|m| m.role == "user" && !plan::is_harness(m))?;
    m.meta.as_ref()?.get("mode")?.as_str().map(str::to_string)
}

/// B07b: the plan file a "Implement the plan in `…`" request names (or the
/// chat's own), and what the checker finds in it as it is now.
fn plan_check_at_start(
    recent: &[MessageRow],
    root: Option<&str>,
    project_root: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let named = recent
        .iter()
        .rev()
        .find(|m| m.role == "user" && !plan::is_harness(m))
        .and_then(|m| {
            let rest = m.content.strip_prefix("Implement the plan in `")?;
            Some(rest[..rest.find('`')?].to_string())
        });
    let file = match named {
        Some(rel) => {
            let base = project_root.or(root)?;
            let p = std::path::Path::new(&rel);
            if p.is_absolute() {
                rel
            } else {
                std::path::Path::new(base)
                    .join(p)
                    .to_string_lossy()
                    .into_owned()
            }
        }
        None => chat_plan_file(recent)?,
    };
    let text = std::fs::read_to_string(&file).ok()?;
    let problems = plan::plan_problems_in(&text, root.map(std::path::Path::new));
    (!problems.is_empty()).then_some((file, problems))
}

/// The chat's plan file: the latest one a plan was saved to.
pub(super) fn chat_plan_file(messages: &[MessageRow]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        m.meta
            .as_ref()?
            .get("planFile")?
            .as_str()
            .map(str::to_string)
    })
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

/// Files the chat read or changed, latest last.
fn touched_paths(messages: &[MessageRow]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for call in messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
    {
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
    let m = messages
        .iter()
        .rev()
        .find(|m| m.role == "user" && !m.failed && !plan::is_harness(m))?;
    sent_content(m).or_else(|| Some(m.content.clone()))
}

/// Whether this CLM turn is restricted to context tools, and why: at 90%+
/// while the urgent state waits for an edit, or at 50%+ once a
/// step-compaction request or the 75% reminder has gone unanswered for
/// CLM_ESCALATE_AFTER_CALLS tool calls. The waits are cleared by an edit,
/// so a wait that's still here means no edit since it started.
fn restriction_reason(
    pct: u32,
    urgent_waiting: bool,
    step_asked_at: Option<u32>,
    tier75_seen_at: Option<u32>,
    tool_calls: u32,
) -> Option<&'static str> {
    let waited = |since: u32| tool_calls.saturating_sub(since) >= CLM_ESCALATE_AFTER_CALLS;
    if pct >= crate::clm::prompt::URGENT && urgent_waiting {
        Some("urgent")
    } else if pct >= CLM_ESCALATE_MIN_PCT && step_asked_at.is_some_and(waited) {
        Some("step")
    } else if pct >= CLM_ESCALATE_MIN_PCT && tier75_seen_at.is_some_and(waited) {
        Some("tier75")
    } else {
        None
    }
}

/// The real user message before the latest one, as sent: what a short
/// "continue" is continuing. None when there is no earlier request.
fn previous_user_wire_content(messages: &[MessageRow]) -> Option<String> {
    let mut real = messages
        .iter()
        .filter(|m| m.role == "user" && !m.failed && !plan::is_harness(m))
        .rev();
    real.next()?;
    let m = real.next()?;
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
            Some(calls)
                if m.role == "assistant"
                    && m.finalized
                    && m.error.is_none()
                    && !calls.is_empty() =>
            {
                calls
            }
            _ => return Vec::new(),
        };
        let answered: Vec<&str> = messages[i + 1..]
            .iter()
            .filter(|x| x.role == "tool")
            .filter_map(|x| x.tool_call_id.as_deref())
            .collect();
        return calls
            .iter()
            .filter(|c| !answered.contains(&c.id.as_str()))
            .cloned()
            .collect();
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
        ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: "{}".into(),
            extra_content: None,
        }
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
        assert!(
            unanswered_tool_calls(&[step]).is_empty(),
            "a reply cut off mid-stream isn't resumed"
        );
    }

    #[test]
    fn efforts_compare_by_level_and_keep_refused_forms_out() {
        let options = |v: Value| serde_json::from_value::<Option<OpenAIChatOptions>>(v).unwrap();
        let high = options(
            json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort"] }),
        );
        let high_more = options(
            json!({ "reasoning_effort": "high", "reasoning_formats": ["reasoning_effort", "reasoning"] }),
        );
        assert!(same_effort(&high, &high_more));
        assert!(!same_effort(
            &high,
            &options(json!({ "reasoning_effort": "low" }))
        ));
        assert!(!same_effort(&high, &None));

        // A new pick still listing a form refused earlier in the run.
        let mut picked = options(
            json!({ "reasoning_effort": "low", "reasoning_formats": ["reasoning_effort", "chat_template_kwargs"] }),
        );
        without_refused(&mut picked, &["chat_template_kwargs".to_string()]);
        assert_eq!(
            picked.unwrap().reasoning_formats.unwrap(),
            ["reasoning_effort"]
        );
        // No forms given: the default ones, minus the refused.
        let mut default_forms = options(json!({ "reasoning_effort": "low" }));
        without_refused(&mut default_forms, &["reasoning_effort".to_string()]);
        assert_eq!(
            default_forms.unwrap().reasoning_formats.unwrap(),
            ["chat_template_kwargs"]
        );
    }

    #[test]
    fn reads_mode_and_request_from_the_chat() {
        let user = row(
            "user",
            Some(json!({ "mode": "plan", "sentContent": "wire" })),
        );
        let harness = row("user", Some(json!({ "harness": "verification" })));
        let messages = vec![
            user,
            row("assistant", Some(json!({ "planFile": "/p/a.md" }))),
            harness,
        ];
        assert_eq!(run_mode_for(&messages).as_deref(), Some("plan"));
        assert_eq!(last_user_wire_content(&messages).as_deref(), Some("wire"));
        assert_eq!(chat_plan_file(&messages).as_deref(), Some("/p/a.md"));
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
        assert_eq!(
            repo_map_chars(None),
            2_560,
            "an unknown window counts as 32k"
        );
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
                    .map(|(name, args)| ToolCall {
                        id: String::new(),
                        name: name.to_string(),
                        arguments: args.to_string(),
                        extra_content: None,
                    })
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
            with_calls(&[
                ("read", r#"{"path":"a.rs"}"#),
                ("search_code", r#"{"query":"x"}"#),
            ]),
            with_calls(&[
                ("write", r#"{"path":"b.rs","old_text":"x","content":"y"}"#),
                ("bash", r#"{"command":"ls"}"#),
            ]),
            with_calls(&[
                ("read", r#"{"path":"a.rs"}"#),
                ("write", "not json"),
                ("list_dir", r#"{"path":"src"}"#),
            ]),
        ];
        assert_eq!(touched_paths(&messages), ["b.rs", "a.rs"]);
    }

    #[test]
    fn only_the_latest_touched_paths_are_kept() {
        let calls: Vec<String> = (0..30)
            .map(|i| format!(r#"{{"path":"f{i}.rs"}}"#))
            .collect();
        let pairs: Vec<(&str, &str)> = calls.iter().map(|c| ("read", c.as_str())).collect();
        let paths = touched_paths(&[with_calls(&pairs)]);
        assert_eq!(paths.len(), MAX_TOUCHED_PATHS);
        assert_eq!(paths.first().unwrap(), "f10.rs");
        assert_eq!(paths.last().unwrap(), "f29.rs");
    }
}

#[cfg(test)]
mod clm_pressure_tests {
    use super::*;

    #[test]
    fn restriction_needs_an_unanswered_pressure() {
        // Urgent: restricted at once while it waits for an edit.
        assert_eq!(restriction_reason(92, true, None, None, 0), Some("urgent"));
        assert_eq!(
            restriction_reason(92, false, None, None, 0),
            None,
            "an edit cleared the wait"
        );
        // A step-compaction request escalates after 4 calls with no edit, at 50%+.
        assert_eq!(restriction_reason(60, false, Some(10), None, 13), None);
        assert_eq!(
            restriction_reason(60, false, Some(10), None, 14),
            Some("step")
        );
        assert_eq!(
            restriction_reason(40, false, Some(10), None, 30),
            None,
            "below 50%"
        );
        // The 75% reminder, likewise.
        assert_eq!(
            restriction_reason(78, false, None, Some(5), 9),
            Some("tier75")
        );
        assert_eq!(restriction_reason(78, false, None, Some(5), 8), None);
        // Nothing waiting: never restricted, whatever the size.
        assert_eq!(restriction_reason(85, false, None, None, 100), None);
    }
}

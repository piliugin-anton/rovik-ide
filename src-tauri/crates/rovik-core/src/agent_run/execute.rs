//! One turn's tool calls: authorized, run and recorded in order. Each
//! result is stored before the next turn, which reads the history from the
//! database. Every call gets a result, even after Stop: providers reject a
//! history with tool calls left unanswered.
//!
//! A run of consecutive local reads that need no approval runs at the same
//! time (parallel.rs); their results are still recorded in the calls' order.

use super::builtin_tools::{BuiltinTool, Dispatch};
use super::diff::{self, Hunk};
use super::events::{AgentEvent, ApprovalRequest, EditProposal, QuestionsRequest};
use super::guard::{FinishedCall, GuardVerdict, Write};
use super::parallel::{self, InOrder};
use super::permissions::{self, Approver, ToolVerdict};
use super::review::{self, Group, ReviewInput, ReviewResult};
use super::run::{chat_plan_file, Run};
use super::tools::Outcome;
use super::{log, plan, tools, ApprovalDecision};
use crate::chat_db::store::ToolCall;
use serde_json::{json, Map, Value};

/// The agent's own context folder, as its file tools name it (CLM mode).
const CONTEXT_PATH_PREFIX: &str = "@context";

/// What a call after the one the user denied gets, when they sent a
/// message meanwhile (D6): not run, and not counted by the loop guard.
const DENIED_WITH_MESSAGE: &str = "Error: not run: the user denied an earlier call in this reply and sent a message. Read it first.";

/// What authorizing a call decided.
#[derive(Default)]
struct Authorized {
    /// The refusal to send back to the model; None: run it.
    refusal: Option<String>,
    /// The decision to show on the tool block.
    approval: Option<Value>,
    /// Partially accepted write: the arguments to run instead (a partial
    /// write becomes a whole-file one).
    args_override: Option<String>,
    /// Added to the tool result for the model.
    result_note: Option<String>,
    /// The refusal is a "no" from the user, their AI reviewer or their
    /// permission settings (not a call that couldn't apply).
    rejected: bool,
    /// An approved edit: the file it was reviewed against. The file may
    /// change while the approval waits (reconcile_review).
    reviewed: Option<Reviewed>,
    /// The "no" is the user's own, answering the approval: not their AI
    /// reviewer's, their settings', nor Autopilot's deadline (D6).
    by_user: bool,
}

/// The file an approved edit was reviewed against.
#[derive(Clone)]
struct Reviewed {
    abs_path: String,
    rel_path: String,
    current: String,
}

fn refused(text: impl Into<String>) -> Authorized {
    Authorized {
        refusal: Some(text.into()),
        ..Default::default()
    }
}

fn rejected(text: impl Into<String>) -> Authorized {
    Authorized {
        rejected: true,
        ..refused(text)
    }
}

/// A restricted-file check, off the async threads: a command's may go
/// through the folders it searches. A check that fails refuses the call.
async fn checked_off_thread(
    check: impl FnOnce() -> Option<String> + Send + 'static,
) -> Option<String> {
    crate::rt::spawn_blocking(check).await.unwrap_or_else(|e| {
        Some(format!(
            "Error: this call couldn't be checked against the restricted files ({e}), so it didn't run."
        ))
    })
}

/// How a call ran: as which tool, with which arguments (an approval may
/// have changed both), and the file it writes.
struct Ran {
    tool_name: String,
    args_json: String,
    write: Option<WriteTarget>,
}

struct WriteTarget {
    /// As the call gave it, and absolute.
    path: String,
    abs_path: Option<String>,
    /// The file before the write, so the guard can tell an edit undoing earlier ones.
    before: Option<String>,
}

enum Prepared {
    Diff {
        proposal: EditProposal,
        hunks: Vec<Hunk>,
    },
    New,
    Unchanged,
    Invalid(String),
}

fn approval_json(
    by: Approver,
    approved: bool,
    model: Option<String>,
    reason: Option<String>,
    checks: &[review::Check],
) -> Value {
    let mut v = json!({ "by": by, "approved": approved });
    if !checks.is_empty() {
        v["checks"] = json!(checks);
    }
    if let Some(m) = model {
        v["model"] = json!(m);
    }
    if let Some(r) = reason {
        v["reason"] = json!(r);
    }
    v
}

/// Whether a file tool's path means the agent's context folder.
fn is_context_path(path: Option<&str>, real_dir: Option<&str>) -> bool {
    let Some(path) = path else { return false };
    let p = path.trim().replace('\\', "/");
    if p == CONTEXT_PATH_PREFIX || p.starts_with(&format!("{CONTEXT_PATH_PREFIX}/")) {
        return true;
    }
    let Some(dir) = real_dir else { return false };
    let dir = dir.replace('\\', "/");
    let dir = dir.trim_end_matches('/');
    p == dir || p.starts_with(&format!("{dir}/"))
}

/// Tells the model which of its changes were not applied.
fn describe_rejected_hunks(path: &str, rejected: &[Hunk], total: usize) -> String {
    let preview = |lines: &[String]| {
        let mut shown: Vec<String> = lines.iter().take(3).map(|l| format!("    {l}")).collect();
        if lines.len() > 3 {
            shown.push(format!("    … ({} more lines)", lines.len() - 3));
        }
        shown.join("\n")
    };
    let mut out = vec![
        String::new(),
        format!(
            "The user reviewed this edit and accepted {} of {total} changes; the file now has only those. Rejected changes:",
            total.saturating_sub(rejected.len())
        ),
    ];
    for h in rejected {
        let at = format!("around original line {}", h.orig_start);
        out.push(if h.added.is_empty() {
            format!(
                "- {at}: deleting these lines (kept):\n{}",
                preview(&h.deleted)
            )
        } else if h.deleted.is_empty() {
            format!(
                "- {at}: inserting these lines (not added):\n{}",
                preview(&h.added)
            )
        } else {
            format!(
                "- {at}: replacing\n{}\n  with (not applied):\n{}",
                preview(&h.deleted),
                preview(&h.added)
            )
        });
    }
    out.push(format!(
        "Read {path} again before editing it further, and don't redo the rejected changes unless the user asks."
    ));
    out.join("\n")
}

/// Paths extend_scope grants a step worker at most (P2.3).
const MAX_GRANTS: usize = 8;

impl Run {
    pub(super) async fn execute_tool_calls(
        &mut self,
        calls: &[ToolCall],
        resume_note: Option<&str>,
    ) -> Result<(), String> {
        // CLM mode: the agent's context folder.
        let vfs = &self.core.vfs;
        let clm =
            self.context_mode == crate::clm::MODE_CLM && crate::clm::availability(vfs).is_ok();
        let context_dir = if clm {
            crate::clm::real_paths(vfs, &self.dialog_id).map(|(_, dir)| dir)
        } else {
            None
        };

        // G02: calls an earlier run made (a resume) are this run's now.
        self.calls.made(calls);
        self.steer
            .note(super::steer::Doing::Calls, self.guard.tool_calls);
        let mut rest = calls;
        while !rest.is_empty() {
            // P2.2: a subagent's time runs out within a reply too: what's
            // left of it isn't run.
            if self.wall_left() == Some(std::time::Duration::ZERO) {
                let wall = self.sub().map_or(0, |s| s.budget.wall.as_secs());
                for tc in rest {
                    self.calls.start(&tc.id);
                    let content = format!(
                        "Error: not run: it's past your time budget of about {wall} seconds. Write your report now with what you have."
                    );
                    self.answer_unrun(tc, content, Outcome::Failed, true, None)
                        .await?;
                }
                break;
            }
            // The lead's subagents: a run of `task` calls starts together
            // (subagent_run.rs), at most the settings' number at once.
            let tasks = if self.sub().is_none() {
                rest.iter()
                    .take_while(|tc| {
                        BuiltinTool::parse(&tc.name)
                            .is_some_and(|t| t.spec().dispatch == Dispatch::Subagents)
                    })
                    .count()
            } else {
                0
            };
            if tasks > 0 {
                for tc in &rest[..tasks] {
                    self.calls.start(&tc.id);
                }
                self.execute_tasks(&rest[..tasks], resume_note.is_some())
                    .await?;
                rest = &rest[tasks..];
                continue;
            }
            self.note_progress(&rest[0].name);
            // Decided when it's this call's turn: an approval before it may
            // have taken a while, and Stop may have come.
            let n = parallel::batch_len(rest, |tc| {
                self.runs_in_parallel(tc, resume_note, clm, context_dir.as_deref())
            });
            if n > 0 {
                for tc in &rest[..n] {
                    self.calls.start(&tc.id);
                }
                self.execute_parallel(&rest[..n], clm).await?;
                rest = &rest[n..];
            } else {
                self.calls.start(&rest[0].id);
                self.execute_one(&rest[0], resume_note, clm, context_dir.as_deref())
                    .await?;
                rest = &rest[1..];
                // D6: the user denied that call with a message waiting: the
                // message redirects the reply, whose later calls don't run.
                // It's taken now, so the note below always has it after it.
                if std::mem::take(&mut self.denied_by_user) && !rest.is_empty() && !self.stopped() {
                    let waiting = self.steer.take();
                    if !waiting.is_empty() {
                        for tc in rest {
                            self.calls.start(&tc.id);
                            self.add_tool_result(
                                &tc.id,
                                DENIED_WITH_MESSAGE.into(),
                                Outcome::Failed,
                                None,
                                Vec::new(),
                            )
                            .await?;
                        }
                        self.read_steering(waiting, "deny").await?;
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether a call can run alongside its neighbours: a local read that
    /// is permitted without asking, outside the agent's context folder.
    fn runs_in_parallel(
        &self,
        tc: &ToolCall,
        resume_note: Option<&str>,
        clm: bool,
        context_dir: Option<&str>,
    ) -> bool {
        // A local read (its row's `parallel`), in a mode that has it.
        let Some(spec) = BuiltinTool::parse(&tc.name).map(|t| t.spec()) else {
            return false;
        };
        if resume_note.is_some()
            || self.stopped()
            || !spec.parallel
            || !spec.modes.offered_in(self.plan_mode)
            || self.sub().is_some_and(|s| !s.allows(&tc.name))
        {
            return false;
        }
        // A restricted CLM turn: every call goes through execute_one, which
        // refuses project work until the context is compacted.
        if clm && self.clm_restricted {
            return false;
        }
        let args = permissions::parse_args(&tc.arguments);
        if clm && is_context_path(args.get("path").and_then(|p| p.as_str()), context_dir) {
            return false;
        }
        let config = self.config();
        // A read of a restricted file goes through `authorize`, refused.
        let restricted = |root: &str| {
            super::restricted::problem(&tc.name, &args, root, &self.restricted).is_some()
        };
        matches!(
            permissions::check_tool_call(&tc.name, &tc.arguments, &config.permissions),
            ToolVerdict::Permitted { approval: None }
        ) && !self.names_daemon_folder(tc)
            && !self.root.as_deref().is_some_and(restricted)
    }

    /// Why a call may not run, when it would read or change one of the
    /// project's restricted files (restricted.rs).
    async fn restricted_problem(&self, name: &str, args: Map<String, Value>) -> Option<String> {
        let root = self.root.clone()?;
        let (name, restricted) = (name.to_string(), self.restricted.clone());
        checked_off_thread(move || super::restricted::problem(&name, &args, &root, &restricted))
            .await
    }

    /// [`Self::restricted_problem`] for a harness's call, whose command its
    /// bash runs in `cwd` (its shell's folder), or in the project's.
    async fn harness_restricted_problem(
        &self,
        name: &str,
        args: Map<String, Value>,
        cwd: Option<String>,
    ) -> Option<String> {
        let root = self.root.clone()?;
        let cwd = cwd.unwrap_or_else(|| root.clone());
        let (name, restricted) = (name.to_string(), self.restricted.clone());
        checked_off_thread(move || {
            super::restricted::harness_problem_in(&name, &args, &root, &cwd, &restricted)
        })
        .await
    }

    /// A command that names a server's own folder: it waits for the user,
    /// whatever the settings say (D8).
    fn names_daemon_folder(&self, tc: &ToolCall) -> bool {
        tc.name == "bash"
            && self.core.agents.on_server()
            && permissions::str_arg(&tc.arguments, "command")
                .is_some_and(|command| super::server::names_daemon_folder(&command))
    }

    /// Calls that run together: each as a task of its own, at most
    /// MAX_PARALLEL at once, recorded in order as they finish. A call still
    /// running when Stop comes is aborted and recorded as stopped.
    async fn execute_parallel(&mut self, calls: &[ToolCall], clm: bool) -> Result<(), String> {
        #[cfg(test)]
        parallel::BATCHES.lock().unwrap().push((
            self.dialog_id.clone(),
            calls.iter().map(|c| c.id.clone()).collect(),
        ));
        let started = std::time::Instant::now();
        let jobs: Vec<_> = calls
            .iter()
            .map(|tc| {
                let (app, root) = (self.core.clone(), self.root.clone());
                let (name, args, stop) = (tc.name.clone(), tc.arguments.clone(), self.stop.clone());
                let ctx = self.call_context(&tc.id);
                async move {
                    // G02: its own time, from when its slot came.
                    let started = std::time::Instant::now();
                    let result = parallel::spawned(async move {
                        tools::execute(&app, root.as_deref(), &name, &args, &stop, &ctx).await
                    })
                    .await
                    .unwrap_or_else(|e| tools::ToolResult::error(e.to_string()));
                    (result, started.elapsed())
                }
            })
            .collect();
        let mut results = InOrder::new(jobs, parallel::MAX_PARALLEL, self.stop.subscribe());
        for tc in calls {
            match results.next().await.flatten() {
                Some((result, took)) => {
                    self.calls.ran(&tc.id, took);
                    let run = Ran {
                        tool_name: tc.name.clone(),
                        args_json: tc.arguments.clone(),
                        write: None,
                    };
                    self.record(tc, Authorized::default(), run, result, clm)
                        .await?;
                }
                None => self.stopped_result(tc).await?,
            }
        }
        log::write(
            &self.core,
            json!({
                "event": "parallelTools",
                "dialogId": self.dialog_id,
                "calls": calls.len(),
                "ms": started.elapsed().as_millis(),
            }),
        );
        Ok(())
    }

    /// A call answered without running it (a subagent's refusal, a scope
    /// grant): it counts for the budget and the loop checks like any other
    /// (P2.1), so refusals can't go on for ever.
    ///
    /// `refused`: a "no" the guard counts (`FinishedCall::rejected`), apart
    /// from the outcome: a call not run for the budget failed, unrefused.
    pub(super) async fn answer_unrun(
        &mut self,
        tc: &ToolCall,
        mut content: String,
        outcome: Outcome,
        refused: bool,
        meta: Option<Value>,
    ) -> Result<(), String> {
        self.note_finished(tc, &mut content, outcome.failed(), refused);
        self.add_tool_result(&tc.id, content, outcome, meta, Vec::new())
            .await
    }

    /// A call that didn't run a tool (or ran one here, without a write) is
    /// still a finished call for the loop guard, whose note joins its
    /// result.
    fn note_finished(&mut self, tc: &ToolCall, content: &mut String, failed: bool, rejected: bool) {
        let verdict = self.guard.record(FinishedCall {
            name: &tc.name,
            args_json: &tc.arguments,
            result: content,
            failed,
            write: None,
            rejected,
        });
        self.log_guard(&tc.name, &verdict);
        if let Some(note) = verdict.note() {
            content.push_str(note);
        }
    }

    /// One call: authorized (asking if the settings say so), run, recorded.
    async fn execute_one(
        &mut self,
        tc: &ToolCall,
        resume_note: Option<&str>,
        clm: bool,
        context_dir: Option<&str>,
    ) -> Result<(), String> {
        if self.stopped() {
            return self.stopped_result(tc).await;
        }
        // A subagent may call its kind's tools only, whatever it was sent.
        if let Some(sub) = self.sub() {
            let spec = sub.kind.spec();
            if !sub.allows(&tc.name) {
                let content = format!(
                    "Error: {} isn't available to a {} subagent. Its tools: {}. If you can't do the task with them, say so under \"## Blocked\" in your report.",
                    tc.name,
                    spec.label.to_lowercase(),
                    spec.tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ")
                );
                return self
                    .answer_unrun(tc, content, Outcome::Failed, true, None)
                    .await;
            }
        }
        let tool = BuiltinTool::parse(&tc.name);
        let args = permissions::parse_args(&tc.arguments);
        let path_arg = args.get("path").and_then(|p| p.as_str());

        // A writing subagent writes nothing outside its scope (D7).
        // The test writer writes test files only (E02).
        if let (true, Some(path), Some(root)) = (
            tc.name == "write" && self.sub().is_some_and(|s| s.tests_only),
            path_arg,
            self.root.as_deref(),
        ) {
            // By its name, and by where it really lands: a link under the
            // tests (`tests/fixtures -> ../src`) isn't a way to the code.
            let key = super::repair::rel_key(root, path);
            let real = super::repair::real_key(root, path);
            if !super::project::is_test_path(&key)
                || !real.as_deref().is_some_and(super::project::is_test_path)
            {
                let content = super::test_writer::not_a_test_file(&key);
                return self
                    .answer_unrun(tc, content, Outcome::Failed, true, None)
                    .await;
            }
        }
        // A writing subagent changes files only with write, where its scope
        // is checked; the test writer doesn't read the code with bash either.
        if tc.name == "bash" {
            if let Some(sub) = self.sub().filter(|s| s.scope.is_some() || s.tests_only) {
                let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
                if let Some(content) = super::worker::writer_command_problem(
                    command,
                    sub.tests_only,
                    self.root.as_deref().map(std::path::Path::new),
                ) {
                    return self
                        .answer_unrun(tc, content, Outcome::Failed, true, None)
                        .await;
                }
            }
        }
        // Light commands only: builds and test runs are the lead's (E02, F05).
        // Only what's known to be light runs: a command that can't be told
        // (a wrapper, a script, a subshell) is refused too.
        if tc.name == "bash" && self.sub().is_some_and(|s| s.light_bash) {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let class = crate::workload::classify::classify(
                command,
                self.root.as_deref().map(std::path::Path::new),
            );
            if class != crate::workload::classify::Class::Light {
                let what = match class {
                    crate::workload::classify::Class::Unknown => {
                        "a command that can't be told to be light"
                    }
                    _ => "a heavy command (a build, a test run or a server)",
                };
                let content = format!(
                    "Error: `{command}` is {what}. The lead runs those after merging your work; use diagnostics, or a light check on one file, run directly."
                );
                return self
                    .answer_unrun(tc, content, Outcome::Failed, true, None)
                    .await;
            }
        }
        if tc.name == "write" {
            if let (Some(scope), Some(path), Some(root)) = (
                self.sub().and_then(|s| s.scope.as_ref()),
                path_arg,
                self.root.as_deref(),
            ) {
                let key = super::repair::rel_key(root, path);
                let exists = std::path::Path::new(&tools::to_absolute_path(&key, root)).exists();
                // By its name, and by where it really lands: a link in the
                // step's folder (or a granted path that is one) isn't a way
                // into another step's files.
                let real = super::repair::real_key(root, path);
                if !scope.allows(&key, exists)
                    || !real.as_deref().is_some_and(|r| scope.allows(r, exists))
                {
                    // Named by where it leads when only that is outside.
                    let named = match &real {
                        Some(r) if scope.allows(&key, exists) => r.as_str(),
                        _ => key.as_str(),
                    };
                    let content = super::worker::outside_scope(named, scope);
                    return self
                        .answer_unrun(tc, content, Outcome::Failed, true, None)
                        .await;
                }
            }
        }

        // The agent's own context: no approval, no editor review, not a
        // project change. It applies to the next request.
        if clm
            && (tc.name == "restore_context"
                || matches!(tc.name.as_str(), "read" | "write" | "list_dir")
                    && is_context_path(path_arg, context_dir))
        {
            let mut result = self.context_tool(&tc.name, &args).await;
            // A change to the context file itself (or a restore): the
            // compaction the CLM pressure waits for (run.rs,
            // update_clm_pressure). Notes and the task card don't count.
            if !result.outcome.failed()
                && (tc.name == "restore_context"
                    || tc.name == "write"
                        && crate::clm::is_live_path(
                            &self.core.vfs,
                            &self.dialog_id,
                            path_arg.unwrap_or(""),
                        ))
            {
                self.context_edited = true;
            }
            self.note_finished(tc, &mut result.content, result.outcome.failed(), false);
            let meta = match result.outcome.failed() {
                false => tools::result_meta(&tc.name, &result.content).map(Value::Object),
                true => None,
            };
            return self
                .add_tool_result(&tc.id, result.content, result.outcome, meta, Vec::new())
                .await;
        }
        // Phase E: a restricted turn offers only context tools (handled
        // above), `restore_context`, and the checklist / plan questions.
        // Anything else is refused without running, so the model compacts
        // before doing more work. The restriction lifts with an edit.
        if clm
            && self.clm_restricted
            && !matches!(tc.name.as_str(), "todo" | "ask_user" | "submit_plan")
        {
            let content = format!(
                "Error: this turn offers only context tools (read/write/list_dir on @context/…), \
                 restore_context and todo: the context is over budget and a compaction request is still \
                 open. Compact your context file now in one edit, copying facts forward, then repeat \
                 this {} call.",
                tc.name
            );
            return self
                .add_tool_result(&tc.id, content, Outcome::Failed, None, Vec::new())
                .await;
        }
        // Q05 (mcp-plan.md): a tool's description, or a call that can't be
        // made (a name that isn't listed, a tool the mode doesn't call), is
        // answered at once: no approval is asked for it.
        if tool == Some(BuiltinTool::Mcp) {
            let read_only = self.mcp_read_only();
            if let super::mcp_tool::Prepared::Answer { content, error } =
                super::mcp_tool::prepare(self.mcp.as_deref(), &args, read_only)
            {
                return self
                    .answer_unrun(tc, content, Outcome::of(error), error, None)
                    .await;
            }
        }
        // A tool the mode doesn't offer (its row's modes): refused, though
        // a model may call it from an earlier turn's history.
        if let Some(t) = tool.filter(|t| !t.spec().modes.offered_in(self.plan_mode)) {
            let content = match self.plan_mode {
                true => plan::blocked_in_plan_mode(t.name(), self.autopilot),
                false => plan::only_in_plan_mode(t.name()),
            };
            return self
                .add_tool_result(&tc.id, content, Outcome::Failed, None, Vec::new())
                .await;
        }
        // The run's own tools (their row's dispatch): answered here, with
        // the run's state, and never put to an approval. `mcp` is the run's
        // too, but goes by its rule: authorized and called below.
        match tool {
            // The plan (Plan mode): checked, then saved; problems go back to the model.
            Some(BuiltinTool::SubmitPlan) => {
                return self.submit_plan(tc, &args).await;
            }
            // S06: the answers to the change review's findings.
            Some(BuiltinTool::AnswerReview) if self.sub().is_none() => {
                let result = self.answer_review(&args);
                return self
                    .add_tool_result(&tc.id, result.content, result.outcome, None, Vec::new())
                    .await;
            }
            // The checklist: shown in the panel, never asks, isn't work the
            // loop checks or the pause limit count.
            Some(BuiltinTool::Todo) => {
                let items = plan::parse_todo(&args);
                let tools::ToolResult {
                    mut content,
                    outcome,
                    ..
                } = plan::describe_todo(&items);
                // Phase D: a step just finished in CLM mode asks for a
                // compaction of that step's span (and a TASK.md update).
                // The previous checklist is what was current before this
                // call: the history up to the message holding it.
                if clm && !outcome.failed() {
                    let previous: Option<Vec<plan::TodoItem>> =
                        self.recent().await.ok().and_then(|recent| {
                            let cut = recent
                                .iter()
                                .position(|m| {
                                    m.tool_calls
                                        .as_ref()
                                        .is_some_and(|calls| calls.iter().any(|c| c.id == tc.id))
                                })
                                .unwrap_or(recent.len());
                            plan::current_checklist(&recent[..cut]).map(|c| c.items)
                        });
                    if let Some(before) = previous {
                        let settled_before = |title: &str| {
                            before
                                .iter()
                                .find(|i| i.title == title)
                                .is_some_and(plan::is_settled)
                        };
                        let newly: Vec<&plan::TodoItem> = items
                            .iter()
                            .filter(|i| plan::is_settled(i) && !settled_before(&i.title))
                            .collect();
                        if !newly.is_empty() {
                            let dialog_id = self.dialog_id.clone();
                            let (tokens, budget) = self
                                .core
                                .db
                                .read(move |conn| {
                                    Ok(crate::clm::store::load_state(conn, &dialog_id)
                                        .map(|s| (s.tokens, s.budget))
                                        .unwrap_or_default())
                                })
                                .await
                                .unwrap_or_default();
                            let pct = crate::clm::prompt::percent(tokens, budget);
                            let next = items
                                .iter()
                                .find(|i| !plan::is_settled(i))
                                .map(|i| i.title.as_str());
                            let done_names = newly
                                .iter()
                                .map(|i| format!("\"{}\"", i.title))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let done_word = if newly.len() == 1 { "Step" } else { "Steps" };
                            if pct >= super::run::CLM_STEP_COMPACT_MIN {
                                // Asked for: the pressure escalates if it's ignored.
                                self.pending_step_compaction =
                                    Some(super::run::PendingStepCompaction {
                                        tool_calls: self.guard.tool_calls,
                                    });
                                self.step_asked_turn = Some(self.clm_turns);
                                match next {
                                    Some(n) => content.push_str(&format!(
                                        "\n\n{done_word} {done_names} done. Before \"{n}\": compact that step's span in one edit, copy facts forward, and update @context/TASK.md (done, NEXT)."
                                    )),
                                    None => content.push_str(&format!(
                                        "\n\n{done_word} {done_names} done. Compact that step's span in one edit, copy facts forward, and update @context/TASK.md (done, NEXT)."
                                    )),
                                }
                            } else {
                                // Only a card update: nothing to escalate.
                                content.push_str("\n\nUpdate @context/TASK.md: done/NEXT.");
                            }
                        }
                    }
                }
                let accepted = !outcome.failed();
                self.add_tool_result(&tc.id, content, outcome, None, Vec::new())
                    .await?;
                // The panel's live source while the run goes: every list the
                // IDE took. One it refused (two steps in progress, an empty
                // list) isn't the checklist: the model was told it's wrong.
                if accepted {
                    self.core.emit(AgentEvent::Checklist {
                        dialog_id: self.dialog_id.clone(),
                        items,
                    });
                }
                return Ok(());
            }
            // Questions to the user: answered in the chat, no approval
            // involved (a resumed one after a restart is simply asked again).
            Some(BuiltinTool::AskUser) => {
                return self.ask_user(tc, &args).await;
            }
            // D07: more scope for a writing subagent, on request.
            Some(BuiltinTool::ExtendScope) => {
                let (content, granted) = self.extend_scope(&args);
                let refused = granted.is_empty();
                // Kept with the result: a continued worker keeps its grants.
                let meta = (!refused).then(|| json!({ "granted": granted }));
                return self
                    .answer_unrun(tc, content, Outcome::of(refused), refused, meta)
                    .await;
            }
            // In CLM mode, answered with the context folder's tools above.
            Some(BuiltinTool::RestoreContext) => {
                let content =
                    "Error: restore_context works only in CLM mode, where your context is a file you manage.";
                return self
                    .add_tool_result(&tc.id, content.into(), Outcome::Failed, None, Vec::new())
                    .await;
            }
            _ => {}
        }

        // Checklist gate (Agent mode, the checklist setting not Off): no
        // checklist yet, and the task has clearly grown past the "3 or more
        // steps" rule the model decides by (plan::checklist_gate_reason).
        // Refused once per run: if the model judged the task small and
        // repeats the call, it runs. Writes to the CLM context folder are
        // exempt (context management, not project work), as are fixes after
        // the checks failed (continuing started work, not new scope).
        if !self.plan_mode
            && self.sub().is_none()
            && !self.checklist_gate_refused
            && resume_note.is_none()
            && !self.last_verify_failed
            && self.config().permissions.checklist != permissions::ChecklistMode::Off
            && !(clm && is_context_path(path_arg, context_dir))
        {
            if let Some(reason) = plan::checklist_gate_reason(
                &tc.name,
                path_arg,
                &self.changed_files,
                self.guard.tool_calls,
            ) {
                if let Ok(recent) = self.recent().await {
                    if plan::current_checklist(&recent).is_none() {
                        self.checklist_gate_refused = true;
                        let content = plan::checklist_gate_error(&reason, &tc.name);
                        return self
                            .add_tool_result(&tc.id, content, Outcome::Failed, None, Vec::new())
                            .await;
                    }
                }
            }
        }

        let auth = self.authorize(tc, resume_note).await;
        // Stop may have been pressed while approval was pending (the user's,
        // which Stop answers "no", or the AI reviewer's, whose "yes" may
        // come after it): nothing runs once the run is stopped.
        if self.stopped() {
            return self.stopped_result(tc).await;
        }
        let auth = self.reconcile_review(tc, auth).await;
        if self.stopped() {
            return self.stopped_result(tc).await;
        }
        self.denied_by_user = auth.by_user && self.sub().is_none();
        // G02: its time is its own from here, not its approval's wait.
        self.calls.start(&tc.id);
        let mut args_json = auth
            .args_override
            .clone()
            .unwrap_or_else(|| tc.arguments.clone());
        let tool_name = tc.name.clone();
        // P2.2: a subagent's command can't run past its time budget.
        if tool_name == "bash" {
            if let Some(clamped) = clamp_timeout(&args_json, self.wall_left()) {
                args_json = clamped;
            }
        }
        let writes = auth.refusal.is_none() && permissions::modifies_files(&tool_name);
        let write_path = if writes {
            permissions::str_arg(&args_json, "path")
        } else {
            None
        };
        let abs_write_path = match (&write_path, &self.root) {
            (Some(p), Some(root)) => Some(tools::to_absolute_path(p, root)),
            _ => None,
        };
        // What the file was, for the fix round's checkpoint and the run's
        // test-integrity baseline (repair.rs).
        if let Some(path) = &write_path {
            self.note_before_write(path);
        }
        // The file before a write, so the guard can tell an edit undoing earlier ones.
        let before = abs_write_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());

        let result = match &auth.refusal {
            Some(refusal) => tools::ToolResult::error(refusal.clone()),
            None if tool == Some(BuiltinTool::Mcp) => match self.mcp.clone() {
                Some(catalog) => {
                    let args = permissions::parse_args(&args_json);
                    super::mcp_tool::call(
                        &self.core,
                        &catalog,
                        &args,
                        self.mcp_read_only(),
                        &self.stop,
                        self.profile.vision,
                    )
                    .await
                }
                None => tools::ToolResult::error("Error: no MCP server is set up for this chat."),
            },
            None => {
                let ctx = self.call_context(&tc.id);
                tools::execute(
                    &self.core,
                    self.root.as_deref(),
                    &tool_name,
                    &args_json,
                    &self.stop,
                    &ctx,
                )
                .await
            }
        };
        // An isolated chat's changes are saved as they happen (overlay/fs.rs).
        // Then compared with the other open chats on the project, so a clash
        // on the same lines is told with this result.
        if crate::overlay::chats::get(&self.overlay_owner()).is_some() {
            let id = self.overlay_owner();
            let _ = crate::rt::spawn_blocking(move || {
                if let Some(o) = crate::overlay::chats::get(&id) {
                    o.fs.flush();
                    crate::overlay::chats::check_peer_conflicts(&id);
                }
            })
            .await;
        }
        // The project changed under an isolated chat: its model hears it with
        // this result.
        let mut result = result;
        if let Some(note) = crate::overlay::chats::take_note(&self.dialog_id) {
            result.content.push_str(&note);
        }
        let write = match (writes, write_path) {
            (true, Some(path)) => Some(WriteTarget {
                path,
                abs_path: abs_write_path,
                before,
            }),
            _ => None,
        };
        let run = Ran {
            tool_name,
            args_json,
            write,
        };
        self.record(tc, auth, run, result, clm).await
    }

    /// A loop check that fired, for tuning them: which pattern, on which
    /// call, and whether the run was warned or paused.
    fn log_guard(&self, tool: &str, verdict: &GuardVerdict) {
        let kind = match verdict {
            GuardVerdict::Ok => return,
            GuardVerdict::Nudge(_) => "nudge",
            GuardVerdict::Stop(_) => "stop",
        };
        let (pattern, reason) = self.guard.finding.clone().unwrap_or(("", String::new()));
        log::write(
            &self.core,
            json!({
                "event": "guard",
                "dialogId": self.dialog_id,
                "model": self.profile.model,
                "verdict": kind,
                "pattern": pattern,
                "reason": reason,
                "tool": tool,
                "toolCalls": self.guard.tool_calls,
            }),
        );
    }

    /// Everything after a call ran: notes for the model (an approval's,
    /// CLM's recall hint, the guard's), the files it changed, the editor,
    /// and its result stored.
    async fn record(
        &mut self,
        tc: &ToolCall,
        auth: Authorized,
        run: Ran,
        mut result: tools::ToolResult,
        clm: bool,
    ) -> Result<(), String> {
        let Ran {
            tool_name,
            args_json,
            write,
        } = run;
        let writes = write.is_some();
        let (write_path, abs_write_path, before) = match write {
            Some(w) => (Some(w.path), w.abs_path, w.before),
            None => (None, None, None),
        };
        let failed = result.outcome.failed();
        if let Some(note) = &auth.result_note {
            if !failed {
                result.content.push_str(note);
            }
        }
        // Redoing work it compacted away: offer the revision that still has it.
        // A hint that fires counts redone work (Phase E compliance).
        if clm && auth.refusal.is_none() && !failed {
            let (d, id, name, a) = (
                self.dialog_id.clone(),
                tc.id.clone(),
                tool_name.clone(),
                args_json.clone(),
            );
            let hint = self
                .core
                .db
                .read(move |c| crate::clm::recall_hint(c, &d, &id, &name, &a))
                .await;
            if let Ok(Some(hint)) = hint {
                result.content.push_str(&hint);
                self.clm_recall_hits += 1;
            }
        }

        let after = result.written.as_ref().map(|(_, c)| c.clone()).or_else(|| {
            (tool_name == "write")
                .then(|| permissions::str_arg(&args_json, "content"))
                .flatten()
        });

        let verdict = self.guard.record(FinishedCall {
            name: &tc.name,
            args_json: &tc.arguments,
            result: &result.content,
            failed,
            write: match (&write_path, &after) {
                (Some(path), Some(after)) if writes && !failed => Some(Write {
                    path,
                    before: before.as_deref(),
                    after,
                }),
                _ => None,
            },
            rejected: auth.rejected,
        });
        self.log_guard(&tc.name, &verdict);
        // The model reads the note with the result; a stop pauses the run
        // once this turn's calls are done.
        if !matches!(verdict, GuardVerdict::Ok) {
            result.content.push_str(verdict.note().unwrap_or(""));
        }
        // A command's result that shows only part of its output, or an MCP
        // tool's (mcp-plan.md, Q05): the whole of it is saved for the agent
        // to read, not run again, named after the call, so no name is ever
        // reused for other output. Said after the guard saw the result, so
        // a repeated command, whose saved copy has a new name, still reads
        // as a repeat. In its own folder, earlier runs' output goes first
        // (P4.7); a subagent's goes to its lead's, which the lead's run
        // clears.
        if let Some(whole) = result.whole_output.take() {
            let owner = self.checks_owner();
            if owner == self.dialog_id {
                self.clean_earlier_output();
            }
            let (stem, again) = match tc.name.as_str() {
                super::mcp_tool::NAME => ("mcp", "calling the tool again"),
                _ => ("bash", "running the command again"),
            };
            match super::check_output::save(&owner, stem, &tc.id, &whole) {
                Some(path) => result.content.push_str(&format!(
                    "\n(All of its output is saved as `{path}`: read it there by line range instead of {again}.)"
                )),
                None => eprintln!("rovik: couldn't save a command's whole output for chat {owner}"),
            }
        }
        if writes && !failed {
            if let Some(path) = &write_path {
                self.note_write(path, abs_write_path.as_deref());
            }
        }
        // A shell command may have changed any files (git checkout, codegen,
        // formatters): mtime/size catches it on the map's next rebuild —
        // unlike the write tool, no same-tick cache race is possible across
        // processes.
        if tool_name == "bash" && auth.refusal.is_none() {
            let command = permissions::str_arg(&args_json, "command").unwrap_or_default();
            self.note_command(&command);
        }
        if tool_name == "bash" && !failed {
            // A command on the context file's real path (sed, python3, …)
            // is a context edit for the CLM pressure (run.rs).
            if clm {
                let vfs = &self.core.vfs;
                if let Some((file, _)) = crate::clm::real_paths(vfs, &self.dialog_id) {
                    let command = permissions::str_arg(&args_json, "command").unwrap_or_default();
                    if (command.contains(&file) || command.contains("LIVE_CONTEXT.md"))
                        && permissions::command_modifies_files(&command)
                    {
                        self.context_edited = true;
                    }
                }
            }
        }

        // Open tabs of the file show what was written; an edit the user
        // didn't review is marked in the editor until they save.
        if writes && !failed {
            if let (Some(abs), Some(after)) = (&abs_write_path, &after) {
                let reviewed = auth
                    .approval
                    .as_ref()
                    .and_then(|a| a.get("by"))
                    .and_then(|b| b.as_str())
                    == Some("user");
                self.core.emit(AgentEvent::FileWritten {
                    dialog_id: Some(self.dialog_id.clone()),
                    abs_path: abs.clone(),
                    before,
                    content: after.clone(),
                    reviewed,
                    deleted: false,
                });
            }
        }

        let mut meta = match failed {
            false => tools::result_meta(&tc.name, &result.content),
            true => None,
        };
        if let Some(extra) = result.meta {
            meta.get_or_insert_with(Map::new).extend(extra);
        }
        if let Some(approval) = auth.approval {
            meta.get_or_insert_with(Map::new)
                .insert("approval".into(), approval);
        }
        self.add_tool_result(
            &tc.id,
            result.content,
            result.outcome,
            meta.map(Value::Object),
            result.attachments,
        )
        .await
    }

    /// A file the agent changed (`path` as its call named it; `abs_path`,
    /// where it is): one of the run's changed files, which the review and
    /// the checks run on next.
    pub(super) fn note_write(&mut self, path: &str, abs_path: Option<&str>) {
        if !self.changed_files.iter().any(|p| p == path) {
            self.changed_files.push(path.to_string());
        }
        self.writes_since_verify = true;
        self.writes_since_review = true;
        // The parse caches key on mtime+size: a fast same-size rewrite
        // could otherwise serve the old parse all run. Invalidate now,
        // while the absolute path is at hand; failures never break the run.
        if let (Some(root), Some(abs)) = (self.root.as_deref(), abs_path) {
            let abs_path = std::path::Path::new(abs);
            crate::outline::invalidate_path(abs_path);
            crate::repo_map::invalidate_path(std::path::Path::new(root), abs_path);
        }
    }

    /// A command the agent ran. One that may change files (`sed -i`) is a
    /// change the checks run on again before "verified" (round 2, P3.6).
    /// P3.5: outside an isolated chat, what it changed can't be rolled
    /// back; the round's rollback says it ran.
    pub(super) fn note_command(&mut self, command: &str) {
        if !permissions::command_modifies_files(command) {
            return;
        }
        self.writes_since_verify = true;
        if self.checkpoint.is_some() && !self.isolated() {
            self.round_commands
                .push(crate::chat_db::store::char_prefix(command, 120).to_string());
        }
    }

    async fn stopped_result(&self, tc: &ToolCall) -> Result<(), String> {
        self.add_tool_result(
            &tc.id,
            "Error: stopped by the user before this ran.".into(),
            Outcome::Failed,
            None,
            Vec::new(),
        )
        .await
    }

    /// read / write / list_dir on the agent's context
    /// folder, and restore_context.
    async fn context_tool(&self, name: &str, args: &Map<String, Value>) -> tools::ToolResult {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let text = |k: &str| args.get(k).and_then(|v| v.as_str());
        let db = self.core.db.clone();
        let vfs = &self.core.vfs;
        let result = match name {
            "read" => {
                crate::clm::tool_read(
                    &db,
                    vfs,
                    &self.dialog_id,
                    path,
                    tools::line_arg(args.get("start_line")),
                    tools::line_arg(args.get("end_line")),
                )
                .await
            }
            "write" if tools::is_partial_write(name, args) => {
                match (text("old_text"), text("content")) {
                    (None, _) => {
                        return tools::ToolResult::error("Error: \"old_text\" must be a string.")
                    }
                    (_, None) => {
                        return tools::ToolResult::error("Error: \"content\" is required.")
                    }
                    (Some(old), Some(new)) => crate::clm::tool_edit(
                        vfs,
                        &self.dialog_id,
                        path,
                        old,
                        new,
                        tools::line_arg(args.get("start_line")),
                        tools::line_arg(args.get("end_line")),
                    ),
                }
            }
            "write" => match text("content") {
                None => return tools::ToolResult::error("Error: \"content\" is required."),
                Some(content) => crate::clm::tool_write(vfs, &self.dialog_id, path, content),
            },
            "list_dir" => crate::clm::tool_list(&db, vfs, &self.dialog_id, path).await,
            "restore_context" => {
                crate::clm::tool_restore(&db, vfs, &self.dialog_id, args.get("revision")).await
            }
            _ => {
                return tools::ToolResult::error(format!(
                    "Error: {name} can't be used on your context folder."
                ))
            }
        };
        match result {
            Ok(text) => tools::ToolResult::ok(text),
            Err(e) => tools::ToolResult::error(format!("Error: {e}")),
        }
    }

    /*
     * -----------------------------------------------------------------------
     * Permission and approval
     * -----------------------------------------------------------------------
     */

    /// Permission first (refused calls never reach approval), then approval
    /// when the settings require it: by the user, or by the AI reviewer.
    async fn authorize(&self, tc: &ToolCall, resume_note: Option<&str>) -> Authorized {
        let config = self.config();
        let p = &config.permissions;
        let daemon_folder = self.names_daemon_folder(tc);
        // Nobody is there in Autopilot (D6): what would wait for the user
        // is the AI reviewer's to decide, or refused (U02).
        let unattended = self.in_autopilot();
        if daemon_folder && unattended {
            return rejected(super::autopilot::DAEMON_FOLDER_REFUSED);
        }
        let approval = match permissions::check_tool_call(&tc.name, &tc.arguments, p) {
            ToolVerdict::Refused(reason) => {
                return rejected(format!(
                    "Error: blocked by the user's permission settings. {reason}"
                ))
            }
            ToolVerdict::Permitted { approval } => approval,
        };
        // The files the user keeps from the agent: refused, never asked about.
        let args = permissions::parse_args(&tc.arguments);
        if let Some(problem) = self.restricted_problem(&tc.name, args).await {
            return rejected(problem);
        }
        // An interrupted call is always confirmed by the user, and so is a
        // command that names a server's own folder. In Autopilot a resumed
        // call goes by its rule, as a new one does.
        let approver = match resume_note {
            Some(_) if !unattended => Approver::User,
            _ if daemon_folder => Approver::User,
            _ => match approval {
                Some(_) if unattended => Approver::Ai,
                Some(a) => a,
                None => return Authorized::default(),
            },
        };

        let mut summary = permissions::describe_tool_call(&tc.name, &tc.arguments);
        // A call to a server of the project's own files says so (Q12,
        // mcp-plan-2.md): its tools are the project's, not the user's.
        if tc.name == super::mcp_tool::NAME {
            let tool = permissions::str_arg(&tc.arguments, "tool");
            let whose = (self.mcp.as_deref())
                .zip(tool)
                .and_then(|(c, t)| c.whose(&t));
            summary.push_str(whose.as_deref().unwrap_or_default());
        }
        let command = (tc.name == "bash")
            .then(|| permissions::str_arg(&tc.arguments, "command"))
            .flatten();
        // Edits are diffed once: the AI reviewer reads the diff, the user
        // reviews it in the editor.
        let is_file_edit = permissions::modifies_files(&tc.name);
        let prepared = if is_file_edit {
            self.prepare_edit_review(&tc.name, &tc.arguments)
        } else {
            None
        };
        let mut is_new_file = false;
        let (proposal, hunks) = match prepared {
            // Writing the same content changes nothing: no need to ask.
            Some(Prepared::Unchanged) => return Authorized::default(),
            // An edit that can't apply (text not found, outside its lines):
            // the model gets the reason; there's nothing for anyone to approve.
            Some(Prepared::Invalid(error)) => return refused(error),
            Some(Prepared::Diff { proposal, hunks }) => (Some(proposal), hunks),
            Some(Prepared::New) => {
                is_new_file = true;
                (None, Vec::new())
            }
            None => (None, Vec::new()),
        };
        let mut manual_reason: Option<String> = resume_note
            .map(str::to_string)
            .or_else(|| daemon_folder.then(|| super::server::DAEMON_FOLDER_NOTE.to_string()));
        let reviewed = proposal.as_ref().map(|p| Reviewed {
            abs_path: p.abs_path.clone(),
            rel_path: p.rel_path.clone(),
            current: p.current.clone(),
        });

        // In Autopilot: why what the reviewer didn't approve isn't run,
        // and whether the reviewer said so (it was unsure).
        let mut unattended_why: Option<String> = None;
        let mut reviewer_unsure = false;
        if approver == Approver::Ai {
            let models = &config.approval_profiles;
            let edit_path = if is_file_edit {
                permissions::str_arg(&tc.arguments, "path")
            } else {
                None
            };

            // Shapes no reviewer model may approve on its own.
            if command
                .as_deref()
                .is_some_and(permissions::is_risky_command)
            {
                manual_reason = Some(
                    "Commands that push, delete, deploy, escalate privileges, run inline scripts or touch secrets always need your approval."
                        .into(),
                );
                unattended_why = Some(
                    "Commands that push, delete, deploy, escalate privileges, run inline scripts or touch secrets need a person's approval, and nobody is there to give it in Autopilot."
                        .into(),
                );
            } else if edit_path.as_deref().is_some_and(|path| {
                // Its diff would show the reviewer what's in it.
                let root = self.root.as_deref().unwrap_or_default();
                self.restricted
                    .blocked(root, path, crate::restricted_files::Op::Read)
                    .is_some()
            }) {
                manual_reason = Some(
                    "Edits to restricted files, which the agent may not read, always need your approval."
                        .into(),
                );
                unattended_why = Some(
                    "Edits to restricted files need a person's approval, and nobody is there to give it in Autopilot."
                        .into(),
                );
            } else if models.is_empty() {
                manual_reason =
                    Some("No AI reviewer model is set, so this needs your approval.".into());
                unattended_why = Some(
                    "It needs an approval, and no AI reviewer model is set to give it in Autopilot."
                        .into(),
                );
            } else {
                self.tool_note(&tc.id, Some(format!("Reviewing with {}…", models[0].label)));
                let input = ReviewInput {
                    group: if tc.name == "bash" {
                        Group::Commands
                    } else if is_file_edit {
                        Group::Edits
                    } else {
                        Group::Tools
                    },
                    user_request: self.latest_user_request().await,
                    project: self.root.clone().unwrap_or_default(),
                    tool_name: tc.name.clone(),
                    summary: summary.clone(),
                    args_json: tc.arguments.clone(),
                    edit_diff: (!hunks.is_empty()).then(|| diff::compact_diff(&hunks)),
                    edit_path,
                    is_new_file,
                    chat_id: Some(self.dialog_id.clone()),
                };
                // The reviewer model follows the setting; a Jev fallback can
                // only be asked with typed input.
                let typed = p.approval_typed_input;
                let review = review::review(&self.core, models, &input, |i, profile| {
                    if i == 0 {
                        typed
                    } else {
                        review::is_jev_model(&profile.model)
                    }
                })
                .await;
                self.tool_note(&tc.id, None);

                match review {
                    ReviewResult::Decided {
                        approved,
                        reason,
                        model,
                        checks,
                    } => {
                        let approval = approval_json(
                            Approver::Ai,
                            approved,
                            Some(model.clone()),
                            Some(reason.clone()),
                            &checks,
                        );
                        // Nobody to ask in Autopilot: another approach.
                        let or_ask = if unattended { "" } else { " or ask the user" };
                        return if approved {
                            Authorized {
                                approval: Some(approval),
                                reviewed,
                                ..Default::default()
                            }
                        } else {
                            Authorized {
                                refusal: Some(format!(
                                    "Error: rejected by the user's AI reviewer ({model}): {reason} Do not retry the same action; adjust the approach{or_ask}."
                                )),
                                approval: Some(approval),
                                rejected: true,
                                ..Default::default()
                            }
                        };
                    }
                    ReviewResult::Unsure { reason } => {
                        manual_reason = Some(format!("{reason} This needs your approval."));
                        unattended_why =
                            Some(format!("{reason} Nobody else can approve it in Autopilot."));
                        reviewer_unsure = true;
                    }
                    ReviewResult::Failed { errors } => {
                        eprintln!("agent: AI approval gave no decision: {errors:?}");
                        manual_reason = Some(
                            "The AI reviewer couldn’t answer, so this needs your approval.".into(),
                        );
                        unattended_why = Some(
                            "The AI reviewer couldn't answer, and nobody else can approve it in Autopilot."
                                .into(),
                        );
                    }
                }
            }
        }

        // Autopilot: what the reviewer didn't approve is refused, never
        // left waiting (D6, U02).
        if unattended {
            let why = unattended_why.unwrap_or_else(|| {
                "It needs an approval, and nobody is there to give it in Autopilot.".into()
            });
            return Authorized {
                refusal: Some(format!(
                    "Error: not run. {why} Do not retry it; do without it or take another approach."
                )),
                // The reviewer's verdict shows where it gave one.
                approval: reviewer_unsure
                    .then(|| approval_json(Approver::Ai, false, None, Some(why), &[])),
                rejected: true,
                ..Default::default()
            };
        }

        // Manual: the request shows in the chat (and, for an edit of an
        // existing file, in the editor as changes to accept or reject one by one).
        let note = [
            manual_reason,
            is_new_file.then(|| "New file: expand to see its content.".to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        let rel_path = proposal.as_ref().map(|p| p.rel_path.clone());
        let request = ApprovalRequest {
            tool_call_id: tc.id.clone(),
            dialog_id: self.dialog_id.clone(),
            tool_name: tc.name.clone(),
            summary,
            command,
            note: (!note.is_empty()).then_some(note),
            edit: proposal,
            // A subagent's approvals show in its parent's chat (D8).
            parent_dialog_id: self.sub().map(|s| s.parent_dialog_id.clone()),
            parent_call_id: self.sub().map(|s| s.parent_call_id.clone()),
            remote: false,
        };
        let waiting = std::time::Instant::now();
        let (decision, unanswered) = self
            .core
            .agents
            .wait_for_decision(&self.core, request, &self.stop)
            .await;
        // Waiting for the user isn't the subagent's time (K07).
        if let Some(sub) = self.sub() {
            sub.add_waited(waiting.elapsed());
        }

        match decision {
            ApprovalDecision::Allow => Authorized {
                approval: Some(approval_json(Approver::User, true, None, None, &[])),
                reviewed,
                ..Default::default()
            },
            // Autopilot's deadline (U03): nobody answered.
            ApprovalDecision::Deny if unanswered => Authorized {
                refusal: Some(
                    "Error: not run: nobody answered its approval in time, and nobody is there to answer it in Autopilot. Do not retry it; do without it or take another approach."
                        .into(),
                ),
                approval: Some(approval_json(
                    Approver::User,
                    false,
                    None,
                    Some("Nobody answered in time.".into()),
                    &[],
                )),
                rejected: true,
                ..Default::default()
            },
            ApprovalDecision::Deny => Authorized {
                refusal: Some(match &rel_path {
                    Some(rel) => format!(
                        "Error: the user rejected every change to {rel}; the file was not modified. Do not retry the same edit; ask the user or take another approach."
                    ),
                    None => format!(
                        "Error: the user denied this \"{}\" call. Do not retry it; continue without it or ask the user how to proceed.",
                        tc.name
                    ),
                }),
                approval: Some(approval_json(Approver::User, false, None, None, &[])),
                rejected: true,
                by_user: true,
                ..Default::default()
            },
            // Some changes accepted: write the original with only those applied.
            ApprovalDecision::Partial { content, total, rejected } => {
                let mut args = permissions::parse_args(&tc.arguments);
                let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
                // A partial write's result is a whole new file: written as one.
                let args_override = if tools::is_partial_write(&tc.name, &args) {
                    json!({ "path": path, "content": content }).to_string()
                } else {
                    args.insert("content".into(), json!(content));
                    Value::Object(args).to_string()
                };
                // Partly accepted: it runs, so it isn't a rejection.
                Authorized {
                    refusal: None,
                    rejected: false,
                    approval: Some(approval_json(
                        Approver::User,
                        true,
                        None,
                        Some(format!("Accepted {} of {total} changes.", total.saturating_sub(rejected.len()))),
                        &[],
                    )),
                    args_override: Some(args_override),
                    result_note: Some(describe_rejected_hunks(
                        rel_path.as_deref().unwrap_or("the file"),
                        &rejected,
                        total,
                    )),
                    reviewed,
                    by_user: false,
                }
            }
        }
    }

    /// A connector's harness asks before one of its own tools runs (its
    /// runs follow Rovik's permissions): answered as a call of Rovik's tool
    /// of the same kind is, by `authorize`: refused by the settings, run,
    /// or put to the user or the AI reviewer, an edit as its changes.
    ///
    /// Refused first, as Rovik's own calls are: a shell command that can't
    /// run where the chat works (git that changes the repository, in a
    /// chat's copy of the project). A harness asks only in Agent mode: in
    /// Plan mode and Autopilot it runs Rovik's tools (`run::lead_profile`).
    pub(super) async fn answer_tool_ask(&self, ask: crate::connectors::ToolAsk) {
        let call = ToolCall {
            id: ask.call_id,
            name: ask.tool,
            arguments: ask.arguments,
            extra_content: None,
        };
        if call.name == "bash" {
            let args = permissions::parse_args(&call.arguments);
            let command = args.get("command").and_then(Value::as_str).unwrap_or("");
            let owner = self.overlay_owner();
            if let Some(reason) =
                tools::copy_command_problem(&owner, self.core.agents.on_server(), command)
            {
                let _ = ask
                    .reply
                    .send(crate::connectors::ToolAnswer::Deny { reason });
                return;
            }
        }
        let authorized = self.authorize(&call, None).await;
        let answer = match authorized.refusal {
            Some(reason) => crate::connectors::ToolAnswer::Deny { reason },
            None => crate::connectors::ToolAnswer::Allow {
                arguments: authorized.args_override,
            },
        };
        let _ = ask.reply.send(answer);
    }

    /// A connector's harness that runs its own tools calls the `mcp` tool
    /// Rovik serves it (Agent mode, mcp-plan.md Q09): answered as the run's
    /// own `mcp` call is. A description, or a call that can't be made, at
    /// once; a call under the MCP rule (`authorize`: refused, run, or put to
    /// the user or the AI reviewer), then run, its result the harness's.
    pub(super) async fn answer_mcp_call(&self, call: crate::connectors::mcp::McpCall) {
        use crate::connectors::mcp::McpReply;
        let args = call.arguments.as_object().cloned().unwrap_or_default();
        let read_only = self.mcp_read_only();
        let reply = match super::mcp_tool::prepare(self.mcp.as_deref(), &args, read_only) {
            super::mcp_tool::Prepared::Answer { content, error } => McpReply {
                text: content,
                is_error: error,
            },
            super::mcp_tool::Prepared::Call => {
                let tc = ToolCall {
                    id: call.tool_use_id.clone().unwrap_or_default(),
                    name: super::mcp_tool::NAME.into(),
                    arguments: Value::Object(args.clone()).to_string(),
                    extra_content: None,
                };
                match (self.authorize(&tc, None).await.refusal, self.mcp.clone()) {
                    (Some(reason), _) => McpReply::error(reason),
                    (None, None) => {
                        McpReply::error("Error: no MCP server is set up for this chat.")
                    }
                    (None, Some(catalog)) => {
                        // Text alone: the harness's model gets no attachments.
                        let result = super::mcp_tool::call(
                            &self.core, &catalog, &args, read_only, &self.stop, false,
                        )
                        .await;
                        McpReply {
                            is_error: result.outcome.failed(),
                            text: result.content,
                        }
                    }
                }
            }
        };
        let _ = call.reply.send(reply);
    }

    /// A connector's harness reports a command its own shell runs, before
    /// it runs (nothing is installed then in Plan mode) and after it
    /// failed: what it needs is made ready as for Rovik's own commands
    /// (environment.rs), noted on its call's line while something
    /// installs. The model is told what was done, and, after a failure,
    /// that the command can run again.
    /// Before it runs, whatever the harness's own permissions, one is
    /// refused that would read or change a restricted file, or that can't
    /// run where the chat works (git that changes the repository, in a
    /// chat's copy of the project).
    ///
    /// A call of its file tools that may change a file, and isn't refused,
    /// gives back that file as it is before the call runs: what it was, for
    /// the run's record of the harness's changes once its turn is over
    /// (harness_changes.rs).
    pub(super) async fn answer_command_check(
        &self,
        check: crate::connectors::CommandCheck,
    ) -> Option<super::harness_changes::Before> {
        use crate::connectors::{CommandAnswer, HarnessCall};
        let Some(root) = self.root.clone() else {
            let _ = check.reply.send(CommandAnswer::Note(None));
            return None;
        };
        let command = match check.call {
            // Its own file tools: only what they may do with files.
            HarnessCall::Tool { name, args } => {
                let path = permissions::modifies_files(&name)
                    .then(|| permissions::trimmed_str(args.get("path")).map(str::to_string))
                    .flatten();
                let refused = self
                    .harness_restricted_problem(&name, args, check.cwd)
                    .await;
                // Read before the answer lets the call run.
                let before = match &refused {
                    None => path.and_then(|path| self.before_harness_write(&path)),
                    Some(_) => None,
                };
                let _ = check.reply.send(match refused {
                    Some(reason) => CommandAnswer::Deny(reason),
                    None => CommandAnswer::Note(None),
                });
                return before;
            }
            HarnessCall::Command(command) => command,
        };
        if check.failed.is_none() {
            let args = json!({ "command": command });
            let args = args.as_object().cloned().unwrap_or_default();
            if let Some(reason) = self
                .harness_restricted_problem("bash", args, check.cwd)
                .await
            {
                let _ = check.reply.send(CommandAnswer::Deny(reason));
                return None;
            }
            let owner = self.overlay_owner();
            if let Some(reason) =
                tools::copy_command_problem(&owner, self.core.agents.on_server(), &command)
            {
                let _ = check.reply.send(CommandAnswer::Deny(reason));
                return None;
            }
        }
        let (core, dialog_id, call) = (
            self.core.clone(),
            self.dialog_id.clone(),
            check.call_id.clone(),
        );
        let cx = super::environment::Context {
            core: self.core.clone(),
            chat: self.dialog_id.clone(),
            root,
            stop: self.stop.clone(),
            note: std::sync::Arc::new(move |note| {
                core.emit(AgentEvent::ToolNote {
                    dialog_id: dialog_id.clone(),
                    tool_call_id: call.clone(),
                    note,
                })
            }),
            plan_mode: self.plan_mode,
        };
        let said = match &check.failed {
            None => super::environment::before_harness_command(&cx, &command)
                .await
                .done_sentence()
                .map(|done| format!("Rovik IDE, before this command ran: {done}")),
            Some((code, output)) => super::environment::after(&cx, &command, *code, output)
                .await
                .note()
                .map(|note| format!("Rovik IDE: {note}")),
        };
        let _ = check.reply.send(CommandAnswer::Note(said));
        None
    }

    /// An approved edit whose file changed while the approval waited (another
    /// chat, the user, a command). Writing the approved file whole would
    /// erase that change, so the approved changes are merged into the file
    /// as it is now. Changes that overlap are reviewed again, once, against
    /// the current file; if it changes yet again, the model is told.
    ///
    /// Either way the run counts as having seen the file it's written over
    /// (file_guard.rs): the user approved the change against it.
    async fn reconcile_review(&self, tc: &ToolCall, auth: Authorized) -> Authorized {
        if let Ok(auth) = self.merge_into_current(tc, auth) {
            return auth;
        }
        let again = self
            .authorize(
                tc,
                Some("The file changed while this edit waited for approval, and the changes overlap: review it again against the file as it is now."),
            )
            .await;
        match self.merge_into_current(tc, again) {
            Ok(auth) => auth,
            Err(path) => refused(format!(
                "Error: {path} kept changing while this edit waited for approval, and the changes overlap; the file was not modified. Read it again and redo the edit."
            )),
        }
    }

    /// `Ok`: the call can run as authorized (adjusted to the merge when the
    /// file changed). `Err` (the file's path): the approved changes overlap
    /// the file's new changes; it needs another review.
    fn merge_into_current(
        &self,
        tc: &ToolCall,
        mut auth: Authorized,
    ) -> Result<Authorized, String> {
        let Some(reviewed) = auth.reviewed.clone() else {
            return Ok(auth);
        };
        if auth.refusal.is_some() {
            return Ok(auth);
        }
        // What the approval writes whole. An approved partial write (old_text)
        // applies to the file as it is when it runs: nothing to merge.
        let args = permissions::parse_args(auth.args_override.as_deref().unwrap_or(&tc.arguments));
        if tools::is_partial_write(&tc.name, &args) && auth.args_override.is_none() {
            return Ok(auth);
        }
        let Some(approved) = args
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string)
        else {
            return Ok(auth);
        };
        let abs = std::path::Path::new(&reviewed.abs_path);
        let Ok(now) = std::fs::read(abs) else {
            // Gone or unreadable: the write recreates it, as approved.
            return Ok(auth);
        };
        // It was text when reviewed and isn't UTF-8 now (re-encoded, or
        // made binary): nothing to merge into, and writing over it would
        // lose that change. Another review.
        let Ok(now) = String::from_utf8(now) else {
            return Err(reviewed.rel_path);
        };
        if now == reviewed.current {
            self.seen.record(abs, now.as_bytes());
            return Ok(auth);
        }
        match crate::merge::merge3(&reviewed.current, &approved, &now) {
            Ok(crate::merge::Merged::Clean(merged)) => {
                self.seen.record(abs, now.as_bytes());
                auth.args_override =
                    Some(json!({ "path": reviewed.rel_path, "content": merged }).to_string());
                let note = format!(
                    "{} changed while this edit waited for approval; the approved changes were merged into the new version.",
                    reviewed.rel_path
                );
                auth.result_note = Some(match auth.result_note.take() {
                    Some(earlier) => format!("{earlier}\n{note}"),
                    None => note,
                });
                Ok(auth)
            }
            Ok(crate::merge::Merged::Conflict(_)) | Err(_) => Err(reviewed.rel_path),
        }
    }

    /// The diff of a write call against the file on disk.
    /// A new file has nothing to compare against: it's approved as a whole.
    /// A partial write's result is computed (not written) first.
    fn prepare_edit_review(&self, tool_name: &str, args_json: &str) -> Option<Prepared> {
        let root = self.root.as_deref()?;
        let args = permissions::parse_args(args_json);
        let path = args.get("path").and_then(|p| p.as_str())?.to_string();
        let partial = tools::is_partial_write(tool_name, &args);
        let proposed = if partial {
            let edit = match tools::edit_args(&args) {
                Ok(edit) => edit,
                Err(e) => return Some(Prepared::Invalid(e)),
            };
            match tools::apply_edit(root, &edit, true, None) {
                Ok(outcome) => outcome.content,
                Err(e) => return Some(Prepared::Invalid(format!("Error: {e}"))),
            }
        } else {
            args.get("content").and_then(|c| c.as_str())?.to_string()
        };
        // Inside the project, as the write itself insists: a file outside
        // it (a key file, a shell's history) must not be read into the
        // review, which goes to the reviewer's provider and the approval.
        let abs_path = match crate::agent_tools::resolve_within_root(root, &path) {
            Ok(abs) => abs,
            Err(e) => return Some(Prepared::Invalid(format!("Error: {e}"))),
        };
        let Ok(current) = std::fs::read_to_string(&abs_path) else {
            return Some(Prepared::New);
        };
        // Nothing to approve: the write itself refuses it. (An edit may
        // remove everything: its old_text says what it replaces.)
        if !partial {
            if let Some(error) =
                crate::agent_tools::empty_overwrite_error(&path, &current, &proposed)
            {
                return Some(Prepared::Invalid(format!("Error: {error}")));
            }
        }
        let hunks = diff::diff_lines(&diff::split_lines(&current), &diff::split_lines(&proposed));
        if hunks.is_empty() {
            return Some(Prepared::Unchanged);
        }
        Some(Prepared::Diff {
            proposal: EditProposal {
                abs_path: abs_path.to_string_lossy().into_owned(),
                rel_path: path,
                current,
                proposed,
            },
            hunks,
        })
    }

    /// The chat whose overlay this run's calls work in, and whose commands
    /// they are: its own when it has one (a group worker's child overlay),
    /// else, for a subagent, its lead's (a sequential worker, a test writer
    /// or a scout works in the lead's view, under the lead's isolation and
    /// its hold on the project's commands).
    pub(super) fn overlay_owner(&self) -> String {
        match self.sub() {
            Some(sub) if crate::overlay::chats::get(&self.dialog_id).is_none() => {
                sub.parent_dialog_id.clone()
            }
            _ => self.dialog_id.clone(),
        }
    }

    /// What a call runs with: the run's record of files it has seen, and
    /// its note on the tool block (a wait for a CPU slot shows there).
    fn call_context(&self, tool_call_id: &str) -> tools::CallContext {
        let (app, dialog_id, id) = (
            self.core.clone(),
            self.dialog_id.clone(),
            tool_call_id.to_string(),
        );
        tools::CallContext {
            seen: self.seen.clone(),
            restricted: self.restricted.clone(),
            owner: self.overlay_owner(),
            checks_owner: self.checks_owner(),
            hidden: self.sub().map(|s| s.hidden.clone()).unwrap_or_default(),
            background_chat: self.sub().is_none().then(|| self.dialog_id.clone()),
            reply_cut_off: self.reply_cut_off,
            note: std::sync::Arc::new(move |note| {
                app.emit(AgentEvent::ToolNote {
                    dialog_id: dialog_id.clone(),
                    tool_call_id: id.clone(),
                    note,
                })
            }),
        }
    }

    /// The chat whose saved output (`@checks/`, check_output.rs) this run's
    /// calls read and add to: its own, or its lead's for a subagent.
    fn checks_owner(&self) -> String {
        self.sub()
            .map_or_else(|| self.dialog_id.clone(), |s| s.parent_dialog_id.clone())
    }

    fn tool_note(&self, tool_call_id: &str, note: Option<String>) {
        self.core.emit(AgentEvent::ToolNote {
            dialog_id: self.dialog_id.clone(),
            tool_call_id: tool_call_id.to_string(),
            note,
        });
    }

    /*
     * -----------------------------------------------------------------------
     * Questions, scope and plans
     * -----------------------------------------------------------------------
     */

    /// ask_user: shows the questions in the chat and waits for the answer.
    /// In Agent and Plan mode, a few per run; the answer is the tool's
    /// result, kept with the questions in its meta for the card.
    async fn ask_user(&mut self, tc: &ToolCall, args: &Map<String, Value>) -> Result<(), String> {
        let log_question = |run: &Run, questions: usize, answers: Vec<String>| {
            log::write(
                &run.core,
                json!({
                    "event": "question",
                    "dialogId": run.dialog_id,
                    "model": run.profile.model,
                    "questions": questions,
                    "answers": answers,
                }),
            );
        };
        // D5: in Autopilot nobody answers; the plan says what it assumed.
        if self.autopilot {
            log_question(self, 0, vec!["autopilot".into()]);
            return self
                .add_tool_result(
                    &tc.id,
                    super::autopilot::NO_QUESTIONS.into(),
                    Outcome::Failed,
                    None,
                    Vec::new(),
                )
                .await;
        }
        if self.questions_asked >= plan::MAX_QUESTIONS_PER_RUN {
            log_question(self, 0, vec!["over-limit".into()]);
            let content = plan::past_the_question_limit(self.questions_asked, self.plan_mode);
            return self
                .add_tool_result(&tc.id, content, Outcome::Done, None, Vec::new())
                .await;
        }
        let questions = plan::parse_questions(args);
        if questions.is_empty() {
            let content =
                "Error: no question to ask. Pass questions: [{ question, options: [{ label }] }].";
            return self
                .add_tool_result(&tc.id, content.into(), Outcome::Failed, None, Vec::new())
                .await;
        }
        self.questions_asked += 1;

        let request = QuestionsRequest {
            tool_call_id: tc.id.clone(),
            dialog_id: self.dialog_id.clone(),
            questions: questions.clone(),
        };
        let outcome = self
            .core
            .agents
            .wait_for_answers(&self.core, request, &self.stop)
            .await;
        log_question(self, questions.len(), plan::outcome_kinds(&outcome));
        if matches!(outcome, plan::QuestionsOutcome::Stopped { .. }) {
            self.core.agents.stop(&self.core, &self.dialog_id);
        }
        let answer = plan::describe_outcome(&questions, &outcome, self.plan_mode);
        let meta = json!({ "questions": { "questions": questions, "outcome": outcome } });
        self.add_tool_result(
            &tc.id,
            answer.content,
            answer.outcome,
            Some(meta),
            Vec::new(),
        )
        .await
    }

    /// D07: a writing subagent asks for more paths. A sequential worker is
    /// granted them at once; every grant is listed in its result.
    /// D07: more scope for a writing subagent, within limits (P2.3): files,
    /// or a folder inside one of its step's own; at most
    /// `MAX_GRANTS`; nothing another step owns. Returns the result and
    /// what was granted (kept with the result, for a continued worker).
    fn extend_scope(&mut self, args: &Map<String, Value>) -> (String, Vec<String>) {
        let paths = match super::worker::extend_scope_paths(args) {
            Ok(p) => p,
            Err(e) => return (e, Vec::new()),
        };
        // A group worker: not what another worker or the contract owns.
        if let Some(refused) = self
            .sub()
            .and_then(|s| s.group.as_ref())
            .and_then(|g| super::group::refusal(g, &paths))
        {
            return (refused, Vec::new());
        }
        let Some(sub) = self.sub() else {
            return (
                "Error: extend_scope is for writing subagents.".into(),
                Vec::new(),
            );
        };
        let Some(scope) = sub.scope.as_ref() else {
            return (
                "Error: extend_scope is for writing subagents.".into(),
                Vec::new(),
            );
        };
        // A folder only inside the step's own folders: a whole `src/` or
        // `.github/` isn't a step's to take.
        let own: Vec<&String> = scope.folders.iter().chain(&scope.new_in).collect();
        if let Some(folder) = paths.iter().find(|p| {
            p.ends_with('/')
                && !own
                    .iter()
                    .any(|d| !d.is_empty() && super::paths::under(d, p))
        }) {
            return (
                format!("Error: not granted: `{folder}` is a folder outside your step's own. Name the files the step needs."),
                Vec::new(),
            );
        }
        // Another step's files, in every mode; a folder, when another
        // step's file or folder is in it (round 2).
        if let Some((n, p)) = paths.iter().find_map(|p| {
            if scope.allows(p.trim_end_matches('/'), true) {
                return None;
            }
            sub.other_steps
                .iter()
                .find(|(_, s)| {
                    s.allows(p.trim_end_matches('/'), true)
                        || p.ends_with('/')
                            && s.files
                                .iter()
                                .chain(&s.folders)
                                .any(|f| super::paths::under(p, f))
                })
                .map(|(n, _)| (*n, p))
        }) {
            return (
                format!("Error: not granted: `{p}` is step {n}'s to change. Leave it, and list it under \"## Open questions\"."),
                Vec::new(),
            );
        }
        let new: Vec<&String> = paths.iter().filter(|p| !scope.grants.contains(p)).collect();
        if scope.grants.len() + new.len() > MAX_GRANTS {
            return (
                format!("Error: not granted: a step worker gets at most {MAX_GRANTS} paths beyond its step, and this would be {}. Finish what's in your scope and list the rest under \"## Open questions\".", scope.grants.len() + new.len()),
                Vec::new(),
            );
        }
        let Some(scope) = self.sub_mut().and_then(|s| s.scope.as_mut()) else {
            return (
                "Error: extend_scope is for writing subagents.".into(),
                Vec::new(),
            );
        };
        for p in &paths {
            if !scope.grants.contains(p) {
                scope.grants.push(p.clone());
            }
        }
        if let Some(g) = self.sub().and_then(|s| s.group.as_ref()) {
            g.granted.record(&paths, &g.label);
        }
        (
            format!(
                "Granted: you may now write {}. The lead will see this in your result.",
                paths
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            paths,
        )
    }

    /// submit_plan: the plan is checked and, if it holds up, saved to the
    /// chat's plan file (a revision replaces it). Problems go back to the
    /// model to fix. Only this changes the file: a reply never does.
    pub(super) async fn submit_plan(
        &mut self,
        tc: &ToolCall,
        args: &Map<String, Value>,
    ) -> Result<(), String> {
        // S02: blocked by its last review, the plan is never saved.
        if self.plan_blocked {
            return self
                .add_tool_result(
                    &tc.id,
                    super::plan_review::BLOCKED_AGAIN.into(),
                    Outcome::Failed,
                    None,
                    Vec::new(),
                )
                .await;
        }
        // Findings are the reviewer's: a "## Findings" section the agent
        // wrote (or the IDE's open findings from an earlier save) is left
        // out before the plan is checked, reviewed and saved.
        let recent = self.recent().await?;
        // Step 7: the plan as sent, or the last plan with `edits` applied.
        let edits = args.get("edits").and_then(Value::as_array);
        let given = match (args.get("plan").and_then(|p| p.as_str()), edits) {
            (Some(plan), _) => plan.to_string(),
            (None, None) => {
                let content = "Error: the plan wasn't saved. Neither `plan` nor `edits` was given: send the complete plan.";
                return self
                    .add_tool_result(&tc.id, content.into(), Outcome::Failed, None, Vec::new())
                    .await;
            }
            (None, Some(edits)) => {
                if self.plan_draft.is_none() {
                    let file = match self.in_autopilot() {
                        true => {
                            super::autopilot::task_plan_file(&self.core, &self.dialog_id).await?
                        }
                        false => chat_plan_file(&recent),
                    };
                    self.plan_draft = file.and_then(|f| std::fs::read_to_string(f).ok());
                }
                let Some(draft) = self.plan_draft.clone() else {
                    let content = "Error: the plan wasn't saved. There's no earlier plan to edit: send the complete plan.";
                    return self
                        .add_tool_result(&tc.id, content.into(), Outcome::Failed, None, Vec::new())
                        .await;
                };
                match edit_last_plan(&draft, self.plan_sent.as_deref(), edits) {
                    Ok(plan) => plan,
                    Err(e) => {
                        return self
                            .add_tool_result(
                                &tc.id,
                                format!("Error: the plan wasn't saved. {e}"),
                                Outcome::Failed,
                                Some(json!({ "plan": draft })),
                                Vec::new(),
                            )
                            .await;
                    }
                }
            }
        };
        self.plan_sent = Some(given.clone());
        let (plan_text, findings_dropped) = plan::without_findings(&given);
        let request = recent
            .iter()
            .rev()
            .find(|m| plan::is_request(m))
            .map(|m| m.content.clone());
        // Balanced Autopilot: the IDE fixes the plan's mechanical format
        // problems itself (never a reviewer's finding).
        let (plan_text, fixes) = match self.balanced_autopilot() {
            true => plan::normalize(
                &plan_text,
                self.root.as_deref().map(std::path::Path::new),
                request.as_deref(),
            ),
            false => (plan_text, Vec::new()),
        };
        let plan_text = plan_text.as_str();
        // B12: the agent's answers to the findings that held the plan back.
        let responses = super::plan_review::parse_responses(args.get("responses"));
        // B07: against the project too, as this chat sees it; S01: an
        // Autopilot task's plan says what done means.
        let mut problems = plan::plan_problems_for(
            plan_text,
            self.root.as_deref().map(std::path::Path::new),
            self.in_autopilot(),
        );
        let format_problems = !problems.is_empty();
        // B12: a held finding left unanswered sends the plan back with the
        // checker's problems, before a review is spent on it.
        let unanswered = self.unanswered(&responses);
        let answers_only = problems.is_empty() && unanswered.is_some();
        problems.extend(unanswered);
        // Step 10: what a submit did before the checks' verdict.
        if self.balanced_autopilot() {
            let from_reply = recent.iter().rev().take(3).any(|m| {
                m.meta
                    .as_ref()
                    .and_then(|v| v.get("harness"))
                    .and_then(Value::as_str)
                    == Some("planFromReply")
            });
            log::write(
                &self.core,
                json!({
                    "event": "planSubmit",
                    "dialogId": self.dialog_id,
                    "formatRejections": self.plan_format_rejections + u32::from(format_problems && !answers_only),
                    "fixes": fixes.len(),
                    "edits": edits.map_or(0, Vec::len),
                    "fromReply": from_reply,
                    "problems": problems.len(),
                }),
            );
        }
        if !problems.is_empty() {
            let mut content = format!(
                "Error: the plan wasn't saved. Fix this and call submit_plan again with the complete plan:\n- {}",
                problems.join("\n- ")
            );
            if !fixes.is_empty() {
                content.push_str(&format!("\n\nThe IDE fixed: {}.", fixes.join("; ")));
            }
            // Balanced Autopilot: after repeated format rejections, the
            // plan's frame to fill in. They never use a review round.
            if self.balanced_autopilot() && format_problems && !answers_only {
                self.plan_format_rejections += 1;
                if self.plan_format_rejections >= 2 {
                    content.push_str(&format!(
                        "\n\nThe plan was sent back {} times for its form. Here is its frame, with TODO where you must write; fill it in with your content and send the complete plan:\n\n{}",
                        self.plan_format_rejections,
                        plan::skeleton(plan_text)
                    ));
                }
            }
            // The plan sent back is the last one submitted: what `edits`
            // apply to next.
            self.plan_draft = Some(plan_text.to_string());
            return self
                .add_tool_result(
                    &tc.id,
                    content,
                    Outcome::Failed,
                    Some(json!({ "plan": plan_text })),
                    Vec::new(),
                )
                .await;
        }
        // Plans are the project's, not a chat's change.
        let Some(root) = self.project_root.clone() else {
            return self
                .add_tool_result(
                    &tc.id,
                    "Error: no project is open, so the plan can't be saved.".into(),
                    Outcome::Failed,
                    None,
                    Vec::new(),
                )
                .await;
        };
        let request = request.unwrap_or_else(|| "plan".into());
        let existing = chat_plan_file(&recent);
        // The plan a revision is small against: in Autopilot the task's own
        // saved plan (S02), never an earlier task's, which a short first
        // plan would otherwise pass as a small revision of.
        let revised = match self.in_autopilot() {
            true => super::autopilot::task_plan_file(&self.core, &self.dialog_id).await?,
            false => existing.clone(),
        };
        // B09: the plan reviewer, when it's on, before the plan is saved.
        let review = self
            .review_plan(&tc.id, plan_text, revised.as_deref(), &responses)
            .await?;
        let record = review.record;
        let checked = plan_text.to_string();
        let ide_fixed =
            (!fixes.is_empty()).then(|| format!(" The IDE fixed: {}.", fixes.join("; ")));
        let (plan_text, note) = match review.decision {
            super::plan_review::Decision::Hold { result } => {
                return self
                    .add_tool_result(
                        &tc.id,
                        format!("Error: {result}{}", ide_fixed.unwrap_or_default()),
                        Outcome::Failed,
                        Some(json!({ "review": record, "plan": checked })),
                        Vec::new(),
                    )
                    .await;
            }
            super::plan_review::Decision::Save { plan, note } => (plan, note),
        };
        match plan::save_plan_file(
            &root,
            &request,
            &plan_text,
            existing.as_deref(),
            &self.local_date,
        ) {
            Ok(path) => {
                let mut content = format!(
                    "Saved the plan to {}.",
                    plan::relative_plan_path(&path, Some(&root))
                );
                if let Some(note) = note {
                    content.push(' ');
                    content.push_str(&note);
                }
                if let Some(fixed) = &ide_fixed {
                    content.push_str(fixed);
                }
                if findings_dropped {
                    content.push_str(
                        " Its findings section was left out: findings are the reviewer's, and open ones are added to the saved plan by the IDE.",
                    );
                }
                // Open tabs of the plan show the new version.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    self.core.emit(AgentEvent::FileWritten {
                        dialog_id: Some(self.dialog_id.clone()),
                        abs_path: path.clone(),
                        before: None,
                        content: text,
                        reviewed: true,
                        deleted: false,
                    });
                }
                let mut meta = json!({ "planFile": path, "plan": checked });
                if let Some(record) = record {
                    meta["review"] = record;
                }
                self.add_tool_result(&tc.id, content, Outcome::Done, Some(meta), Vec::new())
                    .await
            }
            Err(e) => {
                self.add_tool_result(
                    &tc.id,
                    format!("Error: the plan couldn't be saved: {e}"),
                    Outcome::Failed,
                    None,
                    Vec::new(),
                )
                .await
            }
        }
    }
}

/// Step 7: `edits` (`{old_text, new_text}` items) applied in order to the
/// last plan, with `edit_file`'s matching rules; an edit that doesn't
/// match exactly once fails them all.
pub(super) fn apply_plan_edits(draft: &str, edits: &[Value]) -> Result<String, String> {
    let mut plan = draft.to_string();
    for (i, edit) in edits.iter().enumerate() {
        let text = |k: &str| edit.get(k).and_then(Value::as_str);
        let (Some(old), Some(new)) = (text("old_text"), text("new_text")) else {
            return Err(format!(
                "Edit {}: it needs both `old_text` and `new_text`.",
                i + 1
            ));
        };
        plan = crate::edit_file::apply_edit(&plan, old, new, None, None)
            .map_err(|e| format!("Edit {}: {e}", i + 1))?
            .0;
    }
    Ok(plan)
}

/// `edits` applied to the last plan (`draft`, as the IDE fixed its form),
/// or, when one of them doesn't match it, to that plan as the agent sent
/// it (`sent`): an `old_text` copied from its own submission names a path
/// the IDE corrected. The draft's error when neither matches.
pub(super) fn edit_last_plan(
    draft: &str,
    sent: Option<&str>,
    edits: &[Value],
) -> Result<String, String> {
    let e = match apply_plan_edits(draft, edits) {
        Ok(plan) => return Ok(plan),
        Err(e) => e,
    };
    match sent.filter(|sent| *sent != draft) {
        Some(sent) => apply_plan_edits(sent, edits).map_err(|_| e),
        None => Err(e),
    }
}

/// A command's arguments with its timeout cut to the time `left` of a
/// subagent's budget (at least a second); None when it's within it, or for
/// the lead, which has no time budget.
fn clamp_timeout(args_json: &str, left: Option<std::time::Duration>) -> Option<String> {
    let left = left?;
    let mut args = permissions::parse_args(args_json);
    let asked = args
        .get("timeout_seconds")
        .and_then(|t| t.as_f64())
        .filter(|t| *t > 0.0)
        .unwrap_or(120.0);
    let allowed = asked.min(left.as_secs_f64().max(1.0));
    (allowed < asked).then(|| {
        args.insert("timeout_seconds".into(), json!(allowed));
        Value::Object(args).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 7: an edit copied from the agent's own submission matches the
    /// plan as it sent it when the IDE's fixed draft no longer has its
    /// text; one that matches the draft goes there; one that matches
    /// neither fails with the draft's error.
    #[test]
    fn plan_edits_match_the_plan_as_sent_when_the_draft_was_fixed() {
        let sent = "## Steps\n\n1. Change users\n   Files: `../../src/api/users.rs`\n";
        let draft = "# Fix users\n\n## Steps\n\n1. Change users\n   Files: `src/api/users.rs`\n";
        let edit = |old: &str, new: &str| vec![json!({ "old_text": old, "new_text": new })];

        let from_sent = edit(
            "Files: `../../src/api/users.rs`",
            "Files: `../../src/api/users.rs`, `src/db.rs`",
        );
        let edited = edit_last_plan(draft, Some(sent), &from_sent).unwrap();
        assert!(
            edited.contains("`../../src/api/users.rs`, `src/db.rs`"),
            "{edited}"
        );

        let from_draft = edit("# Fix users", "# Fix the users page");
        let edited = edit_last_plan(draft, Some(sent), &from_draft).unwrap();
        assert!(edited.starts_with("# Fix the users page\n"), "{edited}");
        assert!(
            edited.contains("`src/api/users.rs`"),
            "the draft's fix stays"
        );

        let neither = edit("Files: `nowhere.rs`", "x");
        let err = edit_last_plan(draft, Some(sent), &neither).unwrap_err();
        assert_eq!(err, apply_plan_edits(draft, &neither).unwrap_err());
        // Nothing sent apart from the draft: its error alone.
        assert!(edit_last_plan(draft, None, &from_sent).is_err());
    }

    /// Round 2 (section 4.2): a subagent's command timeout is cut to the
    /// time it has left (at least a second, and later ones no longer than
    /// earlier ones); the lead's never is.
    #[test]
    fn bash_timeout_is_clamped_to_the_time_left() {
        use std::time::Duration;
        let timeout = |json: &str| -> f64 {
            serde_json::from_str::<Value>(json).unwrap()["timeout_seconds"]
                .as_f64()
                .unwrap()
        };
        let asked = r#"{"command":"npm test","timeout_seconds":600}"#;
        let first = timeout(&clamp_timeout(asked, Some(Duration::from_secs(90))).unwrap());
        let second = timeout(&clamp_timeout(asked, Some(Duration::from_secs(30))).unwrap());
        assert!(
            second <= first && first <= 90.0 && second >= 1.0,
            "{first} {second}"
        );
        // No timeout given: the default 120 s, cut too.
        assert_eq!(
            timeout(&clamp_timeout(r#"{"command":"x"}"#, Some(Duration::from_secs(10))).unwrap()),
            10.0
        );
        // No time left: a second, not zero.
        assert_eq!(
            timeout(&clamp_timeout(asked, Some(Duration::ZERO)).unwrap()),
            1.0
        );
        // Within the budget: as asked.
        assert_eq!(
            clamp_timeout(
                r#"{"command":"x","timeout_seconds":5}"#,
                Some(Duration::from_secs(60))
            ),
            None
        );
    }

    /// Round 2 (section 4.2): the lead has no time budget.
    #[test]
    fn the_lead_has_no_wall() {
        assert_eq!(
            clamp_timeout(r#"{"command":"x","timeout_seconds":600}"#, None),
            None
        );
    }

    #[test]
    fn context_paths() {
        assert!(is_context_path(Some("@context/LIVE_CONTEXT.md"), None));
        assert!(is_context_path(Some(" @context "), None));
        assert!(is_context_path(
            Some("/mnt/v/context/d/x.md"),
            Some("/mnt/v/context/d/")
        ));
        assert!(!is_context_path(Some("src/a.ts"), Some("/mnt/v/context/d")));
        assert!(!is_context_path(None, None));
    }

    #[test]
    fn rejected_hunks_for_the_model() {
        let hunks = vec![
            Hunk {
                orig_start: 3,
                deleted: vec!["a".into()],
                added: vec![],
            },
            Hunk {
                orig_start: 9,
                deleted: vec!["b".into()],
                added: vec!["c".into(), "d".into()],
            },
        ];
        let text = describe_rejected_hunks("x.ts", &hunks, 5);
        assert!(text.contains("accepted 3 of 5 changes"));
        assert!(text.contains("- around original line 3: deleting these lines (kept):\n    a"));
        assert!(text.contains("replacing\n    b\n  with (not applied):\n    c\n    d"));
    }
}

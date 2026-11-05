//! The project's long-term memory in a run (the `verified-code-memory`
//! crate, opened per project by `crate::memory`): what the run recalls
//! before its first request — into its prompt, and the chat's "Relevant
//! prior work" panel — what it's told about the files it edits, what its
//! checks promote or invalidate, and what its end records.

use super::verify::CheckResult;
use crate::core::Core;
use rovik_protocol::events::{AgentEvent, RecalledMemory};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;
use verified_code_memory::{
    MemoryKind, MemoryRecord, NewMemory, RetrievalQuery, SharedMemoryStore, Verification,
};

/// How many memories a run recalls: enough to be useful in the prompt, few
/// enough that a weak match doesn't crowd out the project itself.
const RECALL_LIMIT: usize = 8;

/// How much of the request retrieval matches a memory's words against: its
/// first characters, so a pasted log at the end doesn't decide what's
/// relevant.
const TASK_CHARS: usize = 2_000;

/// How much of a memory the note on an edit carries: one sentence, on the
/// line above the call while it runs.
const HINT_CHARS: usize = 160;

/// How much of the request becomes a reflection's title.
const TITLE_CHARS: usize = 80;

/// The project's memory as one run uses it. Default: nothing, which is
/// every subagent's and every project with memory turned off.
#[derive(Default)]
pub(super) struct RunMemory {
    /// The project's store; None when its memory is off or its file can't be
    /// opened, and the run goes on without it.
    store: Option<SharedMemoryStore>,
    /// The project, as the store files memories under it.
    repo: String,
    /// The request, as retrieval matches it.
    task: String,
    /// What the run recalled, as the store has it now.
    recalled: Vec<MemoryRecord>,
    /// The prompt's section of it; empty when nothing was recalled.
    context: String,
    /// The files already asked about: a file written twice in a run asks
    /// once, whether the memory had something to say or not.
    asked: HashSet<String>,
    /// The calls a note was put on, cleared when their result is stored —
    /// only those, so no other part's note is taken from its call.
    noted: HashSet<String>,
    /// The rounds of checks that failed in this run, and the first of them
    /// as the lesson at the run's end needs it.
    failed_rounds: u32,
    first_failure: Option<FailedRound>,
    /// Whether the last round of checks passed; unset: no round ran. What a
    /// `done` end says about the memories the run used.
    last_round_passed: Option<bool>,
}

impl RunMemory {
    /// The section of the system prompt about the project's prior work.
    pub(super) fn context(&self) -> &str {
        &self.context
    }

    /// What the project's memory knows about `path`, as a note on the call
    /// `tool_call_id`: the closest memory that names that file, and none
    /// when the memory retrieval offers is about another one (a note about
    /// somewhere else isn't a constraint here).
    pub(super) async fn hint(&mut self, tool_call_id: &str, path: &str) -> Option<String> {
        let store = self.store.clone()?;
        if !self.asked.insert(path.to_string()) {
            return None;
        }
        let (repo, task, file) = (self.repo.clone(), self.task.clone(), path.to_string());
        let found = crate::rt::spawn_blocking(move || {
            let query = RetrievalQuery::new(repo.as_str(), task.as_str())
                .with_files([file])
                .with_limit(1);
            store.retrieve(&query).ok()?.into_iter().next()
        })
        .await
        .ok()
        .flatten()?;
        if !names_file(&found.record, path) {
            return None;
        }
        let note = format!(
            "Remembered: {} — {}",
            found.record.title,
            brief(&found.record.content, HINT_CHARS)
        );
        self.noted.insert(tool_call_id.to_string());
        Some(note)
    }

    /// Whether this call was given a note, which goes now its result is in.
    pub(super) fn take_note(&mut self, tool_call_id: &str) -> bool {
        self.noted.remove(tool_call_id)
    }

    /// What one round of checks makes of the memories this run used: a round
    /// with no failure confirms them, and a failure invalidates only a memory
    /// about a file the failure points at — a red check doesn't show a memory
    /// was wrong, so the rest keep their state and the run's end records only
    /// that the approach fell short.
    pub(super) fn check_plan(&self, results: &[CheckResult]) -> CheckPlan {
        let mut plan = CheckPlan::default();
        if self.store.is_none() || self.recalled.is_empty() || results.is_empty() {
            return plan;
        }
        let failed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "failed").collect();
        if failed.is_empty() {
            let Some(evidence) = passed_evidence(results) else {
                return plan;
            };
            plan.verify = self
                .recalled
                .iter()
                .filter(|memory| memory.verification == Verification::Provisional)
                .map(|memory| (memory.id, evidence.clone()))
                .collect();
            return plan;
        }
        let mut planned: HashSet<Uuid> = HashSet::new();
        for check in failed {
            let Some(files) = check.error_files.as_ref() else {
                continue;
            };
            for file in &files.changed {
                for memory in &self.recalled {
                    if memory.verification == Verification::Deprecated || !names_file(memory, file)
                    {
                        continue;
                    }
                    if planned.insert(memory.id) {
                        plan.deprecate.push((
                            memory.id,
                            format!(
                                "`{}` failed in {file}, which this memory is about",
                                check.command
                            ),
                        ));
                    }
                }
            }
        }
        plan
    }

    /// What this round of checks was, for the lesson the run's end may keep:
    /// whether it passed, and — the first time one fails — what failed and
    /// where.
    fn note_round(&mut self, results: &[CheckResult]) {
        if results.is_empty() {
            return;
        }
        let failed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "failed").collect();
        self.last_round_passed = Some(failed.is_empty());
        let Some(check) = failed.first() else {
            return;
        };
        self.failed_rounds = self.failed_rounds.saturating_add(1);
        if self.first_failure.is_some() {
            return;
        }
        self.first_failure = Some(FailedRound {
            command: check.command.clone(),
            failures: check.failures.clone().unwrap_or_default(),
            files: check
                .error_files
                .as_ref()
                .map(|files| files.changed.clone())
                .unwrap_or_default(),
        });
    }

    /// The memory as the wait for the checks' approval carries it (verify.rs
    /// `VerifyCarry`): none when the project's memory is off, so the run that
    /// starts the checks recalls nothing for it either.
    pub(super) fn carry(&self) -> Option<MemoryCarry> {
        self.store.as_ref()?;
        Some(MemoryCarry {
            repo: self.repo.clone(),
            recalled: self.recalled.clone(),
            failed_rounds: self.failed_rounds,
            first_failure: self.first_failure.clone(),
            last_round_passed: self.last_round_passed,
        })
    }

    /// The run that starts the checks goes on with the carried memories and
    /// rounds; the project's store is opened again (a store is a connection,
    /// not data to hand on).
    pub(super) fn restore(core: &Core, carry: MemoryCarry) -> RunMemory {
        RunMemory {
            store: core.memory.open(&core.paths, Path::new(&carry.repo)),
            repo: carry.repo,
            task: String::new(),
            recalled: carry.recalled,
            context: String::new(),
            asked: HashSet::new(),
            noted: HashSet::new(),
            failed_rounds: carry.failed_rounds,
            first_failure: carry.first_failure,
            last_round_passed: carry.last_round_passed,
        }
    }
}

/// What one round of checks makes of the memories a run used.
#[derive(Default, Debug, PartialEq)]
pub(super) struct CheckPlan {
    /// To promote, with the evidence that did it: the checks that passed.
    pub(super) verify: Vec<(Uuid, String)>,
    /// To invalidate, with why: a failure in the file the memory is about.
    pub(super) deprecate: Vec<(Uuid, String)>,
}

impl CheckPlan {
    fn is_empty(&self) -> bool {
        self.verify.is_empty() && self.deprecate.is_empty()
    }
}

/// The first round of checks a run failed, as its end's lesson needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct FailedRound {
    /// The command that failed.
    pub(super) command: String,
    /// The keys it failed on (`command_output::failure_keys`).
    pub(super) failures: Vec<String>,
    /// The changed files its failures point at.
    pub(super) files: Vec<String>,
}

/// A run's memory as it crosses a wait for the checks' approval (verify.rs
/// `VerifyCarry`): without it, the run that starts the checks would promote,
/// invalidate and record nothing, and would have no failed round to learn
/// from.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MemoryCarry {
    pub(super) repo: String,
    pub(super) recalled: Vec<MemoryRecord>,
    pub(super) failed_rounds: u32,
    pub(super) first_failure: Option<FailedRound>,
    pub(super) last_round_passed: Option<bool>,
}

/// Whether `record` is about `path`: one of its files is the path's tail or
/// the path its tail, as the crate matches files (its
/// `retrieval::path_overlap`, which the crate keeps to itself).
fn names_file(record: &MemoryRecord, path: &str) -> bool {
    let shown = |p: &str| p.replace('\\', "/").trim_matches('/').to_string();
    let want = shown(path);
    record.files.iter().map(|f| shown(f)).any(|file| {
        !file.is_empty() && (file == want || want.ends_with(&file) || file.ends_with(&want))
    })
}

/// The tag a run puts on what it writes, so the chat that wrote a memory
/// doesn't recall it as prior work (its own history has it).
fn chat_tag(dialog_id: &str) -> String {
    format!("chat:{dialog_id}")
}

/// `text`'s first `chars` characters on one line, marked when cut.
fn brief(text: &str, chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = line.chars().take(chars).collect();
    if cut.chars().count() < line.chars().count() {
        cut.push('…');
    }
    cut
}

/// A memory as the chat's panel shows it.
fn recalled(memory: &MemoryRecord, helped: Option<bool>) -> RecalledMemory {
    RecalledMemory {
        id: memory.id.to_string(),
        kind: memory.kind.to_string(),
        title: memory.title.clone(),
        content: memory.content.clone(),
        files: memory.files.clone(),
        verification: memory.verification.to_string(),
        evidence: memory.evidence.clone(),
        helped,
    }
}

/// The chat's list of the memories a run used, replacing what it had
/// (agentRuns.ts): sent when the run recalls, and again at its end with
/// what the run made of them.
pub(super) fn event(
    dialog_id: &str,
    memories: &[MemoryRecord],
    helped: Option<bool>,
) -> AgentEvent {
    AgentEvent::MemoryRecalled {
        dialog_id: dialog_id.to_string(),
        memories: memories.iter().map(|m| recalled(m, helped)).collect(),
    }
}

impl super::run::Run {
    /// Recalls the project's prior work for this request, before the first
    /// request goes: the prompt's section (`model_turn`) and the chat's
    /// "Relevant prior work" panel.
    ///
    /// Nothing is recalled and nothing is sent — no `memoryRecalled` at all —
    /// when the project's memory is turned off, when its store can't be
    /// opened, or when there is no request to match at all (a chat with no
    /// user message yet). The chat then keeps the list its last run showed,
    /// as it does after any run that left one.
    ///
    /// What is matched is the message this run sends, or, for a run that
    /// sends none (a Continue, a Retry, a task's stage), the chat's latest:
    /// the same chat recalls the same prior work whichever way its run
    /// started.
    ///
    /// With memory on and nothing relevant, the empty list is sent: the panel
    /// shows none, not an earlier run's.
    pub(super) async fn recall_memory(&mut self, settings_root: &str) {
        let text = match self.current_user_content.as_deref() {
            Some(text) if !text.trim().is_empty() => text.to_string(),
            _ => self.latest_user_request().await,
        };
        let task = text.chars().take(TASK_CHARS).collect::<String>();
        let repo = settings_root.to_string();
        let chat = self.dialog_id.clone();

        let found = if task.trim().is_empty() {
            None
        } else {
            let (core, query_repo, query_task) = (self.core.clone(), repo.clone(), task.clone());
            let project = Path::new(settings_root).to_path_buf();
            // Opening the store and querying it are disk work, as the
            // project's settings are: off the async threads.
            let own = chat_tag(&chat);
            crate::rt::spawn_blocking(move || {
                let settings = crate::memory::load(&core.paths.config_dir, &query_repo);
                if !settings.enabled {
                    return None;
                }
                let store = core.memory.open(&core.paths, &project)?;
                let query = RetrievalQuery::new(query_repo.as_str(), query_task.as_str())
                    .with_limit(RECALL_LIMIT);
                let (memories, _telemetry) = store.retrieve_with_telemetry(&query).ok()?;
                // The chat's own work isn't prior work: it is in the chat
                // already, and telling the model what it just did would move
                // its prompt on every run of the same chat.
                let memories: Vec<_> = memories
                    .into_iter()
                    .filter(|memory| !memory.record.tags.contains(&own))
                    .collect();
                // The prompt carries what evidence backs: a provisional
                // memory shows in the chat's panel as a hint, but it doesn't
                // steer the model (the crate's gate is evidence, and a
                // memory it recalled untested is the one thing a run must
                // not follow).
                let proven: Vec<_> = memories
                    .iter()
                    .filter(|memory| memory.record.verification == Verification::Verified)
                    .cloned()
                    .collect();
                // Nothing proven: no section at all, rather than a note that
                // there is none (render_context always writes a heading).
                let context = if proven.is_empty() {
                    String::new()
                } else {
                    store.render_context(&proven)
                };
                let records = memories.into_iter().map(|m| m.record).collect();
                Some((store, records, context))
            })
            .await
            .ok()
            .flatten()
        };

        // Only a run that has the project's memory sends its list: off, or
        // a store that didn't open, leaves the chat as it was.
        match found {
            Some((store, recalled, context)) => {
                self.memory = RunMemory {
                    store: Some(store),
                    repo,
                    task,
                    recalled,
                    context,
                    ..Default::default()
                };
                let list = event(&self.dialog_id, &self.memory.recalled, None);
                self.core.emit(list);
            }
            None => {
                self.memory = RunMemory {
                    repo,
                    task,
                    ..Default::default()
                };
            }
        }
    }

    /// What this round of checks made of the memories the run used: the ones
    /// a green round confirms are promoted, the ones a failure points at are
    /// invalidated, and the chat's panel shows their new state. The round is
    /// kept too, for the run's end (`memory_after_run`).
    pub(super) async fn memory_after_checks(&mut self, results: &[CheckResult]) {
        let plan = self.memory.check_plan(results);
        self.memory.note_round(results);
        let Some(store) = self.memory.store.clone() else {
            return;
        };
        if plan.is_empty() {
            return;
        }
        let (verify, deprecate) = (plan.verify, plan.deprecate);
        let changed = crate::rt::spawn_blocking(move || {
            let mut changed = Vec::new();
            for (id, evidence) in verify {
                match store.verify(id, evidence) {
                    Ok(record) => changed.push(record),
                    Err(e) => eprintln!("memory: {id} wasn't promoted: {e}"),
                }
            }
            for (id, reason) in deprecate {
                match store.deprecate(id, reason) {
                    Ok(record) => changed.push(record),
                    Err(e) => eprintln!("memory: {id} wasn't invalidated: {e}"),
                }
            }
            changed
        })
        .await
        .unwrap_or_default();
        if changed.is_empty() {
            return;
        }
        for record in changed {
            if let Some(slot) = self
                .memory
                .recalled
                .iter_mut()
                .find(|memory| memory.id == record.id)
            {
                *slot = record;
            }
        }
        let list = event(&self.dialog_id, &self.memory.recalled, None);
        self.core.emit(list);
    }

    /// The run's end, for the project's memory: whether the memories it used
    /// helped, the run's own account of itself, and the lesson a repaired
    /// failure taught. Called once the outcome is final (`go`).
    pub(super) async fn memory_after_run(&mut self, outcome: &str) {
        let Some(store) = self.memory.store.clone() else {
            return;
        };
        let helped = verdict(outcome, &self.memory);
        let (repo, task) = (self.memory.repo.clone(), self.memory.task.clone());
        let chat = self.dialog_id.clone();
        let recalled = self.memory.recalled.clone();
        let failure = self.memory.first_failure.clone();
        let files = self.changed_files.clone();
        let results = self.verification.last_results().to_vec();
        let outcome = outcome.to_string();

        let touched = crate::rt::spawn_blocking(move || {
            let mut touched = Vec::new();
            if let Some(helped) = helped {
                touched = record_verdict(&store, &recalled, helped);
            }
            if !files.is_empty() && !task.trim().is_empty() {
                keep_reflection(&Ending {
                    store: &store,
                    repo: &repo,
                    chat: &chat,
                    task: &task,
                    outcome: &outcome,
                    files: &files,
                    results: &results,
                    helped,
                });
            }
            // A lesson only from a failure this run itself fixed.
            if let (Some(failure), Some(true)) = (&failure, helped) {
                if !files.is_empty() {
                    keep_lesson(&store, &repo, &chat, failure, &files, &results);
                }
            }
            touched
        })
        .await
        .unwrap_or_default();

        for record in touched {
            if let Some(slot) = self
                .memory
                .recalled
                .iter_mut()
                .find(|memory| memory.id == record.id)
            {
                *slot = record;
            }
        }
        let list = event(&self.dialog_id, &self.memory.recalled, helped);
        self.core.emit(list);
    }
}

/// What a run's end says about the memories it used: whether they helped,
/// or none when the approach was never put to a test — an interrupted run
/// (a stop, an error, the budget, a pause, a wait for the checks' approval),
/// or one whose checks didn't run at all.
fn verdict(outcome: &str, memory: &RunMemory) -> Option<bool> {
    match outcome {
        // The checks ran and passed.
        "verified" | "verify-unchecked" | "repaired" => Some(true),
        // A plain end only counts when a round of checks passed first: with
        // no round (nothing to check, or checking off) it proved nothing.
        "done" => match memory.last_round_passed {
            Some(true) => Some(true),
            _ => None,
        },
        // The checks ran and the change came out short.
        "verify-flagged" | "verify-unresolved" | "verify-gave-up" | "unmet" => Some(false),
        _ => None,
    }
}

/// The commands that passed, as the evidence that confirms a memory; none
/// when nothing passed.
fn passed_evidence(results: &[CheckResult]) -> Option<String> {
    let passed: Vec<&str> = results
        .iter()
        .filter(|result| result.status == "passed")
        .map(|result| result.command.as_str())
        .collect();
    (!passed.is_empty()).then(|| format!("the checks passed: {}", passed.join(", ")))
}

/// Gives the memories the run recalled its verdict, skipping the ones its
/// checks invalidated (their evidence already says so); the records as the
/// store has them now.
fn record_verdict(
    store: &SharedMemoryStore,
    recalled: &[MemoryRecord],
    helped: bool,
) -> Vec<MemoryRecord> {
    let mut touched = Vec::new();
    for memory in recalled {
        if memory.verification == Verification::Deprecated {
            continue;
        }
        match store.record_outcome(memory.id, helped) {
            Ok(record) => touched.push(record),
            Err(e) => eprintln!("memory: {} wasn't counted: {e}", memory.id),
        }
    }
    touched
}

/// The run's own account of itself — never "I finished": what was asked,
/// how it ended, what changed and what the checks said. Confirmed with the
/// checks that passed, so a later run can trust it; provisional otherwise.
fn reflect(
    repo: &str,
    task: &str,
    outcome: &str,
    files: &[String],
    results: &[CheckResult],
) -> NewMemory {
    let first = task
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("A run of this project");
    let mut content = format!("Outcome: {outcome}");
    if !files.is_empty() {
        content.push_str(&format!("\nChanged: {}", files.join(", ")));
    }
    let checks: Vec<String> = results
        .iter()
        .map(|result| format!("{} {}", result.command, result.status))
        .collect();
    if !checks.is_empty() {
        content.push_str(&format!("\nChecks: {}", checks.join("; ")));
    }
    NewMemory::episode(repo, brief(first, TITLE_CHARS), content).with_files(files.iter().cloned())
}

/// What a run's end tells the project's memory about itself.
struct Ending<'a> {
    store: &'a SharedMemoryStore,
    repo: &'a str,
    /// The chat whose work it is: it doesn't recall its own.
    chat: &'a str,
    /// The request, as its reflection's title.
    task: &'a str,
    outcome: &'a str,
    files: &'a [String],
    results: &'a [CheckResult],
    /// What the run's end said about the memories it used.
    helped: Option<bool>,
}

/// Keeps the run's reflection, and confirms it when the checks passed.
fn keep_reflection(end: &Ending<'_>) {
    let (store, chat) = (end.store, end.chat);
    let (repo, task, outcome) = (end.repo, end.task, end.outcome);
    let (files, results, helped) = (end.files, end.results, end.helped);
    let reflection = reflect(repo, task, outcome, files, results).with_tags([chat_tag(chat)]);
    let record = match store.remember(reflection) {
        Ok(record) => record,
        Err(e) => {
            eprintln!("memory: the run's reflection wasn't kept: {e}");
            return;
        }
    };
    let evidence = match (helped, passed_evidence(results)) {
        (Some(true), Some(evidence)) => evidence,
        _ => return,
    };
    if let Err(e) = store.verify(record.id, evidence) {
        eprintln!("memory: the run's reflection wasn't confirmed: {e}");
    }
}

/// The lesson of a failure this run repaired: the check that failed, what it
/// failed on, and the files that made it pass. An older lesson for the same
/// check is what it replaces (a remembered title and kind, exactly: nothing
/// else is invalidated).
fn keep_lesson(
    store: &SharedMemoryStore,
    repo: &str,
    chat: &str,
    failure: &FailedRound,
    files: &[String],
    results: &[CheckResult],
) {
    let Some(evidence) = passed_evidence(results) else {
        return;
    };
    let mut content = format!("A fix made `{}` pass after it failed.", failure.command);
    if !failure.failures.is_empty() {
        content.push_str(&format!("\nFailed on: {}", failure.failures.join("; ")));
    }
    if !failure.files.is_empty() {
        content.push_str(&format!("\nPointed at: {}", failure.files.join(", ")));
    }
    content.push_str(&format!("\nChanged: {}", files.join(", ")));
    let title = format!("Making `{}` pass", failure.command);
    let lesson = NewMemory::procedure(repo, title.clone(), content)
        .with_files(files.iter().cloned())
        .with_tags([chat_tag(chat)]);
    let record = match store.remember(lesson) {
        Ok(record) => record,
        Err(e) => {
            eprintln!("memory: the lesson wasn't kept: {e}");
            return;
        }
    };
    if let Err(e) = store.verify(record.id, evidence) {
        eprintln!("memory: the lesson wasn't confirmed: {e}");
        return;
    }
    // Retrieval falls back to the project's most recently updated memories
    // when full-text search matches nothing, so a candidate counts as the
    // older lesson only with this one's title and kind.
    let query = RetrievalQuery::new(repo, title.as_str()).with_limit(RECALL_LIMIT);
    let older: Vec<Uuid> = match store.retrieve(&query) {
        Ok(found) => found
            .into_iter()
            .map(|memory| memory.record)
            .filter(|memory| {
                memory.id != record.id
                    && memory.title == title
                    && memory.kind == MemoryKind::Procedure
                    && memory.verification == Verification::Verified
            })
            .map(|memory| memory.id)
            .collect(),
        Err(e) => {
            eprintln!("memory: the project's lessons couldn't be read: {e}");
            return;
        }
    };
    if older.is_empty() {
        return;
    }
    if let Err(e) = store.resolve_conflict(record.id, &older, "a newer run's fix replaced it") {
        eprintln!("memory: an older lesson wasn't replaced: {e}");
    }
}

#[cfg(test)]
mod tests;

//! The verification gate. When the model finishes a run in which it changed
//! files, the IDE runs the project's checks (type check, lint, tests:
//! project.rs) and, if any fail, sends the failures back to the agent to
//! fix, up to MAX_VERIFY_ROUNDS times. The environment, not the model,
//! decides that the change works.
//!
//! Each round's record shows in the chat: an empty assistant message with
//! `meta.verification`, never sent to the model, saved on every change so it
//! reads the same after a restart.

use super::command_output::{
    check_keys, environment_failure, error_locations, focus_failures, split_by_changed, tests_ran,
};
use super::log;
use super::permissions::{check_command, denied_entry, Permissions, Verdict};
use super::project::{check_label, ordered_checks, ProjectCheck};
use super::repair::{self, Judgement, RoundFailures, Verdict as Progress};
use super::run::{new_message, Run};
use super::subagent::SubagentSettings;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Fix-and-recheck rounds before the gate gives up and reports.
pub const MAX_VERIFY_ROUNDS: u32 = 3;
/// Autopilot's rounds end after this many in a row without progress, not
/// at MAX_VERIFY_ROUNDS (V03, D12).
pub const STALLED_ROUNDS: u32 = 2;
/// The most rounds an Autopilot run has, progress or not: a stop for a
/// task with no budget whose rounds go on changing what fails.
pub const AUTOPILOT_MAX_ROUNDS: u32 = 10;
/// Per check. Test suites can be slow; this is the bash maximum.
const CHECK_TIMEOUT_MS: u64 = 600_000;

/// Tests: a shorter limit for one chat's checks.
#[cfg(test)]
pub(super) static CHECK_TIMEOUTS: std::sync::Mutex<std::collections::BTreeMap<String, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

pub(super) fn check_timeout_ms(_dialog_id: &str) -> u64 {
    #[cfg(test)]
    if let Some(ms) = CHECK_TIMEOUTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(_dialog_id)
    {
        return *ms;
    }
    CHECK_TIMEOUT_MS
}
/// A failing check's output the agent gets, at most; the whole of a longer
/// one is saved (check_output.rs) and can go to triage (triage.rs).
pub const FAILURE_OUTPUT_CHARS: usize = 8_000;

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorFiles {
    pub changed: Vec<String>,
    pub other: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub kind: String,
    pub command: String,
    /// pending, running, passed, failed, skipped, slow (V06), and
    /// preexisting: it failed, but only as it does on the code before the
    /// change (before.rs).
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// skipped: why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// failed: the relevant part of the output (error blocks and summary).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// failed: files the errors point at, split by whether this run changed them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_files: Option<ErrorFiles>,
    /// Waiting for a CPU slot (while it waits), or how long it waited
    /// when that was long (workload/queue.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// failed: a key for each failure, the same when only lines moved
    /// (command_output.rs `failure_keys`; repair.rs compares rounds by them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failures: Option<Vec<String>>,
    /// The test check: how many tests ran, when its summary said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_ran: Option<u32>,
    /// failed, with more output than the report shows: where the whole of
    /// it is saved, `@checks/…` (check_output.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_output: Option<String>,
    /// Autopilot, failed: the line saying its environment failed, not the
    /// code (`command_output::environment_failure`, V03).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Failed, or `preexisting`: the keys of its failures that fail on the
    /// code before the change too, not the change's (before.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preexisting: Option<Vec<String>>,
}

impl CheckResult {
    pub(super) fn pending(check: &ProjectCheck) -> Self {
        CheckResult {
            kind: check.kind.clone(),
            command: check.command.trim().to_string(),
            status: "pending",
            exit_code: None,
            timed_out: None,
            duration_ms: None,
            reason: None,
            output: None,
            error_files: None,
            note: None,
            failures: None,
            tests_ran: None,
            saved_output: None,
            environment: None,
            preexisting: None,
        }
    }

    fn skip(&mut self, reason: &str) {
        self.status = "skipped";
        self.reason = Some(reason.into());
    }
}

/// A run's fix-round state while its checks wait for the user's approval:
/// with command approval on, each round of checks starts a new run, which
/// takes this up (R01 progress, R02 rollback, R04 the repair loop, R05
/// integrity, R08 the review after green). Kept in memory: after a
/// restart, the next round starts afresh, as before.
pub struct VerifyCarry {
    pub record_id: String,
    history: Vec<RoundFailures>,
    last_results: Vec<CheckResult>,
    last_failed: bool,
    checkpoint: Option<repair::Checkpoint>,
    baseline: std::collections::HashMap<String, super::subagent::Original>,
    integrity_open: Vec<String>,
    asked_integrity: bool,
    rolled_back: Vec<repair::Rollback>,
    bonus_rounds: u32,
    groups: Vec<super::repair_run::GroupRecord>,
    last_triage: Option<super::triage::Triage>,
    test_writer_files: Vec<String>,
    tests_changed: Vec<String>,
    worker_steps: Vec<super::worker::DoneStep>,
    reviews_total: u32,
    review_findings: Vec<super::change_review::Finding>,
    review_optional: Vec<super::change_review::Finding>,
    /// What the run last wrote or read: in a chat that isn't isolated, a
    /// rollback undoes only what's still the run's (K15).
    seen: std::sync::Arc<crate::file_guard::Seen>,
    /// What the run's subagents used of their limits (P3.10): the next
    /// rounds' fixers count on.
    ledger: super::subagent::Ledger,
    rechecks: u32,
    /// Commands the waiting round's fix ran (P3.5): listed if it's rolled
    /// back after "Run checks" (round 2).
    round_commands: Vec<String>,
    /// How the checks went on the code before the change (before.rs).
    befores: super::before::Befores,
    /// When it was kept: one older than `CARRY_TTL` is dropped (P1.8).
    pub stored: std::time::Instant,
    /// Baseline contents past `CARRY_MEMORY_BYTES`, on disk until taken up.
    spilled: Spilled,
}

#[cfg(test)]
impl VerifyCarry {
    /// An empty carry for `record_id`, kept `age` ago, with `spilled`
    /// files (removed when it goes).
    pub(super) fn for_test(
        record_id: &str,
        age: std::time::Duration,
        spilled: Vec<std::path::PathBuf>,
    ) -> VerifyCarry {
        VerifyCarry {
            record_id: record_id.into(),
            history: Vec::new(),
            last_results: Vec::new(),
            last_failed: false,
            checkpoint: None,
            baseline: Default::default(),
            integrity_open: Vec::new(),
            asked_integrity: false,
            rolled_back: Vec::new(),
            bonus_rounds: 0,
            groups: Vec::new(),
            last_triage: None,
            test_writer_files: Vec::new(),
            tests_changed: Vec::new(),
            worker_steps: Vec::new(),
            reviews_total: 0,
            review_findings: Vec::new(),
            review_optional: Vec::new(),
            seen: crate::file_guard::Seen::new("test"),
            ledger: Default::default(),
            rechecks: 0,
            round_commands: Vec::new(),
            befores: Default::default(),
            stored: std::time::Instant::now() - age,
            spilled: Spilled(
                spilled
                    .into_iter()
                    .map(|f| ("k".to_string(), f, super::subagent::Source::ViewAtRunStart))
                    .collect(),
            ),
        }
    }
}

/// A carry is kept this long at most.
pub const CARRY_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
/// Baseline contents a carry keeps in memory; the rest goes to disk.
pub const CARRY_MEMORY_BYTES: usize = 8 * 1024 * 1024;

/// Baseline entries kept on disk: path → (file, where the original is from).
/// Their files go when the carry does, taken up or dropped.
#[derive(Default)]
struct Spilled(Vec<(String, std::path::PathBuf, super::subagent::Source)>);

impl Spilled {
    /// Puts the spilled contents back in `baseline`. False: one couldn't be
    /// read back (its file is gone).
    fn restore(
        &self,
        baseline: &mut std::collections::HashMap<String, super::subagent::Original>,
    ) -> bool {
        let mut all = true;
        for (key, file, source) in &self.0 {
            match std::fs::read(file) {
                Ok(bytes) => {
                    baseline
                        .entry(key.clone())
                        .or_insert(super::subagent::Original::new(Some(bytes), *source));
                }
                Err(_) => all = false,
            }
        }
        all
    }
}

impl Drop for Spilled {
    fn drop(&mut self) {
        for (_, file, _) in &self.0 {
            let _ = std::fs::remove_file(file);
        }
        // And the chat's carry folder, once nothing else is in it (round 2).
        if let Some(dir) = self.0.first().and_then(|(_, f, _)| f.parent()) {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Moves the largest baseline contents to disk until what's left fits in
/// `CARRY_MEMORY_BYTES`. One that can't be written stays in memory.
fn spill(
    dialog_id: &str,
    baseline: &mut std::collections::HashMap<String, super::subagent::Original>,
) -> Spilled {
    let mut spilled = Spilled::default();
    let mut total: usize = baseline
        .values()
        .map(|o| o.bytes.as_ref().map_or(0, Vec::len))
        .sum();
    if total <= CARRY_MEMORY_BYTES {
        return spilled;
    }
    let Some(dir) = super::check_output::carry_dir(dialog_id) else {
        return spilled;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return spilled;
    }
    let mut by_size: Vec<(String, usize)> = baseline
        .iter()
        .filter_map(|(k, o)| Some((k.clone(), o.bytes.as_ref()?.len())))
        .collect();
    by_size.sort_by_key(|e| std::cmp::Reverse(e.1));
    for (key, size) in by_size {
        if total <= CARRY_MEMORY_BYTES {
            break;
        }
        let file = dir.join(format!("{}.bin", uuid::Uuid::new_v4()));
        let Some(original) = baseline.get(&key) else {
            continue;
        };
        let bytes = original.bytes.as_deref().unwrap_or_default();
        if std::fs::write(&file, bytes).is_ok() {
            let source = original.source;
            baseline.remove(&key);
            spilled.0.push((key, file, source));
            total -= size;
        }
    }
    spilled
}

/// A file's content hash as hex (P3.9).
fn hex_hash(bytes: &[u8]) -> String {
    crate::file_guard::hash(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The record's meta: `{ verification: … }`.
pub(super) fn record(
    status: &str,
    round: u32,
    checks: &[CheckResult],
    changed_files: &[String],
    reason: Option<&str>,
) -> Value {
    let mut v = json!({
        "status": status,
        "round": round,
        "maxRounds": MAX_VERIFY_ROUNDS,
        "checks": checks,
        "changedFiles": changed_files,
    });
    if let Some(reason) = reason {
        v["reason"] = json!(reason);
    }
    json!({ "verification": v })
}

/// A record with more facts: the round's progress, a rollback, flags.
pub(super) fn with(mut meta: Value, extra: Value) -> Value {
    if let (Some(v), Value::Object(extra)) = (meta.get_mut("verification"), extra) {
        if let Some(v) = v.as_object_mut() {
            v.extend(extra.into_iter().filter(|(_, x)| !x.is_null()));
        }
    }
    meta
}

/// Whether a check ran to its end and said how the code is: passed,
/// failed, or failed only as before the change (before.rs).
fn has_run(r: &CheckResult) -> bool {
    matches!(r.status, "passed" | "failed" | "preexisting")
}

/// Whether a check found nothing wrong with the change: it passed, or
/// failed only as it does on the code before the change.
fn clear(r: &CheckResult) -> bool {
    matches!(r.status, "passed" | "preexisting")
}

/// D27: whether a check's `command` checks a workspace chat that changed
/// the folders `changed` (None: a folder chat, whose checks all do): run in
/// a changed folder (`cd api && …`, `cd api/ui && …`), or in none (a plan's
/// own commands).
pub fn checks_changed(command: &str, changed: Option<&std::collections::BTreeSet<String>>) -> bool {
    let folder = super::project::folder_of(command);
    let name = folder.split('/').next().unwrap_or_default();
    name.is_empty() || changed.is_none_or(|names| names.contains(name))
}

/// This round's failures, for comparing rounds (repair.rs).
fn round_failures(results: &[CheckResult]) -> RoundFailures {
    RoundFailures {
        keys: results
            .iter()
            .flat_map(|r| r.failures.iter().flatten().cloned())
            .collect(),
        ran: results
            .iter()
            .filter(|r| has_run(r))
            .map(|r| r.kind.clone())
            .collect(),
        tests_ran: results
            .iter()
            .filter_map(|r| r.tests_ran)
            .reduce(|a, b| a + b),
        own_tests: None,
    }
}

/// What the project changed in `files` (each with its diff, in the chat's
/// view at `root`) while the chat worked, for the chat's report: not a
/// restricted file's lines, which the agent may not read.
fn project_changes(
    files: &[(String, String)],
    root: &str,
    restricted: &crate::restricted_files::RestrictedFiles,
) -> String {
    let read = crate::restricted_files::Op::Read;
    let restricted = |path: &str| {
        let abs = super::tools::to_absolute_path(path, root);
        restricted.blocks_file(path, std::path::Path::new(&abs), read)
    };
    files
        .iter()
        .filter(|(path, diff)| !diff.is_empty() && !restricted(path))
        .map(|(path, diff)| {
            format!("\nWhat the project changed in `{path}`:\n```diff\n{diff}\n```\n")
        })
        .collect()
}

/// What the agent hears when its fix broke something and was rolled back.
fn rollback_note(rb: &repair::Rollback) -> String {
    let broke = repair::key_list(&rb.broke, 8);
    let mut note = format!(
        "Your last fix broke something that didn't fail before:\n{broke}\nSo it was rolled back"
    );
    if rb.restored.is_empty() {
        note.push_str(", but no file could be put back");
    } else {
        note.push_str(&format!(
            ": {} {} as before that fix",
            rb.restored.join(", "),
            if rb.restored.len() == 1 { "is" } else { "are" }
        ));
    }
    note.push('.');
    // P3.5: what commands changed isn't known, so it isn't undone.
    if !rb.commands.is_empty() {
        let shown: Vec<String> = rb
            .commands
            .iter()
            .take(5)
            .map(|c| format!("`{c}`"))
            .collect();
        note.push_str(&format!(
            " Commands ran in that fix ({}{}); what they changed wasn't undone, and the checks run again when you finish.",
            shown.join(", "),
            if rb.commands.len() > 5 { ", …" } else { "" }
        ));
    }
    if rb.skipped.is_empty() {
        note.push_str(
            " The failures below are the ones from before that fix. Read the files again before editing them, and try a different fix.",
        );
    } else {
        note.push_str(&format!(
            " Left as they are, because they changed outside this chat meanwhile: {}. So the code isn't quite as it was \
             before that fix: the failures below are from then, and the checks run again when you finish. Read the files \
             again before editing them, and try a different fix.",
            rb.skipped.join(", ")
        ));
    }
    note
}

/// "Errors point at …", for the report: changed files first, the rest flagged.
fn describe_error_files(files: &ErrorFiles) -> Option<String> {
    let list = |paths: &[String]| {
        if paths.len() > 6 {
            format!("{} and {} more", paths[..6].join(", "), paths.len() - 6)
        } else {
            paths.join(", ")
        }
    };
    let mut parts = Vec::new();
    if !files.changed.is_empty() {
        parts.push(format!(
            "Errors point at files you changed: {}.",
            list(&files.changed)
        ));
    }
    if !files.other.is_empty() {
        parts.push(if files.changed.is_empty() {
            format!(
                "Errors point only at files you didn't change: {} (they may have failed before your change, or be affected by it).",
                list(&files.other)
            )
        } else {
            format!("Also at files you didn't change: {}.", list(&files.other))
        });
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The message the agent gets when checks fail (sent as a user-role message).
/// `limit`: the rounds there are; None in Autopilot, whose rounds end after
/// two without progress (V03).
pub fn failure_report(
    results: &[CheckResult],
    changed_files: &[String],
    round: u32,
    limit: Option<u32>,
    notes: &[String],
) -> String {
    let failed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "failed").collect();
    let passed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "passed").collect();
    let fail = if failed.len() == 1 {
        "a check fails".to_string()
    } else {
        format!("{} checks fail", failed.len())
    };
    let attempt = match limit {
        Some(max) => format!("attempt {round} of {max}"),
        None => format!(
            "attempt {round}; the checks stop after {STALLED_ROUNDS} attempts in a row without progress"
        ),
    };
    let mut lines = vec![
        "[Automatic check by the IDE, not written by the user]".to_string(),
        format!("Your changes are not done yet: {fail} ({attempt})."),
    ];
    for note in notes {
        lines.push(String::new());
        lines.push(note.clone());
    }
    lines.extend([
        String::new(),
        format!(
            "Files you changed in this task: {}",
            if changed_files.is_empty() {
                "(none recorded)".to_string()
            } else {
                changed_files.join(", ")
            }
        ),
    ]);
    let named = |r: &&CheckResult| format!("{} (`{}`)", check_label(&r.kind), r.command);
    if !passed.is_empty() {
        let list: Vec<String> = passed.iter().map(named).collect();
        lines.push(format!("Passed: {}", list.join(", ")));
    }
    // before.rs: what fails only as on the code before the change.
    let as_before: Vec<String> = results
        .iter()
        .filter(|r| r.status == "preexisting")
        .map(|r| named(&r))
        .collect();
    if !as_before.is_empty() {
        lines.push(format!(
            "Failing only as on the code before your change, so not yours to fix: {}",
            as_before.join(", ")
        ));
    }
    for r in &failed {
        let how = if r.timed_out == Some(true) {
            "timed out".to_string()
        } else {
            match r.exit_code.flatten() {
                Some(code) => format!("exit code {code}"),
                None => "failed".into(),
            }
        };
        lines.push(String::new());
        lines.push(format!(
            "## {} failed: `{}` ({how})",
            check_label(&r.kind),
            r.command
        ));
        if let Some(where_) = r.error_files.as_ref().and_then(describe_error_files) {
            lines.push(where_);
        }
        if let (Some(_), Some(own)) = (&r.preexisting, &r.failures) {
            lines.push(format!(
                "Some of its failures are there on the code before your change too: they aren't yours, so leave them. Yours:\n{}",
                repair::key_list(own, 20)
            ));
        }
        lines.push(String::new());
        lines.push(r.output.clone().unwrap_or_default());
        if let Some(path) = &r.saved_output {
            lines.push(String::new());
            lines.push(format!(
                "That's the relevant part of the output. The whole of it is saved as `{path}`: read it with read and start_line/end_line if you need more."
            ));
        }
    }
    let skipped: Vec<String> = results
        .iter()
        .filter(|r| r.status == "skipped")
        .map(|r| format!("`{}` ({})", r.command, r.reason.as_deref().unwrap_or("")))
        .collect();
    if !skipped.is_empty() {
        lines.push(String::new());
        lines.push(format!("Not run: {}", skipped.join(", ")));
    }
    // V06: a whole suite that timed out, its changed files' tests run instead.
    let slow: Vec<String> = results
        .iter()
        .filter(|r| r.status == "slow")
        .map(|r| format!("`{}` ({})", r.command, r.reason.as_deref().unwrap_or("")))
        .collect();
    if !slow.is_empty() {
        lines.push(String::new());
        lines.push(format!("Too slow: {}", slow.join(", ")));
    }
    lines.push(String::new());
    lines.push(
        "Find the cause in your changes and fix it; the checks run again when you finish. \
         If a failure has nothing to do with your changes (it fails on the original code too), \
         don't change unrelated code: finish and say so."
            .into(),
    );
    lines.join("\n")
}

impl Run {
    /// The checks that run: on, and with a command. A workspace chat's are
    /// those of the folders it changed (`cd api && …`), and those in no
    /// folder (a plan's own commands, V02), which run whatever changed
    /// (D27): the prompt lists every folder's, so the agent can run the
    /// others itself.
    pub(super) fn enabled_checks(&self) -> Vec<ProjectCheck> {
        let changed = self.changed_folders();
        self.checks
            .iter()
            .filter(|c| c.enabled && !c.command.trim().is_empty())
            .filter(|c| checks_changed(&c.command, changed.as_ref()))
            .cloned()
            .collect()
    }

    /// The names of the folders a workspace chat changed: its changed
    /// files' first parts, earlier runs' and commands' too. None for a
    /// folder chat, whose checks all run.
    fn changed_folders(&self) -> Option<std::collections::BTreeSet<String>> {
        let folders = self.folders.as_ref().filter(|f| f.single().is_none())?;
        let mut keys = self.touched_keys();
        // On a daemon, what earlier runs changed (and what a merge did) is
        // what its worktree changed from the landing base (4.7, 4.11).
        if let Some(worktree) = folders.root() {
            keys.extend(super::server::unlanded(worktree));
        }
        let names = keys
            .iter()
            .filter_map(|key| folders.split(key))
            .map(|(folder, _)| folder.name.clone());
        Some(names.collect())
    }

    /// Appends a verification record; returns its id.
    async fn add_record(&self, meta: Value) -> Result<String, String> {
        let mut m = new_message("assistant", "");
        m.meta = Some(self.with_max_rounds(meta));
        let id = m.id.clone();
        self.append(vec![m]).await?;
        Ok(id)
    }

    /// Runs the gate at the end of a run. "continue" means the failures were
    /// sent to the model and the run goes on; anything else is the outcome.
    /// `approved`: the user started the checks (`record_id` is the record
    /// that waited for that, updated in place).
    pub(super) async fn verify_at_end(
        &mut self,
        approved: bool,
        record_id: Option<String>,
    ) -> Result<String, String> {
        let mut record_id = record_id;
        loop {
            let outcome = self.verify_round(approved, record_id.take()).await?;
            // R04: the workers fixed every cause: the checks run again now.
            if outcome != "repaired" {
                return Ok(outcome);
            }
            // P5 (steering-plan.md): a message the user sent meanwhile is
            // read first, and the checks run again at the next end.
            if self.read_waiting("repair").await? {
                return Ok("continue".into());
            }
            // Between rounds is a safe point too: a move or a drain asked
            // meanwhile pauses the run here (6.8, R02), and the checks run
            // again where it continues.
            if let Some(pause) = self.pause.get() {
                return self.pause_for(pause).await;
            }
        }
    }

    /// The fix-round state, moved out as the run stops for approval.
    fn take_verify_state(&mut self, record_id: String) -> VerifyCarry {
        use std::mem::take;
        VerifyCarry {
            record_id,
            history: take(&mut self.verify_history),
            last_results: take(&mut self.last_verify_results),
            last_failed: self.last_verify_failed,
            checkpoint: self.checkpoint.take(),
            baseline: take(&mut self.baseline),
            integrity_open: take(&mut self.integrity_open),
            asked_integrity: self.asked_integrity,
            rolled_back: take(&mut self.rolled_back_attempts),
            bonus_rounds: self.bonus_rounds,
            groups: take(&mut self.groups),
            last_triage: self.last_triage.take(),
            test_writer_files: take(&mut self.test_writer_files),
            tests_changed: take(&mut self.tests_changed),
            worker_steps: take(&mut self.worker_steps),
            reviews_total: self.reviews_total,
            review_findings: take(&mut self.review_findings),
            review_optional: take(&mut self.review_optional),
            seen: self.seen.clone(),
            ledger: take(&mut self.ledger),
            rechecks: self.rechecks,
            round_commands: take(&mut self.round_commands),
            befores: take(&mut self.befores),
            stored: std::time::Instant::now(),
            spilled: Spilled::default(),
        }
    }

    /// The run that starts the checks takes up where the last one stopped.
    /// None found (the app restarted, the chat was discarded, it expired):
    /// the record and the agent are told the checks start afresh (P1.7).
    pub(super) fn restore_verify_state(&mut self, record_id: &str) {
        let Some(mut c) = self.core.agents.take_carry(&self.dialog_id, record_id) else {
            self.carry_lost = Some(record_id.to_string());
            return;
        };
        // Spilled contents come back; one that can't is left out (its
        // file's original isn't known, like a command's change), and the
        // integrity check can't be complete (round 2, P3.9).
        if !c.spilled.restore(&mut c.baseline) {
            self.integrity_unchecked = true;
        }
        self.verify_history = c.history;
        self.last_verify_results = c.last_results;
        self.last_verify_failed = c.last_failed;
        self.checkpoint = c.checkpoint;
        // What this run's writes find first stays theirs.
        for (path, original) in c.baseline {
            self.baseline.entry(path).or_insert(original);
        }
        self.integrity_open = c.integrity_open;
        self.asked_integrity = c.asked_integrity;
        self.rolled_back_attempts = c.rolled_back;
        self.bonus_rounds = c.bonus_rounds;
        self.groups = c.groups;
        self.last_triage = c.last_triage;
        self.test_writer_files = c.test_writer_files;
        self.tests_changed = c.tests_changed;
        self.worker_steps = c.worker_steps;
        self.reviews_total = c.reviews_total;
        self.review_findings = c.review_findings;
        self.review_optional = c.review_optional;
        self.seen = c.seen;
        self.ledger = c.ledger;
        self.rechecks = c.rechecks;
        self.round_commands = c.round_commands;
        self.befores = c.befores;
    }

    /// P3.9: the carried state was lost (a restart): what each file was
    /// before the run is rebuilt from the record's hashes, from a copy that
    /// still matches (the project's copy in an isolated chat, the file as
    /// it is). What can't be leaves the integrity check incomplete. The
    /// record's round limit stays too.
    pub(super) async fn rebuild_baseline(&mut self, record_id: &str) {
        let waiting = self.recent().await.ok().and_then(|recent| {
            recent
                .into_iter()
                .find(|m| m.id == record_id)?
                .meta?
                .get("verification")
                .cloned()
        });
        // Round 2: the rounds a review after green granted are the
        // record's too, so they outlast the carry.
        if let Some(max) = waiting.as_ref().and_then(|v| v["maxRounds"].as_u64()) {
            self.bonus_rounds = self
                .bonus_rounds
                .max((max as u32).saturating_sub(MAX_VERIFY_ROUNDS));
        }
        let entries = waiting
            .as_ref()
            .and_then(|v| v.get("baselineHashes")?.as_array().cloned());
        let Some(entries) = entries else {
            self.integrity_unchecked = true;
            return;
        };
        let root = self.root.clone().unwrap_or_default();
        let isolated = self.isolated();
        for e in entries {
            let Some(path) = e["path"].as_str() else {
                continue;
            };
            let rebuilt = match e["hash"].as_str() {
                None => Some(super::subagent::Original::new(
                    None,
                    super::subagent::Source::ViewAtRunStart,
                )),
                Some(want) => {
                    let copies = [
                        self.project_file(path).filter(|_| isolated),
                        Some(std::path::PathBuf::from(super::tools::to_absolute_path(
                            path, &root,
                        ))),
                    ];
                    copies
                        .into_iter()
                        .flatten()
                        .find_map(|f| std::fs::read(f).ok().filter(|b| hex_hash(b) == want))
                        .map(|b| {
                            super::subagent::Original::new(
                                Some(b),
                                super::subagent::Source::ViewAtRunStart,
                            )
                        })
                }
            };
            match rebuilt {
                Some(o) => {
                    self.baseline.entry(path.to_string()).or_insert(o);
                }
                None => self.integrity_unchecked = true,
            }
        }
    }

    /// P3.10: a record's round limit is the run's (bonus rounds counted).
    pub(super) fn with_max_rounds(&self, mut meta: Value) -> Value {
        if let Some(v) = meta.get_mut("verification").and_then(|v| v.as_object_mut()) {
            v.insert("maxRounds".into(), json!(self.max_rounds()));
            // V02: the checks are the plan's validation commands.
            if self.checks_from_plan {
                v.insert("fromPlan".into(), json!(true));
            }
            // V03: the rounds end on progress, not at the count.
            if self.in_autopilot() {
                v.insert("byProgress".into(), json!(true));
            }
        }
        meta
    }

    /// The round limit: MAX_VERIFY_ROUNDS, and one more for each review
    /// after green whose findings went back (R08). In Autopilot, whose
    /// rounds end after two without progress, AUTOPILOT_MAX_ROUNDS.
    fn max_rounds(&self) -> u32 {
        let base = if self.in_autopilot() {
            AUTOPILOT_MAX_ROUNDS
        } else {
            MAX_VERIFY_ROUNDS
        };
        base + self.bonus_rounds
    }

    /// The limit the agent is told of: none in Autopilot (V03).
    fn round_limit(&self) -> Option<u32> {
        (!self.in_autopilot()).then(|| self.max_rounds())
    }

    /// One round of the gate; "repaired": the workers fixed this round's
    /// failures, and the checks should run again at once.
    async fn verify_round(
        &mut self,
        approved: bool,
        record_id: Option<String>,
    ) -> Result<String, String> {
        let p = self.config().permissions;
        let Some(root) = self.root.clone() else {
            return Ok("done".into());
        };
        if !p.verify_changes {
            return Ok("done".into());
        }
        let changed = self.changed_files.clone();

        if !self.writes_since_verify {
            // Asked about changes that weaken the checks, and it kept them:
            // not verified; the user reviews them (R05).
            if !self.integrity_open.is_empty() && !self.last_verify_failed {
                let flags = self.integrity_open.clone();
                self.add_record(with(
                    record(
                        "flagged",
                        self.verify_rounds,
                        &self.last_verify_results,
                        &changed,
                        None,
                    ),
                    json!({ "integrity": flags }),
                ))
                .await?;
                return Ok("verify-flagged".into());
            }
            // Failures were sent back, and the model finished without touching
            // anything: it decided not to (or couldn't). Say so; don't loop.
            if self.last_verify_failed {
                self.add_record(record(
                    "unresolved",
                    self.verify_rounds,
                    &[],
                    &changed,
                    None,
                ))
                .await?;
                return Ok("verify-unresolved".into());
            }
            // P3.6: the last round passed and nothing changed since (the
            // agent answered a review after green without a fix): verified.
            let passed = self.verify_rounds > 0
                && !self.last_verify_results.is_empty()
                && self
                    .last_verify_results
                    .iter()
                    .all(|r| r.status != "failed")
                && self.last_verify_results.iter().any(clear);
            return Ok(match (passed, self.integrity_unchecked) {
                (true, true) => "verify-unchecked",
                (true, false) => "verified",
                _ => "done",
            }
            .into());
        }

        let checks = self.enabled_checks();
        if checks.is_empty() {
            let reason = match self.changed_folders() {
                Some(_) => "no checks are set up for the folders it changed",
                None => "no checks are set up for this project",
            };
            self.add_record(record(
                "unverified",
                self.verify_rounds,
                &[],
                &changed,
                Some(reason),
            ))
            .await?;
            self.writes_since_verify = false;
            return Ok("unverified".into());
        }

        // Command approval is on: the gate's commands need the user's go
        // too. Not in Autopilot, where nobody gives it: they're the IDE's
        // commands, which the user set up, not the model's (D7).
        if p.command_approval.required && !approved && !self.in_autopilot() {
            let pending: Vec<CheckResult> = checks.iter().map(CheckResult::pending).collect();
            // P3.9: what each file was, by hash, kept with the record: after
            // a restart the checks rebuild what they can from it.
            let hashes: Vec<Value> = self
                .baseline
                .iter()
                .map(|(path, o)| json!({ "path": path, "hash": o.bytes.as_deref().map(hex_hash) }))
                .collect();
            let record_id = self
                .add_record(with(
                    record(
                        "needs-approval",
                        self.verify_rounds,
                        &pending,
                        &changed,
                        None,
                    ),
                    json!({ "baselineHashes": hashes }),
                ))
                .await?;
            // The run ends here: what its fix rounds know waits for the
            // run that starts the checks.
            if self.sub().is_none() {
                let mut carry = self.take_verify_state(record_id);
                carry.spilled = spill(&self.dialog_id, &mut carry.baseline);
                self.core.agents.put_carry(&self.dialog_id, carry);
            }
            return Ok("verify-needs-approval".into());
        }

        self.verify_rounds += 1;
        self.writes_since_verify = false;
        // The checks run: the next change review isn't "in a row".
        self.reviews_in_row = 0;
        let round = self.verify_rounds;
        let record_id = match record_id {
            Some(id) => {
                self.update_meta(&id, record("running", round, &[], &changed, None))
                    .await?;
                id
            }
            None => {
                self.add_record(record("running", round, &[], &changed, None))
                    .await?
            }
        };

        self.clean_earlier_output();
        let results = self
            .run_checks(
                &root,
                &checks,
                &p,
                &changed,
                &record_id,
                round,
                &format!("round-{round}"),
            )
            .await?;

        let failed = results.iter().any(|r| r.status == "failed");
        let ran = results.iter().any(has_run);
        // V03 (D12): a check that failed on its environment, not the code (a
        // program or a package that isn't installed): in Autopilot, once a
        // run, the round isn't counted and Prepare runs again first.
        let autopilot = self.in_autopilot();
        if failed && autopilot && !self.prepared_again && !self.stopped() {
            if let Some(check) = results.iter().find(|r| r.environment.is_some()) {
                let failure = check.environment.clone().unwrap_or_default();
                let command = check.command.clone();
                self.prepared_again = true;
                return self
                    .environment_round(
                        &root, &record_id, round, &results, &changed, failure, command,
                    )
                    .await;
            }
        }
        // R01: how this round compares with the earlier ones.
        let mut now = round_failures(&results);
        now.own_tests = Some(self.own_test_count());
        let mut judgement = repair::judge(&self.verify_history, &now);
        // T04: the adversarial tester's tests, run for the first time: what
        // fails is what they found, not a fix that broke something.
        if self.breaker_round.is_some() {
            judgement = judgement.as_first();
        }
        // R05: asked to put back a weakened test, the agent did, and it
        // fails: that's what was asked for, not a fix that broke something.
        if std::mem::take(&mut self.asked_integrity) {
            judgement = judgement.without_regression_in(&self.integrity_open);
        }
        // P3.2: an error in what the round wrote is a slip, not a regression.
        // This round's: the checkpoint's files that differ from it now (it
        // also holds what earlier rounds changed).
        let roots = self.output_roots();
        let mut written: Vec<String> = Vec::new();
        if let Some(cp) = &self.checkpoint {
            written = cp
                .files
                .iter()
                .filter(|(key, before)| {
                    std::fs::read(super::tools::to_absolute_path(key, &root)).ok() != **before
                })
                .map(|(key, _)| key.clone())
                .collect();
            judgement = judgement.keeping_slips_in(&written, &roots);
        }
        // V05: the checks of what a partial rollback kept (below): the same
        // round's, counted once.
        let after_partial = std::mem::take(&mut self.after_partial_rollback);
        // V03 (D12): Autopilot's rounds end after STALLED_ROUNDS in a row
        // without progress (a rolled-back regression is none), or at the
        // budget, not at a count. A round that only shows failures the last
        // one hid is progress: what hid them was fixed.
        if autopilot {
            match judgement.verdict {
                _ if !failed => self.stalled_rounds = 0,
                Progress::Progress => self.stalled_rounds = 0,
                _ if after_partial => {}
                Progress::NoProgress | Progress::Regression => self.stalled_rounds += 1,
                Progress::First => {}
            }
        }
        let stalled = autopilot && self.stalled_rounds >= STALLED_ROUNDS;
        // R05: what weakens the checks, and fewer tests than last round.
        let mut flags = if ran {
            self.integrity_flags()
        } else {
            Vec::new()
        };
        // P3.7: fewer tests than an earlier round, the run's own test files
        // aside (round 2: a test file a command deleted counts too, and a
        // run whose first write is a test file still has a reference).
        if let Some(flag) = repair::fewer_tests_flag(&self.verify_history, &now) {
            flags.push(flag);
        }
        // Nothing weakens the checks any more: an earlier question is settled.
        if ran && flags.is_empty() {
            self.integrity_open.clear();
        }
        let tests_before = self.verify_history.last().and_then(|r| r.tests_ran);
        let max_rounds = self.max_rounds();
        let extra = |rolled_back: Option<&repair::Rollback>| {
            json!({
                "maxRounds": max_rounds,
                "progress": (judgement.verdict != Progress::First).then_some(&judgement),
                "failureCount": now.keys.len(),
                "testsRun": now.tests_ran,
                "testsRunBefore": tests_before,
                "integrity": (!flags.is_empty()).then_some(&flags),
                "rolledBack": rolled_back,
            })
        };

        // Why a regression can't be undone, if it can't (no rollback below).
        let cant_undo = match &self.checkpoint {
            Some(cp) if cp.too_big => "the files it changed were too large to keep a copy of",
            Some(_) => "",
            None => "there's no copy of the files from before it",
        };
        // R02: a fix that broke what didn't fail before is undone; the
        // agent hears what it broke and gets the earlier failures again.
        // V05: only the files behind what it broke go back, when its
        // failures name them and a round is left to check the rest of the
        // fix; when that round still finds it broken, everything does.
        if failed && judgement.verdict == Progress::Regression && !self.stopped() {
            let only = (!after_partial && round < max_rounds && !stalled)
                .then(|| super::causes::files_behind(&judgement.broke, &written, &roots))
                .flatten();
            if let Some(rb) = self
                .roll_back(judgement.broke.clone(), only.as_deref())
                .await
            {
                self.rolled_back_attempts.push(rb.clone());
                if !rb.kept.is_empty() {
                    // The rest of the fix stays: the checks run again on it
                    // now. Theirs is this round's result: it counts once.
                    let changed = self.changed_files.clone();
                    let meta = with(
                        record("rolled-back", round, &results, &changed, None),
                        extra(Some(&rb)),
                    );
                    self.update_meta(&record_id, meta).await?;
                    self.log_round(round, "rolled-back", &results, &judgement, &flags);
                    self.after_partial_rollback = true;
                    self.last_verify_failed = true;
                    self.writes_since_verify = true;
                    self.verify_rounds = self.verify_rounds.saturating_sub(1);
                    return Ok("repaired".into());
                }
                let status = if round >= max_rounds || stalled {
                    "gave-up"
                } else {
                    "rolled-back"
                };
                let changed = self.changed_files.clone();
                let mut meta = with(
                    record(status, round, &results, &changed, None),
                    extra(Some(&rb)),
                );
                if status == "gave-up" {
                    // What's left: the failures of the round it went back to.
                    let left: Vec<String> = self
                        .verify_history
                        .last()
                        .map(|r| r.keys.iter().cloned().collect())
                        .unwrap_or_default();
                    meta = with(meta, self.gave_up_facts(&left));
                    if stalled {
                        meta = with(meta, json!({ "noProgress": true }));
                    }
                }
                self.update_meta(&record_id, meta).await?;
                self.log_round(round, status, &results, &judgement, &flags);
                self.last_verify_failed = true;
                if status == "gave-up" {
                    return Ok("verify-gave-up".into());
                }
                // The code is back where the last round left it: its
                // failures stand, and the round's history stays as it was.
                let earlier = if self.last_verify_results.is_empty() {
                    results.clone()
                } else {
                    self.last_verify_results.clone()
                };
                let report = failure_report(
                    &earlier,
                    &changed,
                    round,
                    self.round_limit(),
                    &[rollback_note(&rb)],
                );
                self.add_harness(report, "verification").await?;
                // Not all of it went back: the code is new, check it again.
                self.writes_since_verify = !rb.skipped.is_empty() || !rb.commands.is_empty();
                self.take_checkpoint();
                return Ok("continue".into());
            }
        }
        self.verify_history.push(now.clone());
        let previous = std::mem::replace(&mut self.last_verify_results, results.clone());

        let status = if self.stopped() {
            "stopped"
        } else if !ran {
            "unverified"
        } else if !failed && !flags.is_empty() && !self.integrity_open.is_empty() {
            // Asked about them once already, and they're still there.
            "flagged"
        } else if !failed {
            "passed"
        } else if round >= max_rounds || stalled {
            "gave-up"
        } else {
            "failed"
        };
        let reason = (status == "unverified").then_some("none of the checks were allowed to run");
        let mut meta = with(
            record(status, round, &results, &changed, reason),
            extra(None),
        );
        if status == "gave-up" {
            // R06: what's left, and what was tried and undone.
            let left: Vec<String> = now.keys.iter().cloned().collect();
            meta = with(meta, self.gave_up_facts(&left));
            if stalled {
                meta = with(meta, json!({ "noProgress": true }));
            }
        }
        // A regression that wasn't rolled back above: why not.
        let not_rolled_back = (failed && judgement.verdict == Progress::Regression).then_some(
            if cant_undo.is_empty() {
                "it couldn't be put back"
            } else {
                cant_undo
            },
        );
        if let Some(why) = not_rolled_back {
            meta = with(meta, json!({ "notRolledBack": why }));
        }
        self.update_meta(&record_id, meta.clone()).await?;
        self.log_round(round, status, &results, &judgement, &flags);
        // T04: the round that ran the adversarial tester's tests: what they
        // found, for its record and the lead.
        let breaker_note = self.breaker_round_judged(failed).await?;

        self.last_verify_failed = failed;
        if status == "passed" && !flags.is_empty() {
            // R05: the checks pass, but maybe only because the tests or what
            // the checks run were weakened. Asked once; kept: the run isn't
            // verified, and the user decides.
            self.integrity_open = flags.clone();
            self.asked_integrity = true;
            self.take_checkpoint();
            self.add_harness(repair::integrity_note(&flags, true), "integrity")
                .await?;
            return Ok("continue".into());
        }
        // R08: green after a fix round: a fresh review of the whole change.
        let repaired = self.verify_history.len() > 1
            && self.verify_history[..self.verify_history.len() - 1]
                .iter()
                .any(|r| !r.keys.is_empty());
        if status == "passed"
            && repaired
            && self.sub().is_none()
            && self.review_after_green(&record_id, &meta).await?
        {
            // P3.6: the fix for the findings is a round like any other: one
            // that breaks what passes now is rolled back.
            self.take_checkpoint();
            return Ok("continue".into());
        }
        // S06: that review left a finding open, and it was the last.
        if self.review_blocked {
            self.checkpoint = None;
            return Ok("review-open".into());
        }
        // S07: green on a change with code and no test: the test writer
        // writes its tests, and the checks run them now.
        if status == "passed" && self.tests_for_an_untested_change().await? {
            self.take_checkpoint();
            return Ok("repaired".into());
        }
        // T04: green in a Thorough task: the adversarial tester's tests,
        // which the checks run now; what they find is the next round's.
        if status == "passed" && self.break_after_green().await? {
            self.take_checkpoint();
            return Ok("repaired".into());
        }
        // S08: green: the acceptance check, by the plan's criteria. Its
        // unsatisfied criteria get one more round, like any other.
        if status == "passed" && self.accept_after_green().await? {
            self.take_checkpoint();
            return Ok("continue".into());
        }
        if self.acceptance_unmet {
            self.checkpoint = None;
            return Ok("unmet".into());
        }
        // before.rs: nothing the change did fails, but some checks fail as
        // they do on the code before it. Fixing them may be the task ("fix
        // the tests"), which only the agent knows: it's told, and goes on
        // or finishes; with nothing changed then, the run is verified as it
        // is (above). Not on the last round: there's none left to go on in.
        if status == "passed" && round < max_rounds {
            if let Some(note) = super::before::note(&results) {
                let told = with(meta.clone(), json!({ "preexistingTold": true }));
                self.update_meta(&record_id, told).await?;
                self.add_harness(note, "preexisting").await?;
                self.take_checkpoint();
                return Ok("continue".into());
            }
        }
        if status != "failed" {
            self.checkpoint = None;
            return Ok(match status {
                // P3.9: what the files were before was lost (a restart):
                // passed, but whether tests were weakened isn't known.
                "passed" if self.integrity_unchecked => "verify-unchecked".into(),
                "passed" => "verified".into(),
                _ => format!("verify-{status}"),
            });
        }

        // Back to the model. User-role, so every provider accepts it after an
        // assistant reply; marked as the IDE's, so it's never shown as the
        // user's message or replaced by the request's editor context.
        let mut notes: Vec<String> = Vec::new();
        if let Some(note) = breaker_note {
            notes.push(note);
        }
        if self.carry_lost.is_some() && !std::mem::replace(&mut self.carry_lost_told, true) {
            notes.push(
                "What the earlier rounds of checks knew (their failures, what each file was before) didn't carry over to these, so they start afresh: nothing is compared with or rolled back to before them.".into(),
            );
        }
        if let Some(note) = repair::progress_note(&judgement, now.keys.len()) {
            notes.push(note);
        }
        if let Some(why) = not_rolled_back {
            // No checkpoint to go back to (too large, or the first round).
            notes.push(format!(
                "Your last fix broke something that didn't fail before:\n{}\nIt couldn't be undone ({why}): fix what it broke as the code is now.",
                repair::key_list(&judgement.broke, 8)
            ));
        }
        if !flags.is_empty() {
            notes.push(repair::integrity_note(&flags, false));
        }
        // V03: still the environment, the dependencies installed once: the
        // agent may need a package the project doesn't have.
        if let Some(line) = results.iter().find_map(|r| r.environment.as_deref()) {
            if self.prepared_again {
                notes.push(format!(
                    "A check still fails on the environment (`{line}`), and the IDE already installed the project's dependencies. \
                     If your change needs a package the project doesn't have, add it to the project's manifest and lockfile; \
                     otherwise it's the environment, not your change: don't change unrelated code for it."
                ));
            }
        }
        // C03: a large or repeated failure goes to triage first; the agent
        // gets its diagnosis in place of that check's output.
        let base = meta.clone();
        let triage = self
            .triage_failure(&record_id, round, &results, &previous, |t| {
                with(base.clone(), json!({ "triage": t }))
            })
            .await?;
        let mut sent = results.clone();
        if let Some(t) = &triage {
            if let Some(section) = &t.section {
                sent[t.index].output = Some(section.clone());
                sent[t.index].saved_output = None;
            }
        }
        if self.stopped() {
            return Ok("verify-stopped".into());
        }
        // R03/R04: after a parallel group, the causes go to their owners
        // first; the lead hears what's left.
        let with_triage = match &triage {
            Some(t) => with(base.clone(), json!({ "triage": t.record })),
            None => base.clone(),
        };
        let triaged = triage.as_ref().is_some_and(|t| t.section.is_some());
        let mut fixers_ran = false;
        match self
            .repair_by_owners(
                &record_id,
                round,
                &results,
                &judgement,
                triaged,
                &with_triage,
            )
            .await?
        {
            super::repair_run::Repair::None => {}
            super::repair_run::Repair::Recheck => return Ok("repaired".into()),
            super::repair_run::Repair::Lead { note, fixed } => {
                notes.push(note);
                fixers_ran = fixed;
            }
        }
        if self.stopped() {
            return Ok("verify-stopped".into());
        }
        let mut report = failure_report(&sent, &changed, round, self.round_limit(), &notes);
        if let Some(files) = &self.merged_from_project {
            let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
            let root = self.root.clone().unwrap_or_default();
            let diffs = project_changes(files, &root, &self.restricted);
            report = report.replacen(
                "\n",
                &format!(
                    "\nThe project changed meanwhile ({}): those changes are now merged into your files, and the checks ran on the result. Fix what fails so your changes work with the project's.\n{diffs}",
                    names.join(", ")
                ),
                1,
            );
        }
        self.add_harness(report, "verification").await?;
        // R02: what the next fix round can be rolled back to (taken before
        // the workers' fixes, when they ran).
        if !fixers_ran {
            self.take_checkpoint();
        }
        Ok("continue".into())
    }

    /// R06: a gave-up record's facts: the failures left, the fix attempts
    /// rolled back, and the last triage's diagnosis.
    fn gave_up_facts(&self, remaining: &[String]) -> Value {
        json!({
            "remaining": remaining,
            "attempts": (!self.rolled_back_attempts.is_empty()).then_some(&self.rolled_back_attempts),
            "triageSummary": self.last_triage.as_ref().map(|t| t.root_cause.clone()),
        })
    }

    /// R08: the change reviewer, once the checks pass after a fix round,
    /// within the run's review budget. True: findings went to the
    /// agent, and one more fix and check round is allowed.
    async fn review_after_green(&mut self, record_id: &str, meta: &Value) -> Result<bool, String> {
        let config = self.config().subagents;
        if config.kind(super::subagent::Kind::Reviewer).is_none() || self.plan_mode {
            return Ok(false);
        }
        let cap = self.change_review_cap();
        if self.reviews_total >= cap {
            self.update_meta(
                record_id,
                with(
                    meta.clone(),
                    json!({ "reviewAfterGreen": {
                        "status": "skipped",
                        "reason": format!("the run's {cap} change reviews are used up"),
                    } }),
                ),
            )
            .await?;
            return Ok(false);
        }
        // The fixes are new to the reviewer, whatever came before.
        self.writes_since_review = true;
        self.review_outcome = None;
        let sent = self.review_change_as(true).await?;
        let outcome = self.review_outcome.take();
        // Optional findings alone went to the agent: nothing serious, and
        // its fix, if it makes one, still gets a check round.
        let serious = sent && !matches!(&outcome, Some((status, _)) if status == "clean");
        // "done" only when a reviewer read the change and reported.
        let line = match outcome {
            _ if serious => json!({ "status": "findings" }),
            Some((status, _)) if status == "clean" || status == "findings" => {
                json!({ "status": "done" })
            }
            Some((status, _)) if status == "stopped" => {
                json!({ "status": "skipped", "reason": "the run was stopped" })
            }
            Some((_, Some(reason))) => json!({ "status": "skipped", "reason": reason }),
            _ if self.stopped() => json!({ "status": "skipped", "reason": "the run was stopped" }),
            _ => json!({ "status": "skipped", "reason": "the reviewer didn't run" }),
        };
        self.update_meta(
            record_id,
            with(meta.clone(), json!({ "reviewAfterGreen": line })),
        )
        .await?;
        if sent {
            // The fix gets its own check round, past the usual limit if need be.
            if self.verify_rounds >= self.max_rounds() {
                self.bonus_rounds += 1;
            }
            self.writes_since_verify = false;
            // The fix goes to the checks next, not to another review.
            self.reviews_in_row = super::change_review::MAX_REVIEWS_IN_ROW;
        }
        Ok(sent)
    }

    /// F06: the checks at once, in the middle of a run (after a parallel
    /// group was joined), with each failure put down to the worker whose
    /// scope owns its file or to a seam. A record shows it; the run's fix
    /// rounds aren't counted, and the checks run again when it finishes.
    /// None when there's nothing to run.
    pub(super) async fn verify_now(
        &mut self,
        owners: &[(String, usize, super::worker::WriteScope)],
    ) -> Option<String> {
        let p = self.config().permissions;
        let root = self.root.clone()?;
        if !p.verify_changes {
            return None;
        }
        let checks = self.enabled_checks();
        if checks.is_empty() {
            return None;
        }
        // Autopilot's checks don't wait for an approval nobody gives (D7).
        if p.command_approval.required && !self.in_autopilot() {
            return Some("The checks didn't run on the merged result yet: command approval is on, so they run when you finish.".into());
        }
        let changed = self.changed_files.clone();
        let round = self.verify_rounds;
        let record_id = self
            .add_record(with(
                record("running", round, &[], &changed, None),
                json!({ "midRun": true }),
            ))
            .await
            .ok()?;
        // Its own name: the round's checks, saved under `round-N`, still
        // stand (their paths are in the round's record and triage).
        let saved_as = format!(
            "round-{round}-midrun-{}",
            &record_id[..8.min(record_id.len())]
        );
        self.clean_earlier_output();
        let results = self
            .run_checks(&root, &checks, &p, &changed, &record_id, round, &saved_as)
            .await
            .ok()?;
        let failed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "failed").collect();
        let mut attribution: Vec<Value> = Vec::new();
        let mut text = Vec::new();
        for r in &failed {
            text.push(format!(
                "## {} failed: `{}`",
                check_label(&r.kind),
                r.command
            ));
            let output = r.output.clone().unwrap_or_default();
            let mut seen: Vec<String> = Vec::new();
            let (roots, known) = self.path_context(owners);
            for loc in error_locations(&output) {
                // As a chat path (absolute, or from a subfolder).
                let loc = super::causes::resolve(&loc, &roots, &known);
                if seen.contains(&loc) || seen.len() >= 30 {
                    continue;
                }
                seen.push(loc.clone());
                // A file the run created (it wasn't there before it).
                let key = super::repair::rel_key(&root, &loc);
                let created = matches!(self.original_of(&key), Some(None));
                let owner = super::group::attribute(&loc, owners, created);
                attribution.push(json!({ "location": loc, "owner": owner }));
                text.push(format!("- {loc} ({owner})"));
            }
            text.push(format!("\n{}", output.trim()));
            if let Some(path) = &r.saved_output {
                text.push(format!("(The whole output is saved as `{path}`.)"));
            }
        }
        let status = if self.stopped() {
            "stopped"
        } else if !failed.is_empty() {
            "failed"
        } else if results.iter().any(clear) {
            "passed"
        } else {
            "unverified"
        };
        // The agent still gets the results; a record that couldn't be
        // stored only stays "running" in the chat, so say why in the log.
        if let Err(e) = self
            .update_meta(
                &record_id,
                with(
                    record(status, round, &results, &changed, None),
                    json!({ "midRun": true, "attribution": attribution }),
                ),
            )
            .await
        {
            eprintln!(
                "agent: could not store the mid-run checks' record in {}: {e}",
                self.dialog_id
            );
        }
        // The checks run again when the run finishes.
        self.writes_since_verify = true;
        Some(match status {
            "passed" => "The project's checks ran on the merged result and pass.".to_string(),
            "failed" => format!(
                "The project's checks ran on the merged result, and some fail. Each error is put down to the worker whose step owns its file, or to a seam between them. \
                 A failure inside one worker's files: continue that worker (task with continue and its handle) to fix it. A seam: fix it yourself.\n\n{}",
                text.join("\n")
            ),
            _ => "The project's checks couldn't run on the merged result; they run when you finish.".to_string(),
        })
    }

    /// One line per round in the run log.
    fn log_round(
        &self,
        round: u32,
        status: &str,
        results: &[CheckResult],
        judgement: &Judgement,
        flags: &[String],
    ) {
        log::write(
            &self.core,
            json!({
                "event": "verify",
                "dialogId": self.dialog_id,
                "round": round,
                "status": status,
                "progress": judgement.verdict,
                "newFailures": judgement.new.len(),
                "fixed": judgement.fixed.len(),
                "integrityFlags": flags.len(),
                "checks": results.iter().map(|r| json!({
                    "kind": r.kind,
                    "command": r.command,
                    "status": r.status,
                    "exitCode": r.exit_code,
                    "ms": r.duration_ms,
                    "failures": r.failures.as_ref().map(Vec::len),
                    "testsRan": r.tests_ran,
                })).collect::<Vec<_>>(),
            }),
        );
    }

    /// Before the run's first saved output (its first checks, mid-run or at
    /// the end, or a join's conflict versions): earlier runs' saved output
    /// goes, once (P4.7: never this run's). A task's later runs keep its
    /// Prepare stage's (S03): the stage's record links it, and the
    /// implement run may read it. The stage starts afresh.
    pub(super) fn clean_earlier_output(&mut self) {
        if !self.cleaned_this_run {
            self.cleaned_this_run = true;
            if self.in_autopilot() && !self.harness_stage() {
                super::check_output::clean_except(&self.dialog_id, "prepare-");
            } else {
                super::check_output::clean(&self.dialog_id);
            }
        }
    }

    /// Runs the enabled checks in order (type check, lint, tests, build),
    /// updating the record as each finishes. A check the user's command
    /// permissions don't allow is skipped, not run: the gate is held to the
    /// same rules as the agent. Tests are skipped when their folder's type
    /// check failed: its errors would only reappear as test failures. A check
    /// that fails is compared with the code before the change, where there
    /// is a copy of it, and what fails there too is set apart (before.rs).
    #[allow(clippy::too_many_arguments)]
    async fn run_checks(
        &mut self,
        root: &str,
        checks: &[ProjectCheck],
        permissions: &Permissions,
        changed: &[String],
        record_id: &str,
        round: u32,
        saved_as: &str,
    ) -> Result<Vec<CheckResult>, String> {
        let mut results: Vec<CheckResult> = ordered_checks(checks)
            .iter()
            .map(CheckResult::pending)
            .collect();
        let progress = |results: &[CheckResult]| record("running", round, results, changed, None);
        self.update_meta(record_id, progress(&results)).await?;
        // How the checks' output names files: failure keys name them by
        // their chat paths, the same in a copy of the folders elsewhere.
        let roots = self.output_roots();
        // The code before the change, for the checks that fail (before.rs):
        // opened for the first, gone after the last.
        let mut copy: Option<super::before::BeforeCopy> = None;

        // V06: the tests of the changed files, run where the whole suite
        // timed out: its index.
        let mut scoped: Option<usize> = None;
        let mut i = 0;
        while i < results.len() {
            'check: {
                if self.stopped() {
                    results[i].skip("stopped");
                    break 'check;
                }
                if let Verdict::Refused(_) = check_command(&results[i].command, permissions) {
                    // Autopilot's run any command but its denylist's (V01).
                    let why = match denied_entry(&results[i].command, permissions) {
                        Some(entry) => format!("`{entry}` is a command it may never run"),
                        None => "not in your allowed commands".into(),
                    };
                    results[i].skip(&why);
                    self.update_meta(record_id, progress(&results)).await?;
                    break 'check;
                }
                // V09: only the type check of the tests' own folder (`cd web
                // && …`), in every chat: another folder's errors aren't theirs.
                let folder = super::project::folder_of(&results[i].command);
                if results[i].kind == "test"
                    && results.iter().any(|r| {
                        r.kind == "typecheck"
                            && r.status == "failed"
                            && super::project::folder_of(&r.command) == folder
                    })
                {
                    results[i].skip("the type check failed");
                    self.update_meta(record_id, progress(&results)).await?;
                    break 'check;
                }

                results[i].status = "running";
                self.update_meta(record_id, progress(&results)).await?;
                let stop = self.stop.clone();
                // Checks are heavy work: they may wait for a CPU slot, and the
                // record says what for while they do.
                let waiting: Arc<Mutex<Option<Option<String>>>> = Arc::default();
                let overlay = crate::overlay::chats::get(&self.dialog_id);
                let noted = waiting.clone();
                // V07: kept whole for the failure keys, and for comparing
                // them with the code before the change's (before.rs).
                let queued =
                    crate::command_tool::Queued::check(&self.dialog_id, overlay.as_deref())
                        .noting(move |note| {
                            *noted.lock().unwrap_or_else(|e| e.into_inner()) = Some(note)
                        })
                        .whole(true);
                let run = crate::command_tool::run_command(
                    root.into(),
                    results[i].command.clone(),
                    Some(check_timeout_ms(&self.dialog_id)),
                    move || stop.is_set(),
                    Some(queued),
                );
                tokio::pin!(run);
                let outcome = loop {
                    tokio::select! {
                        outcome = &mut run => break outcome,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                            let changed = waiting.lock().unwrap_or_else(|e| e.into_inner()).take();
                            if let Some(note) = changed {
                                results[i].note = note;
                                self.update_meta(record_id, progress(&results)).await?;
                            }
                        }
                    }
                };
                // A failed check's whole output, when what fails on the code
                // before the change can be told apart in it (before.rs).
                let mut compare: Option<String> = None;
                let r = &mut results[i];
                r.note = outcome
                    .as_ref()
                    .ok()
                    .and_then(|o| o.waited.as_ref())
                    .and_then(|w| w.describe());
                match outcome {
                    Ok(out) if out.stopped => r.skip("stopped"),
                    Ok(out) => {
                        r.duration_ms = Some(out.duration_ms);
                        r.exit_code = Some(out.exit_code);
                        r.timed_out = Some(out.timed_out);
                        // Compilers print to either stream; keep both, in order of usefulness.
                        let combined = out.text();
                        // A scoped run's count isn't the suite's (P3.7 compares them).
                        if r.kind == "test" && scoped != Some(i) {
                            r.tests_ran = tests_ran(&combined);
                        }
                        if out.exit_code == Some(0) && !out.timed_out {
                            r.status = "passed";
                        } else {
                            r.status = "failed";
                            let environment = (!out.timed_out)
                                .then(|| {
                                    environment_failure(out.exit_code, &combined, &roots.paths())
                                })
                                .flatten();
                            // V03: in Autopilot, whether its environment failed.
                            if self.in_autopilot() {
                                r.environment = environment.clone();
                            }
                            // From the whole output, not the focused part nor
                            // the start and end kept to show: every failure
                            // counts when rounds are compared (V07).
                            let whole = out.whole_text().unwrap_or_else(|| combined.clone());
                            r.failures =
                                Some(check_keys(&r.kind, &whole, out.timed_out, &roots, &folder));
                            // A timeout says nothing of what it didn't reach,
                            // and an environment's failure nothing of the code.
                            if !out.timed_out && environment.is_none() {
                                compare = Some(whole.clone());
                            }
                            let focused = focus_failures(&combined, FAILURE_OUTPUT_CHARS);
                            let output = if focused.is_empty() {
                                "(no output)".to_string()
                            } else {
                                focused
                            };
                            // From the focused output: the error blocks, not passing tests' paths.
                            let (changed_hits, other) =
                                split_by_changed(&error_locations(&output), changed);
                            if !changed_hits.is_empty() || !other.is_empty() {
                                r.error_files = Some(ErrorFiles {
                                    changed: changed_hits,
                                    other,
                                });
                            }
                            r.output = Some(output);
                            // What the reports call the whole output: not the
                            // start and end `combined` keeps (V07).
                            if whole.chars().count() > FAILURE_OUTPUT_CHARS {
                                r.saved_output = super::check_output::save(
                                    &self.dialog_id,
                                    saved_as,
                                    &r.kind,
                                    &whole,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        r.status = "failed";
                        r.output = Some(format!("Could not run the command: {e}"));
                        r.failures = Some(vec![format!("{}|could not run", r.kind)]);
                    }
                }
                self.update_meta(record_id, progress(&results)).await?;
                // What fails on the code before the change too isn't the
                // change's: the check runs there, once a run (before.rs).
                if let (Some(output), Some(folders)) = (compare, self.code_before()) {
                    let command = results[i].command.clone();
                    if self.befores.get(&command).is_none() {
                        let note = results[i]
                            .note
                            .replace(super::before::RUNNING_BEFORE.into());
                        results[i].status = "running";
                        self.update_meta(record_id, progress(&results)).await?;
                        let before = self.check_before(&results[i], &folders, &mut copy).await;
                        // Stopped, it's asked again next time.
                        if !self.stopped() {
                            self.befores.insert(&command, before);
                        }
                        results[i].status = "failed";
                        results[i].note = note;
                    }
                    super::before::set_apart(
                        &mut results[i],
                        self.befores.get(&command),
                        &output,
                        &roots,
                    );
                    self.update_meta(record_id, progress(&results)).await?;
                }
                // V06: in Autopilot, a whole test suite that timed out is
                // recorded slow, not failed, and the tests of the changed files
                // run in its place: theirs is the round's result.
                if scoped.is_none()
                    && results[i].kind == "test"
                    && results[i].timed_out == Some(true)
                    && self.in_autopilot()
                {
                    if let Some(command) = scoped_tests(root, changed, &results[i].command) {
                        let limit =
                            std::time::Duration::from_millis(check_timeout_ms(&self.dialog_id));
                        let r = &mut results[i];
                        r.status = "slow";
                        r.reason = Some(format!(
                            "timed out after {}; the tests of the changed files ran instead",
                            crate::chat_request::human_duration(limit)
                        ));
                        // Not comparable with other rounds, as any timeout.
                        r.failures = Some(vec![format!("{}|timed out", r.kind)]);
                        r.output = None;
                        r.error_files = None;
                        r.tests_ran = None;
                        let check = ProjectCheck {
                            kind: "test".into(),
                            command,
                            enabled: true,
                        };
                        results.insert(i + 1, CheckResult::pending(&check));
                        scoped = Some(i + 1);
                        self.update_meta(record_id, progress(&results)).await?;
                    }
                }
            }
            i += 1;
        }
        if let Some(copy) = copy {
            copy.discard().await;
        }
        Ok(results)
    }
}

/// V06: the command that runs the tests of the changed files in the
/// suite's folder (`cd web && …`; `project::test_command_for`), run there:
/// the test files among them, and those by their stack's conventions beside
/// the others. None when there are none, the stack has no way to run some,
/// or it's the whole suite again.
fn scoped_tests(root: &str, changed: &[String], suite: &str) -> Option<String> {
    let folder = super::project::folder_of(suite);
    let dir = std::path::Path::new(root).join(&folder);
    let mut files: Vec<String> = Vec::new();
    for f in changed {
        let key = super::repair::rel_key(root, f);
        // The suite's folder's files, by their paths in it.
        let key = match folder.as_str() {
            "" => key.as_str(),
            folder => match key.strip_prefix(folder).and_then(|k| k.strip_prefix('/')) {
                Some(in_folder) => in_folder,
                None => continue,
            },
        };
        for test in super::project::test_paths_for(key) {
            if dir.join(&test).is_file() && !files.contains(&test) {
                files.push(test);
            }
        }
    }
    let command = super::project::test_command_for(&dir, &files, None)?;
    let command = super::project::in_folder(&folder, &command);
    (command.trim() != suite.trim()).then_some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D27: a workspace chat's checks are those of the folders it changed,
    /// one level down too, and those in no folder (a plan's own commands);
    /// a folder chat's all run.
    #[test]
    fn a_workspace_chats_checks_are_its_changed_folders_and_those_in_none() {
        let changed: std::collections::BTreeSet<String> = ["web".to_string()].into();
        let changed = Some(&changed);
        assert!(checks_changed("cd web && npm test", changed));
        assert!(checks_changed("cd web/ui && npm test", changed));
        assert!(!checks_changed("cd api && npm test", changed));
        assert!(checks_changed("node check.js", changed), "a plan's own");
        assert!(
            checks_changed("cd api && npm test", None),
            "a folder chat's"
        );
    }

    /// V06: the tests of the changed files in the timed-out suite's folder,
    /// run there; another folder's changes aren't its.
    #[test]
    fn a_suites_scoped_tests_are_its_folders_run_in_it() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.to_string_lossy().into_owned();
        for (path, content) in [
            ("web/package.json", r#"{"devDependencies":{"vitest":"1"}}"#),
            ("web/src/a.ts", ""),
            ("web/src/a.test.ts", ""),
            ("api/src/b.ts", ""),
            ("api/src/b.test.ts", ""),
        ] {
            let file = dir.0.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
        let changed = ["web/src/a.ts".to_string(), "api/src/b.ts".to_string()];
        assert_eq!(
            scoped_tests(&root, &changed, "cd web && npm test").as_deref(),
            Some("cd web && npx vitest run src/a.test.ts")
        );
        assert_eq!(
            scoped_tests(&root, &changed[1..], "cd web && npm test"),
            None
        );
    }

    /// What the project changed meanwhile is shown the chat, but not a
    /// restricted file's lines.
    #[test]
    fn the_projects_changes_shown_leave_out_a_restricted_files_lines() {
        let restricted = crate::restricted_files::RestrictedFiles::default();
        let files = vec![
            ("a.txt".to_string(), "-1\n+2".to_string()),
            (
                ".env.local".to_string(),
                "-KEY=old\n+KEY=sk-live".to_string(),
            ),
            ("b.txt".to_string(), String::new()),
        ];
        let shown = project_changes(&files, "/p", &restricted);
        assert!(
            shown.contains("What the project changed in `a.txt`"),
            "{shown}"
        );
        assert!(shown.contains("+2"), "{shown}");
        assert!(
            !shown.contains(".env.local") && !shown.contains("sk-live"),
            "{shown}"
        );
        assert!(!shown.contains("b.txt"), "{shown}");
    }

    /// P1.8: a carry keeps at most CARRY_MEMORY_BYTES of file contents in
    /// memory; the largest go to disk, come back intact, and their files go
    /// with the carry.
    #[test]
    fn a_large_baseline_spills_to_disk_and_comes_back() {
        use super::super::subagent::{Original, Source};
        let chat = format!("spill-{}", uuid::Uuid::new_v4());
        let big = vec![0xffu8; CARRY_MEMORY_BYTES];
        let mut baseline = std::collections::HashMap::new();
        baseline.insert(
            "big.bin".to_string(),
            Original::new(Some(big.clone()), Source::ViewAtRunStart),
        );
        baseline.insert(
            "small.txt".to_string(),
            Original::new(Some(b"x".to_vec()), Source::ProjectCopy),
        );
        baseline.insert(
            "new.txt".to_string(),
            Original::new(None, Source::ViewAtRunStart),
        );
        let spilled = spill(&chat, &mut baseline);
        assert_eq!(spilled.0.len(), 1);
        assert!(!baseline.contains_key("big.bin") && baseline.contains_key("small.txt"));
        let (key, file, source) = &spilled.0[0];
        assert_eq!((key.as_str(), *source), ("big.bin", Source::ViewAtRunStart));
        assert_eq!(std::fs::read(file).unwrap(), big);
        let file = file.clone();
        drop(spilled);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(super::super::check_output::carry_dir(&chat).unwrap());
    }

    /// Round 2, P3.9: a spilled file that's gone is reported, so the run
    /// marks its integrity check as incomplete.
    #[test]
    fn a_spilled_file_lost_marks_integrity_unchecked() {
        use super::super::subagent::{Original, Source};
        let chat = format!("spill-{}", uuid::Uuid::new_v4());
        let mut baseline = std::collections::HashMap::new();
        baseline.insert(
            "big.bin".to_string(),
            Original::new(
                Some(vec![1u8; CARRY_MEMORY_BYTES + 1]),
                Source::ViewAtRunStart,
            ),
        );
        let spilled = spill(&chat, &mut baseline);
        let mut kept = baseline.clone();
        assert!(spilled.restore(&mut kept), "all of it comes back");
        assert!(kept.contains_key("big.bin"));
        std::fs::remove_file(&spilled.0[0].1).unwrap();
        let mut lost = baseline.clone();
        assert!(!spilled.restore(&mut lost));
        assert!(!lost.contains_key("big.bin"));
        let _ = std::fs::remove_dir_all(super::super::check_output::carry_dir(&chat).unwrap());
    }

    #[test]
    fn the_report_the_agent_gets() {
        let check = |kind: &str, command: &str, status| CheckResult {
            status,
            ..CheckResult::pending(&ProjectCheck {
                kind: kind.into(),
                command: command.into(),
                enabled: true,
            })
        };
        let mut failed = check("typecheck", "tsc", "failed");
        failed.exit_code = Some(Some(2));
        failed.output = Some("src/a.ts(1,1): error".into());
        failed.error_files = Some(ErrorFiles {
            changed: vec!["src/a.ts".into()],
            other: vec![],
        });
        let mut skipped = check("test", "npm test", "pending");
        skipped.skip("the type check failed");
        let results = vec![check("lint", "eslint .", "passed"), failed, skipped];
        let text = failure_report(
            &results,
            &["src/a.ts".into()],
            1,
            Some(3),
            &["No progress: x".into()],
        );
        assert!(text.contains("(attempt 1 of 3).\n\nNo progress: x\n\nFiles you changed"));
        assert!(text.contains("Your changes are not done yet: a check fails (attempt 1 of 3)."));
        assert!(text.contains("Passed: Lint (`eslint .`)"));
        assert!(text.contains("## Type check failed: `tsc` (exit code 2)\nErrors point at files you changed: src/a.ts."));
        assert!(text.contains("Not run: `npm test` (the type check failed)"));
        // V03: Autopilot's rounds have no count to tell.
        let autopilot = failure_report(&results, &[], 2, None, &[]);
        assert!(autopilot.contains(
            "a check fails (attempt 2; the checks stop after 2 attempts in a row without progress)."
        ));

        let meta = record("running", 1, &results[..1], &[], None);
        assert_eq!(
            meta["verification"]["checks"][0],
            json!({ "kind": "lint", "command": "eslint .", "status": "passed" })
        );
        assert_eq!(meta["verification"]["maxRounds"], 3);
    }

    /// What fails on the code before the change too (before.rs) is told
    /// apart: a check that fails only so isn't the agent's to fix, and of
    /// one that fails on the change too, the agent gets its own failures.
    /// The round counts only those, and both checks ran.
    #[test]
    fn the_report_sets_apart_what_fails_before_the_change() {
        let check = |kind: &str, command: &str, status| CheckResult {
            status,
            ..CheckResult::pending(&ProjectCheck {
                kind: kind.into(),
                command: command.into(),
                enabled: true,
            })
        };
        let mut lint = check("lint", "yarn run lint", "preexisting");
        lint.preexisting = Some(vec!["lint|src/a.ts|no-unused-vars|'x' is unused".into()]);
        let mut tsc = check("typecheck", "yarn tsc --noEmit", "failed");
        tsc.exit_code = Some(Some(2));
        tsc.output = Some("src/old.ts(1,1): error TS2304\nsrc/new.ts(9,3): error TS2322".into());
        let own = "typecheck|src/new.ts|TS2322|Type 'string' is not assignable to type 'number'.";
        tsc.failures = Some(vec![own.into()]);
        tsc.preexisting = Some(vec![
            "typecheck|src/old.ts|TS2304|Cannot find name 'a'.".into()
        ]);
        let results = vec![tsc, lint];
        let text = failure_report(&results, &["src/new.ts".into()], 1, Some(3), &[]);
        assert!(text.contains("Your changes are not done yet: a check fails (attempt 1 of 3)."));
        assert!(
            text.contains("Failing only as on the code before your change, so not yours to fix: Lint (`yarn run lint`)"),
            "{text}"
        );
        assert!(
            text.contains("Some of its failures are there on the code before your change too: they aren't yours, so leave them. Yours:\n- src/new.ts|TS2322|Type 'string' is not assignable to type 'number'.\n"),
            "{text}"
        );
        let round = round_failures(&results);
        assert_eq!(round.keys.into_iter().collect::<Vec<_>>(), [own]);
        assert_eq!(
            round.ran.into_iter().collect::<Vec<_>>(),
            ["lint", "typecheck"]
        );
        assert_eq!(
            record("failed", 1, &results[1..], &[], None)["verification"]["checks"][0],
            json!({
                "kind": "lint",
                "command": "yarn run lint",
                "status": "preexisting",
                "preexisting": ["lint|src/a.ts|no-unused-vars|'x' is unused"]
            })
        );
    }
}

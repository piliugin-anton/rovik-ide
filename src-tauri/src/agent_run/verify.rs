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
    error_locations, failure_keys, focus_failures, split_by_changed, tests_ran,
};
use super::log;
use super::permissions::{check_command, Permissions, Verdict};
use super::project::{check_label, ordered_checks, ProjectCheck};
use super::repair::{self, Judgement, RoundFailures, Verdict as Progress};
use super::run::{new_message, Run};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri::Runtime;

/// Fix-and-recheck rounds before the gate gives up and reports.
pub const MAX_VERIFY_ROUNDS: u32 = 3;
/// Per check. Test suites can be slow; this is the bash maximum.
const CHECK_TIMEOUT_MS: u64 = 600_000;
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
    /// pending, running, passed, failed, skipped.
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
    baseline: std::collections::HashMap<String, Option<String>>,
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
    /// What the run last wrote or read: in a chat that isn't isolated, a
    /// rollback undoes only what's still the run's (K15).
    seen: std::sync::Arc<crate::file_guard::Seen>,
}

/// The record's meta: `{ verification: … }`.
fn record(
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

/// This round's failures, for comparing rounds (repair.rs).
fn round_failures(results: &[CheckResult]) -> RoundFailures {
    RoundFailures {
        keys: results
            .iter()
            .flat_map(|r| r.failures.iter().flatten().cloned())
            .collect(),
        ran: results
            .iter()
            .filter(|r| r.status == "passed" || r.status == "failed")
            .map(|r| r.kind.clone())
            .collect(),
        tests_ran: results
            .iter()
            .filter_map(|r| r.tests_ran)
            .reduce(|a, b| a + b),
    }
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
pub fn failure_report(
    results: &[CheckResult],
    changed_files: &[String],
    round: u32,
    notes: &[String],
) -> String {
    let failed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "failed").collect();
    let passed: Vec<&CheckResult> = results.iter().filter(|r| r.status == "passed").collect();
    let fail = if failed.len() == 1 {
        "a check fails".to_string()
    } else {
        format!("{} checks fail", failed.len())
    };
    let mut lines = vec![
        "[Automatic check by the IDE, not written by the user]".to_string(),
        format!("Your changes are not done yet: {fail} (attempt {round} of {MAX_VERIFY_ROUNDS})."),
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
    if !passed.is_empty() {
        let list: Vec<String> = passed
            .iter()
            .map(|r| format!("{} (`{}`)", check_label(&r.kind), r.command))
            .collect();
        lines.push(format!("Passed: {}", list.join(", ")));
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
    lines.push(String::new());
    lines.push(
        "Find the cause in your changes and fix it; the checks run again when you finish. \
         If a failure has nothing to do with your changes (it fails on the original code too), \
         don't change unrelated code: finish and say so."
            .into(),
    );
    lines.join("\n")
}

impl<R: Runtime> Run<R> {
    /// Appends a verification record; returns its id.
    async fn add_record(&self, meta: Value) -> Result<String, String> {
        let mut m = new_message("assistant", "");
        m.meta = Some(meta);
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
            seen: self.seen.clone(),
        }
    }

    /// The run that starts the checks takes up where the last one stopped.
    pub(super) fn restore_verify_state(&mut self, record_id: &str) {
        let Some(c) = self.state.take_carry(&self.dialog_id, record_id) else {
            return;
        };
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
        self.seen = c.seen;
    }

    /// The round limit: MAX_VERIFY_ROUNDS, and one more for each review
    /// after green whose findings went back (R08).
    fn max_rounds(&self) -> u32 {
        MAX_VERIFY_ROUNDS + self.bonus_rounds
    }

    /// One round of the gate; "repaired": the workers fixed this round's
    /// failures, and the checks should run again at once.
    async fn verify_round(
        &mut self,
        approved: bool,
        record_id: Option<String>,
    ) -> Result<String, String> {
        let p = self.state.config().permissions;
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
            return Ok("done".into());
        }

        let checks: Vec<ProjectCheck> = self
            .checks
            .iter()
            .filter(|c| c.enabled && !c.command.trim().is_empty())
            .cloned()
            .collect();
        if checks.is_empty() {
            self.add_record(record(
                "unverified",
                self.verify_rounds,
                &[],
                &changed,
                Some("no checks are set up for this project"),
            ))
            .await?;
            self.writes_since_verify = false;
            return Ok("unverified".into());
        }

        // Command approval is on: the gate's commands need the user's go too.
        if p.command_approval.required && !approved {
            let pending: Vec<CheckResult> = checks.iter().map(CheckResult::pending).collect();
            let record_id = self
                .add_record(record(
                    "needs-approval",
                    self.verify_rounds,
                    &pending,
                    &changed,
                    None,
                ))
                .await?;
            // The run ends here: what its fix rounds know waits for the
            // run that starts the checks.
            if self.sub().is_none() {
                let carry = self.take_verify_state(record_id);
                self.state.put_carry(&self.dialog_id, carry);
            }
            return Ok("verify-needs-approval".into());
        }

        self.verify_rounds += 1;
        if self.verify_rounds == 1 {
            // A run's first round: earlier runs' saved output goes.
            super::check_output::clean(&self.dialog_id);
        }
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

        let results = self
            .run_checks(&root, &checks, &p, &changed, &record_id, round)
            .await?;

        let failed = results.iter().any(|r| r.status == "failed");
        let ran = results
            .iter()
            .any(|r| r.status == "passed" || r.status == "failed");
        // R01: how this round compares with the earlier ones.
        let now = round_failures(&results);
        let mut judgement = repair::judge(&self.verify_history, &now);
        // R05: asked to put back a weakened test, the agent did, and it
        // fails: that's what was asked for, not a fix that broke something.
        if std::mem::take(&mut self.asked_integrity) {
            judgement = judgement.without_regression();
        }
        // R05: what weakens the checks, and fewer tests than last round.
        let mut flags = if ran {
            self.integrity_flags()
        } else {
            Vec::new()
        };
        if let (Some(before), Some(after)) = (
            self.verify_history.last().and_then(|r| r.tests_ran),
            now.tests_ran,
        ) {
            if after < before {
                flags.push(format!(
                    "fewer tests ran than in the last round ({before} → {after})"
                ));
            }
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

        // R02: a fix that broke what didn't fail before is undone; the
        // agent hears what it broke and gets the earlier failures again.
        if failed && judgement.verdict == Progress::Regression && !self.stopped() {
            if let Some(rb) = self.roll_back(judgement.broke.clone()).await {
                self.rolled_back_attempts.push(rb.clone());
                let status = if round >= max_rounds {
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
                let report = failure_report(&earlier, &changed, round, &[rollback_note(&rb)]);
                self.add_harness(report, "verification").await?;
                // Not all of it went back: the code is new, check it again.
                self.writes_since_verify = !rb.skipped.is_empty();
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
        } else if round >= max_rounds {
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
        }
        self.update_meta(&record_id, meta.clone()).await?;
        self.log_round(round, status, &results, &judgement, &flags);

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
            self.checkpoint = None;
            return Ok("continue".into());
        }
        if status != "failed" {
            self.checkpoint = None;
            return Ok(match status {
                "passed" => "verified".into(),
                _ => format!("verify-{status}"),
            });
        }

        // Back to the model. User-role, so every provider accepts it after an
        // assistant reply; marked as the IDE's, so it's never shown as the
        // user's message or replaced by the request's editor context.
        let mut notes: Vec<String> = Vec::new();
        if let Some(note) = repair::progress_note(&judgement, now.keys.len()) {
            notes.push(note);
        }
        if judgement.verdict == Progress::Regression {
            // No checkpoint to go back to (too large, or the first round).
            notes.push(format!(
                "Your last fix broke something that didn't fail before:\n{}",
                repair::key_list(&judgement.broke, 8)
            ));
        }
        if !flags.is_empty() {
            notes.push(repair::integrity_note(&flags, false));
        }
        // C03: a large or repeated failure goes to triage first; the agent
        // gets its diagnosis in place of that check's output.
        let base = with(
            record(status, round, &results, &changed, reason),
            extra(None),
        );
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
        let mut report = failure_report(&sent, &changed, round, &notes);
        if let Some(files) = &self.merged_from_project {
            let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
            let diffs: String = files
                .iter()
                .filter(|(_, d)| !d.is_empty())
                .map(|(p, d)| format!("\nWhat the project changed in `{p}`:\n```diff\n{d}\n```\n"))
                .collect();
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
    /// within the run's review budget. True: high findings went to the
    /// agent, and one more fix and check round is allowed.
    async fn review_after_green(&mut self, record_id: &str, meta: &Value) -> Result<bool, String> {
        let config = self.state.config().subagents;
        if config.kind(super::subagent::Kind::Reviewer).is_none() || self.plan_mode {
            return Ok(false);
        }
        if self.reviews_total >= super::change_review::MAX_CHANGE_REVIEWS_PER_RUN {
            self.update_meta(
                record_id,
                with(
                    meta.clone(),
                    json!({ "reviewAfterGreen": {
                        "status": "skipped",
                        "reason": format!(
                            "the run's {} change reviews are used up",
                            super::change_review::MAX_CHANGE_REVIEWS_PER_RUN
                        ),
                    } }),
                ),
            )
            .await?;
            return Ok(false);
        }
        // The fixes are new to the reviewer, whatever came before.
        self.writes_since_review = true;
        let sent = self.review_change_as(true).await?;
        self.update_meta(
            record_id,
            with(
                meta.clone(),
                json!({ "reviewAfterGreen": {
                    "status": if sent { "findings" } else { "done" },
                } }),
            ),
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
        let p = self.state.config().permissions;
        let root = self.root.clone()?;
        if !p.verify_changes {
            return None;
        }
        let checks: Vec<ProjectCheck> = self
            .checks
            .iter()
            .filter(|c| c.enabled && !c.command.trim().is_empty())
            .cloned()
            .collect();
        if checks.is_empty() {
            return None;
        }
        if p.command_approval.required {
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
        let results = self
            .run_checks(&root, &checks, &p, &changed, &record_id, round)
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
                // As a project-relative path (absolute, or from a subfolder).
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
        } else if results.iter().any(|r| r.status == "passed") {
            "passed"
        } else {
            "unverified"
        };
        let _ = self
            .update_meta(
                &record_id,
                with(
                    record(status, round, &results, &changed, None),
                    json!({ "midRun": true, "attribution": attribution }),
                ),
            )
            .await;
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
            &self.app,
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

    /// Runs the enabled checks in order (type check, lint, tests, build),
    /// updating the record as each finishes. A check the user's command
    /// permissions don't allow is skipped, not run: the gate is held to the
    /// same rules as the agent. Tests are skipped when the type check
    /// failed: its errors would only reappear as test failures.
    async fn run_checks(
        &self,
        root: &str,
        checks: &[ProjectCheck],
        permissions: &Permissions,
        changed: &[String],
        record_id: &str,
        round: u32,
    ) -> Result<Vec<CheckResult>, String> {
        let mut results: Vec<CheckResult> = ordered_checks(checks)
            .iter()
            .map(CheckResult::pending)
            .collect();
        let progress = |results: &[CheckResult]| record("running", round, results, changed, None);
        self.update_meta(record_id, progress(&results)).await?;

        for i in 0..results.len() {
            if self.stopped() {
                results[i].skip("stopped");
                continue;
            }
            if let Verdict::Refused(_) = check_command(&results[i].command, permissions) {
                results[i].skip("not in your allowed commands");
                self.update_meta(record_id, progress(&results)).await?;
                continue;
            }
            if results[i].kind == "test"
                && results
                    .iter()
                    .any(|r| r.kind == "typecheck" && r.status == "failed")
            {
                results[i].skip("the type check failed");
                self.update_meta(record_id, progress(&results)).await?;
                continue;
            }

            results[i].status = "running";
            self.update_meta(record_id, progress(&results)).await?;
            let stop = self.stop.clone();
            // Checks are heavy work: they may wait for a CPU slot, and the
            // record says what for while they do.
            let waiting: Arc<Mutex<Option<Option<String>>>> = Arc::default();
            let queued = crate::command_tool::Queued {
                kind: crate::workload::JobKind::Check {
                    owner: self.dialog_id.clone(),
                },
                note: {
                    let waiting = waiting.clone();
                    Box::new(move |note| {
                        *waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(note)
                    })
                },
                env: crate::overlay::chats::get(&self.dialog_id)
                    .map(|o| o.command_env())
                    .unwrap_or_default(),
                confine: crate::overlay::chats::get(&self.dialog_id).and_then(|o| o.confine()),
            };
            let run = crate::command_tool::run_command(
                root.into(),
                results[i].command.clone(),
                Some(CHECK_TIMEOUT_MS),
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
                    let combined = [out.stderr.trim(), out.stdout.trim()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if r.kind == "test" {
                        r.tests_ran = tests_ran(&combined);
                    }
                    if out.exit_code == Some(0) && !out.timed_out {
                        r.status = "passed";
                    } else {
                        r.status = "failed";
                        // From the whole output, not the focused part: every
                        // failure counts when rounds are compared.
                        r.failures = Some(failure_keys(&r.kind, &combined, out.timed_out));
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
                        if combined.chars().count() > FAILURE_OUTPUT_CHARS {
                            r.saved_output = super::check_output::save(
                                &self.dialog_id,
                                round,
                                &r.kind,
                                &combined,
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
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            &["No progress: x".into()],
        );
        assert!(text.contains("(attempt 1 of 3).\n\nNo progress: x\n\nFiles you changed"));
        assert!(text.contains("Your changes are not done yet: a check fails (attempt 1 of 3)."));
        assert!(text.contains("Passed: Lint (`eslint .`)"));
        assert!(text.contains("## Type check failed: `tsc` (exit code 2)\nErrors point at files you changed: src/a.ts."));
        assert!(text.contains("Not run: `npm test` (the type check failed)"));

        let meta = record("running", 1, &results[..1], &[], None);
        assert_eq!(
            meta["verification"]["checks"][0],
            json!({ "kind": "lint", "command": "eslint .", "status": "passed" })
        );
        assert_eq!(meta["verification"]["maxRounds"], 3);
    }
}

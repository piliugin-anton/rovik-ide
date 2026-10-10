//! The verification gate. When the model finishes a run in which it changed
//! files, the IDE runs the project's checks (type check, lint, tests:
//! project.rs) and, if any fail, sends the failures back to the agent to
//! fix, up to MAX_VERIFY_ROUNDS times. The environment, not the model,
//! decides that the change works.
//!
//! Each round's record shows in the chat: an empty assistant message with
//! `meta.verification`, never sent to the model, saved on every change so it
//! reads the same after a restart.

use super::command_output::{error_locations, focus_failures, split_by_changed};
use super::log;
use super::permissions::{check_command, Permissions, Verdict};
use super::project::{check_label, ordered_checks, ProjectCheck};
use super::run::{new_message, Run};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri::Runtime;

/// Fix-and-recheck rounds before the gate gives up and reports.
pub const MAX_VERIFY_ROUNDS: u32 = 3;
/// Per check. Test suites can be slow; this is the bash maximum.
const CHECK_TIMEOUT_MS: u64 = 600_000;
const FAILURE_OUTPUT_CHARS: usize = 8_000;

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
}

impl CheckResult {
    fn pending(check: &ProjectCheck) -> Self {
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
        }
    }

    fn skip(&mut self, reason: &str) {
        self.status = "skipped";
        self.reason = Some(reason.into());
    }
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
pub fn failure_report(results: &[CheckResult], changed_files: &[String], round: u32) -> String {
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
        String::new(),
        format!(
            "Files you changed in this task: {}",
            if changed_files.is_empty() {
                "(none recorded)".to_string()
            } else {
                changed_files.join(", ")
            }
        ),
    ];
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
        let p = self.state.config().permissions;
        let Some(root) = self.root.clone() else {
            return Ok("done".into());
        };
        if !p.verify_changes {
            return Ok("done".into());
        }
        let changed = self.changed_files.clone();

        if !self.writes_since_verify {
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
            self.add_record(record(
                "needs-approval",
                self.verify_rounds,
                &pending,
                &changed,
                None,
            ))
            .await?;
            return Ok("verify-needs-approval".into());
        }

        self.verify_rounds += 1;
        self.writes_since_verify = false;
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
        let status = if self.stopped() {
            "stopped"
        } else if !ran {
            "unverified"
        } else if !failed {
            "passed"
        } else if round >= MAX_VERIFY_ROUNDS {
            "gave-up"
        } else {
            "failed"
        };
        let reason = (status == "unverified").then_some("none of the checks were allowed to run");
        self.update_meta(
            &record_id,
            record(status, round, &results, &changed, reason),
        )
        .await?;
        log::write(
            &self.app,
            json!({
                "event": "verify",
                "dialogId": self.dialog_id,
                "round": round,
                "status": status,
                "checks": results.iter().map(|r| json!({
                    "kind": r.kind,
                    "command": r.command,
                    "status": r.status,
                    "exitCode": r.exit_code,
                    "ms": r.duration_ms,
                })).collect::<Vec<_>>(),
            }),
        );

        self.last_verify_failed = failed;
        if status != "failed" {
            return Ok(if status == "passed" {
                "verified".into()
            } else {
                format!("verify-{status}")
            });
        }

        // Back to the model. User-role, so every provider accepts it after an
        // assistant reply; marked as the IDE's, so it's never shown as the
        // user's message or replaced by the request's editor context.
        let mut report = failure_report(&results, &changed, round);
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
        Ok("continue".into())
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
                    if out.exit_code == Some(0) && !out.timed_out {
                        r.status = "passed";
                    } else {
                        r.status = "failed";
                        // Compilers print to either stream; keep both, in order of usefulness.
                        let combined = [out.stderr.trim(), out.stdout.trim()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
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
                    }
                }
                Err(e) => {
                    r.status = "failed";
                    r.output = Some(format!("Could not run the command: {e}"));
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
        let text = failure_report(&results, &["src/a.ts".into()], 1);
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

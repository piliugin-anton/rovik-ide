//! A narrow test probe before repeating a failed suite during repair.
//!
//! A probe may establish that a repair is still broken; it cannot establish
//! that the configured suite passes. Unknown runner commands stay unchanged.

use super::{CheckResult, RoundFailures};
use crate::agent_run::{project, repair};
use serde::Serialize;
use std::path::{Component, Path};

#[derive(Default)]
pub(super) struct State {
    pub full_result: Option<CheckResult>,
    pub attempts: Vec<Attempt>,
    pub failed: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Attempt {
    pub suite_command: String,
    pub result: CheckResult,
}

impl State {
    pub fn start_round(&mut self) {
        self.attempts.clear();
        self.failed = false;
    }

    pub fn command(&self, root: &Path, suite: &str, changed: &[String]) -> Option<String> {
        let previous = self.full_result.as_ref()?;
        if previous.command != suite || !actionable_failure(previous) {
            return None;
        }
        let mut sources = changed.to_vec();
        if let Some(errors) = &previous.error_files {
            sources.extend(errors.changed.iter().chain(&errors.other).cloned());
        }
        // pytest's failure key often names the test even when its traceback
        // has no source location for error_locations to recover.
        for key in previous.failures.iter().flatten() {
            if let Some(name) = key.strip_prefix("test|test|") {
                if let Some((file, _)) = name.split_once("::") {
                    sources.push(file.into());
                }
            }
        }
        let canonical_root = root.canonicalize().ok()?;
        let mut files = Vec::new();
        for source in sources {
            let rel = repair::rel_key(&root.to_string_lossy(), &source);
            for file in project::test_paths_for(&rel) {
                if !Path::new(&file)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                {
                    continue;
                }
                let path = root.join(&file);
                if path.is_file()
                    && path
                        .canonicalize()
                        .ok()
                        .is_some_and(|p| p.starts_with(&canonical_root))
                    && !files.contains(&file)
                {
                    files.push(file);
                }
            }
        }
        files.sort();
        compatible_command(root, suite, &files)
    }

    pub fn keep_unobserved_failures(
        &self,
        previous: Option<&RoundFailures>,
        now: &mut RoundFailures,
    ) {
        if let Some(previous) = previous {
            now.keys.extend(
                previous
                    .keys
                    .iter()
                    .filter(|k| k.starts_with("test|"))
                    .cloned(),
            );
        }
        now.tests_ran = None;
        now.ran.remove("test");
    }

    pub fn judge(&self, history: &[RoundFailures], now: &RoundFailures) -> repair::Judgement {
        let judgement = repair::judge(history, now);
        let partial_before = history.last().is_some_and(|previous| {
            !previous.ran.contains("test") && previous.keys.iter().any(|k| k.starts_with("test|"))
        });
        // Other fully executed kinds can still prove progress or a regression.
        // Unobserved test failures cannot prove that a repair made no progress.
        if (self.failed || partial_before) && judgement.verdict == repair::Verdict::NoProgress {
            judgement.as_first()
        } else {
            judgement
        }
    }
}

/// Only a real, executed failing test can defer the full suite. Collection
/// failures, broken compilers, missing runners and empty selections cannot.
pub(super) fn actionable_failure(result: &CheckResult) -> bool {
    result.kind == "test"
        && result.status == "failed"
        && result
            .exit_code
            .is_some_and(|code| code.is_some_and(|code| code != 0))
        && result.timed_out != Some(true)
        && result.environment.is_none()
        && result
            .failures
            .as_ref()
            .is_some_and(|keys| keys.iter().any(|key| key.starts_with("test|test|")))
        && result.tests_ran != Some(0)
}

/// Derive a subset only for command forms whose runner and options are
/// understood. Custom scripts, environment prefixes and flags keep their
/// original command; dropping one can change which tests execute.
fn compatible_command(root: &Path, suite: &str, files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let command = project::test_command_for(root, files, None)?;
    let matches_runner = match suite {
        "cargo test" => command.starts_with("cargo test "),
        "go test ./..." => command.starts_with("go test ./"),
        "python -m pytest" => command.starts_with("python -m pytest "),
        "npx vitest run" => command.starts_with("npx vitest run "),
        "npx jest" => command.starts_with("npx jest "),
        "npm test" => {
            let text = std::fs::read_to_string(root.join("package.json")).ok()?;
            let package: serde_json::Value = serde_json::from_str(&text).ok()?;
            let script = package.pointer("/scripts/test")?.as_str()?;
            let runner = match script {
                "vitest run" => "npx vitest run ",
                "jest" => "npx jest ",
                _ => return None,
            };
            // Keep npm's lifecycle hooks and environment, passing just the
            // selected file arguments through to that known test script.
            let args = command.strip_prefix(runner)?;
            return Some(format!("npm test -- {args}"));
        }
        _ => false,
    };
    (matches_runner && command != suite).then_some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run::project::ProjectCheck;

    fn failure() -> CheckResult {
        let mut result = CheckResult::pending(&ProjectCheck {
            kind: "test".into(),
            command: "python -m pytest".into(),
            enabled: true,
            reuse_success: false,
        });
        result.status = "failed";
        result.exit_code = Some(Some(1));
        result.tests_ran = Some(12);
        result.failures = Some(vec!["test|test|tests/test_a.py::test_a".into()]);
        result
    }

    #[test]
    fn custom_commands_and_runner_options_are_never_dropped() {
        let root = Path::new(".");
        let files = vec!["tests/test_a.py".into()];
        assert_eq!(
            compatible_command(root, "python -m pytest", &files).as_deref(),
            Some("python -m pytest tests/test_a.py")
        );
        for suite in [
            "python -m pytest -m integration",
            "ENV=x python -m pytest",
            "./test.sh",
            "cargo test",
            "python -m pytest && echo done",
        ] {
            assert_eq!(compatible_command(root, suite, &files), None, "{suite}");
        }
    }

    #[test]
    fn only_executed_test_failures_can_defer_the_full_suite() {
        let result = failure();
        assert!(actionable_failure(&result));
        let mut no_tests = result.clone();
        no_tests.tests_ran = Some(0);
        assert!(!actionable_failure(&no_tests));
        let mut environment = result.clone();
        environment.environment = Some("missing pytest".into());
        assert!(!actionable_failure(&environment));
        let mut timeout = result.clone();
        timeout.timed_out = Some(true);
        assert!(!actionable_failure(&timeout));
        let mut not_started = result.clone();
        not_started.exit_code = None;
        assert!(!actionable_failure(&not_started));
        let mut collection = result;
        collection.failures = Some(vec!["test|tests/test_a.py|ImportError".into()]);
        assert!(!actionable_failure(&collection));
    }

    #[test]
    fn unselected_failures_and_full_test_counts_are_not_mistaken_for_progress() {
        let previous = RoundFailures {
            keys: ["test|test|a".into(), "test|test|b".into()].into(),
            ran: ["test".into()].into(),
            tests_ran: Some(100),
            own_tests: Some(0),
        };
        let mut focused = RoundFailures {
            keys: ["test|test|a".into()].into(),
            ran: ["test".into()].into(),
            tests_ran: Some(1),
            own_tests: Some(0),
        };
        State::default().keep_unobserved_failures(Some(&previous), &mut focused);
        assert_eq!(focused.keys, previous.keys);
        assert_eq!(focused.tests_ran, None);
        assert_eq!(
            repair::judge(std::slice::from_ref(&previous), &focused).verdict,
            repair::Verdict::First
        );
        assert!(repair::fewer_tests_flag(&[previous], &focused).is_none());
    }
    #[test]
    fn improving_partial_repairs_do_not_spend_the_autopilot_stall_budget() {
        let initial = RoundFailures {
            keys: ["test|test|a".into(), "test|test|b".into()].into(),
            ran: ["test".into(), "lint".into()].into(),
            tests_ran: Some(100),
            own_tests: Some(0),
        };
        let state = State {
            failed: true,
            ..State::default()
        };
        let mut verification = super::super::VerificationState::default();
        let mut history = vec![initial];
        for key in ["test|test|a", "test|test|b"] {
            let mut now = RoundFailures {
                keys: [key.into()].into(),
                ran: ["test".into(), "lint".into()].into(),
                tests_ran: Some(1),
                own_tests: Some(0),
            };
            state.keep_unobserved_failures(history.last(), &mut now);
            let judgement = state.judge(&history, &now);
            assert_eq!(judgement.verdict, repair::Verdict::First);
            assert!(!verification.progress_judged(true, true, judgement.verdict, false));
            history.push(now);
        }
        assert_eq!(verification.stalled_rounds, 0);
    }
}

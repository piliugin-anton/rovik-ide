//! A failed check against the code before the change.
//!
//! A project's checks may fail before the agent changes anything: type
//! errors, lint errors, tests the project already has broken. React Native
//! apps often do (their bundler never type-checks, and Expo's own template
//! fails its lint), and so do many others. Those failures aren't the
//! change's: sent to the agent, they cost it its fix rounds on code it
//! never touched, tempt it to change unrelated code, and keep its changes
//! from landing.
//!
//! So a check that fails runs again on the code before the change, once a
//! run ([`Befores`]), and its failures are split
//! (`command_output::split_by_before`): what fails there too is set apart
//! (`CheckResult::preexisting`), not sent back to the agent and not counted
//! between rounds; a check none of whose failures is the change's is
//! `preexisting`, and holds nothing back. Failures are told apart by their
//! keys (an error's file, code and message; a test's name), each path taken
//! relative to where its check ran; output with nothing to key compares by
//! its failure lines.
//!
//! Where the code before the change is ([`Run::code_before`]): an isolated
//! chat's project, which a throwaway overlay shows as it is, the chat's
//! changes left out ([`BeforeCopy`]). Not compared: a chat that works in
//! the project itself or on a server (there's no copy of the code before),
//! the round that checks the project's own new changes merged into the
//! chat's (that merge is what it checks), and a check that timed out or
//! failed on its environment (a program that isn't installed fails the same
//! way anywhere, and says nothing of the code). To compare in another kind
//! of chat, give `code_before` that chat's copy of the code before.

use super::command_output::{cap_keys, environment_failure, failed_in, split_by_before, Failed};
use super::run::Run;
use super::verify::{check_timeout_ms, CheckResult};
use crate::command_tool::CommandOutput;
use crate::overlay::chats::{self, ChatOverlay};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a check's record says while it runs on the code before the change.
pub const RUNNING_BEFORE: &str = "running on the code before the change…";

/// How a check went on the code before the change.
#[derive(Clone, Debug, PartialEq)]
pub enum Before {
    /// It passed there: its failures are the change's.
    Passed,
    /// It failed there, with these failures.
    Failed(Failed),
    /// It can't say: cut short, it couldn't run, or it failed on its
    /// environment.
    Unknown,
}

impl Before {
    /// What a check's run on the code before the change says, its paths
    /// relative to one of `roots`.
    pub fn of(kind: &str, out: Result<CommandOutput, String>, roots: &[String]) -> Before {
        let Ok(out) = out else {
            return Before::Unknown;
        };
        if out.stopped || out.timed_out {
            return Before::Unknown;
        }
        if out.exit_code == Some(0) {
            return Before::Passed;
        }
        let text = out.text();
        if environment_failure(out.exit_code, &text, roots).is_some() {
            return Before::Unknown;
        }
        let whole = out.whole_text().unwrap_or(text);
        Before::Failed(failed_in(kind, &whole, roots))
    }
}

/// The run's checks on the code before the change, by command: each runs
/// there once a run, the first time it fails.
#[derive(Clone, Debug, Default)]
pub struct Befores(HashMap<String, Before>);

impl Befores {
    pub fn get(&self, command: &str) -> Option<&Before> {
        self.0.get(command.trim())
    }

    pub fn insert(&mut self, command: &str, before: Before) {
        self.0.insert(command.trim().to_string(), before);
    }
}

/// A failed check (`check`, its whole output `output`, run in one of
/// `roots`) with what failed before the change set apart: the failures it
/// had there too go to `preexisting`, out of its `failures`; when none is
/// left, it's `preexisting` itself. True: some were set apart.
pub fn set_apart(
    check: &mut CheckResult,
    before: Option<&Before>,
    output: &str,
    roots: &[String],
) -> bool {
    let Some(Before::Failed(before)) = before else {
        return false;
    };
    let now = failed_in(&check.kind, output, roots);
    let (own, known) = split_by_before(&check.kind, &now, before);
    if known.is_empty() {
        return false;
    }
    check.preexisting = Some(cap_keys(&check.kind, known));
    if own.is_empty() {
        check.status = "preexisting";
        check.failures = None;
    } else {
        check.failures = Some(cap_keys(&check.kind, own));
    }
    true
}

/// What the agent hears when nothing its change did fails, but some
/// checks fail as they do on the code before it: which, and their
/// failures. Fixing them may be its task ("fix the tests"), which only it
/// knows: if so it goes on, else it finishes, and with nothing changed the
/// run is verified as it stands. None when no check fails so.
pub fn note(results: &[CheckResult]) -> Option<String> {
    let as_before: Vec<&CheckResult> = results
        .iter()
        .filter(|r| r.status == "preexisting")
        .collect();
    if as_before.is_empty() {
        return None;
    }
    let mut lines = vec![
        "[Automatic check by the IDE, not written by the user]".to_string(),
        "Nothing your change did fails the checks. These fail as they do on the code before your change:".to_string(),
    ];
    for r in as_before {
        lines.push(String::new());
        lines.push(format!(
            "## {} (`{}`)",
            super::project::check_label(&r.kind),
            r.command
        ));
        lines.push(super::repair::key_list(
            r.preexisting.as_deref().unwrap_or_default(),
            10,
        ));
    }
    lines.push(String::new());
    lines.push(
        "They aren't yours to fix unless fixing them is what you were asked to do. If it is, \
         fix them now: the checks run again when you finish. If it isn't, finish now without \
         changing anything, and mention them in your final message."
            .into(),
    );
    Some(lines.join("\n"))
}

/// The folders a command may print paths under: each of `paths` as given
/// and as the file system has it (a link resolved, macOS's `/private`).
pub fn roots_of(paths: &[&Path]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for path in paths {
        let canonical = path.canonicalize().ok();
        for p in std::iter::once(path.to_path_buf()).chain(canonical) {
            let p = p.to_string_lossy().into_owned();
            if !p.is_empty() && !roots.contains(&p) {
                roots.push(p);
            }
        }
    }
    roots
}

/// The throwaway overlay of the project a round's checks run in on the code
/// before the change: opened for the first that needs it, removed when the
/// round's checks are done ([`BeforeCopy::discard`]), or when it's dropped.
pub struct BeforeCopy {
    id: Option<String>,
    overlay: Arc<ChatOverlay>,
}

impl BeforeCopy {
    /// A copy of `project` as it is, mounted; None where it can't be.
    async fn open(project: &Path) -> Option<BeforeCopy> {
        let id = format!("{}{}", chats::SCRATCH_PREFIX, uuid::Uuid::new_v4());
        let (open_id, base) = (id.clone(), project.to_path_buf());
        let opened = crate::rt::spawn_blocking(move || chats::open_scratch(&open_id, &base)).await;
        match opened {
            Ok(Ok(overlay)) => Some(BeforeCopy {
                id: Some(id),
                overlay,
            }),
            _ => {
                // What it made before it failed (its folder) goes too.
                let _ = crate::rt::spawn_blocking(move || chats::discard_child(&id)).await;
                None
            }
        }
    }

    /// Unmounted and gone, now.
    pub async fn discard(mut self) {
        if let Some(id) = self.id.take() {
            let _ = crate::rt::spawn_blocking(move || chats::discard_child(&id)).await;
        }
    }
}

impl Drop for BeforeCopy {
    fn drop(&mut self) {
        // A round that ended early: unmounting waits on the driver, so not
        // on this thread.
        if let Some(id) = self.id.take() {
            crate::rt::spawn_blocking(move || chats::discard_child(&id));
        }
    }
}

impl Run {
    /// Where the code before the change is, when a failed check can be
    /// compared with it: an isolated chat's project, except on the round
    /// that checks the project's new changes merged in (module docs).
    pub(super) fn code_before(&self) -> Option<PathBuf> {
        if !self.isolated() || self.merged_from_project.is_some() {
            return None;
        }
        self.project_root.as_deref().map(PathBuf::from)
    }

    /// Where this run's checks run, as paths in their output may name it:
    /// its root (an isolated chat's view) and the project, which its
    /// sandbox may show the view at.
    pub(super) fn check_roots(&self) -> Vec<String> {
        let paths: Vec<&Path> = [self.root.as_deref(), self.project_root.as_deref()]
            .into_iter()
            .flatten()
            .map(Path::new)
            .collect();
        roots_of(&paths)
    }

    /// `check`, failed, run on the code before the change at `project`
    /// (in `copy`, opened if it isn't yet): how it went there.
    pub(super) async fn check_before(
        &self,
        check: &CheckResult,
        project: &Path,
        copy: &mut Option<BeforeCopy>,
    ) -> Before {
        if copy.is_none() {
            *copy = BeforeCopy::open(project).await;
        }
        let Some(c) = copy.as_ref() else {
            return Before::Unknown;
        };
        let stop = self.stop.clone();
        // Kept whole, as the round's own are: every failure counts.
        let queued =
            crate::command_tool::Queued::check(&self.dialog_id, Some(&c.overlay)).whole(true);
        let out = crate::command_tool::run_command(
            c.overlay.view.to_string_lossy().into_owned(),
            check.command.clone(),
            Some(check_timeout_ms(&self.dialog_id)),
            move || stop.is_set(),
            Some(queued),
        )
        .await;
        Before::of(&check.kind, out, &roots_of(&[&c.overlay.view, project]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run::command_output::without_roots;
    use crate::agent_run::project::ProjectCheck;

    fn failed(kind: &str, output: &str) -> CheckResult {
        let mut r = CheckResult::pending(&ProjectCheck {
            kind: kind.into(),
            command: "x".into(),
            enabled: true,
        });
        r.status = "failed";
        r.failures = Some(super::super::command_output::failure_keys(
            kind, output, false,
        ));
        r
    }

    fn output(code: i32, stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            exit_code: Some(code),
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            stopped: false,
            truncated: false,
            duration_ms: 1,
            whole: None,
            waited: None,
        })
    }

    const ROOTS: &[&str] = &["/run/chats/c1", "/home/me/app"];

    fn roots() -> Vec<String> {
        ROOTS.iter().map(|r| r.to_string()).collect()
    }

    /// The same errors before the change, printed under the copy's own
    /// folder: the check fails as it did, and isn't the change's.
    #[test]
    fn a_check_failing_as_before_the_change_is_preexisting() {
        let before = Before::of(
            "lint",
            output(
                1,
                "/run/chats/red-1/src/a.ts\n  3:1  error  'x' is unused  no-unused-vars\n\n✖ 1 problem (1 error, 0 warnings)",
            ),
            &["/run/chats/red-1".to_string()],
        );
        let now = "/run/chats/c1/src/a.ts\n  4:1  error  'x' is unused  no-unused-vars\n\n✖ 1 problem (1 error, 0 warnings)";
        let mut r = failed("lint", &without_roots(now, &roots()));
        assert!(set_apart(&mut r, Some(&before), now, &roots()));
        assert_eq!(r.status, "preexisting");
        assert_eq!(r.failures, None);
        assert_eq!(
            r.preexisting,
            Some(vec![
                "lint|src/a.ts|no-unused-vars|'x' is unused".to_string()
            ])
        );
    }

    /// A new error beside an old one: the check still fails, on the new
    /// one alone; the old one is set apart.
    #[test]
    fn only_the_changes_own_failures_are_left() {
        let before = Before::of(
            "typecheck",
            output(2, "src/old.ts(1,1): error TS2304: Cannot find name 'a'."),
            &[],
        );
        let now = "src/old.ts(1,1): error TS2304: Cannot find name 'a'.\nsrc/new.ts(9,3): error TS2322: Type 'string' is not assignable to type 'number'.";
        let mut r = failed("typecheck", now);
        assert!(set_apart(&mut r, Some(&before), now, &[]));
        assert_eq!(r.status, "failed");
        assert_eq!(
            r.failures,
            Some(vec![
                "typecheck|src/new.ts|TS2322|Type 'string' is not assignable to type 'number'."
                    .to_string()
            ])
        );
        assert_eq!(
            r.preexisting,
            Some(vec![
                "typecheck|src/old.ts|TS2304|Cannot find name 'a'.".to_string()
            ])
        );
    }

    /// Nothing to compare with: a pass before the change, a run cut short,
    /// or one that failed on its environment.
    #[test]
    fn without_failures_to_compare_the_check_stands() {
        let now = "src/a.ts(1,1): error TS2304: Cannot find name 'a'.";
        let cut = Ok(CommandOutput {
            timed_out: true,
            ..output(1, now).unwrap()
        });
        let befores = [
            Before::of("typecheck", output(0, ""), &[]),
            Before::of("typecheck", cut, &[]),
            Before::of("typecheck", output(127, "sh: 1: tsc: not found"), &[]),
            Before::of("typecheck", Err("couldn't start".into()), &[]),
        ];
        assert_eq!(befores[0], Before::Passed);
        for before in &befores {
            let mut r = failed("typecheck", now);
            let kept = r.clone();
            assert!(!set_apart(&mut r, Some(before), now, &[]));
            assert_eq!(r, kept);
        }
        assert_eq!(
            befores[1..],
            [Before::Unknown, Before::Unknown, Before::Unknown]
        );
        let mut r = failed("typecheck", now);
        assert!(!set_apart(&mut r, None, now, &[]));
    }

    /// Output with nothing to key: the same failure lines before the change
    /// set it apart (durations aside); one line more is the change's.
    #[test]
    fn output_with_nothing_to_key_compares_by_its_failure_lines() {
        let before = Before::of(
            "test",
            output(1, "Setup failed: no device\nDone in 1.2s."),
            &[],
        );
        let same = "Setup failed: no device\nDone in 3.4s.";
        let mut r = failed("test", same);
        assert!(set_apart(&mut r, Some(&before), same, &[]));
        assert_eq!(r.status, "preexisting");
        let more = "Setup failed: no device\nAnother step failed\nDone in 3.4s.";
        let mut r = failed("test", more);
        assert!(!set_apart(&mut r, Some(&before), more, &[]));
        assert_eq!(r.status, "failed");
        assert_eq!(r.failures, Some(vec!["test|failed".to_string()]));
    }

    /// The agent hears which checks fail only as before its change, and
    /// that they're its to fix only if that's what it was asked: nothing
    /// to tell when none does.
    #[test]
    fn the_note_names_what_fails_as_before() {
        let mut lint = failed("lint", "");
        lint.command = "yarn run lint".into();
        assert_eq!(note(std::slice::from_ref(&lint)), None);
        lint.status = "preexisting";
        lint.preexisting = Some(vec!["lint|src/a.ts|no-unused-vars|'x' is unused".into()]);
        let text = note(&[lint]).unwrap();
        assert!(text.starts_with(
            "[Automatic check by the IDE, not written by the user]\nNothing your change did fails the checks."
        ));
        assert!(
            text.contains("## Lint (`yarn run lint`)\n- src/a.ts|no-unused-vars|'x' is unused"),
            "{text}"
        );
        assert!(text.ends_with("and mention them in your final message."));
    }

    /// A folder as given and as the file system has it, once; a check's
    /// command by its text, trimmed.
    #[test]
    fn roots_and_commands() {
        let dir = std::env::temp_dir();
        let roots = roots_of(&[&dir, &dir]);
        assert_eq!(roots[0], dir.to_string_lossy());
        assert!(roots.len() <= 2, "{roots:?}");
        assert_eq!(
            without_roots(
                "/home/me/app/src/a.ts and /run/chats/c1/src/b.ts",
                &self::roots()
            ),
            "src/a.ts and src/b.ts"
        );
        let mut befores = Befores::default();
        befores.insert(" yarn run lint ", Before::Passed);
        assert_eq!(befores.get("yarn run lint"), Some(&Before::Passed));
    }
}

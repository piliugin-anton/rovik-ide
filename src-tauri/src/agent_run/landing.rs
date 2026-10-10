//! When an isolated chat's run ends: its changes land in the project if
//! they can without asking, otherwise the chat says why and waits for the
//! user (src/overlay, IsolationBar.tsx).
//!
//! They land on their own when the run finished (its checks passed, or
//! there were none to run) and every changed file merges cleanly with the
//! project. If the project changed some of the same files meanwhile, the
//! merged result is checked first, before the run ends: it's written into
//! the chat's view and the run's checks run again there; if they fail, the
//! chat gets the failures to fix, as with its own ([`Run::take_project_changes`]).
//! Anything else (conflicts, failing checks, a run that stopped or failed)
//! is left for the user: resolve the conflicts or hand them to the chat,
//! land anyway, or discard.
//!
//! While many files change in the project at once (a branch switch, a
//! pull), isolated runs wait before their next step and before landing.

use super::events::{emit, AgentEvent};
use super::run::Run;
use crate::overlay::chats::{self, Pending};
use crate::overlay::land::{self, LandError};
use tauri::Runtime;

/// The longest a run waits for the project to settle.
const MAX_BUSY_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Run outcomes after which changes may land without asking.
const LANDS_AFTER: &[&str] = &["done", "verified", "unverified"];

impl<R: Runtime> Run<R> {
    pub(super) async fn land_at_end(&mut self, outcome: &str) {
        let result = self.try_land(outcome).await;
        if let Some((what, files)) = result {
            super::log::write(
                &self.app,
                serde_json::json!({ "event": "landing", "dialogId": self.dialog_id, "result": what, "files": files }),
            );
        }
    }

    /// Lands if it may; returns what happened and to how many files, for
    /// the log (None: nothing to land).
    async fn try_land(&mut self, outcome: &str) -> Option<(&'static str, usize)> {
        if self.root == self.project_root {
            return None;
        }
        let overlay = chats::get(&self.dialog_id)?;
        overlay.fs.flush();
        if LANDS_AFTER.contains(&outcome) {
            self.wait_while_project_busy().await;
        }
        let dialog_id = self.dialog_id.clone();
        let changes = chats::changes(&dialog_id);
        if changes.is_empty() {
            chats::set_pending(&dialog_id, None);
            return None;
        }
        let count = changes.len();
        if !LANDS_AFTER.contains(&outcome) {
            let reason = match outcome {
                "stopped" => "the run was stopped",
                "verify-gave-up" | "verify-unresolved" => "the checks still fail",
                "verify-needs-approval" => "the checks wait for your approval",
                "unfinished-checklist" => "the checklist isn't finished",
                _ => "the run didn't finish",
            };
            chats::set_pending(&dialog_id, Some(&Pending { reason: format!("Not landed: {reason}."), conflicts: Vec::new() }));
            return Some(("waits: run didn't finish", count));
        }

        let marked = chats::markers_left(&dialog_id);
        if !marked.is_empty() {
            let list = marked.join(", ");
            chats::set_pending(
                &dialog_id,
                Some(&Pending { reason: format!("Not landed: conflict markers are still in {list}."), conflicts: Vec::new() }),
            );
            self.notice("warning", "This chat's changes didn't land", &format!("Conflict markers are still in {list}."));
            return Some(("waits: markers left", count));
        }

        let plan = land::plan(&overlay.project, &changes, false);
        if !plan.conflicts.is_empty() {
            let n = plan.conflicts.len();
            chats::set_pending(
                &dialog_id,
                Some(&Pending {
                    reason: format!("Not landed: {n} file{} changed in the project too, on the same lines.", if n == 1 { "" } else { "s" }),
                    conflicts: plan.conflicts.clone(),
                }),
            );
            self.notice("warning", "This chat's changes didn't land", "They conflict with changes in the project. Review them above the message box.");
            return Some(("waits: conflicts", count));
        }
        if plan.merged {
            if let Some(failed) = self.check_merged(&plan, &overlay.view).await {
                chats::set_pending(
                    &dialog_id,
                    Some(&Pending {
                        reason: format!("Not landed: `{failed}` fails once merged with the project's newer changes."),
                        conflicts: Vec::new(),
                    }),
                );
                self.notice("warning", "This chat's changes didn't land", "The checks fail on the merged result.");
                return Some(("waits: merged checks fail", count));
            }
        }
        let result = tauri::async_runtime::spawn_blocking(move || chats::land_now(&dialog_id, false)).await;
        match result {
            Ok(Ok(Some(landed))) => {
                let n = landed.paths.len();
                self.notice(
                    "success",
                    "Landed",
                    &format!("{n} change{} from this chat are in the project. Undo is above the message box.", if n == 1 { "" } else { "s" }),
                );
                // The project changed under the chat's view: the next run
                // starts from it.
                self.landed = true;
                Some(("landed", n))
            }
            Ok(Ok(None)) => Some(("nothing new", count)),
            Ok(Err(LandError::Conflicts(_))) => {
                self.notice("warning", "This chat's changes didn't land", "The project changed meanwhile. Review them above the message box.");
                Some(("waits: conflicts", count))
            }
            Ok(Err(e)) => {
                self.notice("error", "This chat's changes didn't land", &e.to_string());
                Some(("failed", count))
            }
            Err(e) => {
                self.notice("error", "This chat's changes didn't land", &e.to_string());
                Some(("failed", count))
            }
        }
    }

    /// Runs the project's checks on the merged result in the chat's view.
    /// Returns the first failing check's command.
    async fn check_merged(&self, plan: &land::Plan, view: &std::path::Path) -> Option<String> {
        let p = self.state.config().permissions;
        let checks: Vec<_> = self.checks.iter().filter(|c| c.enabled && !c.command.trim().is_empty()).collect();
        if !p.verify_changes || checks.is_empty() {
            return None;
        }
        // Into the chat's files, merged against the project as it is now:
        // what's checked is what lands.
        if let Err(e) = chats::take_merged(&self.dialog_id, plan) {
            return Some(format!("(writing the merged files: {e})"));
        }
        for check in super::project::ordered_checks(&checks.into_iter().cloned().collect::<Vec<_>>()) {
            if let super::permissions::Verdict::Refused(_) = super::permissions::check_command(&check.command, &p) {
                continue;
            }
            let stop = self.stop.clone();
            let queued = crate::command_tool::Queued {
                kind: crate::workload::JobKind::Landing { owner: self.dialog_id.clone() },
                note: Box::new(|_| {}),
                env: chats::get(&self.dialog_id).map(|o| o.command_env()).unwrap_or_default(),
            };
            let out = crate::command_tool::run_command(
                view.to_string_lossy().into_owned(),
                check.command.clone(),
                Some(600_000),
                move || stop.is_set(),
                Some(queued),
            )
            .await;
            match out {
                Ok(o) if o.exit_code == Some(0) && !o.timed_out => {}
                _ => return Some(check.command.clone()),
            }
        }
        None
    }

    /// When the run is about to end: if the project changed files the chat
    /// changed too, both changes are merged into the chat's files and the
    /// checks run again on the result. Returns the run's new outcome:
    /// "continue" when they failed and the chat got the failures to fix.
    /// None: nothing to merge (or the run won't land anyway).
    pub(super) async fn take_project_changes(&mut self, outcome: &str) -> Result<Option<String>, String> {
        if !LANDS_AFTER.contains(&outcome) || self.root == self.project_root {
            return Ok(None);
        }
        let Some(overlay) = chats::get(&self.dialog_id) else { return Ok(None) };
        self.wait_while_project_busy().await;
        overlay.fs.flush();
        let plan = land::plan(&overlay.project, &chats::changes(&self.dialog_id), false);
        // Conflicts are the user's (or, if they say so, the chat's) to settle.
        if !plan.merged || !plan.conflicts.is_empty() {
            return Ok(None);
        }
        let files = chats::take_merged(&self.dialog_id, &plan)?;
        if files.is_empty() {
            return Ok(None);
        }
        super::log::write(
            &self.app,
            serde_json::json!({ "event": "merged-project-changes", "dialogId": self.dialog_id, "files": files }),
        );
        self.merged_from_project = Some(files);
        self.writes_since_verify = true;
        let outcome = self.verify_at_end(false, None).await?;
        self.merged_from_project = None;
        Ok(Some(outcome))
    }

    /// Waits while many files change in the project at once (a branch
    /// switch, a pull): the chat's view would show a half-switched project.
    /// Says so, once, if it has to wait.
    pub(super) async fn wait_while_project_busy(&self) {
        let Some(project) = self.project_root.as_deref().filter(|_| self.root != self.project_root) else { return };
        let project = std::path::Path::new(project);
        if !chats::is_busy(project) {
            return;
        }
        let started = std::time::Instant::now();
        self.notice(
            "info",
            "Waiting for the project",
            "Many files are changing in the project (a branch switch or a pull?). This chat goes on once they settle.",
        );
        // Not forever: a watcher that keeps reporting must not hang the run.
        while chats::is_busy(project) && !self.stopped() && started.elapsed() < MAX_BUSY_WAIT {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        super::log::write(
            &self.app,
            serde_json::json!({ "event": "project-busy", "dialogId": self.dialog_id, "ms": started.elapsed().as_millis() as u64 }),
        );
    }

    fn notice(&self, kind: &str, title: &str, message: &str) {
        emit(&self.app, AgentEvent::Notice { kind: kind.into(), title: title.into(), message: message.into() });
    }
}

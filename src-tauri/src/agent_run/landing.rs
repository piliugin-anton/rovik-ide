//! When an isolated chat's run ends: its changes land in the project if
//! they can without asking, otherwise the chat says why and waits for the
//! user (src/overlay, IsolationBar.tsx).
//!
//! They land on their own when the run finished (its checks passed, or
//! there were none to run) and every changed file merges cleanly with the
//! project. If the project changed some of the same files meanwhile, the
//! merged result is checked first: it's written into the chat's view and
//! the project's checks run there, ahead of other chats' work in the queue
//! for heavy work. Anything else (conflicts, failing checks, a run that
//! stopped or failed) is left for the user to land anyway or discard.

use super::events::{emit, AgentEvent};
use super::run::Run;
use crate::overlay::chats::{self, Pending};
use crate::overlay::land::{self, LandError};
use tauri::Runtime;

/// Run outcomes after which changes may land without asking.
const LANDS_AFTER: &[&str] = &["done", "verified", "unverified"];

impl<R: Runtime> Run<R> {
    pub(super) async fn land_at_end(&mut self, outcome: &str) {
        if self.root == self.project_root {
            return;
        }
        let Some(overlay) = chats::get(&self.dialog_id) else { return };
        overlay.fs.flush();
        let dialog_id = self.dialog_id.clone();
        let changes = chats::changes(&dialog_id);
        if changes.is_empty() {
            chats::set_pending(&dialog_id, None);
            return;
        }
        if !LANDS_AFTER.contains(&outcome) {
            let reason = match outcome {
                "stopped" => "the run was stopped",
                "verify-gave-up" | "verify-unresolved" => "the checks still fail",
                "verify-needs-approval" => "the checks wait for your approval",
                "unfinished-checklist" => "the checklist isn't finished",
                _ => "the run didn't finish",
            };
            chats::set_pending(&dialog_id, Some(&Pending { reason: format!("Not landed: {reason}."), conflicts: Vec::new() }));
            return;
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
            return;
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
                return;
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
            }
            Ok(Ok(None)) => {}
            Ok(Err(LandError::Conflicts(_))) => {
                self.notice("warning", "This chat's changes didn't land", "The project changed meanwhile. Review them above the message box.");
            }
            Ok(Err(e)) => self.notice("error", "This chat's changes didn't land", &e.to_string()),
            Err(e) => self.notice("error", "This chat's changes didn't land", &e.to_string()),
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
        if let Err(e) = plan.write_into(view) {
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

    fn notice(&self, kind: &str, title: &str, message: &str) {
        emit(&self.app, AgentEvent::Notice { kind: kind.into(), title: title.into(), message: message.into() });
    }
}

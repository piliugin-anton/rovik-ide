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
const LANDS_AFTER: &[&str] = &["done", "verified", "unverified", "verify-unverified"];

/// Outcomes that land without any check having run.
const UNCHECKED: &[&str] = &["unverified", "verify-unverified"];

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
        // Plan mode only reads: what the chat changed before waits as it was.
        if self.root == self.project_root || self.plan_mode {
            return None;
        }
        let overlay = chats::get(&self.dialog_id)?;
        if LANDS_AFTER.contains(&outcome) {
            self.wait_while_project_busy().await;
        }
        let dialog_id = self.dialog_id.clone();
        // Reading and merging files: off the async threads.
        let read = {
            let (o, id) = (overlay.clone(), dialog_id.clone());
            tauri::async_runtime::spawn_blocking(move || {
                o.fs.flush();
                (chats::changes(&id), chats::markers_left(&id))
            })
            .await
        };
        // Couldn't even look: nothing is decided, and the pending state the
        // chat has stays as it is.
        let (changes, marked) = match read {
            Ok(r) => r,
            Err(e) => {
                self.notice(
                    "error",
                    "This chat's changes didn't land",
                    &format!("Its changes couldn't be read: {e}"),
                );
                return Some(("failed", 0));
            }
        };
        if changes.is_empty() {
            chats::set_pending(&dialog_id, None);
            return None;
        }
        let count = changes.len();
        if !LANDS_AFTER.contains(&outcome) {
            let reason = match outcome {
                "stopped" | "verify-stopped" => "the run was stopped",
                "repetition" => "the model kept repeating itself",
                "error" => "the run ended with an error",
                "verify-gave-up" | "verify-unresolved" => "the checks still fail",
                "verify-flagged" => {
                    "the change weakens tests or what the checks run: review it first"
                }
                "verify-needs-approval" => "the checks wait for your approval",
                "unfinished-checklist" => "the checklist isn't finished",
                _ => "the run didn't finish",
            };
            chats::set_pending(
                &dialog_id,
                Some(&Pending {
                    reason: format!("Not landed: {reason}."),
                    conflicts: Vec::new(),
                }),
            );
            return Some(("waits: run didn't finish", count));
        }

        if !marked.is_empty() {
            let list = marked.join(", ");
            chats::set_pending(
                &dialog_id,
                Some(&Pending {
                    reason: format!("Not landed: conflict markers are still in {list}."),
                    conflicts: Vec::new(),
                }),
            );
            self.notice(
                "warning",
                "This chat's changes didn't land",
                &format!("Conflict markers are still in {list}."),
            );
            return Some(("waits: markers left", count));
        }

        let plan = {
            let project = overlay.project.clone();
            match tauri::async_runtime::spawn_blocking(move || {
                land::plan(&project, &changes, false)
            })
            .await
            {
                Ok(plan) => plan,
                Err(e) => {
                    self.notice(
                        "error",
                        "This chat's changes didn't land",
                        &format!("Planning the landing failed: {e}"),
                    );
                    return Some(("failed", count));
                }
            }
        };
        if !plan.conflicts.is_empty() {
            let n = plan.conflicts.len();
            chats::set_pending(
                &dialog_id,
                Some(&Pending {
                    reason: format!(
                        "Not landed: {n} file{} changed in the project too, on the same lines.",
                        if n == 1 { "" } else { "s" }
                    ),
                    conflicts: plan.conflicts.clone(),
                }),
            );
            self.notice(
                "warning",
                "This chat's changes didn't land",
                "They conflict with changes in the project. Review them above the message box.",
            );
            return Some(("waits: conflicts", count));
        }
        let p = self.state.config().permissions;
        // Checks that would run: set up, and allowed by the command rules.
        let has_checks = p.verify_changes
            && self.checks.iter().any(|c| {
                c.enabled
                    && !c.command.trim().is_empty()
                    && !matches!(
                        super::permissions::check_command(&c.command, &p),
                        super::permissions::Verdict::Refused(_)
                    )
            });
        if plan.merged && has_checks && p.command_approval.required {
            // The merged result's checks are commands too: not without the
            // user's go. Land (in the bar) lands it as it is.
            chats::set_pending(
                &dialog_id,
                Some(&Pending {
                    reason: "Not landed: the project changed the same files meanwhile, and checking the merged result needs your approval. Land takes it as it is.".into(),
                    conflicts: Vec::new(),
                }),
            );
            self.notice(
                "warning",
                "This chat's changes didn't land",
                "The merged result's checks wait for your approval.",
            );
            return Some(("waits: merged checks need approval", count));
        }
        if plan.merged {
            if let Some(failed) = self.check_merged(&plan, &overlay.view).await {
                // Stopped while checking (Stop, or the chat deleted): not a
                // failure of the checks.
                if self.stopped() {
                    chats::set_pending(
                        &dialog_id,
                        Some(&Pending {
                            reason: "Not landed: the run was stopped.".into(),
                            conflicts: Vec::new(),
                        }),
                    );
                    return Some(("waits: stopped", count));
                }
                chats::set_pending(
                    &dialog_id,
                    Some(&Pending {
                        reason: format!("Not landed: `{failed}` fails once merged with the project's newer changes."),
                        conflicts: Vec::new(),
                    }),
                );
                self.notice(
                    "warning",
                    "This chat's changes didn't land",
                    "The checks fail on the merged result.",
                );
                return Some(("waits: merged checks fail", count));
            }
        }
        // With checks, only what they checked lands: a file the project
        // changed again meanwhile waits.
        let result = tauri::async_runtime::spawn_blocking(move || {
            chats::land_now_with(&dialog_id, false, !has_checks)
        })
        .await;
        match result {
            Ok(Ok(Some(landed))) => {
                let n = landed.paths.len();
                let unchecked = if UNCHECKED.contains(&outcome) {
                    " No checks ran: none are set up or allowed."
                } else {
                    ""
                };
                self.notice(
                    "success",
                    "Landed",
                    &format!(
                        "{n} change{} from this chat are in the project.{unchecked} Undo is above the message box.",
                        if n == 1 { "" } else { "s" }
                    ),
                );
                // The project changed under the chat's view: the next run
                // starts from it.
                self.landed = true;
                Some(("landed", n))
            }
            Ok(Ok(None)) => Some(("nothing new", count)),
            Ok(Err(e @ LandError::Unchecked(_))) => {
                let reason = format!(
                    "Not landed: {e}. Land (above) takes it as it is, unchecked; or send the chat a message, and its checks run on the new result when it finishes."
                );
                chats::set_pending(
                    &self.dialog_id,
                    Some(&Pending {
                        reason,
                        conflicts: Vec::new(),
                    }),
                );
                self.notice(
                    "warning",
                    "This chat's changes didn't land",
                    "The project changed the same files again while the checks ran.",
                );
                Some(("waits: changed again", count))
            }
            Ok(Err(LandError::Conflicts(_))) => {
                self.notice(
                    "warning",
                    "This chat's changes didn't land",
                    "The project changed meanwhile. Review them above the message box.",
                );
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
        let checks: Vec<_> = self
            .checks
            .iter()
            .filter(|c| c.enabled && !c.command.trim().is_empty())
            .collect();
        if !p.verify_changes || checks.is_empty() {
            return None;
        }
        // Into the chat's files, merged against the project as it is now:
        // what's checked is what lands.
        let taken = {
            let (id, files) = (self.dialog_id.clone(), plan.clone());
            tauri::async_runtime::spawn_blocking(move || chats::take_merged(&id, &files))
                .await
                .map_err(|e| e.to_string())
        };
        if let Err(e) = taken.and_then(|r| r) {
            return Some(format!("(writing the merged files: {e})"));
        }
        for check in
            super::project::ordered_checks(&checks.into_iter().cloned().collect::<Vec<_>>())
        {
            if let super::permissions::Verdict::Refused(_) =
                super::permissions::check_command(&check.command, &p)
            {
                continue;
            }
            let stop = self.stop.clone();
            let queued = crate::command_tool::Queued {
                kind: crate::workload::JobKind::Landing {
                    owner: self.dialog_id.clone(),
                },
                note: Box::new(|_| {}),
                env: chats::get(&self.dialog_id)
                    .map(|o| o.command_env())
                    .unwrap_or_default(),
                confine: chats::get(&self.dialog_id).and_then(|o| o.confine()),
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
    pub(super) async fn take_project_changes(
        &mut self,
        outcome: &str,
    ) -> Result<Option<String>, String> {
        if !LANDS_AFTER.contains(&outcome) || self.root == self.project_root || self.plan_mode {
            return Ok(None);
        }
        let Some(overlay) = chats::get(&self.dialog_id) else {
            return Ok(None);
        };
        self.wait_while_project_busy().await;
        let (o, id) = (overlay.clone(), self.dialog_id.clone());
        let files = tauri::async_runtime::spawn_blocking(move || {
            o.fs.flush();
            let plan = land::plan(&o.project, &chats::changes(&id), false);
            // Conflicts are the user's (or the chat's, if it's handed them).
            if !plan.merged || !plan.conflicts.is_empty() {
                return Ok(Vec::new());
            }
            chats::take_merged(&id, &plan)
        })
        .await
        .map_err(|e| e.to_string())??;
        if files.is_empty() {
            return Ok(None);
        }
        super::log::write(
            &self.app,
            serde_json::json!({ "event": "merged-project-changes", "dialogId": self.dialog_id, "files": files.iter().map(|(p, _)| p).collect::<Vec<_>>() }),
        );
        self.merged_from_project = Some(files);
        self.writes_since_verify = true;
        let outcome = self.verify_at_end(false, None).await?;
        self.merged_from_project = None;
        Ok(Some(outcome))
    }

    /// The model says it's done, but conflict markers handed to it are
    /// still in its files: it's told, twice at most. True: it was told.
    pub(super) async fn remind_of_markers(&mut self) -> Result<bool, String> {
        if self.root == self.project_root || self.plan_mode || self.marker_reminders >= 2 {
            return Ok(false);
        }
        let id = self.dialog_id.clone();
        let left = tauri::async_runtime::spawn_blocking(move || chats::markers_left(&id))
            .await
            .unwrap_or_default();
        if left.is_empty() {
            return Ok(false);
        }
        self.marker_reminders += 1;
        self.add_harness(
            format!(
                "{}Conflict markers are still in {}. Your changes can't land until every marker is gone: combine both sides in each conflict, then finish.",
                super::run::IDE_NOTE,
                left.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
            ),
            "markers",
        )
        .await?;
        Ok(true)
    }

    /// Waits while many files change in the project at once (a branch
    /// switch, a pull): the chat's view would show a half-switched project.
    /// Says so, once, if it has to wait.
    pub(super) async fn wait_while_project_busy(&self) {
        let Some(project) = self
            .project_root
            .as_deref()
            .filter(|_| self.root != self.project_root)
        else {
            return;
        };
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
        emit(
            &self.app,
            AgentEvent::Notice {
                kind: kind.into(),
                title: title.into(),
                message: message.into(),
            },
        );
    }
}

//! The repair loop's runner (docs/design/subagents-plan.md, R04, R06,
//! R07): after a parallel group, a failed round's causes (causes.rs) are
//! fixed by their owners before the lead hears of them. A cause inside one
//! worker's files goes back to that worker, resumed in its own chat with
//! its context; the lead fixes the rest. Causes are fixed one at a time,
//! most failures first, and the checks run once after the round's fixes
//! (R04). With the setting on, causes in different workers' scopes, each
//! rated high by triage, are fixed at the same time in child overlays and
//! then joined (R07).
//!
//! The escalation ladder (R06): the owners fix in the first round; in a
//! later one, or after a round that made no progress, the lead fixes
//! everything itself. The gate's round limit ends the loop as gave-up.

use super::causes::{self, Cause, Owner, Owners};
use super::command_output::focus_failures;
use super::repair::{self, Judgement, Verdict};
use super::run::Run;
use super::subagent::{self, Brief, Kind, SubResult, SubStatus};
use super::subagent_run::{none_future, run_child, SubFuture};
use super::verify::{with, CheckResult};
use super::worker::WriteScope;
use super::{log, tools};
use crate::chat_db::store;
use crate::overlay::chats;
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use tauri::Runtime;

/// The part of a check's output a fixer gets, at most.
const FIX_EXCERPT_CHARS: usize = 3_000;

/// Rounds checked again at once after fixers fixed every cause, at most (P3.10).
pub const MAX_RECHECKS: u32 = 2;

/// A parallel group joined in this run: who owns what (R03), and each
/// landed worker's chat, to resume for a fix (R04).
#[derive(Debug, Clone)]
pub struct GroupRecord {
    pub name: String,
    pub owners: Vec<(String, usize, WriteScope)>,
    /// Label, step, child chat.
    pub children: Vec<(String, usize, String)>,
    pub contract: Vec<String>,
}

impl GroupRecord {
    /// The chat of the worker that carried out `step` as `label` (a retry's
    /// label has a prime, which the owner's doesn't).
    pub fn worker_chat(&self, label: &str, step: usize) -> Option<String> {
        self.children
            .iter()
            .find(|(l, n, _)| l.trim_end_matches('\'') == label && *n == step)
            .map(|(_, _, c)| c.clone())
    }
}

/// What the runner did with a failed round.
pub(super) enum Repair {
    /// Nothing: no group, the setting is off: the lead gets the usual report.
    None,
    /// Every cause went to a worker and they changed files: the checks run
    /// again now, without a turn of the lead's.
    Recheck,
    /// The lead fixes (the rest): this goes in its report. `fixed`: fixers
    /// ran, so the round's checkpoint was taken before them.
    Lead { note: String, fixed: bool },
}

/// How one fixer went, for the record and the lead.
#[derive(Debug, Clone)]
struct Fixed {
    status: String,
    child: Option<String>,
    changed: Vec<String>,
    /// Its report's summary line, or why it couldn't run.
    said: String,
}

/// The lines of `output` around the places that name one of `files`.
fn excerpt_for(output: &str, files: &[String], max: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if files.iter().any(|f| line.contains(f.as_str())) {
            let from = i.saturating_sub(2);
            let to = (i + 6).min(lines.len());
            for k in keep.iter_mut().take(to).skip(from) {
                *k = true;
            }
        }
    }
    let mut out = String::new();
    let mut gap = false;
    for (line, k) in lines.iter().zip(&keep) {
        if !k {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push_str("…\n");
        }
        gap = false;
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        return focus_failures(output, max);
    }
    focus_failures(&out, max)
}

/// The message that resumes a worker to fix its cause.
fn fixer_message(cause: &Cause, round: u32, results: &[CheckResult]) -> String {
    let mut out = format!(
        "# Fix from the lead's checks\n\nThe lead merged the group's work and ran the project's checks (round {round}). \
         These failures are in your step's files, so they're yours to fix:\n\n**Cause:** {}\n\n{}\n",
        cause.summary,
        repair::key_list(&cause.keys, 20)
    );
    for r in results.iter().filter(|r| r.status == "failed") {
        let mine = r.failures.iter().flatten().any(|k| cause.keys.contains(k));
        if !mine {
            continue;
        }
        let output = r.output.clone().unwrap_or_default();
        out.push_str(&format!(
            "\n## {} (`{}`)\n\n```\n{}\n```\n",
            super::project::check_label(&r.kind),
            r.command,
            excerpt_for(&output, &cause.files, FIX_EXCERPT_CHARS)
        ));
        if let Some(path) = &r.saved_output {
            out.push_str(&format!(
                "The whole output is saved as `{path}`: read it with read and start_line/end_line.\n"
            ));
        }
    }
    out.push_str(
        "\nFix them in your files only. Don't build or run the tests: the lead runs every check again once the fixes are in. \
         Use diagnostics on your files, and light checks on one file or package. Then report in the same format.",
    );
    out
}

/// One cause's line for the lead and the record.
fn cause_line(i: usize, c: &Cause, fixed: Option<&Fixed>) -> String {
    let files = if c.files.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = c.files.iter().take(6).map(String::as_str).collect();
        format!(
            " in {}{}",
            shown.join(", "),
            if c.files.len() > 6 {
                format!(" and {} more", c.files.len() - 6)
            } else {
                String::new()
            }
        )
    };
    let who = match (fixed, &c.owner) {
        (Some(f), Owner::Worker { label, .. }) if f.status == "done" && !f.changed.is_empty() => {
            format!(
                "Worker {label} fixed it (changed {}): {}",
                f.changed.join(", "),
                f.said
            )
        }
        (Some(f), Owner::Worker { label, .. }) => format!(
            "Worker {label} couldn't fix it ({}): {}. Yours to fix now.",
            f.status, f.said
        ),
        (None, Owner::Worker { .. }) => {
            format!("Owner: {}. Yours to fix this round.", c.owner.describe())
        }
        (_, Owner::TestRule { .. }) => format!(
            "Owner: {}: change the test only if the request or the plan says it's wrong, and say so in your final message.",
            c.owner.describe()
        ),
        (_, Owner::Lead { .. }) => format!("Owner: {}. Yours to fix.", c.owner.describe()),
    };
    format!(
        "{}. {} ({} failure{}{files}). {who}",
        i + 1,
        c.summary,
        c.count(),
        if c.count() == 1 { "" } else { "s" }
    )
}

fn cause_json(c: &Cause, fixed: Option<&Fixed>, parallel: bool) -> Value {
    let mut v = json!(c);
    v["owner"]["describe"] = json!(c.owner.describe());
    if let Some(f) = fixed {
        v["fixer"] = json!({
            "status": f.status,
            "childDialogId": f.child,
            "changed": f.changed,
            "said": f.said,
            "parallel": parallel,
        });
    }
    v
}

/// The first line of a report's Summary, or its first line.
fn said(result: &SubResult) -> String {
    let s = subagent::sections(&result.text);
    let line = s
        .get("Summary")
        .unwrap_or(result.text.as_str())
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    store::char_prefix(&line, 200).to_string()
}

/// A fixer's brief, as the run's ledger counts it: the same cause for the
/// same step is the same brief whatever the round, so it's sent at most
/// `MAX_SAME_BRIEF` times in a run.
fn fixer_brief(step: usize, summary: &str) -> Brief {
    Brief {
        kind: Kind::Worker,
        goal: format!("Fix: {summary}"),
        context: None,
        paths: Vec::new(),
        max_tool_calls: None,
        step: Some(step),
    }
}

/// A fix round's setting, the same for each of its fixers: the group
/// whose workers fix, the round, the checks' results and the record.
#[derive(Clone, Copy)]
struct FixRound<'a> {
    group: &'a GroupRecord,
    round: u32,
    results: &'a [CheckResult],
    record_id: &'a str,
}

impl<R: Runtime> Run<R> {
    /// Whether the repair loop runs here: the lead of an isolated chat in
    /// which a parallel group joined, with step workers on.
    fn repair_on(&self) -> bool {
        self.sub().is_none()
            && !self.groups.is_empty()
            && !self.profile.no_tools
            && self.state.config().subagents.kind(Kind::Worker).is_some()
            && chats::get(&self.dialog_id).is_some()
    }

    /// R03–R07, for a failed round, before its report goes to the lead.
    /// `triaged`: a triage ran this round (`last_triage` is its).
    /// `base`: the round's record, which gets the causes.
    pub(super) async fn repair_by_owners(
        &mut self,
        record_id: &str,
        round: u32,
        results: &[CheckResult],
        judgement: &Judgement,
        triaged: bool,
        base: &Value,
    ) -> Result<Repair, String> {
        if !self.repair_on() || self.stopped() {
            return Ok(Repair::None);
        }
        let Some(group) = self.groups.last().cloned() else {
            return Ok(Repair::None);
        };
        let fix = FixRound {
            group: &group,
            round,
            results,
            record_id,
        };
        let root = self.root.clone().unwrap_or_default();
        let keys: Vec<String> = results
            .iter()
            .flat_map(|r| r.failures.iter().flatten().cloned())
            .collect();
        let found = {
            let created =
                |f: &str| matches!(self.original_of(&repair::rel_key(&root, f)), Some(None));
            let (roots, known) = self.path_context(&group.owners);
            let owners = Owners {
                workers: &group.owners,
                contract: &group.contract,
                test_files: &self.test_writer_files,
                created: &created,
                roots: &roots,
                known: &known,
            };
            causes::group(
                &keys,
                &owners,
                triaged.then_some(self.last_triage.as_ref()).flatten(),
            )
        };
        if found.is_empty() {
            return Ok(Repair::None);
        }
        // R06: the owners fix in the first round; then, or after a round
        // that made no progress, the lead does it all.
        let lead_only = lead_only(judgement.verdict, self.rechecks, round);
        let config = self.state.config().subagents;
        let parallel: Vec<usize> = if lead_only.is_none() && config.parallel_repair {
            let set = causes::parallel_set(&found);
            found
                .iter()
                .enumerate()
                .filter(|(_, c)| set.iter().any(|s| std::ptr::eq(*s, *c)))
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        // Sequential: the workers' causes, most failures first, up to the
        // first one the lead owns (it may explain the rest).
        let sequential: Vec<usize> = if lead_only.is_some() || !parallel.is_empty() {
            Vec::new()
        } else {
            found
                .iter()
                .take_while(|c| c.owner.is_worker())
                .enumerate()
                .map(|(i, _)| i)
                .collect()
        };
        let mut fixed: Vec<Option<Fixed>> = vec![None; found.len()];
        let record = |fixed: &[Option<Fixed>], status: &str| {
            json!({
                "group": group.name,
                "round": round,
                "status": status,
                "leadOnly": lead_only,
                "parallel": !parallel.is_empty(),
                "causes": found
                    .iter()
                    .zip(fixed)
                    .map(|(c, f)| cause_json(c, f.as_ref(), !parallel.is_empty()))
                    .collect::<Vec<_>>(),
            })
        };
        let fixing = !parallel.is_empty() || !sequential.is_empty();
        self.update_meta(
            record_id,
            with(
                base.clone(),
                json!({ "repair": record(&fixed, if fixing { "fixing" } else { "lead" }) }),
            ),
        )
        .await?;
        if fixing {
            // R02: what this round's fixes roll back to.
            self.take_checkpoint();
        }
        if !parallel.is_empty() {
            let outcomes = self.fix_in_parallel(&fix, &found, &parallel).await;
            for (i, f) in parallel.iter().zip(outcomes) {
                fixed[*i] = Some(f);
            }
        } else {
            for &i in &sequential {
                if self.stopped() {
                    break;
                }
                let f = self.fix_one(&fix, &found[i], None).await;
                fixed[i] = Some(f);
                self.update_meta(
                    record_id,
                    with(base.clone(), json!({ "repair": record(&fixed, "fixing") })),
                )
                .await?;
            }
        }
        let all_fixed = found.iter().zip(&fixed).all(|(_, f)| {
            f.as_ref()
                .is_some_and(|f| f.status == "done" && !f.changed.is_empty())
        });
        let status = if self.stopped() {
            "stopped"
        } else if fixing && all_fixed && self.writes_since_verify {
            "recheck"
        } else {
            "lead"
        };
        self.update_meta(
            record_id,
            with(base.clone(), json!({ "repair": record(&fixed, status) })),
        )
        .await?;
        log::write(
            &self.app,
            json!({
                "event": "repairRound",
                "dialogId": self.dialog_id,
                "round": round,
                "causes": found.len(),
                "workerCauses": found.iter().filter(|c| c.owner.is_worker()).count(),
                "fixers": fixed.iter().flatten().count(),
                "parallel": !parallel.is_empty(),
                "leadOnly": lead_only.is_some(),
                "status": status,
            }),
        );
        if status == "recheck" {
            self.rechecks += 1;
            return Ok(Repair::Recheck);
        }
        let mut note = format!(
            "## Root causes\n\nThe failures come from {} cause{}, most failures first:\n",
            found.len(),
            if found.len() == 1 { "" } else { "s" }
        );
        if let Some(why) = lead_only {
            note.push_str(&format!("({why}.)\n"));
        }
        for (i, (c, f)) in found.iter().zip(&fixed).enumerate() {
            note.push_str(&format!("\n{}", cause_line(i, c, f.as_ref())));
        }
        if fixed.iter().flatten().any(|f| !f.changed.is_empty()) {
            note.push_str(
                "\n\nThe workers' fixes are in your files already; the output below is from before them. \
                 The checks run again once you've fixed the rest.",
            );
        }
        Ok(Repair::Lead {
            note,
            fixed: fixing,
        })
    }

    /// For reading paths in a check's output (`causes::resolve`): the roots
    /// absolute paths start with, and the files the run knows.
    pub(super) fn path_context(
        &self,
        owners: &[(String, usize, WriteScope)],
    ) -> (Vec<String>, Vec<String>) {
        let root = self.root.clone().unwrap_or_default();
        let roots: Vec<String> = [self.root.clone(), self.project_root.clone()]
            .into_iter()
            .flatten()
            .collect();
        let mut known: Vec<String> = self
            .changed_files
            .iter()
            .map(|f| repair::rel_key(&root, f))
            .collect();
        for (_, _, scope) in owners {
            for f in &scope.files {
                if !known.contains(f) {
                    known.push(f.clone());
                }
            }
        }
        (roots, known)
    }

    /// A worker resumed in its own chat to fix `cause`; `root`: the child
    /// overlay it works in (R07), else the chat's view.
    async fn start_fixer(
        &mut self,
        fix: &FixRound<'_>,
        cause: &Cause,
        root: Option<String>,
    ) -> Result<(String, super::run::Run<R>), String> {
        let FixRound {
            group,
            round,
            results,
            record_id,
        } = *fix;
        let Owner::Worker { label, step } = &cause.owner else {
            return Err("not a worker's cause".into());
        };
        let child = group
            .worker_chat(label, *step)
            .ok_or_else(|| format!("worker {label}'s chat isn't known"))?;
        if self.state.is_running(&child) {
            return Err(format!("worker {label} is still working"));
        }
        let config = self.state.config().subagents;
        if let Some(why) = self.ledger.over_spend(config.max_run_tokens) {
            return Err(why.trim_start_matches("Error: ").to_string());
        }
        let Some((_, plan)) = self.implementing_plan().await else {
            return Err("the plan can't be found any more".into());
        };
        let steps = super::plan::steps(&plan);
        let plan_step = steps
            .iter()
            .find(|s| s.number == *step)
            .ok_or_else(|| format!("step {step} isn't in the plan any more"))?;
        let goal = format!("Fix round {round}: {}", cause.summary);
        let brief = fixer_brief(*step, &cause.summary);
        let call_id = format!("{record_id}:fix:{label}");
        let mut sub = self
            .new_sub(Kind::Worker, &call_id, config.budget(Kind::Worker), &config)
            .await?;
        // D11 (P3.10): fixers share the run's limit with scouts and test
        // writers, not the plan's workers; counted once one can start.
        let admitted = self
            .ledger
            .admit(&brief, config.max_per_run())
            .map_err(|e| e.trim_start_matches("Error: ").to_string())?;
        sub.scope = Some(WriteScope::for_step(plan_step));
        sub.step = Some(*step);
        // The lead builds once, after the round's fixes (R04).
        sub.light_bash = true;
        // Round 3: a fixer that can't start from here gives its place back.
        let started = async {
            self.close_child_calls(&child).await?;
            // Launched first (P3.10): its chat gets the fix's brief only when
            // the fixer really starts.
            let mut run = self.launch_child(child.clone(), &call_id, &goal, sub)?;
            let text = fixer_message(cause, round, results);
            self.append_follow_up(&child, Kind::Worker, &call_id, &text, true)
                .await?;
            if let Some(root) = root {
                run.root = Some(root);
                // Its copy is discarded if it fails.
                run.checkpoint = None;
            }
            Ok((child.clone(), run))
        }
        .await;
        if started.is_err() {
            self.ledger.release(&admitted);
        }
        started
    }

    /// R04: one cause, fixed by its worker in the chat's view.
    async fn fix_one(&mut self, fix: &FixRound<'_>, cause: &Cause, root: Option<String>) -> Fixed {
        let group = fix.group;
        let (child, run) = match self.start_fixer(fix, cause, root).await {
            Ok(started) => started,
            Err(e) => {
                return Fixed {
                    status: "not started".into(),
                    child: None,
                    changed: Vec::new(),
                    said: e,
                }
            }
        };
        // What the files are now, to tell what the fixer itself changed: a
        // fixer's writes are counted from the project's files, so in an
        // isolated chat every file the group changed would look changed,
        // and one it rolled back too.
        let root_dir = self.root.clone().unwrap_or_default();
        let mut paths: Vec<String> = self
            .changed_files
            .iter()
            .map(|f| repair::rel_key(&root_dir, f))
            .collect();
        if let Owner::Worker { label, .. } = &cause.owner {
            if let Some((_, _, scope)) = group.owners.iter().find(|(l, _, _)| l == label) {
                paths.extend(scope.files.iter().cloned());
            }
        }
        paths.extend(cause.files.iter().cloned());
        paths.sort();
        paths.dedup();
        // R06: what they are in the chat, for Redo sequentially, should the
        // fixer write them (or new files, which then go); and every path the
        // chat has changed so far (P1.4), so a file it changed in an earlier
        // run is never taken for untouched.
        let snapshot = self.redo_snapshot(&paths).await;
        let before: std::collections::HashMap<String, Option<Vec<u8>>> = paths
            .into_iter()
            .map(|p| {
                let now = std::fs::read(tools::to_absolute_path(&p, &root_dir)).ok();
                (p, now)
            })
            .collect();
        let result = run_child(run).await;
        let fixed = self.fixer_done(child, result, Some(&before));
        self.add_to_redo(group, &fixed.changed, &snapshot).await;
        fixed
    }

    /// What `paths` and every path the chat changed are in the chat, each
    /// on its own (P1.4); Err: none could be taken.
    async fn redo_snapshot(&self, paths: &[String]) -> Result<chats::PathSnapshots, String> {
        let mut keep = paths.to_vec();
        keep.extend(self.overlay_paths());
        keep.sort();
        keep.dedup();
        let dialog = self.dialog_id.clone();
        crate::rt::spawn_blocking(move || chats::snapshot_each(&dialog, &keep))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
    }

    /// R06: a fixer's files join what Redo sequentially puts back, as they
    /// were before it. A path the snapshot doesn't have wasn't changed by
    /// the chat (every path it had changed is in it), so it's untouched;
    /// one that couldn't be read, or no snapshot at all, makes the group's
    /// Redo unavailable, with the reason (P1.4), never "untouched".
    async fn add_to_redo(
        &self,
        group: &GroupRecord,
        changed: &[String],
        snapshot: &Result<chats::PathSnapshots, String>,
    ) {
        // What the snapshot saved for paths the fixer didn't change goes.
        let unused: Vec<chats::Before> = snapshot
            .as_ref()
            .map(|taken| {
                taken
                    .iter()
                    .filter(|(p, _)| !changed.contains(p))
                    .filter_map(|(_, b)| b.as_ref().ok().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if !unused.is_empty() {
            let dialog = self.dialog_id.clone();
            let _ =
                crate::rt::spawn_blocking(move || chats::forget_snapshot(&dialog, &unused)).await;
        }
        if changed.is_empty() {
            return;
        }
        let mut entries: Vec<(String, chats::Before)> = Vec::new();
        let mut unknown: Option<String> = None;
        match snapshot {
            Err(e) => unknown = Some(e.clone()),
            Ok(taken) => {
                for p in changed {
                    match taken.iter().find(|(q, _)| q == p).map(|(_, b)| b) {
                        Some(Ok(b)) => entries.push((p.clone(), b.clone())),
                        Some(Err(e)) => {
                            unknown.get_or_insert_with(|| e.clone());
                        }
                        None => entries.push((p.clone(), chats::Before::Untouched)),
                    }
                }
            }
        }
        let (dialog, name) = (self.dialog_id.clone(), group.name.clone());
        let saved = crate::rt::spawn_blocking(move || {
            chats::add_group_paths(&dialog, &name, entries)?;
            match unknown {
                Some(why) => chats::mark_redo_unavailable(&dialog, &name, &why),
                None => Ok(()),
            }
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        if let Err(e) = saved {
            log::write(
                &self.app,
                json!({ "event": "groupSave", "dialogId": self.dialog_id, "error": e }),
            );
        }
    }

    /// `before`: the files just before a fixer that worked in the chat's
    /// view; only the writes that left a file different count.
    fn fixer_done(
        &mut self,
        child: String,
        result: Option<SubResult>,
        before: Option<&std::collections::HashMap<String, Option<Vec<u8>>>>,
    ) -> Fixed {
        let Some(result) = result else {
            return Fixed {
                status: "failed".into(),
                child: Some(child),
                changed: Vec::new(),
                said: "it ended without a report".into(),
            };
        };
        self.note_models_of(&result);
        let root_dir = self.root.clone().unwrap_or_default();
        let writes: Vec<subagent::ChildWrite> = match before {
            None => result.writes.clone(),
            Some(before) => result
                .writes
                .iter()
                .filter(|w| {
                    let now = std::fs::read(tools::to_absolute_path(&w.path, &root_dir)).ok();
                    match before.get(&w.path) {
                        Some(was) => *was != now,
                        // Not looked at before: what it was before the run.
                        None => w.original.bytes != now,
                    }
                })
                .cloned()
                .collect(),
        };
        self.absorb_child_writes(&writes);
        let changed: Vec<String> = writes.iter().map(|w| w.path.clone()).collect();
        let status = match result.status {
            SubStatus::Done => "done",
            other => other.id(),
        };
        Fixed {
            status: status.into(),
            child: Some(child),
            changed,
            said: said(&result),
        }
    }

    /// R07: causes in different workers' scopes, each in a child overlay
    /// of the chat's view, at the same time; then joined one at a time.
    async fn fix_in_parallel(
        &mut self,
        fix: &FixRound<'_>,
        found: &[Cause],
        which: &[usize],
    ) -> Vec<Fixed> {
        let group = fix.group;
        if let Some(o) = chats::get(&self.dialog_id) {
            o.fs.flush();
        }
        // A fixer's chat is its worker's; one kept from the join (a
        // conflict's copy, for `continue "land it"`) outlives a fixer that
        // doesn't start or fails: only a copy opened here is discarded
        // (round 3).
        let mut started: Vec<Result<(String, bool), Fixed>> = Vec::new();
        let mut futures: Vec<SubFuture> = Vec::new();
        for &i in which {
            let cause = &found[i];
            // The worker's chat id is its overlay's too (F04).
            let child = match &cause.owner {
                Owner::Worker { label, step } => group.worker_chat(label, *step),
                _ => None,
            };
            let fresh = child.as_deref().is_none_or(|c| chats::get(c).is_none());
            let overlay = match child
                .as_deref()
                .ok_or_else(|| "its chat isn't known".to_string())
                .and_then(|c| chats::open_child_shared(&self.dialog_id, c, which.len()))
            {
                Ok(o) => o,
                Err(e) => {
                    started.push(Err(Fixed {
                        status: "not started".into(),
                        child,
                        changed: Vec::new(),
                        said: e,
                    }));
                    futures.push(none_future());
                    continue;
                }
            };
            let view = overlay.view.to_string_lossy().into_owned();
            match self.start_fixer(fix, cause, Some(view)).await {
                Ok((child, run)) => {
                    started.push(Ok((child, fresh)));
                    futures.push(Box::pin(run_child(run)));
                }
                Err(e) => {
                    let said = match &child {
                        Some(c) if fresh => {
                            chats::discard_child(c);
                            e
                        }
                        Some(_) => format!("{e}; its copy from the join is kept"),
                        None => e,
                    };
                    started.push(Err(Fixed {
                        status: "not started".into(),
                        child,
                        changed: Vec::new(),
                        said,
                    }));
                    futures.push(none_future());
                }
            }
        }
        let max = self.state.config().subagents.max_at_once();
        let done: Vec<Option<SubResult>> = stream::iter(futures).buffered(max).collect().await;
        // The join, one at a time.
        let root = self.root.clone().unwrap_or_default();
        let mut out = Vec::new();
        for (s, result) in started.into_iter().zip(done) {
            let (child, fresh) = match s {
                Err(f) => {
                    out.push(f);
                    continue;
                }
                Ok(child) => child,
            };
            let Some(mut result) =
                result.filter(|r| !matches!(r.status, SubStatus::Failed | SubStatus::Stopped))
            else {
                let said = if fresh {
                    chats::discard_child(&child);
                    "it failed or was stopped; its copy was discarded"
                } else {
                    "it failed or was stopped; its copy from the join is kept"
                };
                out.push(Fixed {
                    status: "failed".into(),
                    child: Some(child),
                    changed: Vec::new(),
                    said: said.into(),
                });
                continue;
            };
            let (paths, before) = super::group::copy_writes(&child, &root);
            let snapshot = self.redo_snapshot(&paths).await;
            // Round 2, P1.3: landed with retries; on an error the copy and
            // its landing journal stay, so a landing that stopped half way
            // is finished (or refused) before the chat lands.
            let landed = super::group::land_copy(&child, Vec::new()).await;
            if landed.is_ok() {
                let id = child.clone();
                let _ = crate::rt::spawn_blocking(move || chats::discard_child(&id)).await;
            }
            match landed {
                Ok(paths) => {
                    result.writes = before
                        .into_iter()
                        .filter(|w| paths.contains(&w.path))
                        .collect();
                    let fixed = self.fixer_done(child, Some(result), None);
                    self.add_to_redo(group, &fixed.changed, &snapshot).await;
                    out.push(fixed);
                }
                Err(e) => {
                    self.note_models_of(&result);
                    // Nothing landed: the snapshot's blobs go (round 3).
                    self.add_to_redo(group, &[], &snapshot).await;
                    out.push(Fixed {
                        status: "not landed".into(),
                        child: Some(child),
                        changed: Vec::new(),
                        said: format!("its fix couldn't be joined ({e}); its copy is kept"),
                    });
                }
            }
        }
        out
    }
}

/// R06: whether the lead fixes everything this round (and why): after a
/// round that made no progress, once the fixers' rechecks are used up (an
/// explicit bound, P3.10), and from the escalation ladder's second round.
/// None: the owners fix.
fn lead_only(verdict: Verdict, rechecks: u32, round: u32) -> Option<&'static str> {
    if verdict == Verdict::NoProgress {
        Some("the last round made no progress, so the lead fixes everything this round")
    } else if rechecks >= MAX_RECHECKS {
        Some("the fixers' rechecks are used up: the lead fixes everything")
    } else if round >= 2 {
        Some("a later round of the escalation ladder: the lead fixes everything")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 2 (§4.2): the recheck cap holds on its own, whatever the
    /// round: two rechecks, and the lead fixes everything.
    #[test]
    fn recheck_cap_is_explicit() {
        for v in [Verdict::First, Verdict::Progress, Verdict::Regression] {
            assert_eq!(lead_only(v, 0, 1), None, "{v:?}");
            assert_eq!(lead_only(v, MAX_RECHECKS - 1, 1), None);
            assert!(lead_only(v, MAX_RECHECKS, 1)
                .unwrap()
                .contains("rechecks are used up"));
            assert!(lead_only(v, 0, 2).unwrap().contains("escalation ladder"));
        }
        assert!(lead_only(Verdict::NoProgress, 0, 1)
            .unwrap()
            .contains("no progress"));
    }

    /// Round 2 (§4.2): fixers draw on the run's limit of subagents (12 by
    /// default): the twelfth starts, the thirteenth is refused.
    #[test]
    fn the_thirteenth_fixer_is_refused_the_twelfth_starts() {
        let mut ledger = subagent::Ledger::default();
        let max = subagent::SubagentConfig::default().max_per_run();
        assert_eq!(max, 12);
        for n in 1..=12 {
            ledger
                .admit(&fixer_brief(n, &format!("cause {n}")), max)
                .unwrap();
        }
        let e = ledger.admit(&fixer_brief(13, "cause 13"), max).unwrap_err();
        assert!(e.contains("already started 12 subagents, the limit"), "{e}");
    }

    /// The same cause sent to the same step's worker, round after round:
    /// the same brief, refused past `MAX_SAME_BRIEF` (a new cause isn't).
    #[test]
    fn the_same_fix_isnt_sent_a_third_time_in_a_run() {
        let mut ledger = subagent::Ledger::default();
        let cause = "E0308 in src/posts.rs";
        for _round in 0..subagent::MAX_SAME_BRIEF {
            ledger.admit(&fixer_brief(3, cause), 10).unwrap();
        }
        let refused = ledger.admit(&fixer_brief(3, cause), 10).unwrap_err();
        assert!(refused.contains("already tried 2 times"), "{refused}");
        assert!(ledger.admit(&fixer_brief(3, "another cause"), 10).is_ok());
        assert!(
            ledger.admit(&fixer_brief(2, cause), 10).is_ok(),
            "another step's"
        );
    }

    #[test]
    fn a_fixer_gets_the_output_around_its_files() {
        let output = (0..100)
            .map(|i| {
                if i == 50 {
                    "error[E0308]: mismatched types\n  --> src/posts.rs:4:5".to_string()
                } else {
                    format!("noise {i}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let got = excerpt_for(&output, &["src/posts.rs".into()], 3_000);
        assert!(got.contains("src/posts.rs:4:5"));
        assert!(got.contains("mismatched types"));
        assert!(!got.contains("noise 10\n"));
    }
}

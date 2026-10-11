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
use super::run::{new_message, Run};
use super::subagent::{self, Brief, Kind, SubResult, SubStatus};
use super::subagent_run::{go_sub, SubFuture};
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
        let lead_only = if judgement.verdict == Verdict::NoProgress {
            Some("the last round made no progress, so the lead fixes everything this round")
        } else if round >= 2 {
            Some("a later round of the escalation ladder: the lead fixes everything")
        } else {
            None
        };
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
            let outcomes = self
                .fix_in_parallel(&group, &found, &parallel, round, results, record_id)
                .await;
            for (i, f) in parallel.iter().zip(outcomes) {
                fixed[*i] = Some(f);
            }
        } else {
            for &i in &sequential {
                if self.stopped() {
                    break;
                }
                let f = self
                    .fix_one(&group, &found[i], round, results, record_id, None)
                    .await;
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
        group: &GroupRecord,
        cause: &Cause,
        round: u32,
        results: &[CheckResult],
        record_id: &str,
        root: Option<String>,
    ) -> Result<(String, super::run::Run<R>), String> {
        let Owner::Worker { label, step } = &cause.owner else {
            return Err("not a worker's cause".into());
        };
        let child = group
            .children
            .iter()
            .find(|(l, n, _)| l.trim_end_matches('\'') == label && n == step)
            .map(|(_, _, c)| c.clone())
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
        let brief = Brief {
            kind: Kind::Worker,
            goal: goal.clone(),
            context: None,
            paths: Vec::new(),
            max_tool_calls: None,
            step: Some(*step),
        };
        self.ledger
            .admit_worker(&brief, steps.len())
            .map_err(|e| e.trim_start_matches("Error: ").to_string())?;
        let call_id = format!("{record_id}:fix:{label}");
        let mut sub = self
            .new_sub(Kind::Worker, &call_id, config.budget(Kind::Worker), &config)
            .await?;
        sub.scope = Some(WriteScope::for_step(plan_step));
        sub.step = Some(*step);
        // The lead builds once, after the round's fixes (R04).
        sub.light_bash = true;
        self.close_child_calls(&child).await?;
        let mut message = new_message("user", &fixer_message(cause, round, results));
        message.meta = Some(json!({
            "subagentBrief": { "kind": "worker", "parentCallId": call_id, "followUp": true, "repair": true }
        }));
        let id = child.clone();
        self.db
            .write(move |c| store::append_messages(c, &id, &[message]).map(|_| ()))
            .await?;
        let mut run = self.launch_child(child.clone(), &call_id, &goal, sub)?;
        if let Some(root) = root {
            run.root = Some(root);
            // Its copy is discarded if it fails.
            run.checkpoint = None;
        }
        Ok((child, run))
    }

    /// R04: one cause, fixed by its worker in the chat's view.
    async fn fix_one(
        &mut self,
        group: &GroupRecord,
        cause: &Cause,
        round: u32,
        results: &[CheckResult],
        record_id: &str,
        root: Option<String>,
    ) -> Fixed {
        let (child, run) = match self
            .start_fixer(group, cause, round, results, record_id, root)
            .await
        {
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
        let before: std::collections::HashMap<String, Option<Vec<u8>>> = paths
            .into_iter()
            .map(|p| {
                let now = std::fs::read(tools::to_absolute_path(&p, &root_dir)).ok();
                (p, now)
            })
            .collect();
        let result = tauri::async_runtime::spawn(go_sub(run))
            .await
            .ok()
            .flatten();
        self.fixer_done(child, result, Some(&before))
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
                        None => w.original.as_ref().map(|s| s.as_bytes().to_vec()) != now,
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
        group: &GroupRecord,
        found: &[Cause],
        which: &[usize],
        round: u32,
        results: &[CheckResult],
        record_id: &str,
    ) -> Vec<Fixed> {
        if let Some(o) = chats::get(&self.dialog_id) {
            o.fs.flush();
        }
        let mut started: Vec<Result<String, Fixed>> = Vec::new();
        let mut futures: Vec<SubFuture> = Vec::new();
        for &i in which {
            let cause = &found[i];
            // The worker's chat id is its overlay's too (F04).
            let child = match &cause.owner {
                Owner::Worker { label, step } => group
                    .children
                    .iter()
                    .find(|(l, n, _)| l.trim_end_matches('\'') == label && n == step)
                    .map(|(_, _, c)| c.clone()),
                _ => None,
            };
            let overlay = match child
                .as_deref()
                .ok_or_else(|| "its chat isn't known".to_string())
                .and_then(|c| chats::open_child(&self.dialog_id, c))
            {
                Ok(o) => o,
                Err(e) => {
                    started.push(Err(Fixed {
                        status: "not started".into(),
                        child,
                        changed: Vec::new(),
                        said: e,
                    }));
                    futures.push(Box::pin(async { None }));
                    continue;
                }
            };
            let view = overlay.view.to_string_lossy().into_owned();
            match self
                .start_fixer(group, cause, round, results, record_id, Some(view))
                .await
            {
                Ok((child, run)) => {
                    started.push(Ok(child));
                    futures.push(Box::pin(async move {
                        tauri::async_runtime::spawn(go_sub(run))
                            .await
                            .ok()
                            .flatten()
                    }));
                }
                Err(e) => {
                    if let Some(c) = &child {
                        chats::discard_child(c);
                    }
                    started.push(Err(Fixed {
                        status: "not started".into(),
                        child,
                        changed: Vec::new(),
                        said: e,
                    }));
                    futures.push(Box::pin(async { None }));
                }
            }
        }
        let max = self.state.config().subagents.max_at_once();
        let done: Vec<Option<SubResult>> = stream::iter(futures).buffered(max).collect().await;
        // The join, one at a time.
        let root = self.root.clone().unwrap_or_default();
        let mut out = Vec::new();
        for (s, result) in started.into_iter().zip(done) {
            let child = match s {
                Err(f) => {
                    out.push(f);
                    continue;
                }
                Ok(child) => child,
            };
            let Some(mut result) =
                result.filter(|r| !matches!(r.status, SubStatus::Failed | SubStatus::Stopped))
            else {
                chats::discard_child(&child);
                out.push(Fixed {
                    status: "failed".into(),
                    child: Some(child),
                    changed: Vec::new(),
                    said: "it failed or was stopped; its copy was discarded".into(),
                });
                continue;
            };
            let paths: Vec<String> = chats::get(&child)
                .map(|o| {
                    o.fs.flush();
                    o.fs.changes()
                        .iter()
                        .filter(|c| !c.by_product())
                        .map(|c| c.path().replace('\\', "/"))
                        .collect()
                })
                .unwrap_or_default();
            let before: Vec<subagent::ChildWrite> = paths
                .iter()
                .map(|p| subagent::ChildWrite {
                    path: p.clone(),
                    original: std::fs::read(tools::to_absolute_path(p, &root))
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned()),
                })
                .collect();
            let id = child.clone();
            let landed = tauri::async_runtime::spawn_blocking(move || chats::land_child(&id))
                .await
                .unwrap_or_else(|e| Err(crate::overlay::land::LandError::Failed(e.to_string())));
            match landed {
                Ok(paths) => {
                    result.writes = before
                        .into_iter()
                        .filter(|w| paths.contains(&w.path))
                        .collect();
                    out.push(self.fixer_done(child, Some(result), None));
                }
                Err(e) => {
                    chats::discard_child(&child);
                    self.note_models_of(&result);
                    out.push(Fixed {
                        status: "not landed".into(),
                        child: Some(child),
                        changed: Vec::new(),
                        said: format!("its fix couldn't be joined ({e})"),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

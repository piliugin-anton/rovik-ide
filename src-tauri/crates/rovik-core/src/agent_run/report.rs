//! A task's report (docs/design/autopilot-plan.md 4.11, S09): its last
//! stage, the delivery, is a run that records what became of the result
//! and has the model write the report in one turn without tools, in the
//! implementer's context (the chat), from the IDE's facts. Saved as the
//! final assistant message with `meta.report` (the facts); when the turn
//! fails, the IDE writes it from the facts alone.

use super::autopilot::ReportFacts;
use super::run::{reasoning_meta, text_before_cut, Run, IDE_NOTE};
use serde_json::json;

/// The report brief's first line: what the IDE asks for (S09).
pub const REPORT_HEADING: &str = "# Write the task's report";

/// The change reviewer's brief for its view of the result starts so
/// (T08), after the IDE's note.
pub const VIEW_HEADING: &str = "# Write your view of the result";

/// The heading the reviewer's view goes under, after the report (T08).
pub const VIEW_SECTION: &str = "## The reviewer's view";

/// "1 h 5 min", "3 min", "40 s".
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// What the task spent, in a line: its time, its cost where the providers
/// said, and its tokens where they said.
pub fn spent_line(f: &ReportFacts) -> String {
    let time = duration(f.spent.ms);
    let line = match (f.spent.cost > 0.0, f.spent.uncosted) {
        (true, 0) => format!("{time}, {:.2} in the providers' currency", f.spent.cost),
        (true, n) => format!(
            "{time}, at least {:.2} in the providers' currency ({n} repl{} without a reported cost)",
            f.spent.cost,
            if n == 1 { "y" } else { "ies" }
        ),
        (false, _) => format!("{time}; the providers reported no cost"),
    };
    match f.spent.tokens {
        0 => line,
        n => format!("{line}; {} tokens", super::budget::tokens_text(n)),
    }
}

/// The facts as the brief and the IDE's own report list them.
fn facts_text(f: &ReportFacts) -> String {
    let mut out = vec![
        format!("- How it ended: {} (`{}`).", f.ending, f.outcome),
        format!("- What became of the result: {}.", f.delivery),
    ];
    if f.assumptions.is_empty() {
        out.push("- The plan's assumptions: none.".into());
    } else {
        out.push(format!(
            "- The plan's assumptions: {}",
            f.assumptions.join("; ")
        ));
    }
    if !f.criteria.is_empty() {
        let verdicts: Vec<String> = f
            .criteria
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let v = f.verdicts.iter().find(|v| v.criterion == i + 1);
                match v {
                    Some(v) if v.satisfied => format!("{}. {c} — met: {}", i + 1, v.note),
                    Some(v) => format!("{}. {c} — unmet: {}", i + 1, v.note),
                    None => format!("{}. {c} — not judged", i + 1),
                }
            })
            .collect();
        out.push(format!(
            "- The criteria and the acceptance check's verdicts:\n  {}",
            verdicts.join("\n  ")
        ));
    }
    if f.tests.is_empty() {
        out.push("- Tests written for it by the test writer: none.".into());
    } else {
        out.push(format!(
            "- Tests written for it by the test writer: {}.",
            f.tests
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if f.checks.is_empty() {
        out.push("- The checks: none ran.".into());
    } else {
        out.push(format!(
            "- The last checks: {}.",
            f.checks
                .iter()
                .map(|c| format!("`{}` {}", c.command, c.status))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(review) = &f.review {
        out.push(format!("- The change review: {review}."));
    }
    out.push(format!("- Spent: {}.", spent_line(f)));
    out.join("\n")
}

/// The note that asks the model for the report.
pub fn report_brief(f: &ReportFacts) -> String {
    format!(
        "{IDE_NOTE}{REPORT_HEADING}\n\nThe task is over. Write its report for the user, who wasn't here and reads this first. \
         Don't call any tools. Use these headings, in this order:\n\n\
         ## Assumptions\nWhat was decided that the request didn't say, and why: the plan's, then any you made while implementing. \"None.\" if none.\n\n\
         ## What changed\nWhat you changed and why, by file.\n\n\
         ## How it was verified\nThe checks and how they ended, the tests written for it, the acceptance check's verdicts.\n\n\
         ## Open\nWhat is unmet or still open, and what the user should look at{}; \"Nothing.\" if nothing.\n\n\
         The facts, from the IDE's records (use them as they are):\n\n{}",
        if f.partial || !f.delivered {
            ", starting with why the result wasn't delivered whole"
        } else {
            ""
        },
        facts_text(f)
    )
}

/// T08: what the change reviewer is asked, in its own chat, as a Thorough
/// task ends: its view of the result, for the user, from what it read of
/// the change and the IDE's facts.
pub fn view_brief(f: &ReportFacts) -> String {
    format!(
        "{IDE_NOTE}{VIEW_HEADING}\n\nThe task you reviewed is over, and its implementer is writing its report for the user, who wasn't here. \
         Your view goes under that report, as the reviewer's. From what you read of the change and the facts below, write for the user, \
         in a few short paragraphs without headings: whether the result does what the request asked, what you'd still check yourself \
         and why, and anything the implementer's report may make sound surer than it is. Don't call any tools.\n\n\
         The facts, from the IDE's records:\n\n{}",
        facts_text(f)
    )
}

/// A pushed result's pull request's description (D15, H04) until the
/// report is written: the IDE's facts.
pub fn pull_request_body(f: &ReportFacts) -> String {
    format!(
        "{}\n\n*Rovik's Autopilot wrote this from its records; the task's report replaces it once it's written.*",
        facts_text(f)
    )
}

/// The report the IDE writes itself, when the model's turn failed.
pub fn fallback_report(f: &ReportFacts) -> String {
    format!(
        "## Assumptions\n\n{}\n\n## How it was verified\n\n{}\n\n## Open\n\n{}\n\n*The model couldn't write this report: the IDE wrote it from its records.*",
        if f.assumptions.is_empty() {
            "None.".to_string()
        } else {
            f.assumptions
                .iter()
                .map(|a| format!("- {a}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        facts_text(f),
        if f.delivered && !f.partial {
            "Nothing.".to_string()
        } else {
            format!("{}.", f.delivery)
        }
    )
}

impl Run {
    /// T08: the change reviewer's view of the task's end: one turn without
    /// tools in the chat of the task's last change review that reported,
    /// after the IDE's brief. None: no review reported, or the turn gave
    /// nothing.
    async fn reviewers_view(&mut self, facts: &ReportFacts) -> Option<String> {
        use super::subagent::{Kind, SubStatus, SubagentSettings};
        let config = self.config().subagents;
        config.kind(Kind::Reviewer)?;
        // T06: a task its candidates implemented has the winner's review,
        // in the winner's chat.
        let rows = match self.winners_chat().await {
            Some(chat) => {
                let id = chat.clone();
                let rows = self.core.db.read(move |c| {
                    crate::chat_db::store::messages_with_meta(c, &id, "review", None)
                });
                rows.await.ok()?
            }
            None => {
                let recent = self.recent().await.ok()?;
                let start = recent.iter().position(|m| m.id == facts.task)?;
                recent[start..].to_vec()
            }
        };
        let (record_id, child_id) = rows.iter().rev().find_map(|m| {
            let review = m.meta.as_ref()?.get("review")?;
            let reported = matches!(review["status"].as_str(), Some("clean" | "findings"));
            let child = review["childDialogId"].as_str()?;
            (review["kind"] == "change" && reported).then(|| (m.id.clone(), child.to_string()))
        })?;
        let mut brief = super::run::new_message("user", &view_brief(facts));
        brief.meta = Some(json!({ "harness": "reviewerView" }));
        let id = child_id.clone();
        self.core
            .db
            .write(move |c| crate::chat_db::store::append_messages(c, &id, &[brief]))
            .await
            .ok()?;
        // Its own call id: the review's record isn't running again.
        let call_id = format!("view-{record_id}");
        let mut sub = self
            .new_sub(
                Kind::Reviewer,
                &call_id,
                config.budget(Kind::Reviewer),
                &config,
            )
            .await
            .ok()?;
        sub.view_only = true;
        let child = self
            .launch_child(child_id, &call_id, "The reviewer's view of the result", sub)
            .ok()?;
        let result = super::subagent_run::run_child(child).await?;
        self.note_models_of(&result);
        let text = result.text.trim();
        let ok = result.status == SubStatus::Done && !text.is_empty();
        super::log::write(
            &self.core,
            json!({
                "event": "reviewerView",
                "dialogId": self.dialog_id,
                "written": ok,
                "ms": result.ms,
            }),
        );
        ok.then(|| text.to_string())
    }

    /// T06: the chat of the candidate the task's pick took, if it took one.
    async fn winners_chat(&self) -> Option<String> {
        let (_, records) = super::autopilot::current_task(&self.core, &self.dialog_id)
            .await
            .ok()??;
        let winner = super::candidates::winner(&self.core, &self.dialog_id, &records).await;
        winner.ok()?.map(|w| w.chat)
    }

    /// S09: the Deliver stage's run: the delivery's record, and the report
    /// in one turn without tools, saved with its facts (`meta.report`);
    /// when the turn fails, the IDE's own. Always "done": a delivery is
    /// recorded whatever becomes of the report.
    pub(super) async fn deliver_stage(&mut self) -> Result<String, String> {
        if self.stage.is_none() {
            return Ok("done".into());
        }
        let Some(mut facts) = super::autopilot::report_facts(&self.core, &self.dialog_id).await?
        else {
            return Ok("done".into());
        };
        // D15: a server that pushes its results pushes this one first, so
        // the report can say where it went.
        if !self.stopped() {
            super::autopilot::push_result(&self.core, &self.dialog_id, &mut facts).await;
        }
        #[cfg(test)]
        if super::autopilot::crashes(&self.dialog_id, super::autopilot::Crash::InReport) {
            return Err("the report broke off".into());
        }
        let mut reference = None;
        if !self.stopped() {
            self.add_harness(report_brief(&facts), "report").await?;
            self.words_only = true;
            let turn = self.model_turn().await?;
            let written = text_before_cut(&turn.text, turn.repetition_cut)
                .trim()
                .to_string();
            let (content, by_ide) = if turn.failure.is_some() || written.is_empty() {
                (fallback_report(&facts), true)
            } else {
                (written, false)
            };
            // T08: a Thorough task's report has the change reviewer's view
            // under it, written in the reviewer's own chat.
            let thorough = self.thorough();
            let view = match thorough && !self.stopped() {
                true => self.reviewers_view(&facts).await,
                false => None,
            };
            let content = match &view {
                Some(view) => format!("{}\n\n{VIEW_SECTION}\n\n{view}", content.trim_end()),
                None => content,
            };
            let mut meta = reasoning_meta(turn.reasoning).unwrap_or_else(|| json!({}));
            meta["report"] = json!(facts);
            if by_ide {
                meta["report"]["byIde"] = json!(true);
            }
            if thorough {
                meta["report"]["reviewerView"] = json!(view.is_some());
            }
            // The pull request's description: the report, once written.
            let described = facts.pr.clone().zip(self.core.agents.deliverer());
            if let Some((pr, deliverer)) = described {
                deliverer
                    .describe(self.dialog_id.clone(), pr, content.clone())
                    .await;
            }
            self.finalize(&turn.message_id, content, Vec::new(), Some(meta), None)
                .await?;
            self.settle(true).await;
            reference = Some(turn.message_id);
        }
        super::log::write(
            &self.core,
            json!({
                "event": "report",
                "dialogId": self.dialog_id,
                "outcome": facts.outcome,
                "delivered": facts.delivered,
                "partial": facts.partial,
                "written": reference.is_some(),
            }),
        );
        let (status, outcome, summary) =
            (facts.status, facts.outcome.clone(), facts.delivery.clone());
        self.end_own_stage_with(|r| {
            r.status = status;
            r.outcome = Some(outcome);
            r.summary = Some(summary);
            r.reference = reference;
        })
        .await?;
        Ok("done".into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::autopilot::{CheckLine, Status};
    use super::super::budget::Spent;
    use super::*;

    fn facts() -> ReportFacts {
        ReportFacts {
            task: "u1".into(),
            title: "Greet".into(),
            branch: None,
            pr: None,
            outcome: "unmet".into(),
            ending: "A criterion is still unmet after the last acceptance check".into(),
            delivered: false,
            partial: true,
            delivery: "Delivered as partial, not landed: the acceptance check finds a criterion of the plan unmet".into(),
            assumptions: vec!["Names are ASCII.".into()],
            criteria: vec!["greet returns a greeting.".into(), "An empty name throws.".into()],
            verdicts: vec![
                super::super::acceptance::Verdict { criterion: 1, satisfied: true, note: "the test checks it".into() },
                super::super::acceptance::Verdict { criterion: 2, satisfied: false, note: "nothing throws".into() },
            ],
            tests: vec!["src/greet.test.ts".into()],
            checks: vec![CheckLine { command: "npm test".into(), status: "passed".into() }],
            review: Some("Clean".into()),
            spent: Spent { ms: 125_000, cost: 0.42, uncosted: 0, tokens: 0 },
            status: Status::Done,
        }
    }

    /// The brief has every fact the report must name, and asks for the
    /// headings in their order.
    #[test]
    fn the_report_brief_names_the_assumptions_tests_verdicts_and_cost() {
        let text = report_brief(&facts());
        assert!(text.contains(REPORT_HEADING));
        let order = [
            "## Assumptions",
            "## What changed",
            "## How it was verified",
            "## Open",
        ];
        let at: Vec<usize> = order.iter().map(|h| text.find(h).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(text.contains("- The plan's assumptions: Names are ASCII."));
        assert!(text.contains("1. greet returns a greeting. — met: the test checks it"));
        assert!(text.contains("2. An empty name throws. — unmet: nothing throws"));
        assert!(text.contains("`src/greet.test.ts`"));
        assert!(text.contains("`npm test` passed"));
        assert!(text.contains("2 min, 0.42 in the providers' currency"));
        assert!(text.contains("starting with why the result wasn't delivered whole"));
        assert!(text.contains("Delivered as partial, not landed"));
    }

    #[test]
    fn the_reviewers_brief_asks_for_its_view_from_the_facts() {
        let text = view_brief(&facts());
        assert!(text.contains(VIEW_HEADING));
        assert!(text.contains("Your view goes under that report, as the reviewer's"));
        assert!(text.contains("what you'd still check yourself"));
        assert!(text.contains("2. An empty name throws. — unmet: nothing throws"));
        assert!(text.contains("Don't call any tools."));
    }

    #[test]
    fn the_ides_own_report_says_what_it_knows() {
        let text = fallback_report(&facts());
        assert!(text.starts_with("## Assumptions\n\n- Names are ASCII."));
        assert!(text.contains("## Open\n\nDelivered as partial, not landed"));
        assert!(text.contains("the IDE wrote it from its records"));
        assert_eq!(duration(40_000), "40 s");
        assert_eq!(duration(3_900_000), "1 h 5 min");
        let mut none = facts();
        none.spent = Spent {
            ms: 1_000,
            cost: 0.0,
            uncosted: 3,
            tokens: 0,
        };
        assert_eq!(spent_line(&none), "1 s; the providers reported no cost");
        none.spent.cost = 0.1;
        assert!(spent_line(&none).contains("at least 0.10"));
        // The tokens, where the providers said.
        none.spent = Spent {
            ms: 1_000,
            tokens: 2_150_000,
            ..Spent::default()
        };
        assert_eq!(
            spent_line(&none),
            "1 s; the providers reported no cost; 2.15 million tokens"
        );
    }
}

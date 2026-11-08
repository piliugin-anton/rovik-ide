//! Persistent implementation evidence lives on the request, so remote chat
//! mirrors and reopened chats see the same result without a second database.

use crate::agent_run::{checklist, messages, run::Run};
use crate::chat_db::store::{self, MessageRow};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
/// Identifies the exact plan text used by a run, ignoring OS line endings.
pub fn plan_fingerprint(text: &str) -> String {
    Sha256::digest(text.replace("\r\n", "\n").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A project-relative identity shared by a plan's local and isolated copies.
pub fn plan_key(path: &str) -> String {
    format!(
        ".rovik/plans/{}",
        path.rsplit(['/', '\\']).next().unwrap_or(path)
    )
}

/// The most recent implementation attempt for this version of a saved plan.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanImplementation {
    /// Chat whose request implemented the plan.
    pub dialog_id: String,
    /// The implementation request, also the source of its persistent status.
    pub message_id: String,
    /// implementing, implemented, review-needed, or interrupted.
    pub status: String,
    /// Distinguishes this attempt from an unrelated later run in the same chat.
    pub run_id: Option<String>,
}

/// Reads matching evidence from all of a project's chats, including remote
/// mirrors. Matching content prevents an edited plan inheriting completion.
pub fn plan_implementation(
    conn: &Connection,
    project: Option<&str>,
    path: &str,
    content: &str,
) -> Result<Option<PlanImplementation>, String> {
    conn.query_row(
        "SELECT m.dialog_id, m.id, json_extract(m.meta, '$.planImplementation.status'),
                json_extract(m.meta, '$.planImplementation.runId')
         FROM messages m JOIN dialogs d ON d.id = m.dialog_id
         WHERE d.project_id IS ?1 AND d.parent_id IS NULL
           AND json_extract(m.meta, '$.implementsPlan') = 1
           AND json_extract(m.meta, '$.planKey') = ?2
           AND json_extract(m.meta, '$.planFingerprint') = ?3
           AND json_extract(m.meta, '$.planImplementation.status') IS NOT NULL
         ORDER BY json_extract(m.meta, '$.planImplementation.updatedAt') DESC,
                  m.created_at DESC, m.seq DESC LIMIT 1",
        params![project, plan_key(path), plan_fingerprint(content)],
        |row| {
            Ok(PlanImplementation {
                dialog_id: row.get(0)?,
                message_id: row.get(1)?,
                status: row.get(2)?,
                run_id: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(store::err)
}

fn completed_status(
    outcome: &str,
    checklist: Option<&checklist::CurrentChecklist>,
) -> &'static str {
    let success = matches!(outcome, "done" | "verified");
    if success
        && checklist.is_some_and(|c| {
            !c.items.is_empty() && c.items.iter().all(|item| item.status == "done")
        })
    {
        "implemented"
    } else if success || matches!(outcome, "unverified" | "verify-unverified") {
        "review-needed"
    } else {
        "interrupted"
    }
}

fn implementation_request(recent: &[MessageRow]) -> Option<&MessageRow> {
    let last = recent.iter().rposition(messages::is_request)?;
    let own = &recent[last];
    let implements = |m: &&MessageRow| {
        m.meta.as_ref().is_some_and(|meta| {
            meta["implementsPlan"] == true && meta["planFingerprint"].is_string()
        })
    };
    implements(&own).then_some(own)
}

impl Run {
    /// The agent declares semantic intent through a structured checklist
    /// association, backed by the user's own instruction in any language.
    /// File reads alone never create implementation evidence.
    pub(in crate::agent_run) async fn record_plan_implementation_intent(
        &self,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let Some(intent) = args.get("implementation") else {
            return Ok(());
        };
        if self.sub().is_some() || self.plan_mode || self.autopilot {
            return Ok(());
        }
        let recent = self.recent().await?;
        let request = recent
            .iter()
            .rev()
            .find(|m| messages::is_request(m))
            .ok_or("The implementation has no user request.")?;
        // The button already supplies explicit intent. Its expanded wire
        // request differs from the short title shown in chat, so a model
        // quoting that wire text must not invalidate the known association.
        if let Some(meta) = request
            .meta
            .as_ref()
            .filter(|m| m["implementsPlan"] == true)
        {
            return if intent
                .get("planPath")
                .and_then(|v| v.as_str())
                .is_none_or(|path| meta["planKey"] == plan_key(path))
            {
                Ok(())
            } else {
                Err("This checklist is already associated with a different plan.".into())
            };
        }
        let named = intent
            .get("planPath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or("Implementation needs the referenced planPath.")?;
        let quote = intent
            .get("instruction")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or("Implementation needs an exact quote of the user's instruction.")?;
        if !request.content.contains(quote) {
            return Err("The implementation instruction must be an exact quote from the user's current request, in its original language.".into());
        }
        let path = if Path::new(named).is_absolute() {
            named.to_string()
        } else {
            Path::new(
                self.root
                    .as_deref()
                    .ok_or("The referenced plan needs a project folder.")?,
            )
            .join(named.trim_start_matches("./"))
            .to_string_lossy()
            .into_owned()
        };
        let wire = request
            .meta
            .as_ref()
            .and_then(|m| m["sentContent"].as_str())
            .unwrap_or(&request.content);
        let relative = super::relative_plan_path(&path, self.root.as_deref());
        let reference_text = wire.replace('\\', "/");
        if !super::is_plan_file(&path)
            || !(reference_text.contains(&path.replace('\\', "/"))
                || reference_text.contains(&relative.replace('\\', "/")))
        {
            return Err("The implementation must name a saved plan explicitly referenced by the user's request.".into());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("The referenced plan can't be read: {e}"))?;
        let mut tracked = request.clone();
        let mut meta = tracked
            .meta
            .take()
            .filter(|m| m.is_object())
            .unwrap_or_else(|| json!({}));
        meta["implementsPlan"] = json!(true);
        meta["planPath"] = json!(path);
        meta["planKey"] = json!(plan_key(&path));
        meta["planFingerprint"] = json!(plan_fingerprint(&text));
        meta["planImplementationInstruction"] = json!(quote);
        tracked.meta = Some(meta);
        self.save_implementation(&tracked, "implementing", None)
            .await
    }

    /// Starts evidence for the current explicit request, including retries.
    pub(in crate::agent_run) async fn begin_plan_implementation(&self) -> Option<String> {
        if self.sub().is_some() || self.plan_mode {
            return None;
        }
        let recent = match self.recent().await {
            Ok(recent) => recent,
            Err(err) => {
                self.implementation_error(&err);
                return None;
            }
        };
        let request = implementation_request(&recent)?;
        match self
            .save_implementation(request, "implementing", None)
            .await
        {
            Ok(()) => Some(request.id.clone()),
            Err(err) => {
                self.implementation_error(&err);
                None
            }
        }
    }

    /// Saves the final outcome and checklist evidence on its original request.
    pub(in crate::agent_run) async fn finish_plan_implementation(
        &self,
        request_id: Option<&str>,
        outcome: &str,
    ) {
        if self.sub().is_some() || self.plan_mode {
            return;
        }
        let result = async {
            let recent = self.recent().await?;
            let request = match request_id {
                Some(id) => recent
                    .iter()
                    .find(|m| m.id == id)
                    .ok_or_else(|| "The implementation request is missing.".to_string())?,
                None => match implementation_request(&recent) {
                    Some(request) => request,
                    None => return Ok(()),
                },
            };
            let current = checklist::current_checklist(&recent);
            self.save_implementation(
                request,
                completed_status(outcome, current.as_ref()),
                Some(outcome),
            )
            .await
        }
        .await;
        if let Err(err) = result {
            self.implementation_error(&err);
        }
    }

    async fn save_implementation(
        &self,
        request: &MessageRow,
        status: &str,
        outcome: Option<&str>,
    ) -> Result<(), String> {
        let mut meta = request.meta.clone().unwrap_or_else(|| json!({}));
        let run_id = crate::agent_run::agent_status(&self.core)
            .runs
            .into_iter()
            .find(|run| run.dialog_id == self.dialog_id)
            .map(|run| run.run_id);
        meta["planImplementation"] = json!({
            "status": status,
            "outcome": outcome,
            "runId": run_id,
            "updatedAt": store::now_ms(),
        });
        self.store_meta(&request.id, meta).await
    }

    fn implementation_error(&self, error: &str) {
        self.core.emit(rovik_protocol::events::AgentEvent::Notice {
            dialog_id: Some(self.dialog_id.clone()),
            kind: "warning".into(),
            title: "The plan's implementation status couldn't be saved".into(),
            message: error.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use checklist::{CurrentChecklist, TodoItem};

    #[test]
    fn only_success_with_every_step_done_marks_a_plan_implemented() {
        let list = |statuses: &[&str]| CurrentChecklist {
            items: statuses
                .iter()
                .map(|s| TodoItem {
                    title: "Step".into(),
                    status: (*s).into(),
                })
                .collect(),
            carried: false,
            from_message_id: None,
        };
        for outcome in ["done", "verified"] {
            assert_eq!(
                completed_status(outcome, Some(&list(&["done", "done"]))),
                "implemented"
            );
            for statuses in [
                vec![],
                vec!["pending"],
                vec!["done", "in_progress"],
                vec!["skipped"],
                vec!["changed"],
            ] {
                assert_eq!(
                    completed_status(outcome, Some(&list(&statuses))),
                    "review-needed"
                );
            }
            assert_eq!(completed_status(outcome, None), "review-needed");
        }
        for outcome in [
            "stopped",
            "error",
            "unfinished-checklist",
            "verify-gave-up",
            "verify-needs-approval",
            "review-open",
            "budget",
            "paused-move",
        ] {
            assert_eq!(
                completed_status(outcome, Some(&list(&["done"]))),
                "interrupted"
            );
        }
        assert_eq!(
            completed_status("unverified", Some(&list(&["done"]))),
            "review-needed"
        );
    }

    #[test]
    fn plan_revisions_have_distinct_fingerprints_but_line_endings_do_not() {
        assert_eq!(plan_fingerprint("# Plan\r\n"), plan_fingerprint("# Plan\n"));
        assert_ne!(plan_fingerprint("# Plan\n"), plan_fingerprint("# Edited\n"));
    }
}

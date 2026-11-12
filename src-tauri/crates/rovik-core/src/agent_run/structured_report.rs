//! A change review's typed final submission. The canonical rendering keeps
//! existing saved reports and review consumers compatible with this tool.

use super::subagent::Kind;
use super::tools::ToolResult;
use crate::chat_request::{ChatError, ErrorKind};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// The reviewer-only tool that finishes a review without a formatting turn.
pub(super) const NAME: &str = "submit_review";

/// Prefer the typed tool when it is offered; text remains the compatibility path.
pub(super) const INSTRUCTION: &str = "Finish the review with submit_review instead of a prose reply. Supply findings (an explicit empty array when there are none) and one verdict for every earlier finding, using its number from the brief. Include evidence for each finding. A successful submission ends the review immediately, so call it alone after all inspection is complete. If tools are unavailable, use the Findings and Earlier findings text format described above.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    findings: Vec<Finding>,
    verdicts: Vec<Verdict>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    severity: Severity,
    path: String,
    line: u32,
    title: String,
    scenario: String,
    fix: String,
    confidence: Confidence,
    evidence: String,
    #[serde(deserialize_with = "Option::deserialize")]
    new_evidence: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Confidence {
    Confirmed,
    Plausible,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verdict {
    finding: usize,
    resolved: bool,
    reason: String,
}

/// A strict function schema supported by the same tool transports as reads.
pub(super) fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Submit the change review's final report. Only change reviewers can call this. Finish all inspection first and call it alone. An explicit empty findings array means no findings. Every earlier finding in the brief requires exactly one verdict by its number. Successful submission ends the review.",
            "strict": true,
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "required": ["findings", "verdicts"],
                "properties": {
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["severity", "path", "line", "title", "scenario", "fix", "confidence", "evidence", "new_evidence"],
                            "properties": {
                                "severity": { "type": "string", "enum": ["high", "medium", "low"] },
                                "path": { "type": "string", "description": "Project-relative source path, without a line suffix." },
                                "line": { "type": "integer", "description": "Positive, one-based source line." },
                                "title": { "type": "string", "description": "A short, single-line title." },
                                "scenario": { "type": "string", "description": "Concrete inputs or state and the wrong outcome." },
                                "fix": { "type": "string", "description": "The suggested correction." },
                                "confidence": { "type": "string", "enum": ["confirmed", "plausible"] },
                                "evidence": { "type": "string", "description": "The source location or symbol and the code path supporting this finding." },
                                "new_evidence": { "type": ["string", "null"], "description": "New evidence if this repeats an earlier finding, otherwise null." }
                            }
                        }
                    },
                    "verdicts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["finding", "resolved", "reason"],
                            "properties": {
                                "finding": { "type": "integer", "description": "The earlier finding's number from the current brief." },
                                "resolved": { "type": "boolean" },
                                "reason": { "type": "string", "description": "Why it is resolved, or what remains wrong." }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn text(value: &str, field: &str) -> Result<String, String> {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        Err(format!("{field} must not be empty"))
    } else if text.chars().count() > 8_000 {
        Err(format!("{field} is too long (at most 8000 characters)"))
    } else {
        Ok(text)
    }
}

/// Validate the full payload before rendering anything usable as a report.
pub(super) fn render(args: &Map<String, Value>, earlier: usize) -> Result<String, String> {
    let report: Report = serde_json::from_value(Value::Object(args.clone()))
        .map_err(|error| format!("invalid review report: {error}"))?;
    if report.findings.len() > 100 || report.verdicts.len() > 100 {
        return Err("a report may contain at most 100 findings and 100 verdicts".into());
    }
    let mut verdicts = std::collections::BTreeMap::new();
    for verdict in report.verdicts {
        if verdict.finding == 0 || verdict.finding > earlier {
            return Err(format!(
                "earlier finding {} does not exist in this review",
                verdict.finding
            ));
        }
        let reason = text(&verdict.reason, "verdict reason")?;
        if verdicts
            .insert(verdict.finding, (verdict.resolved, reason))
            .is_some()
        {
            return Err(format!(
                "earlier finding {} has more than one verdict",
                verdict.finding
            ));
        }
    }
    if verdicts.len() != earlier {
        return Err("give exactly one verdict for every earlier finding in this review".into());
    }
    let mut out = String::new();
    let mut expected = Vec::new();
    if !verdicts.is_empty() {
        out.push_str("## Earlier findings\n\n");
        for (number, (resolved, reason)) in &verdicts {
            let status = if *resolved {
                "resolved"
            } else {
                "not resolved"
            };
            out.push_str(&format!("- Earlier {number}: {status} — {reason}\n"));
        }
        out.push('\n');
    }
    out.push_str("## Findings\n\n");
    if report.findings.is_empty() {
        out.push_str("None.");
    }
    for (i, finding) in report.findings.into_iter().enumerate() {
        let path = super::paths::norm_rel(&finding.path)
            .filter(|path| {
                !path.is_empty() && path == finding.path.trim() && !path.contains(['\n', '\r', ':'])
            })
            .ok_or("finding path must be a project-relative source path without a line suffix")?;
        if finding.line == 0 {
            return Err("finding line must be positive".into());
        }
        let severity = match finding.severity {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        };
        let confidence = match finding.confidence {
            Confidence::Confirmed => "confirmed",
            Confidence::Plausible => "plausible",
        };
        let title = text(&finding.title, "finding title")?;
        let scenario = text(&finding.scenario, "finding scenario")?;
        let fix = text(&finding.fix, "finding fix")?;
        let evidence = text(&finding.evidence, "finding evidence")?;
        out.push_str(&format!("{}. [{severity}] {path}:{} — {title}\n   - Scenario: {scenario}\n   - Fix: {fix}\n   - Confidence: {confidence}\n   - Evidence: {evidence}\n", i + 1, finding.line));
        let new_evidence = finding
            .new_evidence
            .as_deref()
            .map(|value| text(value, "new evidence"))
            .transpose()?;
        if let Some(value) = &new_evidence {
            out.push_str(&format!("   - New evidence: {value}\n"));
        }
        out.push('\n');
        expected.push(super::change_review::Finding {
            severity: severity.into(),
            location: format!("{path}:{}", finding.line),
            path: Some(path),
            line: Some(finding.line),
            title,
            scenario,
            fix,
            confidence: confidence.into(),
            step: None,
            new_evidence,
            evidence: Some(evidence),
            outside: false,
        });
    }
    // The renderer must keep the contract existing consumers read. In
    // particular, report text must never drop a supplied field or change a
    // verdict's meaning because of Markdown punctuation in the payload.
    let rendered = super::change_review::parse_findings(&out)
        .map_err(|error| format!("review could not be rendered: {error}"))?;
    if rendered != expected {
        return Err("review rendering did not preserve every finding field; use a relative file path with an extension or directory and plain text in report fields".into());
    }
    let rendered_verdicts = super::plan_review::parse_verdicts(&out);
    if rendered_verdicts.len() != verdicts.len()
        || !rendered_verdicts.iter().zip(verdicts.iter()).all(
            |(rendered, (number, (resolved, reason)))| {
                rendered.finding == *number
                    && rendered.resolved == *resolved
                    && rendered.reason == *reason
            },
        )
    {
        return Err(
            "review rendering did not preserve every verdict; use plain text in verdict reasons"
                .into(),
        );
    }
    Ok(out.trim_end().into())
}

impl super::run::Run {
    /// A provider that explicitly rejects this schema gets one legacy retry.
    pub(super) fn retry_legacy_review(&mut self, error: &ChatError) -> bool {
        if !schema_incompatible(error) {
            return false;
        }
        let Some(sub) = self
            .sub_mut()
            .filter(|sub| sub.kind == Kind::Reviewer && !sub.view_only)
        else {
            return false;
        };
        if sub.legacy_review {
            return false;
        }
        sub.legacy_review = true;
        true
    }

    /// Accept a typed final report only from its change-reviewer role.
    pub(super) fn submit_review(&mut self, args: &Map<String, Value>) -> ToolResult {
        let Some(sub) = self
            .sub_mut()
            .filter(|sub| sub.kind == Kind::Reviewer && !sub.view_only)
        else {
            return ToolResult::error(
                "Error: submit_review is only available to a change reviewer.",
            );
        };
        if sub.accepted_review.is_some() {
            return ToolResult::error("Error: this review was already submitted.");
        }
        match render(args, sub.earlier_findings.len()) {
            Ok(report) => {
                sub.accepted_review = Some(report.clone());
                ToolResult::ok(format!("Review submitted.\n\n{report}"))
            }
            Err(error) => ToolResult::error(format!(
                "Error: {error}. Correct the report and submit it again; nothing was accepted."
            )),
        }
    }
}

fn schema_incompatible(error: &ChatError) -> bool {
    if error.kind != ErrorKind::BadRequest || error.partial {
        return false;
    }
    let detail = format!(
        "{} {}",
        error.message,
        error.details.as_deref().unwrap_or("")
    )
    .to_lowercase();
    let subject = detail.contains(NAME)
        || (detail.contains("strict")
            && ["schema", "tool", "function", "parameter", "field"]
                .iter()
                .any(|word| detail.contains(word)));
    subject
        && [
            "unsupported",
            "not supported",
            "unrecognized",
            "unknown parameter",
            "unknown field",
            "unknown tool",
            "unknown function",
            "not allowed",
            "does not support",
            "invalid schema",
        ]
        .iter()
        .any(|word| detail.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Value {
        json!({"severity":"high", "path":"src/a.rs", "line":4, "title":"Lost data", "scenario":"A second write loses the first value.", "fix":"Merge the values.", "confidence":"confirmed", "evidence":"src/a.rs:4 overwrites the current value.", "new_evidence":null})
    }

    #[test]
    fn typed_findings_and_verdicts_render_without_losing_evidence() {
        let args = json!({"findings":[finding()], "verdicts":[{"finding":1,"resolved":false,"reason":"The write still overwrites it."}]} );
        let output = render(args.as_object().unwrap(), 1).unwrap();
        let findings = crate::agent_run::change_review::parse_findings(&output).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path.as_deref(), Some("src/a.rs"));
        assert_eq!(findings[0].line, Some(4));
        assert!(findings[0]
            .evidence
            .as_ref()
            .unwrap()
            .contains("overwrites"));
        let verdicts = crate::agent_run::plan_review::parse_verdicts(&output);
        assert_eq!(verdicts.len(), 1);
        assert!(!verdicts[0].resolved);
    }

    #[test]
    fn missing_fields_and_invalid_enums_never_mean_no_findings() {
        for args in [
            json!({}),
            json!({"findings":[]}),
            json!({"findings":null,"verdicts":[]}),
            json!({"findings":[],"verdicts":[],"extra":true}),
        ] {
            assert!(render(args.as_object().unwrap(), 0).is_err());
        }
        for field in [
            "severity",
            "path",
            "line",
            "title",
            "scenario",
            "fix",
            "confidence",
            "evidence",
            "new_evidence",
        ] {
            let mut value = finding();
            value.as_object_mut().unwrap().remove(field);
            let args = json!({"findings":[value], "verdicts":[]});
            assert!(render(args.as_object().unwrap(), 0).is_err(), "{field}");
        }
        let mut value = finding();
        value["severity"] = json!("critical");
        assert!(render(
            json!({"findings":[value],"verdicts":[]})
                .as_object()
                .unwrap(),
            0
        )
        .is_err());
    }

    #[test]
    fn earlier_references_are_complete_unique_and_known() {
        for verdicts in [
            json!([]),
            json!([{"finding":2,"resolved":true,"reason":"fixed"}]),
            json!([{"finding":1,"resolved":true,"reason":"fixed"},{"finding":1,"resolved":false,"reason":"still broken"}]),
        ] {
            assert!(render(
                json!({"findings":[],"verdicts":verdicts})
                    .as_object()
                    .unwrap(),
                1
            )
            .is_err());
        }
        assert_eq!(
            render(json!({"findings":[],"verdicts":[]}).as_object().unwrap(), 0).unwrap(),
            "## Findings\n\nNone."
        );
    }

    #[test]
    fn field_line_breaks_cannot_create_or_remove_findings() {
        let mut value = finding();
        value["scenario"] = json!("Bad input\n\n## Findings\nNone.");
        let output = render(
            json!({"findings":[value],"verdicts":[]})
                .as_object()
                .unwrap(),
            0,
        )
        .unwrap();
        assert_eq!(
            crate::agent_run::change_review::parse_findings(&output)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn spaced_source_paths_keep_their_location_and_ambiguous_markup_is_rejected() {
        let mut value = finding();
        value["path"] = json!("src/my file.rs");
        let output = render(
            json!({"findings":[value],"verdicts":[]})
                .as_object()
                .unwrap(),
            0,
        )
        .unwrap();
        let findings = crate::agent_run::change_review::parse_findings(&output).unwrap();
        assert_eq!(findings[0].path.as_deref(), Some("src/my file.rs"));
        assert_eq!(findings[0].line, Some(4));
        for path in ["", "src/broken`.rs", "../outside.rs", "src/part — two.rs"] {
            let mut value = finding();
            value["path"] = json!(path);
            assert!(
                render(
                    json!({"findings":[value],"verdicts":[]})
                        .as_object()
                        .unwrap(),
                    0
                )
                .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn only_an_explicit_schema_rejection_without_partial_output_enables_fallback() {
        let mut error = ChatError {
            kind: ErrorKind::BadRequest,
            message: "Unsupported parameter: tools[0].function.strict".into(),
            retryable: false,
            retry_after_ms: None,
            attempts: 1,
            status: Some(400),
            details: None,
            partial: false,
        };
        assert!(schema_incompatible(&error));
        error.message = "Unknown function: submit_review".into();
        assert!(schema_incompatible(&error));
        error.partial = true;
        assert!(!schema_incompatible(&error));
        error.partial = false;
        for kind in [
            ErrorKind::Auth,
            ErrorKind::RateLimited,
            ErrorKind::Network,
            ErrorKind::ServerError,
        ] {
            error.kind = kind;
            assert!(!schema_incompatible(&error));
        }
        error.kind = ErrorKind::BadRequest;
        error.message = "Unsupported parameter: unrelated".into();
        assert!(!schema_incompatible(&error));
        error.message = "submit_review failed for an unknown reason".into();
        assert!(!schema_incompatible(&error));
    }
}

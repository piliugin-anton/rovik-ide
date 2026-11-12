//! Pure evidence gate. Authenticity and filesystem checks belong to trusted adapters.

use crate::{
    id,
    identity::{Digest, EvidenceKey},
    plan::CheckSpec,
    require, unique, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Native observation status; only a consistent pass satisfies a required check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Required properties and observations succeeded.
    Pass,
    /// An executed property was violated.
    Fail,
    /// Infrastructure, parsing or checker failure.
    Error,
    /// Execution deadline exceeded.
    Timeout,
    /// Registered environment lacks a required capability.
    Unsupported,
    /// Applicability must be resolved in the plan, never on the result.
    NotApplicable,
}

/// Bounded actionable diagnostic, independent of outcome classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Registered rule or stable failure category.
    pub rule_id: String,
    /// Explanation and next action.
    pub message: String,
    /// Optional repository-relative file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Optional one-based source line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// V2 normalized observation. Deserialization alone never admits evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    /// Exactly 2.
    pub schema_version: u32,
    /// Active run identity.
    pub run_id: String,
    /// Immutable result identity.
    pub result_id: String,
    /// Fenced execution operation.
    pub operation_id: String,
    /// Registered check identity.
    pub check_id: String,
    /// Exact feature/target/flag matrix member.
    pub configuration_digest: Digest,
    /// Trusted normalizer/driver identity.
    pub checker_digest: Digest,
    /// All acceptance-sensitive inputs.
    pub evidence_key: EvidenceKey,
    /// Deterministic native outcome.
    pub status: Status,
    /// None for signals or missing execution.
    pub exit_code: Option<i32>,
    /// Passed plus failed, excluding skipped cases.
    pub executed_test_count: u64,
    /// Cases independently observed passing.
    pub passed_case_ids: Vec<String>,
    /// Cases independently observed failing.
    pub failed_case_ids: Vec<String>,
    /// Skipped or ignored cases, never passes.
    pub skipped_case_ids: Vec<String>,
    /// Exact raw report/log artifacts.
    pub evidence: Vec<Digest>,
    /// Controller-authenticated execution receipt, not a worker assertion.
    pub runner_receipt_id: String,
    /// Explanatory diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}
impl CheckResult {
    /// Reject ambiguous inventory and unsupported versions before admission.
    pub fn validate(&self) -> Result<()> {
        require(self.schema_version == 2, "unsupported check-result version")?;
        for value in [
            &self.run_id,
            &self.result_id,
            &self.operation_id,
            &self.check_id,
            &self.runner_receipt_id,
        ] {
            id(value)?;
        }
        let mut all = BTreeSet::new();
        for group in [
            &self.passed_case_ids,
            &self.failed_case_ids,
            &self.skipped_case_ids,
        ] {
            unique(group.iter().map(String::as_str))?;
            for case in group {
                require(all.insert(case), "test case has conflicting outcomes")?;
            }
        }
        require(
            !self.evidence.is_empty()
                && self.evidence.iter().collect::<BTreeSet<_>>().len() == self.evidence.len(),
            "missing or duplicate evidence",
        )?;
        for d in &self.diagnostics {
            id(&d.rule_id)?;
            require(
                !d.message.trim().is_empty() && d.message.len() <= 65536 && d.line != Some(0),
                "invalid diagnostic",
            )?;
            if let Some(f) = &d.file {
                crate::scope::validate_path(f)?;
            }
        }
        Ok(())
    }
    /// Identity within a verification matrix.
    pub fn identity(&self) -> (&str, &Digest) {
        (&self.check_id, &self.configuration_digest)
    }
}

/// Controller-created attestations. Intentionally not serializable/deserializable.
#[derive(Debug, Clone, Default)]
pub struct TrustedPreconditions {
    /// Receipts, operation generations and trusted normalizers checked.
    pub evidence_provenance_verified: bool,
    /// Every referenced artifact read and verified.
    pub artifacts_present_and_verified: bool,
    /// Frozen candidate admitted against all scopes.
    pub scope_verified: bool,
    /// Validated full matrix and obligations.
    pub plan_obligations_complete: bool,
    /// Contract and design have no blocking unknowns.
    pub no_blocking_unknowns: bool,
    /// Required semantic findings resolved.
    pub review_requirements_satisfied: bool,
    /// Exact subjects, scopes, expiry and revocations checked now.
    pub approvals_current: bool,
    /// Candidate still matches current merge context.
    pub integration_candidate_current: bool,
    /// All earlier failures have an explicit disposition.
    pub prior_failures_resolved: bool,
    /// No unknown reservations or exhausted accounts.
    pub accounting_complete: bool,
    /// The run has not been cancelled, stopped, rejected, or timed out before acceptance.
    pub run_eligible: bool,
}

/// Deterministic gate verdict, distinct from an accepted workflow state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Complete evidence permits final approval evaluation.
    Ready,
    /// An executed required property failed.
    Failed,
    /// Evidence or a prerequisite is missing or inconsistent.
    Blocked,
}

/// Stable reason codes accompany every non-ready decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateDecision {
    /// Gate outcome; not merge authority.
    pub verdict: Verdict,
    /// Failures first, then blockers.
    pub reasons: Vec<String>,
}

/// Evaluate exactly one admitted result per effective required final check.
pub fn evaluate(
    run_id: &str,
    key: &EvidenceKey,
    required: &[CheckSpec],
    admitted: &[CheckResult],
    trusted: &TrustedPreconditions,
) -> GateDecision {
    let mut blockers = Vec::new();
    let mut failures = Vec::new();
    for (name, passed) in [
        (
            "evidence_provenance_verified",
            trusted.evidence_provenance_verified,
        ),
        (
            "artifacts_present_and_verified",
            trusted.artifacts_present_and_verified,
        ),
        ("scope_verified", trusted.scope_verified),
        (
            "plan_obligations_complete",
            trusted.plan_obligations_complete,
        ),
        ("no_blocking_unknowns", trusted.no_blocking_unknowns),
        (
            "review_requirements_satisfied",
            trusted.review_requirements_satisfied,
        ),
        ("approvals_current", trusted.approvals_current),
        (
            "integration_candidate_current",
            trusted.integration_candidate_current,
        ),
        ("prior_failures_resolved", trusted.prior_failures_resolved),
        ("accounting_complete", trusted.accounting_complete),
        ("run_eligible", trusted.run_eligible),
    ] {
        if !passed {
            blockers.push(format!("precondition:{name}"));
        }
    }
    let mut specs = BTreeMap::new();
    for spec in required {
        if specs.insert(spec.identity(), spec).is_some() {
            blockers.push(format!("duplicate-plan-check:{}", spec.check_id));
        }
    }
    if specs.is_empty() {
        blockers.push("empty-required-final-plan".into());
    }
    let mut results = BTreeMap::new();
    for result in admitted {
        if let Err(e) = result.validate() {
            blockers.push(format!("invalid-result:{}:{e}", result.check_id));
        }
        if results.insert(result.identity(), result).is_some() {
            blockers.push(format!("ambiguous-result:{}", result.check_id));
        }
        if !specs.contains_key(&result.identity()) {
            blockers.push(format!("unplanned-result:{}", result.check_id));
        }
    }
    for (identity, spec) in specs {
        let label = format!("{}@{}", spec.check_id, spec.configuration_digest);
        let Some(result) = results.get(&identity) else {
            blockers.push(format!("missing:{label}"));
            continue;
        };
        if result.run_id != run_id {
            blockers.push(format!("wrong-run:{label}"));
            continue;
        }
        if result.evidence_key != *key {
            blockers.push(format!("stale-binding:{label}"));
            continue;
        }
        if result.checker_digest != spec.checker_digest {
            blockers.push(format!("wrong-checker:{label}"));
            continue;
        }
        if result.evidence.is_empty() {
            blockers.push(format!("missing-evidence:{label}"));
            continue;
        }
        if result.status == Status::Fail {
            failures.push(format!("check-failed:{label}"));
            continue;
        }
        if result.status != Status::Pass {
            blockers.push(format!("unverified:{label}:{:?}", result.status));
            continue;
        }
        if !result
            .exit_code
            .is_some_and(|code| spec.success_exit_codes.contains(&code))
        {
            blockers.push(format!("inconsistent-pass-exit:{label}"));
        }
        if !result.failed_case_ids.is_empty() {
            failures.push(format!("failed-cases:{label}"));
        }
        if result.executed_test_count < spec.minimum_test_count {
            blockers.push(format!("insufficient-test-count:{label}"));
        }
        if result.executed_test_count
            < (result.passed_case_ids.len() + result.failed_case_ids.len()) as u64
        {
            blockers.push(format!("inconsistent-test-inventory:{label}"));
        }
        let missing: Vec<_> = spec
            .expected_case_ids
            .iter()
            .filter(|case| !result.passed_case_ids.contains(case))
            .cloned()
            .collect();
        if !missing.is_empty() {
            blockers.push(format!(
                "required-cases-not-passed:{label}:{}",
                missing.join(",")
            ));
        }
    }
    let verdict = if !failures.is_empty() {
        Verdict::Failed
    } else if !blockers.is_empty() {
        Verdict::Blocked
    } else {
        Verdict::Ready
    };
    failures.extend(blockers);
    GateDecision {
        verdict,
        reasons: failures,
    }
}

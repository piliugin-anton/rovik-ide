//! Explicit disposition of obsolete check identities after a reviewed semantic replan.
use crate::{
    approvals::{ApprovalRequest, Principal},
    controller::{Controller, TaskSetup},
    evidence::{CheckResult, Status},
    identity::Digest,
    plan::ApprovalRequirement,
    require, runner, wire, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
/// A historical failure remains a failure. This decision explains why its old
/// obligation/configuration is superseded by a newly approved and verified plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplannedFailure {
    /// Exact historical failed or incomplete observation.
    pub previous_result_id: String,
    /// Activated replacement setup, not an arbitrary candidate or plan label.
    pub replacement_input: Digest,
    /// Exact current passing results covering the new complete final matrix.
    pub replacement_result_ids: BTreeSet<String>,
    /// Why the obsolete check no longer expresses the approved requirement.
    pub rationale: String,
    /// Explicit remaining uncertainty accepted by the authorized reviewer.
    pub residual_risk: String,
}
impl Controller {
    fn validate_replanned_failure(
        &self,
        setup: &TaskSetup,
        disposition: &ReplannedFailure,
        results: &[CheckResult],
    ) -> Result<()> {
        require(
            !disposition.rationale.trim().is_empty()
                && !disposition.residual_risk.trim().is_empty(),
            "replan failure disposition needs rationale and residual risk",
        )?;
        require(
            self.ledger.run(&setup.run_id)?.input_digest == disposition.replacement_input,
            "disposition names a superseded input",
        )?;
        let previous = self
            .ledger
            .result(&setup.run_id, &disposition.previous_result_id)?;
        require(
            previous.status != Status::Pass
                && previous.evidence_key.plan_digest != Digest::of_json(&setup.plan)?,
            "failure is not from a superseded plan",
        )?;
        require(
            !setup.plan.required_final().any(|check| {
                check.identity() == previous.identity()
                    && check.checker_digest == previous.checker_digest
            }),
            "unchanged checks require a same-check failure resolution",
        )?;
        let needed: BTreeSet<_> = setup
            .plan
            .required_final()
            .map(|check| (check.check_id.clone(), check.configuration_digest.clone()))
            .collect();
        let replacements: Vec<_> = results
            .iter()
            .filter(|result| {
                disposition
                    .replacement_result_ids
                    .contains(&result.result_id)
            })
            .collect();
        require(
            replacements.len() == disposition.replacement_result_ids.len()
                && replacements
                    .iter()
                    .all(|result| result.status == Status::Pass)
                && replacements
                    .iter()
                    .map(|result| (result.check_id.clone(), result.configuration_digest.clone()))
                    .collect::<BTreeSet<_>>()
                    == needed,
            "replanned disposition lacks the exact current passing final matrix",
        )?;
        require(
            self.ledger.approval_current(
                &setup.run_id,
                &ApprovalRequirement {
                    subject_digest: disposition.replacement_input.clone(),
                    approval_type: "replan".into(),
                    obligation_ids: vec![],
                },
                &Digest::of_json(&setup.contract.scope)?,
                &Digest::of_json(&setup.policy)?,
                &setup.authority,
                runner::now_ms()?,
            )?,
            "replacement replan approval is missing or revoked",
        )
    }
    /// Approve a scoped historical disposition; no observation is rewritten as passing.
    pub fn resolve_replanned_failure(
        &self,
        principal: &Principal,
        run_id: &str,
        disposition: &ReplannedFailure,
    ) -> Result<String> {
        let setup = self.setup(run_id)?;
        setup.authority.authorize(principal, "replan-resolution")?;
        let candidate = self
            .current_candidate(run_id)?
            .ok_or_else(|| crate::Error::new("no current candidate".into()))?;
        let results =
            self.candidate_results(run_id, &Digest::of_json(&self.key(&setup, &candidate)?)?)?;
        self.validate_replanned_failure(&setup, disposition, &results)?;
        let now = runner::now_ms()?;
        let artifact = self.ledger.store_json("exception", disposition, now)?;
        let approval = self.ledger.grant(
            principal,
            &setup.authority,
            ApprovalRequest {
                run_id: Some(run_id.into()),
                subject_digest: artifact.digest,
                approval_type: "replan-resolution".into(),
                policy_digest: Digest::of_json(&setup.policy)?,
                scope_digest: Digest::of_json(&setup.contract.scope)?,
                expires_ms: None,
            },
            now,
        )?;
        self.ledger.connection.execute(
            "INSERT INTO failure_resolutions VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                disposition.previous_result_id,
                approval.approval_id,
                now as i64
            ],
        )?;
        Ok(approval.approval_id)
    }
    pub(crate) fn replanned_resolved(
        &self,
        setup: &TaskSetup,
        results: &[CheckResult],
    ) -> Result<BTreeSet<String>> {
        let mut query=self.ledger.connection.prepare("SELECT a.subject_digest FROM failure_resolutions f JOIN approvals a ON a.approval_id=f.approval_id JOIN check_results c ON c.result_id=f.result_id WHERE c.run_id=?1 AND a.approval_type='replan-resolution'")?;
        let mut resolved = BTreeSet::new();
        for row in query.query_map([&setup.run_id], |row| row.get::<_, String>(0))? {
            let subject = Digest::parse(&row?)?;
            let disposition: ReplannedFailure = wire::decode(&self.ledger.read(&subject)?)?;
            if self
                .validate_replanned_failure(setup, &disposition, results)
                .is_ok()
                && self.ledger.approval_current(
                    &setup.run_id,
                    &ApprovalRequirement {
                        subject_digest: subject,
                        approval_type: "replan-resolution".into(),
                        obligation_ids: vec![],
                    },
                    &Digest::of_json(&setup.contract.scope)?,
                    &Digest::of_json(&setup.policy)?,
                    &setup.authority,
                    runner::now_ms()?,
                )?
            {
                resolved.insert(disposition.previous_result_id);
            }
        }
        Ok(resolved)
    }
}

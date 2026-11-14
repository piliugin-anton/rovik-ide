//! Historical baseline checks use the candidate's complete configuration matrix.

use crate::{
    controller::{Controller, TaskSetup},
    evidence::{CheckResult, Status},
    identity::Digest,
    require,
    runner::{self, Cancellation},
    Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An explicitly reviewed pre-existing failure, never a candidate waiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailure {
    /// Registered check identity.
    pub check_id: String,
    /// Exact target/features/flags.
    pub configuration_digest: Digest,
    /// Exact expected failing case set; new or missing failures require another decision.
    pub case_ids: BTreeSet<String>,
    /// Exact observed failure signature, required when no behavioral cases ran.
    /// Raw diagnostic artifacts are included: changed compiler failures need new review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_signature: Option<Digest>,
    /// Why this existing defect is part of the requested change.
    pub rationale: String,
}
/// Hash failure content without operation IDs or timestamps. A historical compiler
/// failure is never generalized into permission for other errors to pass.
pub fn failure_signature(result: &CheckResult) -> Result<Digest> {
    Digest::of_json(
        &serde_json::json!({"status":result.status,"category":result.failure_category,
        "exit":result.exit_code,"failed":result.failed_case_ids,"skipped":result.skipped_case_ids,
        "diagnostics":result.diagnostics,"evidence":result.evidence}),
    )
}
impl ExpectedFailure {
    /// Match only the approved implementation failure, including its exact case inventory.
    pub fn matches(&self, result: &CheckResult) -> Result<bool> {
        Ok(result.check_id == self.check_id
            && result.configuration_digest == self.configuration_digest
            && result.repairable()
            && result.skipped_case_ids.is_empty()
            && result
                .failed_case_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                == self.case_ids
            && match &self.observed_signature {
                Some(signature) => *signature == failure_signature(result)?,
                None => !self.case_ids.is_empty(),
            })
    }
}
/// Separate historical expectations, authenticated against exact base and plan identities.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselinePlan {
    /// Every other check must pass. Infrastructure failures are never baseline exceptions.
    #[serde(default)]
    pub expected_failures: Vec<ExpectedFailure>,
}
impl BaselinePlan {
    /// Bind historical expectations to immutable input and the same final verification plan.
    pub fn subject(&self, setup: &TaskSetup) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!({"base":setup.base.digest()?,"plan":Digest::of_json(&setup.plan)?,"baseline":self}),
        )
    }
    /// Validate exact scopes and protected case references before executing a baseline.
    pub fn validate(&self, setup: &TaskSetup) -> Result<()> {
        let mut identities = BTreeSet::new();
        for expected in &self.expected_failures {
            require(
                identities.insert((&expected.check_id, &expected.configuration_digest)),
                "duplicate baseline expectation",
            )?;
            require(
                (!expected.case_ids.is_empty() || expected.observed_signature.is_some())
                    && !expected.rationale.trim().is_empty(),
                "baseline exception needs cases or an exact failure signature, and rationale",
            )?;
            require(
                setup.plan.required_final().any(|check| {
                    check.check_id == expected.check_id
                        && check.configuration_digest == expected.configuration_digest
                        && expected
                            .case_ids
                            .iter()
                            .all(|case| check.expected_case_ids.contains(case))
                }),
                "baseline exception names an unplanned check or case",
            )?;
        }
        Ok(())
    }
}
impl Controller {
    /// Run every required final check on the immutable baseline and persist its real outcome.
    pub(crate) fn establish_baseline(
        &mut self,
        setup: &TaskSetup,
        cancel: &Cancellation,
    ) -> Result<()> {
        let key = self.select_binding(setup, &setup.base)?;
        let mut observations = Vec::new();
        let mut pending: Vec<_> = setup.plan.required_final().collect();
        let mut done = BTreeSet::new();
        while !pending.is_empty() {
            let at = pending
                .iter()
                .position(|check| {
                    check.dependencies.iter().all(|dependency| {
                        done.contains(&(dependency.clone(), check.configuration_digest.clone()))
                            || setup.plan.checks.iter().any(|prior| {
                                prior.check_id == *dependency
                                    && prior.phase != crate::plan::Phase::Final
                            })
                    })
                })
                .ok_or_else(|| {
                    crate::Error::new("baseline dependencies cannot be scheduled".into())
                })?;
            let check = pending.remove(at);
            let result = self.check(setup, &setup.base, &key, check, "baseline", cancel)?;
            let expected = setup.baseline.expected_failures.iter().find(|e| {
                e.check_id == check.check_id && e.configuration_digest == check.configuration_digest
            });
            let matches = match expected {
                None => result.status == Status::Pass,
                Some(expected) => expected.matches(&result)?,
            };
            observations.push(serde_json::json!({"result_id":result.result_id,"status":result.status,"expectation_matches":matches}));
            self.ledger.store_json("baseline-report", &serde_json::json!({"base":setup.base.digest()?,"binding":Digest::of_json(&key)?,"observations":observations}), runner::now_ms()?)?;
            require(matches, format!("baseline check {} differs from the approved historical expectation; review baseline evidence before continuing", check.check_id))?;
            done.insert((check.check_id.clone(), check.configuration_digest.clone()));
        }
        Ok(())
    }
}

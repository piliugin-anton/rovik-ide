//! Scoped policy dispositions. Approvals authorize exceptions, never rewrite results.

use crate::{
    identity::Digest, ledger::Ledger, plan::ApprovalRequirement, require, scope::Scope, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The explicit meaning of a reviewed obligation disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionKind {
    /// The obligation does not apply in the listed configurations.
    NotApplicable,
    /// Alternative evidence or an explicitly accepted residual risk replaces it.
    Exception,
}

/// Immutable, independently approved rationale for changing an obligation's treatment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyException {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable operator-assigned exception identity.
    pub exception_id: String,
    /// The obligation affected by this decision.
    pub obligation_id: String,
    /// Non-applicability and residual-risk acceptance are distinct decisions.
    pub kind: DispositionKind,
    /// Exact task scope; broader or inherited exceptions are not implicit.
    pub scope: Scope,
    /// Registered person or team accountable for the accepted risk.
    pub owner: String,
    /// Reviewable reason for the decision.
    pub rationale: String,
    /// Exclusive UTC expiry in milliseconds; exceptions cannot be indefinite.
    pub expires_ms: u64,
    /// Only these exact configurations receive the disposition.
    pub configurations: BTreeSet<Digest>,
    /// Immutable evidence supporting non-applicability or the replacement control.
    pub replacement_evidence: Vec<Digest>,
    /// Explicit remaining risk, when an alternative control is insufficient.
    pub residual_risk: Option<String>,
    /// Exports label acceptance as qualified when residual risk is accepted.
    pub qualifies_acceptance: bool,
}

impl PolicyException {
    /// Validate exact scope, configurations and evidence before registering the plan.
    pub fn validate(
        &self,
        scope: &Scope,
        configurations: &BTreeSet<Digest>,
        ledger: &Ledger,
        now: u64,
    ) -> Result<()> {
        require(
            self.schema_version == 1,
            "unsupported policy exception version",
        )?;
        crate::id(&self.exception_id)?;
        crate::id(&self.obligation_id)?;
        crate::id(&self.owner)?;
        require(
            !["TRUST-001", "SPEC-001"].contains(&self.obligation_id.as_str()),
            "non-waivable harness invariant",
        )?;
        self.scope.validate()?;
        require(&self.scope == scope, "exception has a different task scope")?;
        require(
            !self.configurations.is_empty() && self.configurations.is_subset(configurations),
            "exception has missing or unregistered configurations",
        )?;
        require(self.expires_ms > now, "policy exception expired")?;
        require(
            !self.rationale.trim().is_empty() && self.rationale.len() <= 16_384,
            "exception requires a bounded rationale",
        )?;
        require(
            !self.replacement_evidence.is_empty(),
            "exception requires independent supporting evidence",
        )?;
        require(
            self.replacement_evidence
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                == self.replacement_evidence.len(),
            "duplicate exception evidence",
        )?;
        for digest in &self.replacement_evidence {
            ledger.read(digest)?;
        }
        if let Some(risk) = &self.residual_risk {
            require(
                !risk.trim().is_empty()
                    && risk.len() <= 16_384
                    && self.kind == DispositionKind::Exception
                    && self.qualifies_acceptance,
                "residual risk needs explicit qualified acceptance",
            )?;
        }
        Ok(())
    }

    /// Exact authenticated approval needed at execution and final acceptance.
    pub fn approval(&self) -> Result<ApprovalRequirement> {
        Ok(ApprovalRequirement {
            subject_digest: Digest::of_json(self)?,
            approval_type: "policy-exception".into(),
            // Coverage comes only from the configuration-scoped disposition.
            obligation_ids: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exceptions_require_exact_scope_expiry_configs_evidence_and_explicit_residual_risk() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let scope = Scope {
            allow_write: vec!["src/**".into()],
            protected: vec![],
        };
        let config = Digest::of(b"config");
        let evidence = ledger
            .store_json(
                "report",
                &serde_json::json!({"reason":"no allocations in this profile"}),
                1,
            )
            .unwrap();
        let exception = PolicyException {
            schema_version: 1,
            exception_id: "no-allocation".into(),
            obligation_id: "MEM-001".into(),
            kind: DispositionKind::NotApplicable,
            scope: scope.clone(),
            owner: "maintainers".into(),
            rationale: "Profile forbids allocation".into(),
            expires_ms: 20,
            configurations: [config.clone()].into(),
            replacement_evidence: vec![evidence.digest],
            residual_risk: None,
            qualifies_acceptance: false,
        };
        exception
            .validate(&scope, &[config.clone()].into(), &ledger, 10)
            .unwrap();
        for change in 0..7 {
            let mut invalid = exception.clone();
            match change {
                0 => invalid.scope.allow_write = vec!["**".into()],
                1 => invalid.expires_ms = 10,
                2 => invalid.configurations = [Digest::of(b"other")].into(),
                3 => invalid.replacement_evidence.clear(),
                4 => invalid.residual_risk = Some("uncertain".into()),
                5 => invalid.obligation_id = "SPEC-001".into(),
                _ => invalid.obligation_id = "TRUST-001".into(),
            }
            assert!(
                invalid
                    .validate(&scope, &[config.clone()].into(), &ledger, 10)
                    .is_err(),
                "case {change}"
            );
        }
        assert_eq!(
            exception.approval().unwrap().subject_digest,
            Digest::of_json(&exception).unwrap()
        );
        assert!(
            exception.approval().unwrap().obligation_ids.is_empty(),
            "approval must not waive other configurations"
        );
    }
}

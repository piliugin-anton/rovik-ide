//! Complete verification matrices and deterministic dependency validation.

use crate::{id, identity::Digest, require, unique, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Verification stages have separate assurance and caching rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Advisory iteration feedback.
    Fast,
    /// Conditional risk-specific checks.
    Risk,
    /// Clean exact-candidate verification.
    Final,
}

/// The registry, not the worker, determines whether case inventory is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// Compilation, formatting or deterministic static checks.
    Static,
    /// Behavioral test execution with required case accounting.
    Behavioral,
}

/// One registered check under one exact configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    /// Registered check ID.
    pub check_id: String,
    /// Flags, target and feature selection.
    pub configuration_digest: Digest,
    /// Trusted checker implementation.
    pub checker_digest: Digest,
    /// Registered argv-based command, never shell text.
    pub command_id: String,
    /// Typed parameters validated by the command registry.
    pub validated_parameters: BTreeMap<String, String>,
    /// Stage at which the check runs.
    pub phase: Phase,
    /// Requirement authority belongs to the plan.
    pub required: bool,
    /// Obligations this check satisfies.
    pub obligation_ids: Vec<String>,
    /// Cases that must be observed passing.
    pub expected_case_ids: Vec<String>,
    /// Minimum executed cases, excluding skips.
    pub minimum_test_count: u64,
    /// Approved successful exit codes.
    pub success_exit_codes: Vec<i32>,
    /// Bounded wall time.
    pub timeout_ms: u64,
    /// Maximum report/log bytes.
    pub output_limit_bytes: u64,
    /// Check IDs in this configuration that must finish first.
    pub dependencies: Vec<String>,
}
impl CheckSpec {
    /// Identity in the matrix, independent of result ordering.
    pub fn identity(&self) -> (&str, &Digest) {
        (&self.check_id, &self.configuration_digest)
    }
}

/// One obligation can require a semantic approval instead of an executable check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequirement {
    /// Exact artifact under review.
    pub subject_digest: Digest,
    /// Registered approval authority type.
    pub approval_type: String,
    /// Obligations discharged by that review.
    pub obligation_ids: Vec<String>,
}

/// An explicit reviewed disposition; never a rewritten passing observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disposition {
    /// Affected obligation.
    pub obligation_id: String,
    /// Authorized rationale and replacement evidence artifact.
    pub evidence_digest: Digest,
    /// Exact approval requirement for this disposition.
    pub approval: ApprovalRequirement,
}

/// Immutable effective plan; its digest is computed externally, never self-hashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckPlan {
    /// Exactly 1.
    pub schema_version: u32,
    /// Run receiving evidence.
    pub run_id: String,
    /// Approved task contract.
    pub contract_digest: Digest,
    /// Approved design.
    pub design_digest: Digest,
    /// Effective policy and registry.
    pub policy_digest: Digest,
    /// Protected acceptance assets.
    pub oracle_digest: Digest,
    /// Complete required matrix.
    pub configurations: Vec<Digest>,
    /// Registered checks in each configuration.
    pub checks: Vec<CheckSpec>,
    /// Required review identifiers.
    pub review_requirements: Vec<String>,
    /// Authenticated approvals required at final acceptance.
    pub approval_requirements: Vec<ApprovalRequirement>,
    /// Reviewed non-applicability, with explicit evidence.
    pub non_applicable_obligations: Vec<Disposition>,
    /// Approved exceptions with replacement evidence or residual risk.
    pub approved_exceptions: Vec<Disposition>,
}

/// Trusted registry facts used to validate a proposed matrix.
#[derive(Debug, Default)]
pub struct PlanRegistry {
    /// Registered command to its semantic result kind and checker identity.
    pub commands: BTreeMap<String, (CheckKind, Digest)>,
    /// Required obligations for each configuration.
    pub configurations: BTreeMap<Digest, BTreeSet<String>>,
    /// Disposition artifact identities independently approved by the controller.
    pub authorized_dispositions: BTreeSet<Digest>,
    /// Invariants which no exception or non-applicability may remove.
    pub non_waivable: BTreeSet<String>,
}

impl CheckPlan {
    /// Validate coverage, registry identity, inventory contracts and an acyclic DAG.
    pub fn validate(&self, registry: &PlanRegistry) -> Result<()> {
        require(self.schema_version == 1, "unsupported check-plan version")?;
        id(&self.run_id)?;
        let configurations: BTreeSet<_> = self.configurations.iter().cloned().collect();
        require(
            !configurations.is_empty() && configurations.len() == self.configurations.len(),
            "empty or duplicate configuration matrix",
        )?;
        require(
            configurations == registry.configurations.keys().cloned().collect(),
            "incomplete required configuration matrix",
        )?;
        let mut checks = BTreeMap::new();
        let mut coverage: BTreeMap<Digest, BTreeSet<String>> = configurations
            .iter()
            .map(|c| (c.clone(), BTreeSet::new()))
            .collect();
        for check in &self.checks {
            id(&check.check_id)?;
            require(
                configurations.contains(&check.configuration_digest),
                "check has unplanned configuration",
            )?;
            require(
                checks.insert(check.identity(), check).is_none(),
                "duplicate plan check",
            )?;
            let (kind, checker) = registry
                .commands
                .get(&check.command_id)
                .ok_or_else(|| Error(format!("unknown command: {}", check.command_id)))?;
            require(*checker == check.checker_digest, "wrong registered checker")?;
            require(
                check.timeout_ms > 0
                    && check.output_limit_bytes > 0
                    && !check.success_exit_codes.is_empty(),
                "unbounded or impossible check",
            )?;
            require(
                check
                    .success_exit_codes
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    == check.success_exit_codes.len(),
                "duplicate successful exit code",
            )?;
            unique(check.expected_case_ids.iter().map(String::as_str))?;
            unique(check.obligation_ids.iter().map(String::as_str))?;
            unique(check.dependencies.iter().map(String::as_str))?;
            if *kind == CheckKind::Behavioral {
                require(
                    check.minimum_test_count > 0 || !check.expected_case_ids.is_empty(),
                    "behavioral suite has no inventory requirement",
                )?;
            }
            let required = &registry.configurations[&check.configuration_digest];
            require(
                check.obligation_ids.iter().all(|o| required.contains(o)),
                "unknown check obligation",
            )?;
            if check.required && check.phase == Phase::Final {
                coverage
                    .get_mut(&check.configuration_digest)
                    .ok_or_else(|| Error("unplanned configuration".into()))?
                    .extend(check.obligation_ids.iter().cloned());
            }
        }
        require(
            self.checks
                .iter()
                .any(|c| c.required && c.phase == Phase::Final),
            "empty required final plan",
        )?;
        let mut dispositions = BTreeSet::new();
        for disposition in self
            .non_applicable_obligations
            .iter()
            .chain(&self.approved_exceptions)
        {
            require(
                !registry.non_waivable.contains(&disposition.obligation_id),
                "non-waivable invariant",
            )?;
            require(
                registry
                    .authorized_dispositions
                    .contains(&disposition.evidence_digest),
                "unauthorized obligation disposition",
            )?;
            require(
                dispositions.insert(&disposition.obligation_id),
                "competing obligation dispositions",
            )?;
            require(
                disposition.approval.subject_digest == disposition.evidence_digest,
                "disposition approval has wrong subject",
            )?;
            require(
                self.approval_requirements.contains(&disposition.approval),
                "disposition approval missing from final gate",
            )?;
            for set in coverage.values_mut() {
                set.insert(disposition.obligation_id.clone());
            }
        }
        for approval in &self.approval_requirements {
            id(&approval.approval_type)?;
            unique(approval.obligation_ids.iter().map(String::as_str))?;
            for obligation in &approval.obligation_ids {
                require(
                    registry
                        .configurations
                        .values()
                        .any(|r| r.contains(obligation)),
                    "unknown approval obligation",
                )?;
                for set in coverage.values_mut() {
                    set.insert(obligation.clone());
                }
            }
        }
        for (configuration, obligations) in &registry.configurations {
            require(
                obligations.is_subset(&coverage[configuration]),
                format!("uncovered obligations for {configuration}"),
            )?;
        }
        let mut completed = BTreeSet::new();
        while completed.len() < checks.len() {
            let before = completed.len();
            for (identity, check) in &checks {
                for dep in &check.dependencies {
                    let parent = checks
                        .get(&(dep.as_str(), &check.configuration_digest))
                        .ok_or_else(|| Error(format!("missing check dependency: {dep}")))?;
                    require(
                        !check.required || parent.required,
                        "required check depends on advisory work",
                    )?;
                    require(
                        check.phase != Phase::Final || parent.phase == Phase::Final,
                        "final check depends on non-final build outputs",
                    )?;
                }
                if check
                    .dependencies
                    .iter()
                    .all(|dep| completed.contains(&(dep.as_str(), &check.configuration_digest)))
                {
                    completed.insert(*identity);
                }
            }
            require(completed.len() > before, "cyclic check plan")?;
        }
        unique(self.review_requirements.iter().map(String::as_str))
    }

    /// Required final checks are selected by trusted plan authority alone.
    pub fn required_final(&self) -> impl Iterator<Item = &CheckSpec> {
        self.checks
            .iter()
            .filter(|c| c.required && c.phase == Phase::Final)
    }
}

//! Deterministic obligation planning from operator-registered capabilities.

use crate::{
    contract::{DesignRecord, TaskContract},
    identity::Digest,
    plan::{ApprovalRequirement, CheckKind, CheckPlan, CheckSpec, Phase, PlanRegistry},
    policy::EffectivePolicy,
    profile::Profile,
    registry::CommandRegistry,
    require, Error, FailureCategory, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A registered capability for one exact native profile/configuration.
/// Templates belong to trusted onboarding, never an implementer's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckTemplate {
    /// Stable check identity within the configuration.
    pub check_id: String,
    /// Registered native profile.
    pub profile_id: String,
    /// Exact configuration ID in that profile.
    pub configuration_id: String,
    /// Registered argv command.
    pub command_id: String,
    /// Values validated against that command's parameter schema.
    pub parameters: BTreeMap<String, String>,
    /// Obligations this capability can establish.
    pub obligations: BTreeSet<String>,
    /// Protected native case IDs, including any consumer and discovery cases.
    pub case_ids: BTreeSet<String>,
    /// Native cases required even when a suite has no stable names.
    pub minimum_test_count: u64,
    /// Dependencies on other templates in the same configuration.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Also run a separate advisory copy during iteration.
    #[serde(default)]
    pub fast: bool,
}

/// A review capability discharges only explicitly registered semantic obligations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRule {
    /// Required obligation.
    pub obligation_id: String,
    /// Authenticated review authority type.
    pub approval_type: String,
}

/// Versioned capability inventory, promoted independently of generated code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    /// Exactly 1.
    pub schema_version: u32,
    /// Native command capabilities; absence is unsupported, never an implicit pass.
    pub checks: Vec<CheckTemplate>,
    /// Semantic requirements which need authenticated design review.
    #[serde(default)]
    pub semantic_rules: Vec<SemanticRule>,
    /// Candidate review types required after implementation.
    #[serde(default)]
    pub candidate_reviews: Vec<String>,
}

/// Complete inputs for deterministic planning, usable by the core, CLI and packs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningInput {
    /// Durable task identity.
    pub run_id: String,
    /// Domain requirements, including protected case references.
    pub contract: TaskContract,
    /// Responsibility record and conditional obligations.
    pub design: DesignRecord,
    /// Restrictively resolved rules and command allowlist.
    pub policy: EffectivePolicy,
    /// Frozen oracle snapshot identity.
    pub oracle_digest: Digest,
    /// Complete approved configuration matrix.
    pub profiles: Vec<Profile>,
    /// Trusted executable definitions.
    pub registry: CommandRegistry,
    /// Registered mapping from obligations to available native evidence.
    pub catalog: Catalog,
}

impl PlanningInput {
    /// Derive a complete final matrix; never accept a prewritten proposed plan.
    pub fn derive(&self) -> Result<CheckPlan> {
        crate::id(&self.run_id)?;
        require(
            self.catalog.schema_version == 1,
            "unsupported planning catalog",
        )?;
        self.design.validate(&self.contract)?;
        self.registry.validate()?;
        crate::unique(self.profiles.iter().map(|profile| profile.id.as_str()))?;
        require(
            self.profiles
                .iter()
                .map(|p| p.id.clone())
                .collect::<BTreeSet<_>>()
                == self.contract.profile_ids.iter().cloned().collect(),
            "planning profile matrix differs from contract",
        )?;
        let obligations: BTreeSet<_> = self
            .policy
            .obligations
            .iter()
            .chain(&self.design.verification_obligations)
            .cloned()
            .chain(self.contract.invariants.iter().map(|i| i.id.clone()))
            .collect();
        let mut configurations = BTreeMap::new();
        for profile in &self.profiles {
            profile.require_resolved()?;
            for config in &profile.configurations {
                require(
                    configurations
                        .insert(config.digest()?, obligations.clone())
                        .is_none(),
                    "ambiguous configuration shared by profiles",
                )?;
            }
        }
        let mut checks = Vec::new();
        let mut identities = BTreeSet::new();
        for template in &self.catalog.checks {
            crate::id(&template.check_id)?;
            let profile = self
                .profiles
                .iter()
                .find(|p| p.id == template.profile_id)
                .ok_or_else(|| Error::new("catalog names an unknown profile".into()))?;
            let config = profile
                .configurations
                .iter()
                .find(|c| c.id == template.configuration_id)
                .ok_or_else(|| Error::new("catalog names an unknown configuration".into()))?;
            require(
                identities.insert((template.check_id.clone(), config.digest()?)),
                "duplicate catalog check",
            )?;
            let command = self.registry.get(&template.command_id)?;
            require(
                self.policy.commands.contains(&command.id),
                "catalog command is outside effective policy",
            )?;
            command.render(&template.parameters)?;
            let covered: Vec<_> = template
                .obligations
                .intersection(&obligations)
                .cloned()
                .collect();
            if covered.is_empty() && !profile.public_consumer_commands.contains(&command.id) {
                continue;
            }
            if command.kind == CheckKind::Behavioral {
                require(
                    !template.case_ids.is_empty() || template.minimum_test_count > 0,
                    "behavioral capability lacks protected inventory",
                )?;
            } else {
                require(
                    template.case_ids.is_empty() && template.minimum_test_count == 0,
                    "static capability cannot claim behavioral cases",
                )?;
            }
            let spec = CheckSpec {
                check_id: template.check_id.clone(),
                configuration_digest: config.digest()?,
                checker_digest: command.checker_digest.clone(),
                command_id: command.id.clone(),
                validated_parameters: template.parameters.clone(),
                phase: Phase::Final,
                required: true,
                obligation_ids: covered,
                expected_case_ids: template.case_ids.iter().cloned().collect(),
                minimum_test_count: template.minimum_test_count,
                success_exit_codes: vec![0],
                timeout_ms: command.limits.timeout_ms,
                output_limit_bytes: command.limits.output_bytes,
                dependencies: template.dependencies.clone(),
            };
            if template.fast {
                let mut fast = spec.clone();
                fast.check_id = format!("fast:{}", spec.check_id);
                fast.phase = Phase::Fast;
                fast.dependencies.clear();
                checks.push(fast);
            }
            checks.push(spec);
        }
        let design_digest = Digest::of_json(&self.design)?;
        crate::unique(
            self.catalog
                .semantic_rules
                .iter()
                .map(|r| r.obligation_id.as_str()),
        )?;
        let mut approvals = Vec::new();
        for rule in &self.catalog.semantic_rules {
            crate::id(&rule.approval_type)?;
            require(
                !rule.obligation_id.starts_with("ARCH-")
                    && !rule.obligation_id.starts_with("TRUST-"),
                "executable architecture and trust rules cannot become semantic-only reviews",
            )?;
            if obligations.contains(&rule.obligation_id) {
                approvals.push(ApprovalRequirement {
                    subject_digest: design_digest.clone(),
                    approval_type: rule.approval_type.clone(),
                    obligation_ids: vec![rule.obligation_id.clone()],
                });
            }
        }
        let plan = CheckPlan {
            schema_version: 1,
            run_id: self.run_id.clone(),
            contract_digest: Digest::of_json(&self.contract)?,
            design_digest,
            policy_digest: Digest::of_json(&self.policy)?,
            oracle_digest: self.oracle_digest.clone(),
            configurations: configurations.keys().cloned().collect(),
            checks,
            review_requirements: self.catalog.candidate_reviews.clone(),
            approval_requirements: approvals,
            non_applicable_obligations: vec![],
            approved_exceptions: vec![],
        };
        let registry = PlanRegistry {
            configurations,
            commands: self
                .registry
                .commands
                .iter()
                .map(|c| (c.id.clone(), (c.kind, c.checker_digest.clone())))
                .collect(),
            non_waivable: ["TRUST-001".into(), "SPEC-001".into()].into(),
            ..PlanRegistry::default()
        };
        plan.validate(&registry).map_err(|e| {
            Error::classified(
                FailureCategory::Capability,
                format!("registered capabilities cannot cover the required plan: {e}"),
            )
        })?;
        for profile in &self.profiles {
            for config in &profile.configurations {
                let digest = config.digest()?;
                for command in &profile.public_consumer_commands {
                    require(
                        plan.required_final()
                            .any(|c| c.configuration_digest == digest && &c.command_id == command),
                        "planner omitted a required public consumer",
                    )?;
                }
                for invariant in &self.contract.invariants {
                    let cases: BTreeSet<_> = plan
                        .required_final()
                        .filter(|c| {
                            c.configuration_digest == digest
                                && c.obligation_ids.contains(&invariant.id)
                        })
                        .flat_map(|c| c.expected_case_ids.iter())
                        .collect();
                    require(
                        invariant.case_ids.iter().all(|case| cases.contains(case)),
                        "planner cannot establish the invariant's protected case inventory",
                    )?;
                }
            }
        }
        Ok(plan)
    }
}

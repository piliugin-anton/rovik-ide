//! Domain meaning and responsibility records, independent of implementation language.

use crate::{id, identity::Digest, require, scope::Scope, unique, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A named obligation's scalar value; floating point is deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Constraint {
    /// A declared policy choice.
    Text(String),
    /// A bounded integer quantity.
    Integer(i64),
    /// A switch whose semantics belong to the rule registry.
    Boolean(bool),
}

/// Behavior owned by one component and covered by protected cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invariant {
    /// Stable requirement ID.
    pub id: String,
    /// Required behavior.
    pub statement: String,
    /// Authoritative owner.
    pub owner: String,
    /// Registered acceptance cases.
    pub case_ids: Vec<String>,
}

/// A question that must not be silently answered by a model assumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unknown {
    /// Stable question ID.
    pub id: String,
    /// Missing domain or runtime fact.
    pub question: String,
    /// Whether execution must wait for resolution.
    pub blocking: bool,
    /// Person or role responsible for resolution.
    pub owner: String,
}

/// Exact numeric semantics across language boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumericContract {
    /// Domain range and units.
    pub domain: String,
    /// Cross-language representation.
    pub wire_encoding: String,
    /// Required overflow behavior.
    pub overflow: String,
}

/// Version one of Rovik's task contract, including the full invariant and profile model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContract {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable contract ID.
    pub id: String,
    /// Domain context owning the behavior.
    pub bounded_context: String,
    /// All required language/runtime profiles.
    pub profile_ids: Vec<String>,
    /// Requested outcome.
    pub intent: String,
    /// Authorized file changes.
    pub scope: Scope,
    /// Behavioral invariants and their owners.
    pub invariants: Vec<Invariant>,
    /// Registered, frozen oracle suites.
    pub acceptance_suite_ids: Vec<String>,
    /// Recognized policy obligations only.
    pub constraints: BTreeMap<String, Constraint>,
    /// Risk signals; diff-derived signals are added later.
    pub risk_tags: Vec<String>,
    /// Unresolved questions remain visible even in a draft.
    pub unknowns: Vec<Unknown>,
    /// Present only when numeric semantics are part of the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_contract: Option<NumericContract>,
}

/// Trusted onboarding inventory used for semantic reference validation.
#[derive(Debug, Default)]
pub struct ContractRegistry {
    /// Registered profile IDs, including templates while drafting.
    pub profiles: BTreeSet<String>,
    /// Registered suite ID to exact case inventory.
    pub suites: BTreeMap<String, BTreeSet<String>>,
    /// Supported constraint names.
    pub constraints: BTreeSet<String>,
    /// Supported risk tags.
    pub risks: BTreeSet<String>,
}

impl TaskContract {
    /// Validate schema and references, without approving domain truth.
    pub fn validate(&self, registry: &ContractRegistry) -> Result<()> {
        require(
            self.schema_version == 1,
            "unsupported task-contract version",
        )?;
        id(&self.id)?;
        id(&self.bounded_context)?;
        require(!self.intent.trim().is_empty(), "empty contract intent")?;
        self.scope.validate()?;
        unique(self.profile_ids.iter().map(String::as_str))?;
        require(!self.profile_ids.is_empty(), "contract has no profiles")?;
        for profile in &self.profile_ids {
            require(
                registry.profiles.contains(profile),
                format!("unknown profile: {profile}"),
            )?;
        }
        unique(self.acceptance_suite_ids.iter().map(String::as_str))?;
        require(
            !self.acceptance_suite_ids.is_empty(),
            "contract has no acceptance suites",
        )?;
        let mut cases = BTreeSet::new();
        for suite in &self.acceptance_suite_ids {
            let inventory = registry
                .suites
                .get(suite)
                .ok_or_else(|| crate::Error(format!("unknown acceptance suite: {suite}")))?;
            for case in inventory {
                id(case)?;
                cases.insert(case.as_str());
            }
        }
        require(!self.invariants.is_empty(), "contract has no invariants")?;
        unique(self.invariants.iter().map(|i| i.id.as_str()))?;
        for invariant in &self.invariants {
            require(
                !invariant.owner.trim().is_empty() && !invariant.statement.trim().is_empty(),
                "invariant has no owner or statement",
            )?;
            unique(invariant.case_ids.iter().map(String::as_str))?;
            require(!invariant.case_ids.is_empty(), "invariant has no cases")?;
            for case in &invariant.case_ids {
                require(
                    cases.contains(case.as_str()),
                    format!("unknown case: {case}"),
                )?;
            }
        }
        for name in self.constraints.keys() {
            require(
                registry.constraints.contains(name),
                format!("unknown constraint: {name}"),
            )?;
        }
        unique(self.risk_tags.iter().map(String::as_str))?;
        for name in &self.risk_tags {
            require(
                registry.risks.contains(name),
                format!("unknown risk: {name}"),
            )?;
        }
        unique(self.unknowns.iter().map(|u| u.id.as_str()))?;
        for unknown in &self.unknowns {
            require(
                !unknown.question.trim().is_empty() && !unknown.owner.trim().is_empty(),
                "unowned or empty unknown",
            )?;
        }
        if let Some(n) = &self.numeric_contract {
            require(
                [&n.domain, &n.wire_encoding, &n.overflow]
                    .iter()
                    .all(|s| !s.trim().is_empty()),
                "incomplete numeric contract",
            )?;
        }
        Ok(())
    }
    /// Execution waits for blocking facts even when draft schema validation passed.
    pub fn require_resolved(&self) -> Result<()> {
        require(
            !self.unknowns.iter().any(|u| u.blocking),
            "blocking contract unknowns require a semantic decision",
        )
    }
}

/// A source location considered before introducing an abstraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingCode {
    /// Repository-relative path.
    pub file: String,
    /// Symbol inspected, or a module identifier.
    pub symbol: String,
    /// Exact source identity.
    pub source_revision: Digest,
    /// Why it can or cannot satisfy the required semantics.
    pub suitability: String,
}

/// Selected implementation design, bound to the exact approved contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRecord {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable design ID.
    pub id: String,
    /// Approved domain contract identity.
    pub contract_digest: Digest,
    /// Reuse analysis before abstraction.
    pub existing_code_considered: Vec<ExistingCode>,
    /// Component to responsibility and context description.
    pub components: BTreeMap<String, String>,
    /// One authoritative owner for every invariant.
    pub invariant_owners: BTreeMap<String, String>,
    /// Signatures, consumers and compatibility decisions.
    pub public_api_changes: Vec<String>,
    /// Ownership, borrowing, release, aliasing and failure rules.
    pub resource_contracts: Vec<String>,
    /// Expected errors and state/output behavior.
    pub error_contracts: Vec<String>,
    /// Serialization, atomicity and cancellation assumptions.
    pub concurrency_contract: String,
    /// Pressure, simpler alternatives and selected mechanism, including no abstraction.
    pub variation_decisions: Vec<String>,
    /// Registered required obligations.
    pub verification_obligations: Vec<String>,
    /// Questions that remain unresolved.
    pub unknowns: Vec<Unknown>,
}
impl DesignRecord {
    /// Ensure the design covers the current contract and has no blocking questions.
    pub fn validate(&self, contract: &TaskContract) -> Result<()> {
        require(self.schema_version == 1, "unsupported design version")?;
        id(&self.id)?;
        require(
            self.contract_digest == Digest::of_json(contract)?,
            "stale design contract",
        )?;
        let expected: BTreeSet<_> = contract.invariants.iter().map(|i| i.id.as_str()).collect();
        require(
            self.invariant_owners
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == expected,
            "incomplete invariant ownership",
        )?;
        for owner in self.invariant_owners.values() {
            require(
                self.components.contains_key(owner),
                "unknown invariant owner component",
            )?;
        }
        for code in &self.existing_code_considered {
            crate::scope::validate_path(&code.file)?;
            require(
                !code.suitability.trim().is_empty(),
                "missing reuse decision",
            )?;
        }
        require(
            !self.concurrency_contract.trim().is_empty(),
            "missing concurrency contract",
        )?;
        unique(self.verification_obligations.iter().map(String::as_str))?;
        require(
            !self.unknowns.iter().any(|u| u.blocking),
            "blocking design unknowns",
        )
    }
}

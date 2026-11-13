//! Structured mechanism choices and source-verified reuse evidence.

use crate::{contract::DesignRecord, ledger::Ledger, require, snapshot::Snapshot, Result};
use serde::{Deserialize, Serialize};

/// Small intent-driven pattern catalog; using an ordinary module is a first-class choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    /// A direct function or module, with no new variation abstraction.
    FunctionOrModule,
    /// Real interchangeable behavior behind a stable contract.
    Strategy,
    /// Construction varies by an explicit runtime/configuration decision.
    Factory,
    /// Translate an external model at an architectural boundary.
    Adapter,
    /// Give one owner deterministic acquisition and release.
    ResourceGuard,
    /// Enforce legal transitions of a nontrivial lifecycle.
    StateMachine,
    /// Explicit asynchronous delivery, ordering and failure semantics.
    Message,
}

/// A reviewable choice rather than a pattern name or an architecture score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MechanismDecision {
    /// Component which owns the responsibility.
    pub owner: String,
    /// Concrete change pressure or reason to keep the direct implementation.
    pub pressure: String,
    /// Selected card from the registered catalog.
    pub mechanism: Mechanism,
    /// Simpler alternatives and why they are or are not sufficient.
    pub alternatives: Vec<String>,
    /// Added indirection, ownership and maintenance cost.
    pub cost: String,
    /// Existing-code citation paths, or an explicit greenfield explanation.
    pub reuse: Vec<String>,
    /// Obligations/cases which exercise the promised property.
    pub evidence: Vec<String>,
}

impl MechanismDecision {
    /// Explicit fixture and onboarding choice for changes needing no new abstraction.
    pub fn direct(owner: &str, path: &str) -> Self {
        Self {
            owner: owner.into(),
            pressure: "Repair the current behavior without adding a variation point.".into(),
            mechanism: Mechanism::FunctionOrModule,
            alternatives: vec![
                "A new strategy interface adds indirection without a second behavior.".into(),
            ],
            cost: "Keep existing ownership and public surface.".into(),
            reuse: vec![path.into()],
            evidence: vec![],
        }
    }
}

/// Bind every reuse citation to immutable source and validate structured choices.
/// Symbol presence is a lexical citation check; semantic suitability remains a review obligation.
pub fn validate(ledger: &Ledger, base: &Snapshot, design: &DesignRecord) -> Result<()> {
    require(
        base.files.is_empty() || !design.existing_code_considered.is_empty(),
        "design must record inspected existing code",
    )?;
    for code in &design.existing_code_considered {
        require(
            code.source_revision == base.digest()?,
            "reuse citation has a stale source revision",
        )?;
        let file = base
            .files
            .get(&code.file)
            .ok_or_else(|| crate::Error::new("reuse citation names an absent file".into()))?;
        let bytes = ledger.read(&file.content.digest)?;
        let symbol = code
            .symbol
            .rsplit("::")
            .next()
            .unwrap_or(&code.symbol)
            .rsplit('.')
            .next()
            .unwrap_or(&code.symbol);
        require(
            !symbol.trim().is_empty() && String::from_utf8_lossy(&bytes).contains(symbol),
            "reuse citation symbol is absent from source",
        )?;
    }
    require(
        !design.mechanism_decisions.is_empty(),
        "design lacks a structured mechanism decision",
    )?;
    for decision in &design.mechanism_decisions {
        require(
            design.components.contains_key(&decision.owner),
            "mechanism has an unknown responsibility owner",
        )?;
        require(
            !decision.pressure.trim().is_empty()
                && !decision.cost.trim().is_empty()
                && !decision.alternatives.is_empty()
                && decision.alternatives.iter().all(|s| !s.trim().is_empty())
                && !decision.reuse.is_empty(),
            "mechanism needs concrete pressure, alternatives, cost and reuse analysis",
        )?;
        require(
            decision.mechanism == Mechanism::FunctionOrModule || !decision.evidence.is_empty(),
            "a new variation mechanism needs explicit evidence obligations",
        )?;
        for path in &decision.reuse {
            require(
                base.files.is_empty()
                    || design
                        .existing_code_considered
                        .iter()
                        .any(|c| c.file == *path),
                "mechanism reuse reference lacks an inspected source citation",
            )?;
        }
    }
    Ok(())
}

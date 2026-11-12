//! Versioned task-bank accounting. Results stay separated by language and strategy.
use crate::{identity::Digest, profile::Language, require, unique, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
/// Native verifier strategies; these do not claim to measure live model guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Compiler success alone.
    CompileOnly,
    /// Only the public happy-path example.
    ExampleOnly,
    /// Complete protected contract cases.
    ProtectedContract,
}
/// Known label for one independent task implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Immutable task/language/variant identity.
    pub case_id: String,
    /// Compiler family exercised.
    pub language: Language,
    /// Protected ground truth, never inferred from the observed verdict.
    pub expected_correct: bool,
    /// Minimum inventory for protected contract success.
    pub required_cases: u64,
}
/// Closed, versioned benchmark inventory, separate from the quota demonstration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bank {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable dataset identity.
    pub bank_id: String,
    /// Every required task variant.
    pub cases: Vec<Case>,
    /// At least two independent native repetitions.
    pub repetitions: u32,
}
/// Native execution decision, preserving infrastructure uncertainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Required selected observations passed.
    Accepted,
    /// An executed case failed.
    Rejected,
    /// Compiler/runner/protocol failure; not a successful rejection.
    Error,
    /// Environment lacks a required capability.
    Unsupported,
}
/// One measured native trial.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// A member of the protected bank.
    pub case_id: String,
    /// Zero-based independent trial index.
    pub repetition: u32,
    /// Actual verifier selection.
    pub strategy: Strategy,
    /// Native observation, never a model confidence score.
    pub outcome: Outcome,
    /// Observed executed cases; compilation alone has none.
    pub executed_cases: u64,
    /// Compilation plus selected execution wall time.
    pub duration_ms: u64,
}
/// Complete evaluation record tied to the prepared environment and bank.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    /// Exactly 1.
    pub schema_version: u32,
    /// Digest of the closed bank definition.
    pub bank_digest: Digest,
    /// Must match the native runner environment, not a floating tool label.
    pub environment_digest: Digest,
    /// Explicitly identifies this as a native-verifier ablation.
    pub methodology: String,
    /// Every strategy/case/repetition observation.
    pub observations: Vec<Observation>,
}
/// Counts for one language and strategy. No blanket policy-promotion boolean.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    /// Complete observed trials.
    pub trials: u64,
    /// Incorrect implementations accepted.
    pub false_accepts: u64,
    /// Correct implementations rejected by executed cases.
    pub false_rejects: u64,
    /// Correct implementations accepted.
    pub correct_accepts: u64,
    /// Incorrect implementations rejected by executed cases.
    pub correct_rejects: u64,
    /// Infrastructure failures, reported separately.
    pub errors: u64,
    /// Unsupported trials, reported separately.
    pub unsupported: u64,
    /// Sum of measured native wall time.
    pub duration_ms: u64,
}
/// Stratified summary suitable for audit or comparison; live-model quality is unmeasured.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stratum {
    /// Native language.
    pub language: Language,
    /// Verifier strategy.
    pub strategy: Strategy,
    /// Measured counts.
    pub metrics: Metrics,
}
impl Evaluation {
    /// Reject missing, competing, stale and zero-case successful trials before aggregation.
    pub fn summarize(&self, bank: &Bank, environment: &Digest) -> Result<Vec<Stratum>> {
        require(
            self.schema_version == 1 && bank.schema_version == 1,
            "unsupported evaluation schema",
        )?;
        require(
            self.bank_digest == Digest::of_json(bank)? && &self.environment_digest == environment,
            "stale evaluation identity",
        )?;
        require(
            self.methodology == "native-verifier-ablation; no model calls",
            "unknown evaluation methodology",
        )?;
        require(
            bank.repetitions >= 2 && bank.repetitions <= 1000 && !bank.cases.is_empty(),
            "evaluation requires a bounded repeated task bank",
        )?;
        unique(bank.cases.iter().map(|c| c.case_id.as_str()))?;
        let cases: BTreeMap<_, _> = bank.cases.iter().map(|c| (c.case_id.as_str(), c)).collect();
        let mut seen = BTreeSet::new();
        let mut strata: BTreeMap<(Language, Strategy), Metrics> = BTreeMap::new();
        for observation in &self.observations {
            let case = cases
                .get(observation.case_id.as_str())
                .ok_or_else(|| crate::Error("unregistered evaluation case".into()))?;
            require(
                observation.repetition < bank.repetitions
                    && seen.insert((
                        &observation.case_id,
                        observation.repetition,
                        observation.strategy,
                    )),
                "duplicate or out-of-range trial",
            )?;
            require(
                observation.outcome != Outcome::Accepted
                    || observation.strategy == Strategy::CompileOnly
                    || observation.executed_cases > 0,
                "successful evaluation trial executed no cases",
            )?;
            require(
                observation.outcome != Outcome::Accepted
                    || observation.strategy != Strategy::ProtectedContract
                    || observation.executed_cases >= case.required_cases,
                "protected evaluation omitted required cases",
            )?;
            let metrics = strata
                .entry((case.language, observation.strategy))
                .or_default();
            metrics.trials += 1;
            metrics.duration_ms = metrics
                .duration_ms
                .checked_add(observation.duration_ms)
                .ok_or_else(|| crate::Error("evaluation duration overflow".into()))?;
            match (case.expected_correct, observation.outcome) {
                (true, Outcome::Accepted) => metrics.correct_accepts += 1,
                (false, Outcome::Accepted) => metrics.false_accepts += 1,
                (true, Outcome::Rejected) => metrics.false_rejects += 1,
                (false, Outcome::Rejected) => metrics.correct_rejects += 1,
                (_, Outcome::Error) => metrics.errors += 1,
                (_, Outcome::Unsupported) => metrics.unsupported += 1,
            }
        }
        require(
            seen.len() == bank.cases.len() * bank.repetitions as usize * 3,
            "evaluation matrix has missing trials",
        )?;
        Ok(strata
            .into_iter()
            .map(|((language, strategy), metrics)| Stratum {
                language,
                strategy,
                metrics,
            })
            .collect())
    }
}
/// Initial regression bank: arithmetic, parsing and output-state contracts in every language.
pub fn reference_bank() -> Bank {
    let mut cases = vec![];
    for (language, name) in [
        (Language::Rust, "rust"),
        (Language::Cpp, "cpp"),
        (Language::C, "c"),
    ] {
        for task in ["checked-add", "decimal", "copy"] {
            for (variant, expected_correct) in [("correct", true), ("mutant", false)] {
                cases.push(Case {
                    case_id: format!("{name}-{task}-{variant}"),
                    language,
                    expected_correct,
                    required_cases: if task == "decimal" { 5 } else { 4 },
                });
            }
        }
    }
    Bank {
        schema_version: 1,
        bank_id: "native-contracts-heldout-v1".into(),
        cases,
        repetitions: 2,
    }
}

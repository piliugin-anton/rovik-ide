//! Restrictive policy composition: narrower guidance cannot remove obligations.

use crate::{require, scope::Scope, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One trusted layer in the policy hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyLayer {
    /// IDs of registered tools this layer allows.
    pub allow_commands: BTreeSet<String>,
    /// Explicitly forbidden tools.
    pub deny_commands: BTreeSet<String>,
    /// Required evidence obligations.
    pub obligations: BTreeSet<String>,
    /// Maximum budgets; lower values are stricter.
    pub limits: BTreeMap<String, u64>,
    /// Semantic facts must agree across layers.
    pub facts: BTreeMap<String, String>,
    /// Each layer constrains writes independently.
    pub scope: Scope,
    /// Source paths that models and worker read tools cannot inspect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<String>,
}

/// Deterministically resolved policy; its exact bytes are evidence inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectivePolicy {
    /// Intersection of allow sets minus all denies.
    pub commands: BTreeSet<String>,
    /// Union of mandatory obligations.
    pub obligations: BTreeSet<String>,
    /// Most restrictive upper resource bounds.
    pub limits: BTreeMap<String, u64>,
    /// Compatible semantic facts.
    pub facts: BTreeMap<String, String>,
    /// A path must pass every scope; this preserves exact intersections.
    pub scopes: Vec<Scope>,
    /// Trusted read exclusions, independent of the complete verification snapshot.
    /// Omitting an empty list preserves the identity of earlier policy documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<String>,
}
impl EffectivePolicy {
    /// Resolve trusted layers against registered command and obligation IDs.
    pub fn resolve(
        layers: &[PolicyLayer],
        commands: &BTreeSet<String>,
        obligations: &BTreeSet<String>,
    ) -> Result<Self> {
        require(!layers.is_empty(), "missing mandatory policy")?;
        let mut out = Self {
            commands: commands.clone(),
            obligations: BTreeSet::new(),
            limits: BTreeMap::new(),
            facts: BTreeMap::new(),
            scopes: Vec::new(),
            deny_read: Vec::new(),
        };
        for layer in layers {
            layer.scope.validate()?;
            validate_read_patterns(&layer.deny_read)?;
            require(
                layer
                    .allow_commands
                    .union(&layer.deny_commands)
                    .all(|id| commands.contains(id)),
                "unknown policy command",
            )?;
            require(
                layer.obligations.is_subset(obligations),
                "unknown policy obligation",
            )?;
            out.commands.retain(|id| {
                layer.allow_commands.contains(id) && !layer.deny_commands.contains(id)
            });
            out.obligations.extend(layer.obligations.iter().cloned());
            for (unit, limit) in &layer.limits {
                out.limits
                    .entry(unit.clone())
                    .and_modify(|v| *v = (*v).min(*limit))
                    .or_insert(*limit);
            }
            for (key, value) in &layer.facts {
                if let Some(old) = out.facts.insert(key.clone(), value.clone()) {
                    require(old == *value, format!("conflicting policy fact: {key}"))?;
                }
            }
            out.scopes.push(layer.scope.clone());
            out.deny_read.extend(layer.deny_read.iter().cloned());
        }
        out.deny_read.sort();
        out.deny_read.dedup();
        Ok(out)
    }
    /// Validate portable read exclusions before constructing any worker view.
    pub fn validate_reads(&self) -> Result<()> {
        validate_read_patterns(&self.deny_read)
    }
    /// Reject reads forbidden by any trusted policy layer.
    pub fn check_read(&self, path: &str) -> Result<()> {
        crate::scope::validate_path(path)?;
        self.validate_reads()?;
        require(
            readable(&self.deny_read, path),
            "source read is restricted by project policy",
        )
    }
    /// Check a proposed path against every applicable layer.
    pub fn check_write(&self, path: &str) -> Result<()> {
        require(!self.scopes.is_empty(), "unresolved policy scope")?;
        for scope in &self.scopes {
            scope.check(path)?;
        }
        Ok(())
    }
}

/// Validate exclusions before using their non-fallible matcher over an admitted snapshot.
pub(crate) fn validate_read_patterns(patterns: &[String]) -> Result<()> {
    crate::unique(patterns.iter().map(String::as_str))?;
    for pattern in patterns {
        crate::scope::validate_pattern(pattern)?;
    }
    Ok(())
}

/// Patterns and source paths have already been validated by their owner.
pub(crate) fn readable(patterns: &[String], path: &str) -> bool {
    !patterns
        .iter()
        .any(|pattern| crate::scope::matches(pattern, path))
}

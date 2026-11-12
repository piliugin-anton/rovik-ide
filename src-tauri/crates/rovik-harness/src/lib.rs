#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Deterministic control-plane contracts for the coding agent.
//!
//! Repository text, model responses and native reports are observations, never
//! authority. Adapters must validate provenance before admitting evidence.

pub mod approvals;
pub mod artifacts;
pub mod broker;
pub mod context;
pub mod contract;
pub mod controller;
pub mod evidence;
pub mod fixtures;
pub mod identity;
pub mod ledger;
pub mod normalizer;
pub mod pack_protocol;
pub mod packs;
pub mod plan;
pub mod policy;
pub mod profile;
pub mod provider;
pub mod registry;
pub mod risk;
pub mod runner;
pub mod scope;
pub mod snapshot;
pub mod state;
pub mod wire;

/// A validation or operational error, with a stable reason and useful context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

/// Result returned by the harness's domain and application services.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error(message.into()))
    }
}

pub(crate) fn id(value: &str) -> Result<()> {
    require(
        !value.trim().is_empty() && value.len() <= 160 && !value.chars().any(char::is_control),
        format!("invalid identifier: {value:?}"),
    )
}

pub(crate) fn unique<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        id(value)?;
        require(seen.insert(value), format!("duplicate identifier: {value}"))?;
    }
    Ok(())
}

#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Deterministic control-plane contracts for the coding agent.
//!
//! Repository text, model responses and native reports are observations, never
//! authority. Adapters must validate provenance before admitting evidence.

pub mod approvals;
pub mod architecture;
pub mod artifacts;
pub mod baseline;
pub mod broker;
pub mod context;
pub mod contract;
pub mod controller;
pub mod design;
pub mod evaluation;
pub mod evidence;
pub mod exceptions;
pub mod failure;
pub mod fixtures;
pub mod identity;
pub mod ledger;
pub mod native_checks;
mod node_report;
pub mod normalizer;
pub mod onboarding;
pub mod operations;
pub mod pack_protocol;
pub mod packs;
pub mod plan;
pub mod planning;
pub mod policy;
mod process;
pub mod profile;
pub mod provider;
pub mod registry;
pub mod replanning;
pub mod retention;
pub mod review_bundle;
pub mod risk;
pub mod roles;
pub mod runner;
pub mod runtime;
mod runtime_fixtures;
pub mod scope;
pub mod snapshot;
pub mod state;
pub mod tools;
pub mod wire;

pub use failure::{Error, FailureCategory};

/// Result returned by the harness's domain and application services.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message.into()))
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

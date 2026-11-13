//! Attributed, bounded context that never truncates mandatory control clauses.

use crate::{identity::Digest, ledger::Ledger, require, Error, Result};
use serde::{Deserialize, Serialize};

/// Provenance is assigned by the controller, not inferred from document wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Immutable controller instructions and capabilities.
    AuthoritativePolicy,
    /// Authenticated domain/design decisions.
    ApprovedSpec,
    /// Exact source, with no instruction authority.
    ObservedCode,
    /// Diagnostics or retrieved prose, with no capability authority.
    UntrustedText,
}

/// An attributed context item references its exact complete source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextItem {
    /// Stable item label.
    pub item_id: String,
    /// Source artifact identity.
    pub artifact_digest: Digest,
    /// Controller-established trust level.
    pub trust: Trust,
    /// Revision used for source ranges and diagnostics.
    pub source_revision: Digest,
    /// Whether dropping this item would remove a mandatory constraint.
    pub mandatory: bool,
    /// Exact selected UTF-8 content, never an authoritative model summary.
    pub text: String,
}

/// Immutable provider context with explicit omissions and byte accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBundle {
    /// Exactly 1.
    pub schema_version: u32,
    /// Ordered mandatory inputs followed by selected observations.
    pub items: Vec<ContextItem>,
    /// Optional items omitted due to the context budget.
    pub omitted: Vec<String>,
}

/// Resolve every item from the trusted artifact store, retaining exact mandatory text.
pub fn build(
    ledger: &Ledger,
    mut items: Vec<ContextItem>,
    maximum_bytes: usize,
) -> Result<ContextBundle> {
    crate::unique(items.iter().map(|i| i.item_id.as_str()))?;
    for item in &items {
        let bytes = ledger.read(&item.artifact_digest)?;
        require(
            bytes == item.text.as_bytes(),
            format!("stale context item: {}", item.item_id),
        )?;
        require(
            !matches!(item.trust, Trust::AuthoritativePolicy | Trust::ApprovedSpec)
                || item.mandatory,
            "authoritative context cannot be optional",
        )?;
    }
    items.sort_by_key(|i| !i.mandatory);
    let mut bundle = ContextBundle {
        schema_version: 1,
        items: Vec::new(),
        omitted: Vec::new(),
    };
    for item in items {
        let mandatory = item.mandatory;
        let label = item.item_id.clone();
        bundle.items.push(item);
        if crate::wire::canonical(&bundle)?.len() > maximum_bytes {
            bundle.items.pop();
            if mandatory {
                return Err(Error::new(
                    "mandatory context exceeds budget; split the task or approve a compact view"
                        .into(),
                ));
            }
            bundle.omitted.push(label);
        }
    }
    require(
        crate::wire::canonical(&bundle)?.len() <= maximum_bytes,
        "context omission metadata exceeds budget",
    )?;
    Ok(bundle)
}

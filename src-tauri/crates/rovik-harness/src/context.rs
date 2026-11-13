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

/// Rank exact source observations by explicit citations, writable ownership and task terms.
/// Low-ranked files remain retrievable through the scoped broker; no first-N truncation
/// can silently remove a mandatory contract clause.
pub fn ranked_sources(
    ledger: &Ledger,
    snapshot: &crate::snapshot::Snapshot,
    query: &str,
    preferred: &[String],
    scope: Option<&crate::scope::Scope>,
) -> Result<Vec<ContextItem>> {
    let revision = snapshot.digest()?;
    let terms: std::collections::BTreeSet<_> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| word.len() >= 4)
        .take(128)
        .map(str::to_lowercase)
        .collect();
    let mut ranked = Vec::new();
    for (path, file) in &snapshot.files {
        let Ok(text) = String::from_utf8(ledger.read(&file.content.digest)?) else {
            continue;
        };
        let name = path.to_lowercase();
        let searchable: String = text
            .chars()
            .take(16_384)
            .flat_map(char::to_lowercase)
            .collect();
        let score = usize::from(preferred.contains(path)) * 10_000
            + usize::from(
                scope.is_some_and(|s| s.allow_write.iter().any(|p| crate::scope::matches(p, path))),
            ) * 1_000
            + terms
                .iter()
                .filter(|term| name.contains(term.as_str()))
                .count()
                * 20
            + terms
                .iter()
                .filter(|term| searchable.contains(term.as_str()))
                .count();
        ranked.push((
            score,
            ContextItem {
                item_id: format!("source:{path}"),
                artifact_digest: file.content.digest.clone(),
                trust: Trust::ObservedCode,
                source_revision: revision.clone(),
                mandatory: false,
                text,
            },
        ));
    }
    ranked.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.item_id.cmp(&y.item_id)));
    Ok(ranked.into_iter().map(|(_, item)| item).collect())
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

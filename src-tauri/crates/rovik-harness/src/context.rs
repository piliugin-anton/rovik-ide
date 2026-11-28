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

/// Omitted item IDs are inline when they fit, or recorded in full in the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ContextOmissions {
    /// The original v1 representation, also used when nothing was omitted.
    Inline(Vec<String>),
    /// A compact reference to the ordered JSON array of all omitted IDs.
    Manifest {
        /// Number of IDs in the manifest.
        count: usize,
        /// Content address of the manifest in the controller's artifact store.
        manifest_digest: Digest,
    },
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
    pub omitted: ContextOmissions,
}

impl Default for ContextBundle {
    fn default() -> Self {
        Self {
            schema_version: 1,
            items: Vec::new(),
            omitted: ContextOmissions::Inline(Vec::new()),
        }
    }
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
    deny_read: &[String],
) -> Result<Vec<ContextItem>> {
    crate::policy::validate_read_patterns(deny_read)?;
    let revision = snapshot.digest()?;
    let terms: std::collections::BTreeSet<_> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| word.len() >= 4)
        .take(128)
        .map(str::to_lowercase)
        .collect();
    let mut ranked = Vec::new();
    for (path, file) in &snapshot.files {
        if !crate::policy::readable(deny_read, path) {
            continue;
        }
        // Assets stay in the verified snapshot, but cannot fit a model message.
        if file.content.byte_count > crate::wire::MAX_JSON_BYTES as u64
            || ledger.is_binary(&file.content.digest)?
        {
            continue;
        }
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

/// Native output may quote restricted source or values derived from it. Keep only
/// trusted check identities and outcomes when the project excludes source reads;
/// the full immutable evidence still determines verification and remains in the ledger.
pub(crate) fn check_feedback(
    policy: &crate::policy::EffectivePolicy,
    result: &crate::evidence::CheckResult,
) -> Result<serde_json::Value> {
    if policy.deny_read.is_empty() {
        return serde_json::to_value(result).map_err(|error| Error::new(error.to_string()));
    }
    Ok(serde_json::json!({
        "check_id": result.check_id,
        "configuration_digest": result.configuration_digest,
        "status": result.status,
        "failure_category": result.failure_category,
        "exit_code": result.exit_code,
        "executed_test_count": result.executed_test_count,
        "details": "Native output is withheld because it may contain source excluded by project read permissions. Full evidence remains available to verification."
    }))
}

// Canonical JSON sorts object keys, which preserves these types' encoded lengths.
// Measure candidates before applying the wire limit so oversized optional text can be omitted.
fn json_bytes(value: &impl Serialize) -> Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|e| Error::new(format!("cannot size context: {e}")))
}

/// Resolve every item from the trusted artifact store, retaining exact mandatory text.
pub fn build(
    ledger: &Ledger,
    items: Vec<ContextItem>,
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
    let (mandatory, optional): (Vec<_>, Vec<_>) = items.into_iter().partition(|i| i.mandatory);
    let mut bundle = ContextBundle {
        items: mandatory,
        ..ContextBundle::default()
    };
    let budget = maximum_bytes.min(crate::wire::MAX_JSON_BYTES);
    let mandatory_bytes = json_bytes(&bundle)?;
    let mut remaining = budget.checked_sub(mandatory_bytes).ok_or_else(|| {
        Error::new(format!(
            "Required verification inputs need {mandatory_bytes} bytes. The limit is {budget} bytes. \
             Split the plan into smaller tasks and prepare it again."
        ))
    })?;
    let Some(first) = optional.first() else {
        return Ok(bundle);
    };

    // Reserve the cheaper of the remaining names and a ledger reference. All
    // digests have the same width and the count can only shrink as items fit.
    let reference_bytes = json_bytes(&ContextOmissions::Manifest {
        count: optional.len(),
        manifest_digest: first.artifact_digest.clone(),
    })? - b"[]".len();
    let label_sizes = optional
        .iter()
        .map(|item| json_bytes(&item.item_id).map(|n| n + 1))
        .collect::<Result<Vec<_>>>()?;
    // Each size includes a comma. An inline list needs one fewer comma than IDs.
    let mut label_bytes: usize = label_sizes.iter().sum();
    let mut omitted = Vec::new();
    for (item, label_size) in optional.into_iter().zip(label_sizes) {
        let other_labels = label_bytes - label_size;
        let reserve = other_labels.saturating_sub(1).min(reference_bytes);
        let separator = usize::from(!bundle.items.is_empty());
        let available = remaining.saturating_sub(reserve + separator);
        if item.text.len() <= available {
            let size = json_bytes(&item)?;
            if size <= available {
                remaining -= size + separator;
                label_bytes = other_labels;
                bundle.items.push(item);
                continue;
            }
        }
        omitted.push(item.item_id);
    }
    let inline_bytes = label_bytes.saturating_sub(1);
    bundle.omitted = if inline_bytes <= remaining {
        ContextOmissions::Inline(omitted)
    } else {
        if reference_bytes > remaining {
            let minimum = inline_bytes.min(reference_bytes);
            return Err(Error::new(format!(
                "Required verification inputs leave {remaining} bytes for the list of skipped files. \
                 Its smallest record needs {minimum} bytes. Split the plan into smaller tasks and prepare it again."
            )));
        }
        let record = ledger.store_json("context", &omitted, crate::runner::now_ms()?)?;
        ContextOmissions::Manifest {
            count: omitted.len(),
            manifest_digest: record.digest,
        }
    };
    Ok(bundle)
}

#[cfg(test)]
#[path = "context_read_tests.rs"]
mod read_tests;

#[cfg(test)]
#[path = "context_budget_tests.rs"]
mod budget_tests;

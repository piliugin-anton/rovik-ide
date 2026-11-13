//! Scoped worker tools operate on artifacts, never on arbitrary host paths.

use crate::{
    identity::Digest,
    ledger::Ledger,
    policy::EffectivePolicy,
    require,
    scope::{self, Scope},
    snapshot::{self, AdmissionRules, FileEntry, Snapshot},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One proposed text/file change; deletion is explicit through null content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEdit {
    /// Relative path in the active candidate.
    pub path: String,
    /// Exact replacement UTF-8 bytes, or deletion.
    pub content: Option<String>,
    /// Exact regular-file mode; mode changes require additional authorization.
    pub mode: u32,
}

/// Worker-exposed read tools. Final verification and approvals are not tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolRequest {
    /// Inspect lexical declarations without claiming a complete semantic graph.
    InspectSymbol {
        /// Source path in the admitted snapshot.
        path: String,
        /// Literal symbol name to locate.
        symbol: String,
    },
    /// Ask for a registered advisory check, never a final check.
    RunFastCheck {
        /// Check ID in the approved fast phase.
        check_id: String,
    },
    /// Read bounded lines from an admitted file.
    ReadFile {
        /// Relative source path.
        path: String,
        /// First line, one-based.
        start_line: usize,
        /// Bounded line count.
        maximum_lines: usize,
    },
    /// Lexical retrieval in an approved scope, with no semantic completeness claim.
    SearchCode {
        /// Literal search text.
        query: String,
        /// Relative literal/directory scope pattern.
        scope: String,
        /// Maximum returned matches.
        maximum_results: usize,
    },
    /// Read a trusted diagnostic artifact by an admitted handle.
    ReadDiagnostics {
        /// Previously exposed diagnostic identity.
        digest: Digest,
        /// Maximum excerpt bytes.
        maximum_bytes: usize,
    },
    /// Ask the operator for a wider scope, without granting it.
    RequestScopeChange {
        /// Proposed scope.
        scope: Scope,
        /// Why current authority is insufficient.
        reason: String,
    },
    /// Ask for a semantic decision; never modifies the active contract.
    RequestContractChange {
        /// Missing or conflicting semantics.
        reason: String,
    },
}

/// Worker output is a proposal, not an acceptance decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Exact snapshot the patch was constructed against.
    pub base_digest: Digest,
    /// Scoped file changes.
    pub changes: Vec<FileEdit>,
    /// Concise observable summary.
    pub summary: String,
    /// Invariant IDs mapped to changed paths or explicit unchanged behavior.
    pub requirement_mapping: BTreeMap<String, Vec<String>>,
}

/// Capability-bound view of one immutable current candidate.
pub struct Broker<'a> {
    ledger: &'a Ledger,
    snapshot: &'a Snapshot,
    diagnostics: BTreeSet<Digest>,
}
impl<'a> Broker<'a> {
    /// Construct from controller-owned handles; no host paths are accepted.
    pub fn new(ledger: &'a Ledger, snapshot: &'a Snapshot, diagnostics: BTreeSet<Digest>) -> Self {
        Self {
            ledger,
            snapshot,
            diagnostics,
        }
    }
    /// Execute one admitted read or return an explicit non-authorizing change request.
    pub fn call(&self, tool: &ToolRequest) -> Result<serde_json::Value> {
        match tool {
            ToolRequest::InspectSymbol { path, symbol } => {
                scope::validate_path(path)?;
                require(
                    !symbol.is_empty() && symbol.len() <= 256,
                    "invalid symbol query",
                )?;
                self.call(&ToolRequest::SearchCode {
                    query: symbol.clone(),
                    scope: path.clone(),
                    maximum_results: 20,
                })
            }
            ToolRequest::RunFastCheck { .. } => Err(Error::new(
                "advisory execution requires the controller's budget and registered plan".into(),
            )),
            ToolRequest::ReadFile {
                path,
                start_line,
                maximum_lines,
            } => {
                scope::validate_path(path)?;
                require(
                    *start_line > 0 && *maximum_lines > 0 && *maximum_lines <= 400,
                    "invalid source range",
                )?;
                let file = self
                    .snapshot
                    .files
                    .get(path)
                    .ok_or_else(|| Error::new("file is not in the admitted snapshot".into()))?;
                require(
                    !self.ledger.is_binary(&file.content.digest)?,
                    "file is a binary asset, not UTF-8 source text",
                )?;
                let text = String::from_utf8(self.ledger.read(&file.content.digest)?)
                    .map_err(|_| Error::new("file is not UTF-8 text".into()))?;
                let lines: Vec<_> = text
                    .lines()
                    .skip(start_line - 1)
                    .take(*maximum_lines)
                    .collect();
                let excerpt = lines.join("\n");
                require(
                    excerpt.len() <= 64 * 1024,
                    "requested source excerpt exceeds tool-output limit",
                )?;
                Ok(
                    serde_json::json!({"path":path,"snapshot_digest":self.snapshot.digest()?,"start_line":start_line,"text":excerpt,"trust":"observed_code"}),
                )
            }
            ToolRequest::SearchCode {
                query,
                scope: pattern,
                maximum_results,
            } => {
                require(
                    !query.is_empty()
                        && query.len() <= 1024
                        && *maximum_results > 0
                        && *maximum_results <= 100,
                    "invalid search bounds",
                )?;
                scope::validate_pattern(pattern)?;
                let mut hits = Vec::new();
                'files: for (path, file) in &self.snapshot.files {
                    if !scope::matches(pattern, path) {
                        continue;
                    }
                    if self.ledger.is_binary(&file.content.digest)? {
                        continue;
                    }
                    let bytes = self.ledger.read(&file.content.digest)?;
                    let Ok(text) = std::str::from_utf8(&bytes) else {
                        continue;
                    };
                    for (line, text) in text.lines().enumerate() {
                        if text.contains(query) {
                            hits.push(serde_json::json!({"path":path,"line":line+1,"text":text.chars().take(500).collect::<String>()}));
                        }
                        if hits.len() >= *maximum_results {
                            break 'files;
                        }
                    }
                }
                Ok(
                    serde_json::json!({"hits":hits,"precision":"lexical","snapshot_digest":self.snapshot.digest()?}),
                )
            }
            ToolRequest::ReadDiagnostics {
                digest,
                maximum_bytes,
            } => {
                require(
                    self.diagnostics.contains(digest)
                        && *maximum_bytes > 0
                        && *maximum_bytes <= 64 * 1024,
                    "diagnostic handle or bounds not authorized",
                )?;
                let bytes = self.ledger.read(digest)?;
                Ok(
                    serde_json::json!({"digest":digest,"text":String::from_utf8_lossy(&bytes[..bytes.len().min(*maximum_bytes)]),"trust":"untrusted_text"}),
                )
            }
            ToolRequest::RequestScopeChange { scope, reason } => {
                scope.validate()?;
                require(!reason.trim().is_empty(), "scope request needs a reason")?;
                Ok(
                    serde_json::json!({"status":"blocked","reason":"scope change requires authenticated approval","proposal":scope}),
                )
            }
            ToolRequest::RequestContractChange { reason } => {
                require(!reason.trim().is_empty(), "contract request needs a reason")?;
                Ok(
                    serde_json::json!({"status":"blocked","reason":"semantic decision required","question":reason}),
                )
            }
        }
    }
    /// Apply to a new manifest only after validating every change against authority.
    pub fn propose(
        &self,
        base: &Snapshot,
        policy: &EffectivePolicy,
        rules: &AdmissionRules,
        proposal: &Proposal,
        required_invariants: &BTreeSet<String>,
        now: u64,
    ) -> Result<Snapshot> {
        require(
            proposal.base_digest == self.snapshot.digest()?,
            "proposal uses a stale source snapshot",
        )?;
        require(
            proposal
                .requirement_mapping
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
                == *required_invariants,
            "proposal omits or invents a requirement",
        )?;
        require(
            !proposal.summary.trim().is_empty(),
            "proposal has no summary",
        )?;
        let mut candidate = self.snapshot.clone();
        let mut seen = BTreeSet::new();
        for edit in &proposal.changes {
            require(seen.insert(&edit.path), "competing edits for one path")?;
            policy.check_write(&edit.path)?;
            require(
                [0o100644, 0o100755].contains(&edit.mode),
                "worker cannot propose symlinks or special files",
            )?;
            if let Some(content) = &edit.content {
                let content = self.ledger.store(
                    "source",
                    "application/octet-stream",
                    content.as_bytes(),
                    now,
                )?;
                candidate.files.insert(
                    edit.path.clone(),
                    FileEntry {
                        mode: edit.mode,
                        content,
                    },
                );
            } else {
                require(
                    candidate.files.remove(&edit.path).is_some(),
                    "cannot delete an absent file",
                )?;
            }
        }
        candidate.deletions = base
            .files
            .keys()
            .filter(|p| !candidate.files.contains_key(*p))
            .cloned()
            .collect();
        snapshot::admit(self.ledger, base, &candidate, policy, rules)?;
        Ok(candidate)
    }
}

//! A chat's context mode and CLM state, for the UI (the core's clm).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClmStatus {
    /// The chat's mode; None until one is chosen for it.
    pub mode: Option<String>,
    /// Virtual files are mounted, so CLM mode can run.
    pub available: bool,
    /// Why not, for the user.
    pub problem: Option<String>,
    pub path: String,
    pub real_path: Option<String>,
    /// The file's latest revision written for a request.
    pub revision: Option<u64>,
    pub revisions: usize,
    pub edits: usize,
    /// The agent's own context is in use (it has edited it).
    pub projected: bool,
    pub tokens: usize,
    pub budget: usize,
    /// Phase E: a budget reminder the agent hasn't acted on yet, e.g. "a
    /// compaction is due (75% reminder not acted on yet)". None otherwise.
    pub compliance: Option<String>,
}

/// A revision of a chat's context file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RevisionRow {
    pub number: u64,
    /// "context": the context as it was sent; "edit": changed by the agent
    /// (or a program); "restore": an earlier revision brought back.
    pub kind: String,
    pub created_at: i64,
    pub tokens: usize,
}

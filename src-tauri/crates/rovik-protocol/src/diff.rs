//! A change to a file as hunks (the core's agent_run/diff.rs computes them).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// 1-based line in the original where the hunk starts: the first deleted
    /// line, or for a pure insertion, the line the new lines go before.
    pub orig_start: usize,
    pub deleted: Vec<String>,
    pub added: Vec<String>,
}

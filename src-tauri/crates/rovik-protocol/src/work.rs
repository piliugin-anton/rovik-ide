//! The Work group (4.2, 6.4–6.7): a project carried to the server, and the
//! worktree each of its chats works in there.

use serde::{Deserialize, Serialize};

/// A project as a daemon keeps it (6.4): its id is the desktop's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Where the project sits in its repository: "" at its root (K31). A
    /// chat's root on the server is that folder of its worktree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subpath: String,
}

/// A chat's newest result on its daemon (6.5): its worktree committed on
/// the landing base, after a run that ended with `outcome`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatResult {
    pub dialog_id: String,
    /// The result's commit, at the chat's result ref (`packs::result_ref`).
    pub commit: String,
    pub outcome: String,
    /// Why it doesn't land on its own, if it doesn't: the reason a local
    /// chat's landing would give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
}

/// What a sync did (6.7).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Synced {
    /// The chat's root on the server: its worktree, or the project's folder
    /// in it.
    pub root: String,
}

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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatResult {
    pub dialog_id: String,
    /// The result's commit, at the chat's result ref (`packs::result_ref`).
    pub commit: String,
    /// Its number among the chat's results on its daemon, from 1: a newer
    /// result has a higher one, and the same result keeps its own, so a
    /// client passes over one older than the newest it has (6.5). 0: not
    /// numbered.
    #[serde(default)]
    pub seq: u64,
    pub outcome: String,
    /// Why it doesn't land on its own, if it doesn't: the reason a local
    /// chat's landing would give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
    /// Files that still have the conflict markers a sync put in them (6.7),
    /// from the root of the result's tree: it doesn't land until they're
    /// gone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marked: Vec<String>,
    /// No checks would run on it merged with the project's newer changes,
    /// by the chat's settings on its daemon: such a merge lands as it is,
    /// as a local chat's does, without going back to be checked (6.6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unchecked: bool,
    /// A workspace chat's (workspaces-plan.md, 4.11): what its runs made at
    /// the top of its worktree, outside every one of its folders, by name.
    /// It isn't in the result, as no folder holds it: it stays on the
    /// server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outside: Vec<String>,
    /// The branch a server that pushes its results (autopilot-plan D15,
    /// H04) pushed it to, on the project's remote, and the pull request it
    /// opened for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
}

/// What a sync did (6.7).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Synced {
    /// The chat's root on the server: its worktree, or the project's folder
    /// in it.
    pub root: String,
    #[serde(default)]
    pub state: SyncState,
    /// The files `state` is about, from the root of the project's
    /// repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// Where a chat's worktree stands after a sync brought the project's
/// changes (6.7). Waiting, the chat goes on with the older copy of the
/// files the project changed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Applied. The files are those merged with the chat's own changes.
    #[default]
    Synced,
    /// Applied, with conflict markers in the files for the chat to settle.
    Marked,
    /// Waits: the files changed on the same lines here and in the chat,
    /// which doesn't resolve conflicts itself.
    Conflicts,
    /// Waits: among the files, some are binary, or deleted on one side and
    /// changed on the other. They stay with the user, whatever the setting.
    Unmergeable,
}

/// A chat's sync, for the isolation bar (6.7).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatSync {
    pub dialog_id: String,
    pub state: SyncState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// A file a chat changed in its worktree, from its landing base (6.1),
/// from the project's folder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkChange {
    pub path: String,
    pub change: Change,
    /// Moved: from where in the project's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The project's newer changes, or conflict markers, were merged into
    /// it there: Undo the merge takes them back (6.1).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    Added,
    Changed,
    Deleted,
    /// Moved here from `from`, which is listed as deleted, as a local
    /// chat's changes list a move.
    Moved,
}

/// A file of a chat's worktree (6.1): as its landing base has it and as the
/// chat has it now. None where it isn't there, or isn't text.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
}

/// The files handed to a chat, with conflict markers in them (6.1).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Handed {
    pub files: Vec<String>,
}

/// What `WorkUndoMerge` did, from the project's folder.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UndoneMerge {
    /// The chat's own version again.
    pub restored: Vec<String>,
    /// Left as they are: changed since the merge, by the chat, or with
    /// their earlier versions gone.
    pub changed_since: Vec<String>,
}

//! A chat's rows as its owner stores them, for the mirror (6.3): the
//! changes since a revision, a page at a time (`ChatChangesSince`), and
//! the bundle that moves a chat (6.8). Text columns that hold JSON travel
//! as the text they are, so the mirror stores the owner's bytes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A chat's row, less what each database keeps for itself: the chat's
/// project and its location.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredDialog {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: i64,
    pub preview: Option<String>,
    pub archived: i64,
    pub parent_id: Option<String>,
    pub parent_call_id: Option<String>,
    pub kind: Option<String>,
    pub context_mode: Option<String>,
    pub model_provider: Option<String>,
    pub model_id: Option<String>,
    pub effort: Option<String>,
}

/// A message as stored, with its whole body, its attachments in order, and
/// its tool calls' provider data.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub dialog_id: String,
    pub seq: i64,
    pub role: String,
    pub body_external: bool,
    pub body_bytes: i64,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub meta: Option<String>,
    pub error: Option<String>,
    pub failed: bool,
    pub finalized: bool,
    pub created_at: i64,
    /// As stored: an external body's preview.
    pub content: String,
    /// The external body, whole (`message_bodies`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<StoredExtra>,
}

/// Provider data for one of a message's tool calls (`tool_call_extras`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredExtra {
    pub call_id: String,
    pub extra_content: String,
    pub created_at: i64,
}

/// A chat's CLM state as stored (`clm_state`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredClmState {
    pub dialog_id: String,
    pub anchor_id: Option<String>,
    pub rendered_anchor_id: Option<String>,
    pub rendered_revision: Option<i64>,
    pub tokens: i64,
    pub budget: i64,
    pub nudged: Option<String>,
    pub updated_at: i64,
    pub projection: Option<String>,
    pub rendered_messages: Option<String>,
    pub rendered: Option<String>,
    pub task: Option<String>,
}

/// One revision of a chat's CLM context file (`clm_revisions`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredRevision {
    pub dialog_id: String,
    pub number: i64,
    pub kind: String,
    pub created_at: i64,
    pub tokens: i64,
    pub content: String,
}

/// Rows of a chat and its subagents' chats, as the owner has them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialogs: Vec<StoredDialog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clm_states: Vec<StoredClmState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clm_revisions: Vec<StoredRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<StoredMessage>,
    /// On a pull's last page: each chat's CLM revisions the owner still
    /// has, by number. It deletes old ones, and so does the mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clm_kept: Option<BTreeMap<String, Vec<i64>>>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.dialogs.is_empty()
            && self.clm_states.is_empty()
            && self.clm_revisions.is_empty()
            && self.messages.is_empty()
    }
}

/// What a pull reads, in this order: every changed chat row, then each
/// chat's CLM state, CLM revisions and messages.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum ChangedTable {
    Dialogs,
    ClmState,
    ClmRevisions,
    Messages,
}

/// Where a pull stopped, for the owner to go on from: the client hands it
/// back as it came.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangesCursor {
    /// The owner's revision the pull goes up to: its clock when the pull
    /// began. Rows changed since then come with the next pull.
    pub upto: i64,
    /// The chat and the table being read, and the last row sent from it,
    /// by its revision and its key (a message's id, a revision's number).
    pub chat: String,
    pub table: ChangedTable,
    pub rev: i64,
    pub key: String,
}

/// A page of a chat's changes since a revision (`ChatChangesSince`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangesPage {
    pub changes: ChangeSet,
    /// Where the next page starts; none on the pull's last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ChangesCursor>,
    /// The owner's revision the mirror is at once it has applied every
    /// page of the pull: the one to ask from next time.
    pub rev: i64,
}

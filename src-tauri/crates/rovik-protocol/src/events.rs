//! What a run tells the frontend. The core sends them through its host's
//! `EventSink`: the app emits them to its windows as `agent-event`, and the
//! daemon sends them to the chat's computer in `Event::Agent`. Their JSON
//! is what src/lib/agentRuns.ts reads, and test-fixtures/agent-events.json
//! holds one of each for both sides' tests.

use crate::chat::ToolCall;
use crate::clm::ClmStatus;
use crate::plan::{Question, TodoItem};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A message as the frontend reads it from the database (chatApi's
/// MessageRow), plus whether it's a reply still being written.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OutMessage {
    pub id: String,
    pub dialog_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub body_external: bool,
    pub body_bytes: i64,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub meta: Option<Value>,
    pub error: Option<String>,
    pub failed: bool,
    pub finalized: bool,
    pub attachments: Vec<String>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// Live status of a reply being written (never stored).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub text: String,
    /// Show a countdown to this time (epoch ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    /// Show the time elapsed since this (epoch ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

impl Status {
    pub fn text(text: impl Into<String>) -> Self {
        Status {
            text: text.into(),
            until: None,
            since: None,
        }
    }
}

/// Changes to a message on screen. Absent fields stay as they are;
/// `status: null` clears the status.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalized: Option<bool>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<Option<Status>>,
}

/// A field that is there, `null` included: `Some`. With `default`, an absent
/// one stays `None`. A plain `Option<Option<T>>` would read `null` as absent,
/// and a patch that clears the status wouldn't clear it once sent (K39).
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl Patch {
    /// This patch, then `newer`: what the two do together, as one patch.
    pub fn then(self, newer: Patch) -> Patch {
        Patch {
            content: newer.content.or(self.content),
            tool_calls: newer.tool_calls.or(self.tool_calls),
            meta: newer.meta.or(self.meta),
            error: newer.error.or(self.error),
            failed: newer.failed.or(self.failed),
            pending: newer.pending.or(self.pending),
            finalized: newer.finalized.or(self.finalized),
            status: newer.status.or(self.status),
        }
    }
}

/// A file edit for the user to review hunk by hunk in the editor.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditProposal {
    pub abs_path: String,
    /// As the model wrote it, for display.
    pub rel_path: String,
    /// The file as it is.
    pub current: String,
    /// The file with the edit applied.
    pub proposed: String,
}

/// A tool call waiting for the user's decision.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub tool_call_id: String,
    pub dialog_id: String,
    pub tool_name: String,
    /// One line, e.g. "Run `npm test`".
    pub summary: String,
    /// bash: the full command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Why this is asked manually, e.g. the AI reviewer couldn't decide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<EditProposal>,
    /// A subagent's request: the chat it shows in, and the `task` call there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_dialog_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    /// A remote chat's edit (K09): the file it changes is the server's, so
    /// the editor shows the change apart, not on the local file's tab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote: bool,
}

/// An ask_user call waiting for the user's answers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionsRequest {
    pub tool_call_id: String,
    pub dialog_id: String,
    pub questions: Vec<Question>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    RunStarted {
        dialog_id: String,
        run_id: String,
    },
    RunEnded {
        dialog_id: String,
        run_id: String,
        outcome: String,
        /// It carried out a stage of an Autopilot task: its result lands
        /// only once checked (D7, V02). Absent: false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        autopilot: bool,
    },
    /// Stored, in this order, at the end of the chat.
    MessagesAdded {
        dialog_id: String,
        messages: Vec<OutMessage>,
    },
    MessagePatched {
        dialog_id: String,
        message_id: String,
        patch: Patch,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    /// Answered, or dropped (Stop).
    ApprovalSettled {
        dialog_id: String,
        tool_call_id: String,
    },
    QuestionsAsked {
        request: QuestionsRequest,
    },
    QuestionsSettled {
        dialog_id: String,
        tool_call_id: String,
    },
    /// A live note on a tool call, e.g. "Reviewing with …"; None clears it.
    ToolNote {
        dialog_id: String,
        tool_call_id: String,
        note: Option<String>,
    },
    /// The agent wrote a file: open tabs refresh; an edit the user didn't
    /// review (`reviewed` false) is marked in the editor until saved.
    FileWritten {
        /// The chat whose run wrote it (K02).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dialog_id: Option<String>,
        abs_path: String,
        before: Option<String>,
        content: String,
        reviewed: bool,
        /// The file was removed (a rollback of a file the run created,
        /// P3.4): an open tab for it goes, unless it has unsaved edits.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
    },
    /// What a reply showed about the model's reasoning support, to be kept
    /// with the model in the settings (src/lib/reasoning.ts).
    ReasoningLearned {
        profile_id: String,
        accepted: Vec<String>,
        rejected: Vec<String>,
        unresolved: Vec<String>,
        reasons: bool,
    },
    /// The API route a model was found to work on, to be saved with the
    /// model in the settings (`ChatStreamEvent::ApiRoute`).
    ApiRouteLearned {
        profile_id: String,
        route: String,
    },
    /// The first reply to a sent message settled: answered (`ok`), or not,
    /// in which case the message is marked failed and its draft comes back.
    UserMessageSettled {
        dialog_id: String,
        message_id: String,
        ok: bool,
    },
    /// The chat's CLM state after a request's context was prepared (each
    /// turn of a run), so the context row doesn't wait for the run to end.
    ContextUpdated {
        dialog_id: String,
        status: ClmStatus,
    },
    /// The chat's current checklist, after a `todo` result and at run start
    /// when a checklist is carried. The panel reads this while a run is
    /// going; when no run is going it asks `chat_checklist` instead. This
    /// keeps the panel correct even when the chat was opened around a
    /// search hit (newest messages not loaded) or the checklist is older
    /// than the newest page.
    Checklist {
        dialog_id: String,
        items: Vec<TodoItem>,
    },
    Notice {
        /// The chat it's about, if it's about one (K02).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dialog_id: Option<String>,
        kind: String,
        title: String,
        message: String,
    },
    /// A subagent started for the lead's `task` call `tool_call_id` in
    /// `dialog_id` (the parent chat); its own chat is `child_dialog_id`.
    SubagentStarted {
        dialog_id: String,
        tool_call_id: String,
        child_dialog_id: String,
        kind: String,
        model: String,
        goal: String,
    },
    /// How far it got: tool calls so far, and the one it's on.
    SubagentProgress {
        dialog_id: String,
        tool_call_id: String,
        tool_calls: u32,
        current_tool: Option<String>,
        model: String,
    },
    SubagentEnded {
        dialog_id: String,
        tool_call_id: String,
        child_dialog_id: String,
        status: String,
    },
}

impl AgentEvent {
    /// The chat it's about; none for what applies to the settings (a
    /// model's reasoning or route) and a notice about no chat.
    pub fn dialog_id(&self) -> Option<&str> {
        match self {
            AgentEvent::ApprovalRequested { request } => Some(&request.dialog_id),
            AgentEvent::QuestionsAsked { request } => Some(&request.dialog_id),
            AgentEvent::FileWritten { dialog_id, .. } | AgentEvent::Notice { dialog_id, .. } => {
                dialog_id.as_deref()
            }
            AgentEvent::ReasoningLearned { .. } | AgentEvent::ApiRouteLearned { .. } => None,
            AgentEvent::RunStarted { dialog_id, .. }
            | AgentEvent::RunEnded { dialog_id, .. }
            | AgentEvent::MessagesAdded { dialog_id, .. }
            | AgentEvent::MessagePatched { dialog_id, .. }
            | AgentEvent::ApprovalSettled { dialog_id, .. }
            | AgentEvent::QuestionsSettled { dialog_id, .. }
            | AgentEvent::ToolNote { dialog_id, .. }
            | AgentEvent::UserMessageSettled { dialog_id, .. }
            | AgentEvent::ContextUpdated { dialog_id, .. }
            | AgentEvent::Checklist { dialog_id, .. }
            | AgentEvent::SubagentStarted { dialog_id, .. }
            | AgentEvent::SubagentProgress { dialog_id, .. }
            | AgentEvent::SubagentEnded { dialog_id, .. } => Some(dialog_id),
        }
    }
}

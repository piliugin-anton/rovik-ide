//! What a run tells the frontend, as `agent-event` app events. They go to
//! every window and don't depend on any component being mounted: the
//! frontend listens once, at the module level (src/lib/agentRuns.ts), and
//! the database stays the source of truth for anything it misses.

use super::plan::Question;
use crate::chat_db::store::{NewMessage, ToolCall};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Runtime};

pub const EVENT: &str = "agent-event";

/// A message as the frontend reads it from the database (chatApi's
/// MessageRow), plus whether it's a reply still being written.
#[derive(Serialize, Clone, Debug)]
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

impl OutMessage {
    pub fn from_new(dialog_id: &str, seq: i64, m: &NewMessage, pending: bool) -> Self {
        OutMessage {
            id: m.id.clone(),
            dialog_id: dialog_id.to_string(),
            seq,
            role: m.role.clone(),
            content: m.content.clone(),
            body_external: false,
            body_bytes: m.content.len() as i64,
            tool_calls: m.tool_calls.clone(),
            tool_call_id: m.tool_call_id.clone(),
            meta: m.meta.clone(),
            error: m.error.clone(),
            failed: m.failed,
            finalized: m.finalized,
            attachments: m.attachments.clone(),
            created_at: m.created_at,
            pending,
        }
    }
}

/// Live status of a reply being written (never stored).
#[derive(Serialize, Clone, Debug, PartialEq)]
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
        Status { text: text.into(), until: None, since: None }
    }
}

/// Changes to a message on screen. Absent fields stay as they are;
/// `status: null` clears the status.
#[derive(Serialize, Clone, Debug, Default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Option<Status>>,
}

/// A file edit for the user to review hunk by hunk in the editor.
#[derive(Serialize, Clone, Debug)]
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
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub tool_call_id: String,
    pub dialog_id: String,
    pub tool_name: String,
    /// One line, e.g. "Run `npm test`".
    pub summary: String,
    /// run_command: the full command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Why this is asked manually, e.g. the AI reviewer couldn't decide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<EditProposal>,
}

/// An ask_user call waiting for the user's answers.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuestionsRequest {
    pub tool_call_id: String,
    pub dialog_id: String,
    pub questions: Vec<Question>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    RunStarted {
        dialog_id: String,
        run_id: String,
    },
    RunEnded {
        dialog_id: String,
        run_id: String,
        outcome: String,
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
        tool_call_id: String,
    },
    QuestionsAsked {
        request: QuestionsRequest,
    },
    QuestionsSettled {
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
        abs_path: String,
        before: Option<String>,
        content: String,
        reviewed: bool,
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
        status: crate::clm::ClmStatus,
    },
    Notice {
        kind: String,
        title: String,
        message: String,
    },
}

pub fn emit<R: Runtime>(app: &AppHandle<R>, event: AgentEvent) {
    if let Err(e) = app.emit(EVENT, &event) {
        eprintln!("agent: could not send an event: {e}");
    }
}

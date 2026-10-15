//! What a run tells the frontend, through its host's `EventSink`
//! (core.rs): the app sends them to every window as `agent-event` app
//! events. They don't depend on any component being mounted: the frontend
//! listens once, at the module level (src/lib/agentRuns.ts), and the
//! database stays the source of truth for anything it misses.
//!
//! The events are protocol types (rovik-protocol), since the daemon sends
//! them to the app as they are.

use crate::chat_db::store::NewMessage;

pub use rovik_protocol::events::{
    AgentEvent, ApprovalRequest, EditProposal, OutMessage, Patch, QuestionsRequest, Status,
};

pub const EVENT: &str = "agent-event";

/// A message as `MessagesAdded` carries it, from the row it was stored as.
pub fn out_message(dialog_id: &str, seq: i64, m: &NewMessage, pending: bool) -> OutMessage {
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

/// The row `MessagesAdded` announced, as a mirror stores it (6.3).
pub fn stored_row(m: &OutMessage) -> NewMessage {
    NewMessage {
        id: m.id.clone(),
        role: m.role.clone(),
        content: m.content.clone(),
        tool_calls: m.tool_calls.clone(),
        tool_call_id: m.tool_call_id.clone(),
        meta: m.meta.clone(),
        error: m.error.clone(),
        failed: m.failed,
        attachments: m.attachments.clone(),
        created_at: m.created_at,
        finalized: m.finalized,
    }
}

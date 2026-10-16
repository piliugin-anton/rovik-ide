//! A run the process ended (5.4): what the chat's stored rows say to do
//! about it. The daemon asks at start-up, for each chat whose run was
//! going when it stopped.

use super::run::{recent_messages, settle_failed_turn, unanswered_tool_calls};
use super::{failure, StartKind};
use crate::core::Core;

/// A pause an upgrade made (`meta.paused.kind`, R02): the new binary
/// continues the run.
pub const DRAIN: &str = "drain";

/// What a chat whose run the process ended needs next.
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    /// A run of this kind picks up where it was cut.
    Start(StartKind),
    /// Nothing: the run had paused, and waits for the user.
    Paused,
    /// It was cut where no start picks it up: in its end-of-run checks,
    /// between two steps or before its first.
    Interrupted,
}

/// What the chat's last stored rows say. A reply cut mid-stream is
/// settled first, as a failed turn is: kept as an interrupted reply when
/// it has text, which `ContinueReply` picks up, else marked failed, and
/// `Retry` takes the turn again. Tool calls that never got their results
/// go through `Resume`, which asks the user before running one again.
pub async fn after_crash(core: &Core, dialog_id: &str) -> Result<Recovery, String> {
    let recent = recent_messages(&core.db, dialog_id).await?;
    let Some(last) = recent.last() else {
        return Ok(Recovery::Interrupted);
    };
    if last.role == "assistant" && !last.finalized {
        let error = failure::restarted();
        let kept =
            settle_failed_turn(core, dialog_id, &last.id, &last.content, None, &error).await?;
        return Ok(Recovery::Start(if kept {
            StartKind::ContinueReply
        } else {
            StartKind::Retry
        }));
    }
    if !unanswered_tool_calls(&recent).is_empty() {
        return Ok(Recovery::Start(StartKind::Resume));
    }
    let paused = last.meta.as_ref().and_then(|m| m.pointer("/paused/kind"));
    Ok(match paused.and_then(|kind| kind.as_str()) {
        Some(DRAIN) => Recovery::Start(StartKind::ContinueRun),
        Some(_) => Recovery::Paused,
        None => Recovery::Interrupted,
    })
}

#[cfg(test)]
mod tests;

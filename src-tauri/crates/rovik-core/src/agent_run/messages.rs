//! What a stored message is: the user's request, a message they sent
//! while the run worked, one the IDE wrote, one that closed the checklist.

use crate::chat_db::store::MessageRow;
use serde_json::Value;

/// Whether a stored message is one the IDE wrote (check failures, nudges).
pub fn is_harness(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| meta.get("harness"))
        .is_some_and(|h| !h.is_null())
}

/// Whether a stored message is a request of the user's: theirs, not one
/// the IDE wrote in their name, nor one they sent while the run worked
/// (`is_steer`), which is part of the request it was sent in.
pub fn is_request(m: &MessageRow) -> bool {
    m.role == "user" && !is_harness(m) && !is_steer(m)
}

/// A message the user sent while the run worked (`meta.steer`,
/// steering-plan.md): their words, part of the current request.
pub fn is_steer(m: &MessageRow) -> bool {
    m.role == "user"
        && m.meta
            .as_ref()
            .and_then(|meta| meta.get("steer"))
            .is_some_and(|s| !s.is_null())
}

/// Whether the user dismissed a checklist here (`meta.checklistClosed`):
/// on the message holding its `todo` call (the panel's dismiss ✕), or, as
/// older chats have it, on the user message that followed it.
pub fn is_checklist_dismissed(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| {
            meta.get("checklistClosed")
                .or_else(|| meta.get("checklist_closed"))
        })
        .is_some_and(|v| v == &Value::Bool(true))
}

//! The agent's checklist (`todo`, Agent mode), shown read-only above the
//! input. Each call writes the whole list, so the latest call in a run is
//! the current state; the tool calls are the record. And the gate that
//! asks for one when a task has outgrown a few steps.

use super::builtin_tools::{BuiltinTool, Effect};
use super::messages::{is_checklist_dismissed, is_request};
use super::tools::ToolResult;
use crate::chat_db::store::MessageRow;
use serde_json::{Map, Value};

/*
 * ---------------------------------------------------------------------------
 * The checklist (todo)
 * ---------------------------------------------------------------------------
 */

const TODO_STATUSES: &[&str] = &["pending", "in_progress", "done", "skipped", "changed"];
pub const MAX_TODO_ITEMS: usize = 30;

pub use rovik_protocol::plan::{CurrentChecklist, TodoItem};

pub fn parse_todo(args: &Map<String, Value>) -> Vec<TodoItem> {
    args.get("items")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
        .take(MAX_TODO_ITEMS)
        .filter_map(|r| match r {
            Value::String(s) => {
                let title = s.trim();
                (!title.is_empty()).then(|| TodoItem {
                    title: title.into(),
                    status: "pending".into(),
                })
            }
            Value::Object(o) => super::permissions::trimmed_str(o.get("title"))
                .map(str::to_string)
                .map(|title| {
                    let status = o
                        .get("status")
                        .and_then(|s| s.as_str())
                        .filter(|s| TODO_STATUSES.contains(s))
                        .unwrap_or("pending");
                    TodoItem {
                        title,
                        status: status.into(),
                    }
                }),
            _ => None,
        })
        .collect()
}

/// Finished: done, or skipped/changed with the reason given.
pub fn is_settled(item: &TodoItem) -> bool {
    matches!(item.status.as_str(), "done" | "skipped" | "changed")
}

/// A checklist the model can't be acting on: more than one step claimed as
/// current. None when the list is usable (zero in progress is fine: between
/// steps, or all settled). An error string keeps the model from drifting into
/// parallel work without adding new concepts to the prompt.
pub fn todo_problems(items: &[TodoItem]) -> Option<String> {
    let active: Vec<&TodoItem> = items.iter().filter(|i| i.status == "in_progress").collect();
    if active.len() > 1 {
        let names = active
            .iter()
            .map(|i| format!("\"{}\"", i.title))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(format!(
            "Error: only one step can be in_progress at a time (you marked {names}). \
             Keep the step you're on in_progress and the rest pending, then call todo again."
        ));
    }
    None
}

/// What the model reads after an update: the list taken, or why not.
pub fn describe_todo(items: &[TodoItem]) -> ToolResult {
    if items.is_empty() {
        return ToolResult::error(
            "Error: the checklist is empty. Pass items: [{ title, status }].",
        );
    }
    if let Some(problem) = todo_problems(items) {
        return ToolResult::error(problem);
    }
    let done = items.iter().filter(|i| is_settled(i)).count();
    let current = items.iter().find(|i| i.status == "in_progress");
    ToolResult::ok(format!(
        "Checklist updated: {done} of {} done{}.",
        items.len(),
        current
            .map(|c| format!("; now: {}", c.title))
            .unwrap_or_default()
    ))
}

/// The latest `todo` call in `messages[at]` that the IDE took, if it
/// parses to a non-empty list. One it refused (two steps in progress) set
/// nothing: its result failed, and the list before it stays current.
fn todo_in_message(messages: &[MessageRow], at: usize) -> Option<Vec<TodoItem>> {
    let m = &messages[at];
    // A step worker's result: the checklist with its step marked done (D04).
    if let Some(items) = m
        .meta
        .as_ref()
        .and_then(|meta| meta.get("checklistUpdate"))
        .and_then(|v| serde_json::from_value::<Vec<TodoItem>>(v.clone()).ok())
        .filter(|items| !items.is_empty())
    {
        return Some(items);
    }
    let call = m
        .tool_calls
        .iter()
        .flatten()
        .rev()
        .find(|c| c.name == "todo" && !call_failed_in_reply(messages, at, &c.id))?;
    if let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) {
        let items = parse_todo(&args);
        if !items.is_empty() {
            return Some(items);
        }
    }
    None
}

/// Whether the call `id` that `messages[at]` made failed: its result is
/// one of the messages after it, before the next reply (call ids repeat
/// across replies: Gemini numbers them per reply).
fn call_failed_in_reply(messages: &[MessageRow], at: usize, id: &str) -> bool {
    messages[at + 1..]
        .iter()
        .take_while(|m| m.role != "assistant")
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id))
        .is_some_and(MessageRow::call_failed)
}

/// The checklist the chat is currently working from.
///
/// - `carried == false`: the latest `todo` call in the current request
///   (since the last real user message).
/// - `carried == true`: the current request made no `todo` call, so the
///   latest open checklist from the previous request is carried forward. It
///   counts as closed (and is not carried) when every step is settled or the
///   user dismissed it.
///
/// A dismissed checklist (`meta.checklistClosed` on the message holding the
/// `todo` call) is closed wherever it is: nothing is current until a newer
/// `todo` call.
///
/// Looking back past exactly one user message (to the latest `todo` in the
/// previous request only) keeps a checklist alive across a short follow-up
/// like "continue" that never makes a new `todo` call. A second user
/// message stops the search, so an old checklist doesn't linger across
/// unrelated work. A newer `todo` call replaces it, and the carry note
/// tells the model to close the old one when the new message is a different
/// task.
pub fn current_checklist(messages: &[MessageRow]) -> Option<CurrentChecklist> {
    let last_user = messages.iter().rposition(is_request);
    let Some(last) = last_user else {
        // No user message yet: whatever the latest todo call says.
        for (at, m) in messages.iter().enumerate().rev() {
            if let Some(items) = todo_in_message(messages, at) {
                if is_checklist_dismissed(m) {
                    return None;
                }
                return Some(CurrentChecklist {
                    items,
                    carried: false,
                    from_message_id: Some(m.id.clone()),
                });
            }
        }
        return None;
    };
    // The current request: everything after the last real user message.
    for (at, m) in messages.iter().enumerate().skip(last + 1).rev() {
        if let Some(items) = todo_in_message(messages, at) {
            if is_checklist_dismissed(m) {
                return None;
            }
            return Some(CurrentChecklist {
                items,
                carried: false,
                from_message_id: Some(m.id.clone()),
            });
        }
    }
    // No checklist in the current request: a dismissal closes it.
    if is_checklist_dismissed(&messages[last]) {
        return None;
    }
    // The previous request only: past the last user message, up to the one
    // before it. A second user message stops the search, so an old
    // checklist doesn't linger across unrelated work.
    let prev_user = messages[..last].iter().rposition(is_request);
    let from = prev_user.map(|p| p + 1).unwrap_or(0);
    for (at, m) in messages.iter().enumerate().take(last).skip(from).rev() {
        if let Some(items) = todo_in_message(messages, at) {
            if items.iter().all(is_settled) || is_checklist_dismissed(m) {
                return None;
            }
            return Some(CurrentChecklist {
                items,
                carried: true,
                from_message_id: Some(m.id.clone()),
            });
        }
    }
    None
}

/// Past these, a task without a checklist has clearly outgrown the "3 or
/// more steps" rule (the checklist gate, Agent mode): a write to a 3rd
/// distinct file, a command after 2 changed files, or this many tool calls.
pub const GATE_FILES: usize = 3;
pub const GATE_FILES_BEFORE_COMMAND: usize = 2;
pub const GATE_TOOL_CALLS: u32 = 12;

/// Why the gate asks for a checklist before this call, if it does. Only
/// work counts (writes and commands); `changed_files` are the distinct
/// project files written so far in the run. Below the thresholds a small
/// task finishes without a checklist, as the rule allows.
pub fn checklist_gate_reason(
    tool: &str,
    path: Option<&str>,
    changed_files: &[String],
    tool_calls: u32,
) -> Option<String> {
    let norm = |p: &str| super::paths::key(p).unwrap_or_else(|| p.trim().to_string());
    // Work is what changes files or runs commands (the tool's row).
    match BuiltinTool::parse(tool).map(|t| t.spec().effect) {
        Some(Effect::Writes) => {
            let new_file = path
                .map(norm)
                .is_some_and(|p| !changed_files.iter().any(|f| norm(f) == p));
            let files = changed_files.len() + usize::from(new_file);
            if files >= GATE_FILES {
                return Some(format!("{files} files changed"));
            }
        }
        Some(Effect::Runs) => {
            if changed_files.len() >= GATE_FILES_BEFORE_COMMAND {
                return Some(format!(
                    "{} files changed, now a command",
                    changed_files.len()
                ));
            }
        }
        _ => return None,
    }
    (tool_calls >= GATE_TOOL_CALLS).then(|| format!("{tool_calls} tool calls made"))
}

/// The gate's refusal: what to put in the checklist, finished work included.
pub fn checklist_gate_error(reason: &str, tool: &str) -> String {
    format!(
        "Error: this task has grown past a few steps ({reason}) and has no checklist. Create it now with the \
         todo tool: the steps you've already finished as done, the current one in_progress, the rest pending. \
         Then repeat this {tool} call."
    )
}

/// Steps still open, for carry notes and nudges.
pub fn open_steps(items: &[TodoItem]) -> Vec<&TodoItem> {
    items.iter().filter(|i| !is_settled(i)).collect()
}

/// The app's own message carrying an unfinished checklist into a new
/// request: lists what is left and says what to do whether the message
/// continues that work or starts a new task. An app message (harness), so
/// it never starts a new request itself.
pub fn carry_note(items: &[TodoItem]) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = format!(
        "[Automatic note from the IDE, not written by the user]\nUnfinished checklist from earlier work ({done} of {} done):",
        items.len()
    );
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note.push_str(
        "\nIf this message continues that work, mark the first unfinished step in_progress with todo and go on. \
         If it is a new task, either mark the old steps skipped (with the reason) or changed, or create a new checklist with todo.",
    );
    note
}

/// The app's own message resuming an unfinished checklist on Continue:
/// the remaining steps, with the first unfinished step to pick up. An app
/// message (harness), so it never starts a new request itself. No user
/// message is created, so nothing gets lost.
pub fn continue_note(items: &[TodoItem]) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = format!(
        "[Automatic note from the IDE, not written by the user]\nContinuing the unfinished checklist ({done} of {} done) - pick up the first unfinished step with todo and go on:",
        items.len()
    );
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note
}

/// The checklist as repeated on every CLM request (right after the task
/// card): the app shows it, so the model keeps working from it without
/// copying it into the card. Trimming mode keeps using `focus_note`.
pub fn checklist_note(items: &[TodoItem], carried: bool) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = if carried {
        format!(
            "[your checklist, carried from earlier work ({done} of {} done) - the panel shows it, so don't copy it into the task card:]",
            items.len()
        )
    } else {
        format!(
            "[your checklist ({done} of {} done) - the panel shows it, so don't copy it into the task card:]",
            items.len()
        )
    };
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run::messages::is_steer;
    use serde_json::json;

    #[test]
    fn todo_lists() {
        let args = json!({ "items": ["a", { "title": "b", "status": "in_progress" }, { "title": "c", "status": "bogus" }] });
        let items = parse_todo(args.as_object().unwrap());
        assert_eq!(items[2].status, "pending");
        assert_eq!(
            describe_todo(&items).content,
            "Checklist updated: 0 of 3 done; now: b."
        );
    }

    #[test]
    fn checklist_edge_cases() {
        assert_eq!(
            describe_todo(&[]).content,
            "Error: the checklist is empty. Pass items: [{ title, status }]."
        );
        let many: Vec<Value> = (0..40).map(|i| json!(format!("step {i}"))).collect();
        assert_eq!(
            parse_todo(json!({ "items": many }).as_object().unwrap()).len(),
            MAX_TODO_ITEMS
        );
        let all_settled = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "skipped" }, { "title": "c", "status": "changed" }] })
                .as_object()
                .unwrap(),
        );
        assert!(all_settled.iter().all(is_settled));
        assert_eq!(
            describe_todo(&all_settled).content,
            "Checklist updated: 3 of 3 done."
        );
        // Two steps in_progress: rejected, naming both, so the model narrows to one.
        let two_active = parse_todo(
            json!({ "items": [{ "title": "a", "status": "in_progress" }, { "title": "b", "status": "in_progress" }, { "title": "c", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        let refused = describe_todo(&two_active);
        assert!(refused.outcome.failed());
        let err = refused.content;
        assert!(err.starts_with("Error: only one step"), "{err}");
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "{err}");
        // Zero in_progress with open steps stays fine: between steps, not an error.
        let between = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            describe_todo(&between).content,
            "Checklist updated: 1 of 2 done."
        );
    }

    #[test]
    fn checklist_note_repeats_the_list_without_copying_it() {
        let items = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        let note = checklist_note(&items, false);
        assert!(note.contains("1 of 2 done"), "{note}");
        assert!(
            note.contains("- [done] a") && note.contains("- [pending] b"),
            "{note}"
        );
        assert!(note.contains("don't copy it into the task card"), "{note}");
        assert!(
            checklist_note(&items, true).contains("carried from earlier work"),
            "{note}"
        );
    }

    fn row(id: &str, role: &str, meta: Option<Value>) -> MessageRow {
        MessageRow {
            id: id.into(),
            dialog_id: "d".into(),
            seq: 0,
            role: role.into(),
            content: id.into(),
            body_external: false,
            body_bytes: 0,
            tool_calls: None,
            tool_call_id: None,
            meta,
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 0,
        }
    }

    fn todo_row(id: &str, items: Value) -> MessageRow {
        let mut m = row(id, "assistant", None);
        m.tool_calls = Some(vec![crate::chat_db::store::ToolCall {
            id: format!("{id}-call"),
            name: "todo".into(),
            arguments: items.to_string(),
            extra_content: None,
        }]);
        m
    }

    /// A `todo` call the IDE refused (two steps in progress) set nothing:
    /// the list before it stays current, whether its result says so in its
    /// meta or, stored before outcomes, by its text. A refusal is matched
    /// to its own reply's call: Gemini numbers calls per reply.
    #[test]
    fn a_todo_call_the_ide_refused_isnt_the_current_checklist() {
        let result = |id: &str, call: &str, content: &str, meta: Option<Value>| {
            let mut m = row(id, "tool", meta);
            m.content = content.into();
            m.tool_call_id = Some(call.into());
            m
        };
        let first = json!({ "items": [{ "title": "a", "status": "in_progress" }, { "title": "b", "status": "pending" }] });
        let both = json!({ "items": [{ "title": "a", "status": "in_progress" }, { "title": "b", "status": "in_progress" }] });
        let refused = describe_todo(&parse_todo(both.as_object().unwrap()));
        assert!(refused.outcome.failed());
        for meta in [Some(json!({ "outcome": "failed" })), None] {
            let messages = vec![
                row("u1", "user", None),
                todo_row("a1", first.clone()),
                result(
                    "r1",
                    "a1-call",
                    "Checklist updated: 0 of 2 done; now: a.",
                    None,
                ),
                todo_row("a2", both.clone()),
                result("r2", "a2-call", &refused.content, meta.clone()),
            ];
            let current = current_checklist(&messages).expect("the list before it");
            assert_eq!(current.from_message_id.as_deref(), Some("a1"), "{meta:?}");
            assert_eq!(current.items[1].status, "pending");
        }
        let gemini = |id: &str, items: &Value| {
            let mut m = todo_row(id, items.clone());
            m.tool_calls.as_mut().unwrap()[0].id = "function-call-1".into();
            m
        };
        let messages = vec![
            row("u1", "user", None),
            gemini("a1", &first),
            result(
                "r1",
                "function-call-1",
                "Checklist updated.",
                Some(json!({ "outcome": "done" })),
            ),
            gemini("a2", &both),
            result(
                "r2",
                "function-call-1",
                &refused.content,
                Some(json!({ "outcome": "failed" })),
            ),
        ];
        let current = current_checklist(&messages).expect("the first reply's");
        assert_eq!(current.from_message_id.as_deref(), Some("a1"));
    }

    /// An unfinished checklist is carried across one user message.
    #[test]
    fn unfinished_checklist_is_carried_across_one_user_message() {
        let messages = vec![
            row("u1", "user", None),
            todo_row(
                "a1",
                json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] }),
            ),
            row("u2", "user", None),
        ];
        let carried = current_checklist(&messages).expect("carried");
        assert!(carried.carried, "{carried:?}");
        assert_eq!(carried.from_message_id.as_deref(), Some("a1"));
        assert_eq!(carried.items.len(), 2);
        assert_eq!(open_steps(&carried.items).len(), 1);
    }

    /// 3.6: a message the user sent while the run worked is part of its
    /// request: the run's own checklist stays its own, not carried.
    #[test]
    fn a_steering_message_doesnt_carry_the_runs_checklist() {
        let messages = vec![
            row("u1", "user", None),
            todo_row(
                "a1",
                json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] }),
            ),
            row("s1", "user", Some(json!({ "steer": { "queuedAt": 1 } }))),
        ];
        assert!(is_steer(&messages[2]) && !is_request(&messages[2]));
        let current = current_checklist(&messages).expect("the run's");
        assert!(!current.carried, "{current:?}");
        assert_eq!(current.from_message_id.as_deref(), Some("a1"));
    }

    /// A fully settled checklist is not carried: the agent is wrapping up.
    #[test]
    fn settled_checklist_is_not_carried() {
        let messages = vec![
            row("u1", "user", None),
            todo_row(
                "a1",
                json!({ "items": [{ "title": "a", "status": "done" }] }),
            ),
            row("u2", "user", None),
        ];
        assert_eq!(current_checklist(&messages), None);
    }

    /// A newer `todo` call in the current request wins over the carried one.
    #[test]
    fn newer_todo_call_wins() {
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
            todo_row("a2", json!({ "items": ["new step"] })),
        ];
        let current = current_checklist(&messages).expect("current");
        assert!(!current.carried, "{current:?}");
        assert_eq!(current.from_message_id.as_deref(), Some("a2"));
        assert_eq!(current.items[0].title, "new step");
    }

    /// A second user message stops the search: an old checklist doesn't
    /// linger across unrelated work.
    #[test]
    fn second_user_message_stops_the_search() {
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
            row("u3", "user", None),
        ];
        assert_eq!(current_checklist(&messages), None);
    }

    /// The gate follows the "3 or more steps" rule: a 3rd distinct file, a
    /// command after 2 changed files, or 12 tool calls; never reads.
    #[test]
    fn checklist_gate_thresholds() {
        let files = |n: usize| (0..n).map(|i| format!("f{i}.rs")).collect::<Vec<_>>();
        // Writes: the 3rd distinct file trips it; rewriting a known one doesn't.
        assert_eq!(
            checklist_gate_reason("write", Some("new.rs"), &files(1), 2),
            None
        );
        assert_eq!(
            checklist_gate_reason("write", Some("new.rs"), &files(2), 3).as_deref(),
            Some("3 files changed")
        );
        assert_eq!(
            checklist_gate_reason("write", Some("./f1.rs"), &files(2), 3),
            None
        );
        // Commands: after 2 changed files, not after 1 (edit, then test).
        assert_eq!(checklist_gate_reason("bash", None, &files(1), 2), None);
        assert!(checklist_gate_reason("bash", None, &files(2), 3).is_some());
        // Many calls without a checklist trip it for work, not for reads.
        assert_eq!(
            checklist_gate_reason("bash", None, &[], GATE_TOOL_CALLS).as_deref(),
            Some("12 tool calls made")
        );
        assert_eq!(
            checklist_gate_reason("read", Some("a.rs"), &files(5), 40),
            None
        );
        let error = checklist_gate_error("3 files changed", "write");
        assert!(
            error.starts_with("Error:") && error.contains("already finished as done"),
            "{error}"
        );
    }

    /// Dismissed on the message holding the `todo` call (the panel's ✕):
    /// closed in its own request, and never carried.
    #[test]
    fn dismissed_on_its_own_message_closes_the_checklist() {
        let mut held = todo_row("a1", json!({ "items": ["step"] }));
        held.meta = Some(json!({ "checklistClosed": true }));
        // In the current request.
        let current = vec![row("u1", "user", None), held.clone()];
        assert_eq!(current_checklist(&current), None);
        // Not carried into the next one.
        let next = vec![row("u1", "user", None), held, row("u2", "user", None)];
        assert_eq!(current_checklist(&next), None);
        // An older, undismissed one isn't resurrected by the dismissal.
        let older = vec![
            row("u1", "user", None),
            todo_row("a0", json!({ "items": ["old"] })),
            {
                let mut m = todo_row("a1", json!({ "items": ["new"] }));
                m.meta = Some(json!({ "checklistClosed": true }));
                m
            },
        ];
        assert_eq!(current_checklist(&older), None);
    }

    /// The panel's dismiss (meta.checklistClosed) closes the checklist.
    #[test]
    fn checklist_closed_is_respected() {
        let dismissed = |key: &str| {
            vec![
                row("u1", "user", None),
                todo_row("a1", json!({ "items": ["old step"] })),
                row("u2", "user", Some(json!({ key: true }))),
            ]
        };
        assert_eq!(current_checklist(&dismissed("checklistClosed")), None);
        assert_eq!(current_checklist(&dismissed("checklist_closed")), None);
        // Without the flag the same history carries.
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
        ];
        assert!(current_checklist(&messages).is_some());
    }
}

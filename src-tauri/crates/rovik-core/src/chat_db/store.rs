//! Chat data operations. Plain functions on a `Connection`, so they can be
//! tested directly; `commands.rs` runs them on the writer thread or a reader.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Tool results larger than this move to `message_bodies` when finalized.
pub const EXTERNAL_BODY_BYTES: usize = 4096;
/// Characters of an external body kept inline as its preview.
pub const PREVIEW_CHARS: usize = 2048;
const DIALOG_PREVIEW_CHARS: usize = 200;
/// Gap between consecutive messages' `seq`, leaving room for later inserts.
const SEQ_STEP: i64 = 1024;

pub type DbResult<T> = Result<T, String>;

pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// First `max` characters, on a character boundary.
pub fn char_prefix(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/*
 * ---------------------------------------------------------------------------
 * Types (camelCase over IPC)
 * ---------------------------------------------------------------------------
 */

pub use rovik_protocol::chat::{ChatModel, MessagePatch, ToolCall};

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DialogSummary {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: i64,
    pub preview: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Cursor {
    pub updated_at: i64,
    pub id: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DialogPage {
    pub items: Vec<DialogSummary>,
    pub next_cursor: Option<Cursor>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewDialog {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The project it belongs to (chat_db/projects.rs); none: no folder open.
    #[serde(default)]
    pub project_id: Option<String>,
}

/// A subagent's chat (agent_run/subagent.rs), hidden from the chat list and
/// search, in its parent's project.
#[derive(Debug, Clone)]
pub struct NewChildDialog {
    pub id: String,
    pub parent_id: String,
    /// The parent's `task` call this chat answers.
    pub parent_call_id: String,
    /// The subagent kind ("scout", …).
    pub kind: String,
    pub title: String,
    pub created_at: i64,
}

/// A child chat as its parent sees it.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChildDialog {
    pub id: String,
    pub parent_id: String,
    pub parent_call_id: String,
    pub kind: String,
    pub created_at: i64,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MessageRow {
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
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NewMessage {
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub meta: Option<Value>,
    pub error: Option<String>,
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub attachments: Vec<String>,
    pub created_at: i64,
    /// Complete when appended (user and tool messages, finished replies).
    #[serde(default)]
    pub finalized: bool,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<MessageRow>,
    pub has_older: bool,
    pub has_newer: bool,
}

/*
 * ---------------------------------------------------------------------------
 * Dialogs
 * ---------------------------------------------------------------------------
 */

fn dialog_from_row(r: &Row) -> rusqlite::Result<DialogSummary> {
    Ok(DialogSummary {
        id: r.get(0)?,
        title: r.get(1)?,
        created_at: r.get(2)?,
        updated_at: r.get(3)?,
        message_count: r.get(4)?,
        preview: r.get(5)?,
    })
}

const DIALOG_COLUMNS: &str = "id, title, created_at, updated_at, message_count, preview";

/// A project's chats, newest first (`project` None: the chats with no
/// project); keyset paging so deep pages cost the same as the first.
pub fn list_dialogs(
    conn: &Connection,
    project: Option<&str>,
    cursor: Option<&Cursor>,
    limit: usize,
) -> DbResult<DialogPage> {
    let limit = limit.clamp(1, 500);
    let fetch = (limit + 1) as i64;
    let mut items: Vec<DialogSummary> = match cursor {
        None => {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {DIALOG_COLUMNS} FROM dialogs WHERE project_id IS ?2 AND archived = 0
                     ORDER BY updated_at DESC, id DESC LIMIT ?1"
                ))
                .map_err(err)?;
            let rows = stmt
                .query_map(params![fetch, project], dialog_from_row)
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            rows
        }
        Some(c) => {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {DIALOG_COLUMNS} FROM dialogs WHERE project_id IS ?4 AND archived = 0
                       AND (updated_at < ?1 OR (updated_at = ?1 AND id < ?2))
                     ORDER BY updated_at DESC, id DESC LIMIT ?3"
                ))
                .map_err(err)?;
            let rows = stmt
                .query_map(params![c.updated_at, c.id, fetch, project], dialog_from_row)
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            rows
        }
    };

    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|d| Cursor {
            updated_at: d.updated_at,
            id: d.id.clone(),
        })
    } else {
        None
    };
    Ok(DialogPage { items, next_cursor })
}

/// The ids of a project's chats (`project` None: the chats with no project).
/// Subagents' chats aren't among them: they belong to their parent.
pub fn dialogs_in_project(
    conn: &Connection,
    project: Option<&str>,
) -> DbResult<std::collections::HashSet<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT id FROM dialogs WHERE project_id IS ?1 AND parent_id IS NULL")
        .map_err(err)?;
    let rows = stmt
        .query_map(params![project], |r| r.get::<_, String>(0))
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

pub fn get_dialog(conn: &Connection, id: &str) -> DbResult<Option<DialogSummary>> {
    conn.query_row(
        &format!("SELECT {DIALOG_COLUMNS} FROM dialogs WHERE id = ?1"),
        params![id],
        dialog_from_row,
    )
    .optional()
    .map_err(err)
}

/// The chat's project; None for a chat with no project, or no such chat.
pub fn dialog_project(conn: &Connection, id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT project_id FROM dialogs WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(err)
}

/// The chat a message is in; None for no such message.
pub fn message_dialog(conn: &Connection, message_id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT dialog_id FROM messages WHERE id = ?1",
        params![message_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

pub fn create_dialog(conn: &mut Connection, d: &NewDialog) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute(
        "INSERT INTO dialogs (id, title, created_at, updated_at, project_id) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![d.id, d.title, d.created_at, d.updated_at, d.project_id],
    )
    .map_err(err)?;
    index_title(&tx, &d.id, &d.title)?;
    tx.commit().map_err(err)
}

/// A subagent's chat: in its parent's project and wherever its parent is
/// owned (chat_db/location.rs), stored with `archived = 2` so no chat list
/// shows it (older app versions list `archived = 0`), and its title isn't
/// indexed for search.
pub fn create_child_dialog(conn: &mut Connection, d: &NewChildDialog) -> DbResult<()> {
    conn.execute(
        "INSERT INTO dialogs (id, title, created_at, updated_at, project_id, archived, parent_id, parent_call_id, kind, location)
         SELECT ?1, ?2, ?3, ?3, project_id, 2, id, ?5, ?6, location FROM dialogs WHERE id = ?4",
        params![d.id, d.title, d.created_at, d.parent_id, d.parent_call_id, d.kind],
    )
    .map_err(err)
    .and_then(|n| {
        if n == 1 {
            Ok(())
        } else {
            Err(format!("no chat {} to add a subagent to", d.parent_id))
        }
    })
}

fn child_from_row(r: &Row) -> rusqlite::Result<ChildDialog> {
    Ok(ChildDialog {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        parent_call_id: r.get(2)?,
        kind: r.get(3)?,
        created_at: r.get(4)?,
    })
}

const CHILD_COLUMNS: &str = "id, parent_id, parent_call_id, kind, created_at";

/// The subagent chat answering a parent's tool call, if there is one.
pub fn child_for_call(
    conn: &Connection,
    parent_id: &str,
    parent_call_id: &str,
) -> DbResult<Option<ChildDialog>> {
    conn.query_row(
        &format!(
            "SELECT {CHILD_COLUMNS} FROM dialogs WHERE parent_id = ?1 AND parent_call_id = ?2
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ),
        params![parent_id, parent_call_id],
        child_from_row,
    )
    .optional()
    .map_err(err)
}

/// Round 2, P2.7: the tool calls a chat's model made, all of them (not
/// one page's): what a resumed subagent has used of its budget.
pub fn count_tool_calls(conn: &Connection, dialog_id: &str) -> DbResult<usize> {
    conn.query_row(
        "SELECT COALESCE(SUM(json_array_length(tool_calls)), 0) FROM messages
         WHERE dialog_id = ?1 AND tool_calls IS NOT NULL AND json_valid(tool_calls)",
        params![dialog_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as usize)
    .map_err(err)
}

/// A chat's subagent by the handle its report gave the lead
/// ("scout-3f2a9c1d": the kind and the start of its id), or by the `task`
/// call that started it.
pub fn child_by_handle(
    conn: &Connection,
    parent_id: &str,
    handle: &str,
) -> DbResult<Option<ChildDialog>> {
    let handle = handle.trim();
    let prefix = handle
        .split_once('-')
        .map(|(_, rest)| rest)
        .filter(|rest| rest.len() >= 6 && rest.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|rest| format!("{rest}%"))
        .unwrap_or_else(|| "\u{0}".to_string());
    conn.query_row(
        &format!(
            "SELECT {CHILD_COLUMNS} FROM dialogs WHERE parent_id = ?1
               AND (parent_call_id = ?2 OR id LIKE ?3)
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ),
        params![parent_id, handle, prefix],
        child_from_row,
    )
    .optional()
    .map_err(err)
}

pub fn rename_dialog(conn: &mut Connection, id: &str, title: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute(
        "UPDATE dialogs SET title = ?2 WHERE id = ?1",
        params![id, title],
    )
    .map_err(err)?;
    index_title(&tx, id, title)?;
    tx.commit().map_err(err)
}

/// The chat at the top of a subagent's chat's parents: the chat itself
/// for a lead's, or for a chat this database doesn't have. Each parent
/// once, even if the links loop.
pub fn lead_chat(conn: &Connection, id: &str) -> DbResult<String> {
    let mut chat = id.to_string();
    let mut seen = std::collections::HashSet::from([chat.clone()]);
    loop {
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM dialogs WHERE id = ?1",
                params![chat],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?
            .flatten();
        match parent {
            Some(parent) if seen.insert(parent.clone()) => chat = parent,
            _ => return Ok(chat),
        }
    }
}

/// P6.7: a chat's subagents' chats, theirs too, deepest first. Each once,
/// and never `id` itself, even if the parent links loop (round 2).
pub fn descendants(conn: &Connection, id: &str) -> DbResult<Vec<String>> {
    let mut seen = std::collections::HashSet::from([id.to_string()]);
    descendants_in(conn, id, &mut seen)
}

fn descendants_in(
    conn: &Connection,
    id: &str,
    seen: &mut std::collections::HashSet<String>,
) -> DbResult<Vec<String>> {
    let mut out = Vec::new();
    let children: Vec<String> = {
        let mut stmt = conn
            .prepare_cached("SELECT id FROM dialogs WHERE parent_id = ?1")
            .map_err(err)?;
        let ids = stmt
            .query_map(params![id], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        ids
    };
    for child in children {
        if seen.insert(child.clone()) {
            out.extend(descendants_in(conn, &child, seen)?);
            out.push(child);
        }
    }
    Ok(out)
}

/// The chat belongs to `project_id` from now on: a daemon's chat to the
/// project its worktree is of (6.4).
pub fn set_dialog_project(conn: &Connection, id: &str, project_id: Option<&str>) -> DbResult<()> {
    conn.execute(
        "UPDATE dialogs SET project_id = ?2 WHERE id = ?1",
        params![id, project_id],
    )
    .map(drop)
    .map_err(err)
}

pub fn delete_dialog(conn: &mut Connection, id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    // Round 3: the subagents' chats first, each once, however the parent
    // links loop (the same walk `descendants` makes).
    for child in descendants(&tx, id)? {
        delete_dialog_in(&tx, &child)?;
    }
    delete_dialog_in(&tx, id)?;
    tx.commit().map_err(err)
}

/// One chat's rows (the foreign key would remove a subagent's rows, but
/// not its search rows, so `delete_dialog` removes those first).
fn delete_dialog_in(tx: &Connection, id: &str) -> DbResult<()> {
    let rowids: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM search_rows WHERE dialog_id = ?1")
            .map_err(err)?;
        let ids = stmt
            .query_map(params![id], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        ids
    };
    for rowid in rowids {
        tx.execute("DELETE FROM search_index WHERE rowid = ?1", params![rowid])
            .map_err(err)?;
        tx.execute(
            "DELETE FROM title_trigrams WHERE rowid = ?1",
            params![rowid],
        )
        .map_err(err)?;
    }
    tx.execute("DELETE FROM search_rows WHERE dialog_id = ?1", params![id])
        .map_err(err)?;
    tx.execute("DELETE FROM dialogs WHERE id = ?1", params![id])
        .map_err(err)?;
    Ok(())
}

/*
 * ---------------------------------------------------------------------------
 * Search rows (kept in step with dialogs and finalized messages)
 * ---------------------------------------------------------------------------
 */

fn remove_search_row(conn: &Connection, dialog_id: &str, message_id: Option<&str>) -> DbResult<()> {
    let rowid: Option<i64> = match message_id {
        Some(m) => conn
            .query_row(
                "SELECT rowid FROM search_rows WHERE message_id = ?1",
                params![m],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?,
        None => conn
            .query_row(
                "SELECT rowid FROM search_rows WHERE dialog_id = ?1 AND message_id IS NULL",
                params![dialog_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?,
    };
    if let Some(rowid) = rowid {
        conn.execute("DELETE FROM search_index WHERE rowid = ?1", params![rowid])
            .map_err(err)?;
        conn.execute(
            "DELETE FROM title_trigrams WHERE rowid = ?1",
            params![rowid],
        )
        .map_err(err)?;
        conn.execute("DELETE FROM search_rows WHERE rowid = ?1", params![rowid])
            .map_err(err)?;
    }
    Ok(())
}

fn index_title(conn: &Connection, dialog_id: &str, title: &str) -> DbResult<()> {
    remove_search_row(conn, dialog_id, None)?;
    conn.execute(
        "INSERT INTO search_rows (dialog_id, message_id) VALUES (?1, NULL)",
        params![dialog_id],
    )
    .map_err(err)?;
    let rowid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO search_index (rowid, title, body, dialog_id, message_id) VALUES (?1, ?2, '', ?3, NULL)",
        params![rowid, title, dialog_id],
    )
    .map_err(err)?;
    conn.execute(
        "INSERT INTO title_trigrams (rowid, title, dialog_id) VALUES (?1, ?2, ?3)",
        params![rowid, title, dialog_id],
    )
    .map_err(err)?;
    Ok(())
}

fn index_message(conn: &Connection, dialog_id: &str, message_id: &str, text: &str) -> DbResult<()> {
    remove_search_row(conn, dialog_id, Some(message_id))?;
    conn.execute(
        "INSERT INTO search_rows (dialog_id, message_id) VALUES (?1, ?2)",
        params![dialog_id, message_id],
    )
    .map_err(err)?;
    let rowid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO search_index (rowid, title, body, dialog_id, message_id) VALUES (?1, '', ?2, ?3, ?4)",
        params![rowid, text, dialog_id, message_id],
    )
    .map_err(err)?;
    Ok(())
}

/*
 * ---------------------------------------------------------------------------
 * Messages
 * ---------------------------------------------------------------------------
 */

const MESSAGE_COLUMNS: &str = "id, dialog_id, seq, role, body_external, body_bytes, tool_calls, \
     tool_call_id, meta, error, failed, finalized, created_at, content";

fn message_from_row(r: &Row) -> rusqlite::Result<MessageRow> {
    let tool_calls: Option<String> = r.get(6)?;
    let meta: Option<String> = r.get(8)?;
    Ok(MessageRow {
        id: r.get(0)?,
        dialog_id: r.get(1)?,
        seq: r.get(2)?,
        role: r.get(3)?,
        body_external: r.get::<_, i64>(4)? != 0,
        body_bytes: r.get(5)?,
        tool_calls: tool_calls.and_then(|t| serde_json::from_str(&t).ok()),
        tool_call_id: r.get(7)?,
        meta: meta.and_then(|m| serde_json::from_str(&m).ok()),
        error: r.get(9)?,
        failed: r.get::<_, i64>(10)? != 0,
        finalized: r.get::<_, i64>(11)? != 0,
        created_at: r.get(12)?,
        content: r.get(13)?,
        attachments: Vec::new(),
    })
}

/// Fills `attachments` for a page of messages with one query.
fn attach_attachments(conn: &Connection, messages: &mut [MessageRow]) -> DbResult<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; messages.len()].join(",");
    let mut stmt = conn
        .prepare(&format!(
            "SELECT message_id, attachment_id FROM message_attachments
             WHERE message_id IN ({placeholders}) ORDER BY message_id, position"
        ))
        .map_err(err)?;
    let mut by_message: HashMap<String, Vec<String>> = HashMap::new();
    let rows = stmt
        .query_map(
            params_from_iter(messages.iter().map(|m| m.id.as_str())),
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(err)?;
    for row in rows {
        let (message_id, attachment_id) = row.map_err(err)?;
        by_message
            .entry(message_id)
            .or_default()
            .push(attachment_id);
    }
    for m in messages.iter_mut() {
        if let Some(list) = by_message.remove(&m.id) {
            m.attachments = list;
        }
    }
    Ok(())
}

fn to_json_opt<T: Serialize>(value: &Option<T>) -> Option<String> {
    value.as_ref().and_then(|v| serde_json::to_string(v).ok())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Appends those of the messages the chat doesn't have yet, in order: a
/// mirror's rows, which may have come another way first (6.3). Returns
/// each message's `seq`, whether it was appended now or before.
pub fn append_missing(
    conn: &mut Connection,
    dialog_id: &str,
    messages: &[NewMessage],
) -> DbResult<Vec<i64>> {
    let known = messages
        .iter()
        .map(|m| {
            conn.query_row(
                "SELECT seq FROM messages WHERE id = ?1",
                params![m.id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
        })
        .collect::<rusqlite::Result<Vec<Option<i64>>>>()
        .map_err(err)?;
    let missing: Vec<NewMessage> = messages
        .iter()
        .zip(&known)
        .filter(|(_, seq)| seq.is_none())
        .map(|(m, _)| m.clone())
        .collect();
    let mut appended = if missing.is_empty() {
        Vec::new().into_iter()
    } else {
        append_messages(conn, dialog_id, &missing)?.into_iter()
    };
    Ok(known
        .into_iter()
        .map(|seq| seq.or_else(|| appended.next()).unwrap_or_default())
        .collect())
}

/// Appends messages in order; returns the `seq` assigned to each.
pub fn append_messages(
    conn: &mut Connection,
    dialog_id: &str,
    messages: &[NewMessage],
) -> DbResult<Vec<i64>> {
    let tx = conn.transaction().map_err(err)?;
    let mut next: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE dialog_id = ?1",
            params![dialog_id],
            |r| r.get(0),
        )
        .map_err(err)?;

    let mut seqs = Vec::with_capacity(messages.len());
    for m in messages {
        next += SEQ_STEP;
        tx.execute(
            &format!(
                "INSERT INTO messages ({MESSAGE_COLUMNS}) VALUES
                 (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12)"
            ),
            params![
                m.id,
                dialog_id,
                next,
                m.role,
                m.content.len() as i64,
                to_json_opt(&m.tool_calls),
                m.tool_call_id,
                to_json_opt(&m.meta),
                m.error,
                m.failed as i64,
                m.created_at,
                m.content,
            ],
        )
        .map_err(err)?;
        for (position, attachment) in m.attachments.iter().enumerate() {
            tx.execute(
                "INSERT INTO message_attachments (message_id, position, attachment_id) VALUES (?1, ?2, ?3)",
                params![m.id, position as i64, attachment],
            )
            .map_err(err)?;
        }
        if m.finalized {
            finalize(&tx, &m.id)?;
        }
        seqs.push(next);
    }

    tx.execute(
        "UPDATE dialogs SET message_count = message_count + ?2, updated_at = MAX(updated_at, ?3) WHERE id = ?1",
        params![dialog_id, messages.len() as i64, now_ms()],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(seqs)
}

/// Makes a message final: large tool output moves to `message_bodies`,
/// searchable text is indexed, and the chat's preview is refreshed.
fn finalize(conn: &Connection, message_id: &str) -> DbResult<()> {
    let (dialog_id, role, content, error, failed, external): (
        String,
        String,
        String,
        Option<String>,
        bool,
        bool,
    ) = conn
        .query_row(
            "SELECT dialog_id, role, content, error, failed, body_external FROM messages WHERE id = ?1",
            params![message_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, i64>(4)? != 0,
                    r.get::<_, i64>(5)? != 0,
                ))
            },
        )
        .map_err(err)?;

    if external && content.chars().count() <= PREVIEW_CHARS {
        // Finalized before (a failed/error flag set on it since): its body is
        // in `message_bodies` and `content` is the preview. Moving the preview
        // there would lose the body; its size must stay the body's.
        conn.execute(
            "UPDATE messages SET finalized = 1 WHERE id = ?1",
            params![message_id],
        )
        .map_err(err)?;
    } else if role == "tool" && content.len() > EXTERNAL_BODY_BYTES {
        conn.execute(
            "INSERT OR REPLACE INTO message_bodies (message_id, content) VALUES (?1, ?2)",
            params![message_id, content],
        )
        .map_err(err)?;
        conn.execute(
            "UPDATE messages SET content = ?2, body_external = 1, body_bytes = ?3, finalized = 1 WHERE id = ?1",
            params![message_id, char_prefix(&content, PREVIEW_CHARS), content.len() as i64],
        )
        .map_err(err)?;
    } else {
        conn.execute(
            "UPDATE messages SET body_bytes = ?2, finalized = 1 WHERE id = ?1",
            params![message_id, content.len() as i64],
        )
        .map_err(err)?;
    }

    // P6.7: a subagent's chat isn't in the chat list, nor in search.
    let child: bool = conn
        .query_row(
            "SELECT parent_id IS NOT NULL FROM dialogs WHERE id = ?1",
            params![dialog_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
        .unwrap_or(false);
    let searchable =
        !child && (role == "user" || role == "assistant") && error.is_none() && !failed;
    if searchable && !content.trim().is_empty() {
        index_message(conn, &dialog_id, message_id, &content)?;
        let preview = collapse_whitespace(char_prefix(&content, DIALOG_PREVIEW_CHARS * 2));
        conn.execute(
            "UPDATE dialogs SET preview = ?2, updated_at = MAX(updated_at, ?3) WHERE id = ?1",
            params![
                dialog_id,
                char_prefix(&preview, DIALOG_PREVIEW_CHARS),
                now_ms()
            ],
        )
        .map_err(err)?;
    } else {
        remove_search_row(conn, &dialog_id, Some(message_id))?;
    }
    Ok(())
}

pub fn update_message(
    conn: &mut Connection,
    message_id: &str,
    patch: &MessagePatch,
) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    if let Some(content) = &patch.content {
        tx.execute(
            "UPDATE messages SET content = ?2, body_bytes = ?3 WHERE id = ?1",
            params![message_id, content, content.len() as i64],
        )
        .map_err(err)?;
    }
    if let Some(tool_calls) = &patch.tool_calls {
        tx.execute(
            "UPDATE messages SET tool_calls = ?2 WHERE id = ?1",
            params![message_id, serde_json::to_string(tool_calls).map_err(err)?],
        )
        .map_err(err)?;
    }
    if let Some(meta) = &patch.meta {
        tx.execute(
            "UPDATE messages SET meta = ?2 WHERE id = ?1",
            params![message_id, serde_json::to_string(meta).map_err(err)?],
        )
        .map_err(err)?;
    }
    if let Some(error) = &patch.error {
        tx.execute(
            "UPDATE messages SET error = ?2 WHERE id = ?1",
            params![message_id, error],
        )
        .map_err(err)?;
    }
    if let Some(failed) = patch.failed {
        tx.execute(
            "UPDATE messages SET failed = ?2 WHERE id = ?1",
            params![message_id, failed as i64],
        )
        .map_err(err)?;
    }

    // A message that becomes failed or errored must leave the search index.
    let finalized: bool = tx
        .query_row(
            "SELECT finalized FROM messages WHERE id = ?1",
            params![message_id],
            |r| Ok(r.get::<_, i64>(0)? != 0),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| format!("No message {message_id}"))?;
    if patch.finalize || (finalized && (patch.failed.is_some() || patch.error.is_some())) {
        finalize(&tx, message_id)?;
    }
    tx.commit().map_err(err)
}

/// The newest `limit` messages before `before_seq` (or the newest overall),
/// returned oldest first.
pub fn get_messages(
    conn: &Connection,
    dialog_id: &str,
    before_seq: Option<i64>,
    limit: usize,
) -> DbResult<MessagePage> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE dialog_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3"
        ))
        .map_err(err)?;
    let mut messages: Vec<MessageRow> = stmt
        .query_map(
            params![
                dialog_id,
                before_seq.unwrap_or(i64::MAX),
                (limit + 1) as i64
            ],
            message_from_row,
        )
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let has_older = messages.len() > limit;
    messages.truncate(limit);
    messages.reverse();
    attach_attachments(conn, &mut messages)?;
    Ok(MessagePage {
        messages,
        has_older,
        has_newer: before_seq.is_some(),
    })
}

/// Up to `before` messages before `message_id`, the message itself, and up
/// to `after` after it: for opening a chat at a search hit.
pub fn get_messages_around(
    conn: &Connection,
    message_id: &str,
    before: usize,
    after: usize,
) -> DbResult<MessagePage> {
    let (dialog_id, seq): (String, i64) = conn
        .query_row(
            "SELECT dialog_id, seq FROM messages WHERE id = ?1",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;

    let mut older = get_messages(conn, &dialog_id, Some(seq + 1), before + 1)?;
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE dialog_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3"
        ))
        .map_err(err)?;
    let mut newer: Vec<MessageRow> = stmt
        .query_map(
            params![dialog_id, seq, (after + 1) as i64],
            message_from_row,
        )
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let has_newer = newer.len() > after;
    newer.truncate(after);
    attach_attachments(conn, &mut newer)?;

    older.messages.extend(newer);
    Ok(MessagePage {
        messages: older.messages,
        has_older: older.has_older,
        has_newer,
    })
}

pub fn get_message_body(conn: &Connection, message_id: &str) -> DbResult<String> {
    let external: Option<String> = conn
        .query_row(
            "SELECT content FROM message_bodies WHERE message_id = ?1",
            params![message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    match external {
        Some(body) => Ok(body),
        None => conn
            .query_row(
                "SELECT content FROM messages WHERE id = ?1",
                params![message_id],
                |r| r.get(0),
            )
            .map_err(err),
    }
}

/// Which of these user messages have a successful assistant reply after
/// them (used to recover drafts after a restart).
pub fn messages_answered(conn: &Connection, ids: &[String]) -> DbResult<Vec<String>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT 1 FROM messages m
             JOIN messages a ON a.dialog_id = m.dialog_id AND a.seq > m.seq
             WHERE m.id = ?1 AND a.role = 'assistant' AND a.error IS NULL AND a.finalized = 1
             LIMIT 1",
        )
        .map_err(err)?;
    let mut answered = Vec::new();
    for id in ids {
        if stmt.exists(params![id]).map_err(err)? {
            answered.push(id.clone());
        }
    }
    Ok(answered)
}

/*
 * ---------------------------------------------------------------------------
 * Tool call extras (Gemini 3 thought signatures)
 * ---------------------------------------------------------------------------
 */

/// Saves provider data for tool calls of an assistant message, as parsed
/// from the response. Skipped if the message doesn't exist (e.g. deleted).
pub fn save_tool_call_extras(
    conn: &mut Connection,
    message_id: &str,
    extras: &[(String, Value)],
) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    for (call_id, extra) in extras {
        tx.execute(
            "INSERT OR REPLACE INTO tool_call_extras (message_id, call_id, extra_content, created_at)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM messages WHERE id = ?1)",
            params![message_id, call_id, extra.to_string(), now_ms()],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)
}

/// Saved extras for these messages, by (message id, call id).
/// The stored reasoning of messages (`meta.reasoning`), by message id.
pub fn reasoning_of(conn: &Connection, message_ids: &[&str]) -> DbResult<HashMap<String, Value>> {
    let mut out = HashMap::new();
    for chunk in message_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, meta FROM messages WHERE id IN ({placeholders}) AND meta IS NOT NULL"
            ))
            .map_err(err)?;
        let rows = stmt
            .query_map(params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?;
        for row in rows {
            let (id, meta) = row.map_err(err)?;
            let reasoning = serde_json::from_str::<Value>(&meta)
                .ok()
                .and_then(|m| m.get("reasoning").cloned());
            if let Some(reasoning) = reasoning {
                out.insert(id, reasoning);
            }
        }
    }
    Ok(out)
}

pub fn tool_call_extras(
    conn: &Connection,
    message_ids: &[&str],
) -> DbResult<HashMap<(String, String), Value>> {
    let mut out = HashMap::new();
    for chunk in message_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT message_id, call_id, extra_content FROM tool_call_extras WHERE message_id IN ({placeholders})"
            ))
            .map_err(err)?;
        let rows = stmt
            .query_map(params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(err)?;
        for row in rows {
            let (message_id, call_id, extra) = row.map_err(err)?;
            if let Ok(value) = serde_json::from_str(&extra) {
                out.insert((message_id, call_id), value);
            }
        }
    }
    Ok(out)
}

/*
 * ---------------------------------------------------------------------------
 * The chat's model and reasoning effort
 * ---------------------------------------------------------------------------
 */

/// Saves the chat's model and effort. Missing chats are ignored.
pub fn set_chat_model(conn: &Connection, dialog_id: &str, model: &ChatModel) -> DbResult<()> {
    conn.execute(
        "UPDATE dialogs SET model_provider = ?2, model_id = ?3, effort = ?4 WHERE id = ?1",
        params![dialog_id, model.provider_id, model.model_id, model.effort],
    )
    .map_err(err)?;
    Ok(())
}

/// The chat's saved model and effort; none if it never saved one.
pub fn chat_model(conn: &Connection, dialog_id: &str) -> DbResult<Option<ChatModel>> {
    let row = conn
        .query_row(
            "SELECT model_provider, model_id, effort FROM dialogs WHERE id = ?1",
            params![dialog_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    Ok(match row {
        Some((Some(provider_id), Some(model_id), effort)) => Some(ChatModel {
            provider_id,
            model_id,
            effort,
        }),
        _ => None,
    })
}

/*
 * ---------------------------------------------------------------------------
 * Composer drafts (unsent chat input, one row per chat)
 * ---------------------------------------------------------------------------
 */

/// Saves the chat input's unsent input (JSON-encoded `DraftSegment[]`,
/// see lib/composerDraft.ts). Empty/blank text deletes the row so "no draft"
/// and "empty draft" read the same. Missing chats are ignored (e.g. deleted
/// while a debounced save was in flight).
pub fn save_composer_draft(conn: &mut Connection, dialog_id: &str, content: &str) -> DbResult<()> {
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        conn.execute(
            "DELETE FROM composer_drafts WHERE dialog_id = ?1",
            params![dialog_id],
        )
        .map_err(err)?;
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO composer_drafts (dialog_id, content, updated_at)
         SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM dialogs WHERE id = ?1)",
        params![dialog_id, content, now_ms()],
    )
    .map_err(err)?;
    Ok(())
}

/// The chat's unsent input (JSON-encoded segments), if any.
pub fn get_composer_draft(conn: &Connection, dialog_id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT content FROM composer_drafts WHERE dialog_id = ?1",
        params![dialog_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

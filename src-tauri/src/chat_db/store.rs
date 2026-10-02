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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// Provider data that must be sent back unchanged with this call, e.g.
    /// Gemini 3's `{"google": {"thought_signature": "…"}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

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

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MessagePatch {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub meta: Option<Value>,
    pub error: Option<String>,
    pub failed: Option<bool>,
    #[serde(default)]
    pub finalize: bool,
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

/// Newest first; keyset paging so deep pages cost the same as the first.
pub fn list_dialogs(conn: &Connection, cursor: Option<&Cursor>, limit: usize) -> DbResult<DialogPage> {
    let limit = limit.clamp(1, 500);
    let fetch = (limit + 1) as i64;
    let mut items: Vec<DialogSummary> = match cursor {
        None => {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {DIALOG_COLUMNS} FROM dialogs WHERE archived = 0
                     ORDER BY updated_at DESC, id DESC LIMIT ?1"
                ))
                .map_err(err)?;
            let rows = stmt
                .query_map(params![fetch], dialog_from_row)
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            rows
        }
        Some(c) => {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {DIALOG_COLUMNS} FROM dialogs WHERE archived = 0
                       AND (updated_at < ?1 OR (updated_at = ?1 AND id < ?2))
                     ORDER BY updated_at DESC, id DESC LIMIT ?3"
                ))
                .map_err(err)?;
            let rows = stmt
                .query_map(params![c.updated_at, c.id, fetch], dialog_from_row)
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            rows
        }
    };

    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|d| Cursor { updated_at: d.updated_at, id: d.id.clone() })
    } else {
        None
    };
    Ok(DialogPage { items, next_cursor })
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

pub fn create_dialog(conn: &mut Connection, d: &NewDialog) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute(
        "INSERT INTO dialogs (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![d.id, d.title, d.created_at, d.updated_at],
    )
    .map_err(err)?;
    index_title(&tx, &d.id, &d.title)?;
    tx.commit().map_err(err)
}

pub fn rename_dialog(conn: &mut Connection, id: &str, title: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute("UPDATE dialogs SET title = ?2 WHERE id = ?1", params![id, title])
        .map_err(err)?;
    index_title(&tx, id, title)?;
    tx.commit().map_err(err)
}

pub fn delete_dialog(conn: &mut Connection, id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
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
        tx.execute("DELETE FROM search_index WHERE rowid = ?1", params![rowid]).map_err(err)?;
        tx.execute("DELETE FROM title_trigrams WHERE rowid = ?1", params![rowid]).map_err(err)?;
    }
    tx.execute("DELETE FROM search_rows WHERE dialog_id = ?1", params![id]).map_err(err)?;
    tx.execute("DELETE FROM dialogs WHERE id = ?1", params![id]).map_err(err)?;
    tx.commit().map_err(err)
}

/*
 * ---------------------------------------------------------------------------
 * Search rows (kept in step with dialogs and finalized messages)
 * ---------------------------------------------------------------------------
 */

fn remove_search_row(conn: &Connection, dialog_id: &str, message_id: Option<&str>) -> DbResult<()> {
    let rowid: Option<i64> = match message_id {
        Some(m) => conn
            .query_row("SELECT rowid FROM search_rows WHERE message_id = ?1", params![m], |r| r.get(0))
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
        conn.execute("DELETE FROM search_index WHERE rowid = ?1", params![rowid]).map_err(err)?;
        conn.execute("DELETE FROM title_trigrams WHERE rowid = ?1", params![rowid]).map_err(err)?;
        conn.execute("DELETE FROM search_rows WHERE rowid = ?1", params![rowid]).map_err(err)?;
    }
    Ok(())
}

fn index_title(conn: &Connection, dialog_id: &str, title: &str) -> DbResult<()> {
    remove_search_row(conn, dialog_id, None)?;
    conn.execute("INSERT INTO search_rows (dialog_id, message_id) VALUES (?1, NULL)", params![dialog_id])
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
        .query_map(params_from_iter(messages.iter().map(|m| m.id.as_str())), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(err)?;
    for row in rows {
        let (message_id, attachment_id) = row.map_err(err)?;
        by_message.entry(message_id).or_default().push(attachment_id);
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

/// Appends messages in order; returns the `seq` assigned to each.
pub fn append_messages(conn: &mut Connection, dialog_id: &str, messages: &[NewMessage]) -> DbResult<Vec<i64>> {
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
    let (dialog_id, role, content, error, failed): (String, String, String, Option<String>, bool) = conn
        .query_row(
            "SELECT dialog_id, role, content, error, failed FROM messages WHERE id = ?1",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0)),
        )
        .map_err(err)?;

    if role == "tool" && content.len() > EXTERNAL_BODY_BYTES {
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

    let searchable = (role == "user" || role == "assistant") && error.is_none() && !failed;
    if searchable && !content.trim().is_empty() {
        index_message(conn, &dialog_id, message_id, &content)?;
        let preview = collapse_whitespace(char_prefix(&content, DIALOG_PREVIEW_CHARS * 2));
        conn.execute(
            "UPDATE dialogs SET preview = ?2, updated_at = MAX(updated_at, ?3) WHERE id = ?1",
            params![dialog_id, char_prefix(&preview, DIALOG_PREVIEW_CHARS), now_ms()],
        )
        .map_err(err)?;
    } else {
        remove_search_row(conn, &dialog_id, Some(message_id))?;
    }
    Ok(())
}

pub fn update_message(conn: &mut Connection, message_id: &str, patch: &MessagePatch) -> DbResult<()> {
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
        tx.execute("UPDATE messages SET error = ?2 WHERE id = ?1", params![message_id, error])
            .map_err(err)?;
    }
    if let Some(failed) = patch.failed {
        tx.execute("UPDATE messages SET failed = ?2 WHERE id = ?1", params![message_id, failed as i64])
            .map_err(err)?;
    }

    // A message that becomes failed or errored must leave the search index.
    let finalized: bool = tx
        .query_row("SELECT finalized FROM messages WHERE id = ?1", params![message_id], |r| {
            Ok(r.get::<_, i64>(0)? != 0)
        })
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
pub fn get_messages(conn: &Connection, dialog_id: &str, before_seq: Option<i64>, limit: usize) -> DbResult<MessagePage> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE dialog_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3"
        ))
        .map_err(err)?;
    let mut messages: Vec<MessageRow> = stmt
        .query_map(params![dialog_id, before_seq.unwrap_or(i64::MAX), (limit + 1) as i64], message_from_row)
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let has_older = messages.len() > limit;
    messages.truncate(limit);
    messages.reverse();
    attach_attachments(conn, &mut messages)?;
    Ok(MessagePage { messages, has_older, has_newer: before_seq.is_some() })
}

/// Up to `before` messages before `message_id`, the message itself, and up
/// to `after` after it: for opening a chat at a search hit.
pub fn get_messages_around(conn: &Connection, message_id: &str, before: usize, after: usize) -> DbResult<MessagePage> {
    let (dialog_id, seq): (String, i64) = conn
        .query_row("SELECT dialog_id, seq FROM messages WHERE id = ?1", params![message_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(err)?;

    let mut older = get_messages(conn, &dialog_id, Some(seq + 1), before + 1)?;
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE dialog_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3"
        ))
        .map_err(err)?;
    let mut newer: Vec<MessageRow> = stmt
        .query_map(params![dialog_id, seq, (after + 1) as i64], message_from_row)
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let has_newer = newer.len() > after;
    newer.truncate(after);
    attach_attachments(conn, &mut newer)?;

    older.messages.extend(newer);
    Ok(MessagePage { messages: older.messages, has_older: older.has_older, has_newer })
}

pub fn get_message_body(conn: &Connection, message_id: &str) -> DbResult<String> {
    let external: Option<String> = conn
        .query_row("SELECT content FROM message_bodies WHERE message_id = ?1", params![message_id], |r| r.get(0))
        .optional()
        .map_err(err)?;
    match external {
        Some(body) => Ok(body),
        None => conn
            .query_row("SELECT content FROM messages WHERE id = ?1", params![message_id], |r| r.get(0))
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
pub fn save_tool_call_extras(conn: &mut Connection, message_id: &str, extras: &[(String, Value)]) -> DbResult<()> {
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
pub fn tool_call_extras(conn: &Connection, message_ids: &[&str]) -> DbResult<HashMap<(String, String), Value>> {
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
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
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
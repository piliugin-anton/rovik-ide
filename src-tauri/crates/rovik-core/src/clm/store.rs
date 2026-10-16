//! What Context Language Models keep in the chat database (schema v5):
//! the chat's context mode, its CLM state, and its context file's
//! revisions (the virtual volume is in memory; these survive a restart).

use super::document::CtxMsg;
use crate::chat_db::store::{err, DbResult};
use rusqlite::{params, Connection, OptionalExtension};

/// Revisions kept per chat in the database. Revisions written before an
/// edit (backups of what an edit removed) and edits themselves are kept
/// first; older plain snapshots go.
const KEEP_SNAPSHOTS: i64 = 32;
const KEEP_TOTAL: i64 = 160;

#[derive(Debug, Default, Clone)]
pub struct State {
    /// The model's accepted context, covering stored messages up to `anchor_id`.
    pub projection: Option<Vec<CtxMsg>>,
    pub anchor_id: Option<String>,
    /// What was last written to the context file, and from what.
    pub rendered: Option<String>,
    pub rendered_messages: Option<Vec<CtxMsg>>,
    pub rendered_anchor_id: Option<String>,
    pub rendered_revision: Option<u64>,
    pub tokens: usize,
    pub budget: usize,
    /// Budget reminders already given (percent tiers).
    pub nudged: Vec<u32>,
    /// The agent's task card as last seen on the volume.
    pub task: Option<String>,
}

fn json<T: serde::de::DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

pub fn context_mode(conn: &Connection, dialog_id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT context_mode FROM dialogs WHERE id = ?1",
        params![dialog_id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(err)
}

pub fn set_context_mode(conn: &Connection, dialog_id: &str, mode: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE dialogs SET context_mode = ?2 WHERE id = ?1",
        params![dialog_id, mode],
    )
    .map_err(err)?;
    Ok(())
}

pub fn load_state(conn: &Connection, dialog_id: &str) -> DbResult<State> {
    conn.query_row(
        "SELECT projection, anchor_id, rendered, rendered_messages, rendered_anchor_id, rendered_revision,
                tokens, budget, nudged, task
         FROM clm_state WHERE dialog_id = ?1",
        params![dialog_id],
        |r| {
            Ok(State {
                projection: json(r.get(0)?),
                anchor_id: r.get(1)?,
                rendered: r.get(2)?,
                rendered_messages: json(r.get(3)?),
                rendered_anchor_id: r.get(4)?,
                rendered_revision: r.get::<_, Option<i64>>(5)?.map(|n| n as u64),
                tokens: r.get::<_, i64>(6)? as usize,
                budget: r.get::<_, i64>(7)? as usize,
                nudged: json(r.get(8)?).unwrap_or_default(),
                task: r.get(9)?,
            })
        },
    )
    .optional()
    .map(Option::unwrap_or_default)
    .map_err(err)
}

pub fn save_state(conn: &Connection, dialog_id: &str, s: &State) -> DbResult<()> {
    let to_json = |v: &Option<Vec<CtxMsg>>| v.as_ref().and_then(|v| serde_json::to_string(v).ok());
    conn.execute(
        "INSERT INTO clm_state (dialog_id, anchor_id, rendered_anchor_id, rendered_revision, tokens, budget,
                                nudged, updated_at, projection, rendered_messages, rendered, task)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (dialog_id) DO UPDATE SET
           anchor_id = excluded.anchor_id, rendered_anchor_id = excluded.rendered_anchor_id,
           rendered_revision = excluded.rendered_revision, tokens = excluded.tokens, budget = excluded.budget,
           nudged = excluded.nudged, updated_at = excluded.updated_at, projection = excluded.projection,
           rendered_messages = excluded.rendered_messages, rendered = excluded.rendered, task = excluded.task",
        params![
            dialog_id,
            s.anchor_id,
            s.rendered_anchor_id,
            s.rendered_revision.map(|n| n as i64),
            s.tokens as i64,
            s.budget as i64,
            serde_json::to_string(&s.nudged).ok(),
            now_ms(),
            to_json(&s.projection),
            to_json(&s.rendered_messages),
            s.rendered,
            s.task,
        ],
    )
    .map_err(err)?;
    Ok(())
}

/// Drops the model's context: the chat is sent as stored again.
pub fn clear_projection(conn: &Connection, dialog_id: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE clm_state SET projection = NULL, anchor_id = NULL, nudged = NULL WHERE dialog_id = ?1",
        params![dialog_id],
    )
    .map_err(err)?;
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub use rovik_protocol::clm::RevisionRow;

pub fn revisions(conn: &Connection, dialog_id: &str) -> DbResult<Vec<RevisionRow>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT number, kind, created_at, tokens FROM clm_revisions WHERE dialog_id = ?1 ORDER BY number",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![dialog_id], |r| {
            Ok(RevisionRow {
                number: r.get::<_, i64>(0)? as u64,
                kind: r.get(1)?,
                created_at: r.get(2)?,
                tokens: r.get::<_, i64>(3)? as usize,
            })
        })
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    Ok(rows)
}

pub fn revision_content(
    conn: &Connection,
    dialog_id: &str,
    number: u64,
) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT content FROM clm_revisions WHERE dialog_id = ?1 AND number = ?2",
        params![dialog_id, number as i64],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

/// The newest revisions with their content (for refilling the volume).
pub fn latest_revisions(
    conn: &Connection,
    dialog_id: &str,
    limit: usize,
) -> DbResult<Vec<(RevisionRow, String)>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT number, kind, created_at, tokens, content FROM clm_revisions
             WHERE dialog_id = ?1 ORDER BY number DESC LIMIT ?2",
        )
        .map_err(err)?;
    let mut rows: Vec<(RevisionRow, String)> = stmt
        .query_map(params![dialog_id, limit as i64], |r| {
            Ok((
                RevisionRow {
                    number: r.get::<_, i64>(0)? as u64,
                    kind: r.get(1)?,
                    created_at: r.get(2)?,
                    tokens: r.get::<_, i64>(3)? as usize,
                },
                r.get(4)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    rows.reverse();
    Ok(rows)
}

/// Revisions whose document has a block for message `id`, newest first.
pub fn revisions_with_message(
    conn: &Connection,
    dialog_id: &str,
    id: &str,
    limit: usize,
) -> DbResult<Vec<(u64, String)>> {
    let needle = format!(
        "% id={}]]%",
        id.replace('\\', "\\\\")
            .replace('_', "\\_")
            .replace('%', "\\%")
    );
    let mut stmt = conn
        .prepare_cached(
            "SELECT number, content FROM clm_revisions WHERE dialog_id = ?1 AND content LIKE ?2 ESCAPE '\\'
             ORDER BY number DESC LIMIT ?3",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![dialog_id, needle, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get(1)?))
        })
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    Ok(rows)
}

pub fn max_revision(conn: &Connection, dialog_id: &str) -> DbResult<Option<u64>> {
    conn.query_row(
        "SELECT MAX(number) FROM clm_revisions WHERE dialog_id = ?1",
        params![dialog_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .map(|n| n.map(|n| n as u64))
    .map_err(err)
}

pub struct NewRevision {
    pub number: u64,
    pub kind: String,
    pub created_at: i64,
    pub tokens: usize,
    pub content: String,
}

pub fn insert_revisions(
    conn: &Connection,
    dialog_id: &str,
    revisions: &[NewRevision],
) -> DbResult<()> {
    if revisions.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO clm_revisions (dialog_id, number, kind, created_at, tokens, content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(err)?;
    for r in revisions {
        stmt.execute(params![
            dialog_id,
            r.number as i64,
            r.kind,
            r.created_at,
            r.tokens as i64,
            r.content
        ])
        .map_err(err)?;
    }
    prune(conn, dialog_id)
}

/// Keeps edits, the snapshot before each edit (what it changed), and the
/// newest snapshots; at most KEEP_TOTAL rows.
fn prune(conn: &Connection, dialog_id: &str) -> DbResult<()> {
    conn.execute(
        "DELETE FROM clm_revisions WHERE dialog_id = ?1 AND kind = 'context'
           AND number NOT IN (SELECT number - 1 FROM clm_revisions WHERE dialog_id = ?1 AND kind != 'context')
           AND number < (SELECT MAX(number) FROM clm_revisions WHERE dialog_id = ?1) - ?2",
        params![dialog_id, KEEP_SNAPSHOTS],
    )
    .map_err(err)?;
    conn.execute(
        "DELETE FROM clm_revisions WHERE dialog_id = ?1 AND number NOT IN
           (SELECT number FROM clm_revisions WHERE dialog_id = ?1 ORDER BY number DESC LIMIT ?2)",
        params![dialog_id, KEEP_TOTAL],
    )
    .map_err(err)?;
    Ok(())
}

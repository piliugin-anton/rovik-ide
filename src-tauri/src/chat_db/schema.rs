//! Database schema and connection settings. The version lives in
//! `PRAGMA user_version`; each step runs in one transaction that bumps it.

use rusqlite::Connection;

const SCHEMA_V1: &str = r#"
CREATE TABLE dialogs (
  id            TEXT PRIMARY KEY,
  title         TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  message_count INTEGER NOT NULL DEFAULT 0,
  preview       TEXT,
  archived      INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX dialogs_recent ON dialogs (archived, updated_at DESC, id DESC);

-- Large values go last: reading a column after a large one walks its overflow pages.
CREATE TABLE messages (
  id            TEXT PRIMARY KEY,
  dialog_id     TEXT NOT NULL REFERENCES dialogs (id) ON DELETE CASCADE,
  seq           INTEGER NOT NULL,
  role          TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool', 'system')),
  body_external INTEGER NOT NULL DEFAULT 0,
  body_bytes    INTEGER NOT NULL DEFAULT 0,
  tool_calls    TEXT,
  tool_call_id  TEXT,
  meta          TEXT,
  error         TEXT,
  failed        INTEGER NOT NULL DEFAULT 0,
  finalized     INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  content       TEXT NOT NULL DEFAULT '',
  UNIQUE (dialog_id, seq)
) STRICT;

CREATE TABLE message_bodies (
  message_id TEXT PRIMARY KEY REFERENCES messages (id) ON DELETE CASCADE,
  content    TEXT NOT NULL
) STRICT;

CREATE TABLE attachments (
  id         TEXT PRIMARY KEY,
  mime       TEXT NOT NULL,
  bytes      INTEGER NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE message_attachments (
  message_id    TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
  position      INTEGER NOT NULL,
  attachment_id TEXT NOT NULL REFERENCES attachments (id),
  PRIMARY KEY (message_id, position)
) STRICT;
CREATE INDEX message_attachments_by_attachment ON message_attachments (attachment_id);

-- Row ids of a chat's search rows, so deleting a chat never scans the index.
CREATE TABLE search_rows (
  rowid      INTEGER PRIMARY KEY,
  dialog_id  TEXT NOT NULL,
  message_id TEXT
) STRICT;
CREATE INDEX search_rows_by_dialog ON search_rows (dialog_id);
CREATE INDEX search_rows_by_message ON search_rows (message_id);

CREATE VIRTUAL TABLE search_index USING fts5 (
  title, body,
  dialog_id UNINDEXED,
  message_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);
CREATE VIRTUAL TABLE title_trigrams USING fts5 (title, dialog_id UNINDEXED, tokenize = 'trigram');
"#;

pub fn configure(conn: &Connection, writer: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    if writer {
        // journal_mode returns a row, so it can't go through pragma_update.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    } else {
        conn.pragma_update(None, "query_only", true)?;
    }
    Ok(())
}

pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
    }
    Ok(())
}

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

/// Provider data for each tool call (e.g. Gemini 3 thought signatures),
/// saved by the backend the moment it parses the call. Keyed by the
/// assistant message *and* the call id: ids can repeat across turns
/// (Gemini uses "function-call-1"), so the id alone is ambiguous.
const SCHEMA_V2: &str = r#"
CREATE TABLE tool_call_extras (
  message_id    TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
  call_id       TEXT NOT NULL,
  extra_content TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (message_id, call_id)
) STRICT;
"#;

/// Chats belong to projects (chat_db/projects.rs). A project's ID lives in
/// the project folder (`.rovik/project.json`), so it survives the folder
/// being renamed or moved; `path` is only where it was last opened. The git
/// columns are for recognising a project whose ID file went missing (later).
/// A chat with no project (no folder open) has `project_id` NULL.
const SCHEMA_V3: &str = r#"
CREATE TABLE projects (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  path            TEXT NOT NULL,
  git_root_commit TEXT,
  git_remote      TEXT,
  created_at      INTEGER NOT NULL,
  last_opened_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX projects_by_path ON projects (path);

ALTER TABLE dialogs ADD COLUMN project_id TEXT REFERENCES projects (id) ON DELETE SET NULL;
DROP INDEX dialogs_recent;
CREATE INDEX dialogs_recent ON dialogs (project_id, archived, updated_at DESC, id DESC);
"#;

/// Recovering projects (chat_db/projects.rs): every folder a project was
/// opened from (to tell a move from a copy), answers to "this looks like a
/// copy / a moved project" so they aren't asked again, and the project's
/// place inside its git repository (subfolders of one repo are different
/// projects with the same root commit).
const SCHEMA_V4: &str = r#"
CREATE TABLE project_paths (
  project_id     TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
  path           TEXT NOT NULL,
  last_opened_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, path)
) STRICT;
CREATE INDEX project_paths_by_path ON project_paths (path);

CREATE TABLE project_decisions (
  path       TEXT NOT NULL,
  project_id TEXT NOT NULL,
  decision   TEXT NOT NULL,
  decided_at INTEGER NOT NULL,
  PRIMARY KEY (path, project_id)
) STRICT;

ALTER TABLE projects ADD COLUMN git_subpath TEXT;
INSERT INTO project_paths (project_id, path, last_opened_at) SELECT id, path, last_opened_at FROM projects;
"#;

/// Context management per chat (src/clm): the mode (NULL until chosen:
/// "trimming" or "clm"), and for Context Language Models the model's
/// accepted context (`projection`, covering stored messages up to
/// `anchor_id`), what was last written to its context file, and the file's
/// revisions, which outlive the in-memory virtual volume.
const SCHEMA_V5: &str = r#"
ALTER TABLE dialogs ADD COLUMN context_mode TEXT;

CREATE TABLE clm_state (
  dialog_id          TEXT PRIMARY KEY REFERENCES dialogs (id) ON DELETE CASCADE,
  anchor_id          TEXT,
  rendered_anchor_id TEXT,
  rendered_revision  INTEGER,
  tokens             INTEGER NOT NULL DEFAULT 0,
  budget             INTEGER NOT NULL DEFAULT 0,
  nudged             TEXT,
  updated_at         INTEGER NOT NULL,
  projection         TEXT,
  rendered_messages  TEXT,
  rendered           TEXT
) STRICT;

CREATE TABLE clm_revisions (
  dialog_id  TEXT NOT NULL REFERENCES dialogs (id) ON DELETE CASCADE,
  number     INTEGER NOT NULL,
  kind       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  tokens     INTEGER NOT NULL,
  content    TEXT NOT NULL,
  PRIMARY KEY (dialog_id, number)
) STRICT;
"#;

/// The agent's task card in CLM mode (`@context/TASK.md`), which outlives
/// the in-memory virtual volume like the context file's revisions do.
const SCHEMA_V6: &str = r#"
ALTER TABLE clm_state ADD COLUMN task TEXT;
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
    if version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V2)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
    }
    if version < 3 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V3)?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
    }
    if version < 4 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V4)?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
    }
    if version < 5 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V5)?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
    }
    if version < 6 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V6)?;
        tx.pragma_update(None, "user_version", 6)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database from before projects (version 2, with a chat in it)
    /// upgrades in place: the chat stays, with no project.
    #[test]
    fn upgrades_a_version_2_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, true).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO dialogs (id, title, created_at, updated_at) VALUES ('old', 'Old chat', 1, 1)",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 6);
        let project: Option<String> =
            conn.query_row("SELECT project_id FROM dialogs WHERE id = 'old'", [], |r| r.get(0)).unwrap();
        assert_eq!(project, None);
        // The list query uses the new index.
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM dialogs WHERE project_id IS 'p' AND archived = 0 ORDER BY updated_at DESC, id DESC",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("dialogs_recent"), "{plan}");
        // And it runs only once.
        migrate(&mut conn).unwrap();
    }
}

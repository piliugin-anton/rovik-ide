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

/// The chat input's unsent text, per chat (lib/composerDraft.ts). The panel
/// saves it debounced while typing so switching sidebar tabs or restarting
/// the app never loses what was typed. Deleted with the chat; empty text
/// deletes the row (the store does that, not a CHECK, so "no draft" and
/// "empty draft" read the same).
const SCHEMA_V7: &str = r#"
CREATE TABLE composer_drafts (
  dialog_id  TEXT PRIMARY KEY REFERENCES dialogs (id) ON DELETE CASCADE,
  content    TEXT NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;
"#;

/// Subagent chats (agent_run/subagent.rs): a child chat belongs to the chat
/// whose run started it (`parent_id`), answers one of its tool calls
/// (`parent_call_id`), and is of one subagent kind ("scout", …). Deleting the
/// parent deletes its children. Children are stored with `archived = 2`, so
/// older app versions, which list `archived = 0`, don't show them either.
const SCHEMA_V8: &str = r#"
ALTER TABLE dialogs ADD COLUMN parent_id TEXT REFERENCES dialogs (id) ON DELETE CASCADE;
ALTER TABLE dialogs ADD COLUMN parent_call_id TEXT;
ALTER TABLE dialogs ADD COLUMN kind TEXT;
CREATE INDEX dialogs_by_parent ON dialogs (parent_id) WHERE parent_id IS NOT NULL;
"#;

/// The model a chat uses (provider and model id, as in the settings) and
/// its reasoning effort ("default", "low", …), so reopening an old chat
/// continues with them. NULL until the chat's first run or model change.
const SCHEMA_V9: &str = r#"
ALTER TABLE dialogs ADD COLUMN model_provider TEXT;
ALTER TABLE dialogs ADD COLUMN model_id TEXT;
ALTER TABLE dialogs ADD COLUMN effort TEXT;
"#;

/// Chats that move between a computer and a server (cloud-agent-dev-plan.md,
/// 6.3 and 6.8).
///
/// - `dialogs.location`: who owns the chat. NULL: this database. On a
///   computer, `moving:<host>` while it moves there and `host:<id>` once
///   its host owns it, and its rows here are a mirror of the host's; on a
///   daemon, `released` once its computer took it back.
/// - `rev`: a revision from `sync_clock`, one counter per database, raised
///   by every write to a chat's rows and kept by each row from its last
///   change, so a mirror can ask for what changed since the revision it has.
///   Triggers keep it, also for code that writes with plain SQL; a write to
///   a message's body, attachments or tool-call extras raises the
///   message's. Rows from before have revision 1, without being rewritten.
/// - `mirror_state`: each mirrored chat's host, the owner's revision its
///   rows are pulled up to, the result imported last with its number, and
///   a landing its host wasn't told of yet (6.5).
/// - `cloud_moves`: a move in progress, there or back, and the step it
///   reached, so the next start finishes or undoes one cut short (K25, K51).
const SCHEMA_V10: &str = r#"
ALTER TABLE dialogs ADD COLUMN location TEXT;

CREATE TABLE sync_clock (
  id  INTEGER PRIMARY KEY CHECK (id = 1),
  rev INTEGER NOT NULL
) STRICT;
INSERT INTO sync_clock (id, rev) VALUES (1, 1);

ALTER TABLE dialogs ADD COLUMN rev INTEGER NOT NULL DEFAULT 1;
ALTER TABLE messages ADD COLUMN rev INTEGER NOT NULL DEFAULT 1;
ALTER TABLE clm_state ADD COLUMN rev INTEGER NOT NULL DEFAULT 1;
ALTER TABLE clm_revisions ADD COLUMN rev INTEGER NOT NULL DEFAULT 1;
CREATE INDEX messages_by_rev ON messages (dialog_id, rev, id);

CREATE TRIGGER dialogs_rev_insert AFTER INSERT ON dialogs BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE dialogs SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.id;
END;
CREATE TRIGGER dialogs_rev_update AFTER UPDATE ON dialogs WHEN NEW.rev IS OLD.rev BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE dialogs SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.id;
END;

CREATE TRIGGER messages_rev_insert AFTER INSERT ON messages BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.id;
END;
CREATE TRIGGER messages_rev_update AFTER UPDATE ON messages WHEN NEW.rev IS OLD.rev BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.id;
END;

CREATE TRIGGER clm_state_rev_insert AFTER INSERT ON clm_state BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE clm_state SET rev = (SELECT rev FROM sync_clock) WHERE dialog_id = NEW.dialog_id;
END;
CREATE TRIGGER clm_state_rev_update AFTER UPDATE ON clm_state WHEN NEW.rev IS OLD.rev BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE clm_state SET rev = (SELECT rev FROM sync_clock) WHERE dialog_id = NEW.dialog_id;
END;

CREATE TRIGGER clm_revisions_rev_insert AFTER INSERT ON clm_revisions BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE clm_revisions SET rev = (SELECT rev FROM sync_clock)
   WHERE dialog_id = NEW.dialog_id AND number = NEW.number;
END;
CREATE TRIGGER clm_revisions_rev_update AFTER UPDATE ON clm_revisions WHEN NEW.rev IS OLD.rev BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE clm_revisions SET rev = (SELECT rev FROM sync_clock)
   WHERE dialog_id = NEW.dialog_id AND number = NEW.number;
END;

CREATE TRIGGER message_bodies_rev_insert AFTER INSERT ON message_bodies BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER message_bodies_rev_update AFTER UPDATE ON message_bodies BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER message_bodies_rev_delete AFTER DELETE ON message_bodies BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = OLD.message_id;
END;

CREATE TRIGGER message_attachments_rev_insert AFTER INSERT ON message_attachments BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER message_attachments_rev_update AFTER UPDATE ON message_attachments BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER message_attachments_rev_delete AFTER DELETE ON message_attachments BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = OLD.message_id;
END;

CREATE TRIGGER tool_call_extras_rev_insert AFTER INSERT ON tool_call_extras BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER tool_call_extras_rev_update AFTER UPDATE ON tool_call_extras BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = NEW.message_id;
END;
CREATE TRIGGER tool_call_extras_rev_delete AFTER DELETE ON tool_call_extras BEGIN
  UPDATE sync_clock SET rev = rev + 1;
  UPDATE messages SET rev = (SELECT rev FROM sync_clock) WHERE id = OLD.message_id;
END;

CREATE TABLE mirror_state (
  dialog_id          TEXT PRIMARY KEY REFERENCES dialogs (id) ON DELETE CASCADE,
  host               TEXT NOT NULL,
  rev                INTEGER NOT NULL DEFAULT 0,
  imported_result    TEXT,
  imported_seq       INTEGER,
  unreported_landing TEXT
) STRICT;

CREATE TABLE cloud_moves (
  dialog_id  TEXT PRIMARY KEY,
  host       TEXT NOT NULL,
  move_id    TEXT NOT NULL,
  direction  TEXT NOT NULL CHECK (direction IN ('there', 'back')),
  step       TEXT NOT NULL,
  started_at INTEGER NOT NULL
) STRICT;
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
    if version < 7 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V7)?;
        tx.pragma_update(None, "user_version", 7)?;
        tx.commit()?;
    }
    if version < 8 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V8)?;
        tx.pragma_update(None, "user_version", 8)?;
        tx.commit()?;
    }
    if version < 9 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V9)?;
        tx.pragma_update(None, "user_version", 9)?;
        tx.commit()?;
    }
    if version < 10 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V10)?;
        tx.pragma_update(None, "user_version", 10)?;
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

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10);
        let project: Option<String> = conn
            .query_row("SELECT project_id FROM dialogs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
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

    /// A version 7 database gets the subagent columns; its chats are no
    /// one's children, and a v7-style list query never sees a child chat.
    #[test]
    fn upgrades_a_version_7_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, true).unwrap();
        for step in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7,
        ] {
            conn.execute_batch(step).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute(
            "INSERT INTO dialogs (id, title, created_at, updated_at) VALUES ('old', 'Old chat', 1, 1)",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let parent: Option<String> = conn
            .query_row("SELECT parent_id FROM dialogs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(parent, None);
        crate::chat_db::store::create_child_dialog(
            &mut conn,
            &crate::chat_db::store::NewChildDialog {
                id: "kid".into(),
                parent_id: "old".into(),
                parent_call_id: "c1".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: 2,
            },
        )
        .unwrap();
        let listed: Vec<String> = conn
            .prepare("SELECT id FROM dialogs WHERE archived = 0")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(listed, ["old"], "an older app's list leaves the child out");
    }

    /// A version 9 database gets revisions and locations (6.3): its rows
    /// are at revision 1 without being rewritten, each write after raises
    /// the clock and the row takes it, its chats are owned here, and a
    /// mirror's query for a chat's changed messages uses the new index.
    #[test]
    fn upgrades_a_version_9_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn, true).unwrap();
        for step in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
            SCHEMA_V9,
        ] {
            conn.execute_batch(step).unwrap();
        }
        conn.pragma_update(None, "user_version", 9).unwrap();
        conn.execute_batch(
            "INSERT INTO dialogs (id, title, created_at, updated_at) VALUES ('old', 'Old chat', 1, 1);
             INSERT INTO messages (id, dialog_id, seq, role, created_at, content)
               VALUES ('m1', 'old', 1024, 'user', 1, 'Hi'), ('m2', 'old', 2048, 'tool', 1, 'preview');
             INSERT INTO message_bodies (message_id, content) VALUES ('m2', 'the whole result');
             INSERT INTO clm_state (dialog_id, updated_at) VALUES ('old', 1);
             INSERT INTO clm_revisions (dialog_id, number, kind, created_at, tokens, content)
               VALUES ('old', 1, 'context', 1, 3, '# Context');",
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let one = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(one("PRAGMA user_version"), 10);
        assert_eq!(one("SELECT rev FROM sync_clock"), 1);
        for table in ["dialogs", "messages", "clm_state", "clm_revisions"] {
            let revs = one(&format!("SELECT COUNT(*) FROM {table} WHERE rev = 1"));
            let rows = one(&format!("SELECT COUNT(*) FROM {table}"));
            assert_eq!(revs, rows, "{table}");
        }
        let location: Option<String> = conn
            .query_row("SELECT location FROM dialogs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(location, None, "owned here");

        // A write raises the clock, and the row it changed takes it.
        conn.execute("UPDATE messages SET content = 'Hey' WHERE id = 'm1'", [])
            .unwrap();
        let rev = |conn: &Connection, id: &str| -> i64 {
            conn.query_row("SELECT rev FROM messages WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rev(&conn, "m1"), 2);
        assert_eq!(rev(&conn, "m2"), 1);
        // A body written raises its message's.
        conn.execute(
            "UPDATE message_bodies SET content = 'another result' WHERE message_id = 'm2'",
            [],
        )
        .unwrap();
        assert_eq!(rev(&conn, "m2"), 3);

        // The client's own tables.
        conn.execute(
            "INSERT INTO mirror_state (dialog_id, host) VALUES ('old', 'server')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cloud_moves (dialog_id, host, move_id, direction, step, started_at)
             VALUES ('old', 'server', 'mv', 'there', 'locked', 1)",
            [],
        )
        .unwrap();
        let sideways = conn.execute(
            "INSERT INTO cloud_moves (dialog_id, host, move_id, direction, step, started_at)
             VALUES ('other', 'server', 'mv', 'sideways', 'locked', 1)",
            [],
        );
        assert!(sideways.is_err(), "a move goes there or back");

        let plan: Vec<String> = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                crate::chat_db::sync::CHANGED_MESSAGES
            ))
            .unwrap()
            .query_map(
                rusqlite::params!["old", 1, i64::MAX, i64::MIN, "", 100],
                |r| r.get(3),
            )
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| step.contains("messages_by_rev")),
            "{plan:?}"
        );
        // And it runs once.
        migrate(&mut conn).unwrap();
    }
}

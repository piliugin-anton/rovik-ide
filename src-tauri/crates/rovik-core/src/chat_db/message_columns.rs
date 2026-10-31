//! A message row, read the same way by every reader: the app's (`store`),
//! which parses the JSON columns, and a mirror's (`sync`), which keeps the
//! owner's bytes as they are.

use super::store::MessageRow;
use rovik_protocol::sync::StoredMessage;
use rusqlite::Row;
use serde::de::DeserializeOwned;

/// The messages table's own columns, in the order every read selects them.
/// `rev` isn't one: a mirror's read selects it after these.
const COLUMNS: [&str; 14] = [
    "id",
    "dialog_id",
    "seq",
    "role",
    "body_external",
    "body_bytes",
    "tool_calls",
    "tool_call_id",
    "meta",
    "error",
    "failed",
    "finalized",
    "created_at",
    "content",
];

/// `"id, dialog_id, …"`; with an alias (`"m.id, m.dialog_id, …"`) for a join.
pub(super) fn columns(alias: Option<&str>) -> String {
    let prefix = alias.map(|a| format!("{a}.")).unwrap_or_default();
    COLUMNS.map(|c| format!("{prefix}{c}")).join(", ")
}

/// A message as stored: its JSON columns as their text.
pub(super) struct RawMessage {
    id: String,
    dialog_id: String,
    seq: i64,
    role: String,
    body_external: bool,
    body_bytes: i64,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    meta: Option<String>,
    error: Option<String>,
    failed: bool,
    finalized: bool,
    created_at: i64,
    content: String,
}

impl RawMessage {
    /// The columns it reads: a caller's own columns come after.
    pub(super) const COUNT: usize = COLUMNS.len();

    /// The message in a row whose first columns are `columns`.
    pub(super) fn read(r: &Row) -> rusqlite::Result<RawMessage> {
        Ok(RawMessage {
            id: r.get(0)?,
            dialog_id: r.get(1)?,
            seq: r.get(2)?,
            role: r.get(3)?,
            body_external: r.get::<_, i64>(4)? != 0,
            body_bytes: r.get(5)?,
            tool_calls: r.get(6)?,
            tool_call_id: r.get(7)?,
            meta: r.get(8)?,
            error: r.get(9)?,
            failed: r.get::<_, i64>(10)? != 0,
            finalized: r.get::<_, i64>(11)? != 0,
            created_at: r.get(12)?,
            content: r.get(13)?,
        })
    }

    /// For the app: JSON parsed; a malformed value is logged and left out,
    /// and the message still loads. Attachments are the caller's to add.
    pub(super) fn into_row(self) -> MessageRow {
        let tool_calls = parsed(&self.id, "tool_calls", self.tool_calls.as_deref());
        let meta = parsed(&self.id, "meta", self.meta.as_deref());
        MessageRow {
            id: self.id,
            dialog_id: self.dialog_id,
            seq: self.seq,
            role: self.role,
            content: self.content,
            body_external: self.body_external,
            body_bytes: self.body_bytes,
            tool_calls,
            tool_call_id: self.tool_call_id,
            meta,
            error: self.error,
            failed: self.failed,
            finalized: self.finalized,
            attachments: Vec::new(),
            created_at: self.created_at,
        }
    }

    /// For a mirror: the bytes as they are, with the external `body`
    /// (`message_bodies`). Attachments and extras are the caller's to add.
    pub(super) fn into_stored(self, body: Option<String>) -> StoredMessage {
        StoredMessage {
            id: self.id,
            dialog_id: self.dialog_id,
            seq: self.seq,
            role: self.role,
            body_external: self.body_external,
            body_bytes: self.body_bytes,
            tool_calls: self.tool_calls,
            tool_call_id: self.tool_call_id,
            meta: self.meta,
            error: self.error,
            failed: self.failed,
            finalized: self.finalized,
            created_at: self.created_at,
            content: self.content,
            body,
            attachments: Vec::new(),
            extras: Vec::new(),
        }
    }
}

/// Message `id`'s JSON `column`, parsed: none when it's NULL, or when it
/// doesn't parse, which is logged. The log says where it failed and never
/// what the column holds: a parse error can quote it.
fn parsed<T: DeserializeOwned>(id: &str, column: &str, text: Option<&str>) -> Option<T> {
    match serde_json::from_str(text?) {
        Ok(value) => Some(value),
        Err(e) => {
            eprintln!(
                "chat_db: message {id} loads without its {column}: the stored JSON doesn't parse \
                 (line {}, column {})",
                e.line(),
                e.column()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_db::schema;
    use rusqlite::Connection;

    #[test]
    fn every_column_of_the_messages_table_is_read() {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::configure(&conn, true).unwrap();
        schema::migrate(&mut conn).unwrap();
        // `rev` aside: the mirror's read selects it after these.
        let mut table: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('messages') WHERE name != 'rev'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // As sets: a read may select them in another order than the table's.
        let mut read = COLUMNS.map(String::from).to_vec();
        table.sort();
        read.sort();
        assert_eq!(read, table);
    }
}

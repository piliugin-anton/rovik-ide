//! What a computer keeps about its chats on hosts (6.3, 6.5):
//! `mirror_state`, one row per chat a host owns (a lead's, its subagents'
//! chats coming with it). It holds the owner's revision the chat's rows are
//! pulled up to, the chat's result imported last with its number, and a
//! landing its host wasn't told of yet, so none of these is lost when the
//! app restarts.

use crate::chat_db::location::{self, Location};
use crate::chat_db::store::{err, DbResult};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MirrorState {
    pub host: String,
    /// The owner's revision the mirror's rows are pulled up to (6.3).
    pub rev: i64,
    /// The chat's result imported last, and its number (6.5).
    pub imported_result: Option<String>,
    pub imported_seq: Option<u64>,
    /// A landing its host wasn't told of yet, as the client keeps it
    /// (`overlay::import::Imported`, as JSON).
    pub unreported_landing: Option<String>,
}

/// The chat is on `host` from now on, with its subagents' chats: its
/// location says so, and a lead's chat gets its row here. Placing it again
/// on the same host keeps what the row holds.
pub fn place(conn: &mut Connection, dialog_id: &str, host: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    location::set_in(&tx, dialog_id, &Location::Host(host.to_string()))?;
    tx.execute(
        "INSERT INTO mirror_state (dialog_id, host)
         SELECT id, ?2 FROM dialogs WHERE id = ?1 AND parent_id IS NULL
         ON CONFLICT (dialog_id) DO UPDATE SET host = excluded.host",
        params![dialog_id, host],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

pub fn state(conn: &Connection, dialog_id: &str) -> DbResult<Option<MirrorState>> {
    conn.query_row(
        "SELECT host, rev, imported_result, imported_seq, unreported_landing
         FROM mirror_state WHERE dialog_id = ?1",
        params![dialog_id],
        |r| {
            Ok(MirrorState {
                host: r.get(0)?,
                rev: r.get(1)?,
                imported_result: r.get(2)?,
                imported_seq: r.get::<_, Option<i64>>(3)?.map(|n| n.max(0) as u64),
                unreported_landing: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(err)
}

/// The chat's result `commit`, numbered `seq`, is the one imported last:
/// one numbered no higher is passed over from now on (6.5).
pub fn set_imported(conn: &Connection, dialog_id: &str, commit: &str, seq: u64) -> DbResult<()> {
    conn.execute(
        "UPDATE mirror_state SET imported_result = ?2, imported_seq = ?3 WHERE dialog_id = ?1",
        params![dialog_id, commit, seq as i64],
    )
    .map(drop)
    .map_err(err)
}

/// A landing its host is to be told of after its next connection, in
/// place of an older one: a result holds every change since the last
/// landing the host was told of. None: told.
pub fn set_unreported(conn: &Connection, dialog_id: &str, landing: Option<&str>) -> DbResult<()> {
    conn.execute(
        "UPDATE mirror_state SET unreported_landing = ?2 WHERE dialog_id = ?1",
        params![dialog_id, landing],
    )
    .map(drop)
    .map_err(err)
}

/// The landings `host` wasn't told of, by chat.
pub fn unreported(conn: &Connection, host: &str) -> DbResult<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT dialog_id, unreported_landing FROM mirror_state
             WHERE host = ?1 AND unreported_landing IS NOT NULL",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![host], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

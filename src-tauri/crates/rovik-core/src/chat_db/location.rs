//! Who owns a chat (6.3, 6.8): `dialogs.location`. A chat is owned where
//! its run goes and its rows are written. A computer's chat that moved to a
//! host keeps its rows here as a mirror of the host's, which only the
//! mirror writes (K21); a daemon's chat that its computer took back keeps
//! its rows until that computer has them all. A subagent's chat goes with
//! its lead's.

use crate::chat_db::store::{self, err, DbResult};
use rusqlite::{params, Connection, OptionalExtension};

const MOVING: &str = "moving:";
const HOST: &str = "host:";
const RELEASED: &str = "released";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// This database owns the chat.
    Here,
    /// Moving to this host: nothing may change it until the move is over,
    /// and a run going here pauses (K25).
    Moving(String),
    /// Owned by this host: its rows here are a mirror.
    Host(String),
    /// On a daemon: given back to its computer, whose last pull of its
    /// changes is still to come.
    Released,
}

impl Location {
    /// The column's value as stored. Anything unknown, from a newer app,
    /// isn't taken as "here".
    pub fn parse(column: Option<&str>) -> Location {
        match column {
            None => Location::Here,
            Some(text) => {
                if let Some(host) = text.strip_prefix(MOVING) {
                    Location::Moving(host.to_string())
                } else if let Some(host) = text.strip_prefix(HOST) {
                    Location::Host(host.to_string())
                } else if text == RELEASED {
                    Location::Released
                } else {
                    Location::Host(text.to_string())
                }
            }
        }
    }

    /// The value to store.
    pub fn column(&self) -> Option<String> {
        match self {
            Location::Here => None,
            Location::Moving(host) => Some(format!("{MOVING}{host}")),
            Location::Host(host) => Some(format!("{HOST}{host}")),
            Location::Released => Some(RELEASED.to_string()),
        }
    }

    /// The host a computer's chat is on or moving to.
    pub fn host(&self) -> Option<&str> {
        match self {
            Location::Moving(host) | Location::Host(host) => Some(host),
            Location::Here | Location::Released => None,
        }
    }
}

/// The chat's location; `Here` for a chat this database doesn't have.
pub fn of_chat(conn: &Connection, dialog_id: &str) -> DbResult<Location> {
    let column: Option<Option<String>> = conn
        .query_row(
            "SELECT location FROM dialogs WHERE id = ?1",
            params![dialog_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    Ok(Location::parse(column.flatten().as_deref()))
}

/// The location of the chat a message is in.
pub fn of_message(conn: &Connection, message_id: &str) -> DbResult<Location> {
    let column: Option<Option<String>> = conn
        .query_row(
            "SELECT d.location FROM messages m JOIN dialogs d ON d.id = m.dialog_id WHERE m.id = ?1",
            params![message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    Ok(Location::parse(column.flatten().as_deref()))
}

/// Puts the chat and its subagents' chats at `location`, in one go.
pub fn set(conn: &mut Connection, dialog_id: &str, location: &Location) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    set_in(&tx, dialog_id, location)?;
    tx.commit().map_err(err)
}

/// `set`, inside a transaction of the caller's.
pub fn set_in(conn: &Connection, dialog_id: &str, location: &Location) -> DbResult<()> {
    let column = location.column();
    let mut chats = store::descendants(conn, dialog_id)?;
    chats.push(dialog_id.to_string());
    for chat in chats {
        conn.execute(
            "UPDATE dialogs SET location = ?2 WHERE id = ?1",
            params![chat, column],
        )
        .map_err(err)?;
    }
    Ok(())
}

/// The chats a host owns or that are moving to one, subagents' included,
/// with where they are.
pub fn away(conn: &Connection) -> DbResult<Vec<(String, Location)>> {
    let mut stmt = conn
        .prepare_cached("SELECT id, location FROM dialogs WHERE location IS NOT NULL")
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(err)?;
    rows.map(|row| row.map(|(id, column)| (id, Location::parse(column.as_deref()))))
        .collect::<Result<_, _>>()
        .map_err(err)
}

/// How many of the user's chats (subagents' aside) are on `host` or moving
/// to it: the hosts page asks about them before the host goes.
pub fn chats_on(conn: &Connection, host: &str) -> DbResult<usize> {
    let on = Location::Host(host.to_string()).column();
    let moving = Location::Moving(host.to_string()).column();
    conn.query_row(
        "SELECT COUNT(*) FROM dialogs WHERE parent_id IS NULL AND location IN (?1, ?2, ?3)",
        params![on, moving, host],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
    .map_err(err)
}

/// Whether any of the project's chats is on a host or moving to one (K38).
pub fn project_has_chats_away(conn: &Connection, project_id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT location FROM dialogs WHERE project_id = ?1 AND location IS NOT NULL LIMIT 1",
        params![project_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map(|column| column.and_then(|c| Location::parse(Some(&c)).host().map(str::to_string)))
    .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_read_back_as_written() {
        for location in [
            Location::Here,
            Location::Moving("server".into()),
            Location::Host("server".into()),
            Location::Released,
        ] {
            let column = location.column();
            assert_eq!(Location::parse(column.as_deref()), location);
        }
        assert_eq!(Location::Here.column(), None);
        assert_eq!(
            Location::Host("a:b".into()).column().as_deref(),
            Some("host:a:b")
        );
        // Something a newer app wrote is never taken as this database's.
        assert_ne!(Location::parse(Some("elsewhere")), Location::Here);
    }
}

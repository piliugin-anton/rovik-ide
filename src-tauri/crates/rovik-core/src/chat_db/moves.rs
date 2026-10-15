//! A chat's move while it goes (6.8, K25, K51): its `cloud_moves` row, the
//! step the move reached, and the transactions that change where the chat
//! is along with it. The lock puts the chat and its subagents' chats at
//! `moving:<host>`, which nothing local changes; the flip puts them on the
//! host with the mirror's position, a rollback back here. A move cut short
//! is finished or undone from its row once its host answers. A cancelled
//! move whose host may have the chat keeps its row, step `cancelled`, until
//! the release it queued reaches that host.

use super::location::{self, Location};
use super::store::{err, now_ms, DbResult};
use rusqlite::{params, Connection, OptionalExtension};

/// The ways a chat moves.
pub const THERE: &str = "there";
pub const BACK: &str = "back";

/// The steps a move to a host reaches, in order. A crash before
/// `IMPORTING` sent the host nothing.
pub const LOCKED: &str = "locked";
pub const PAUSED: &str = "paused";
pub const PUSHED: &str = "pushed";
pub const UPLOADED: &str = "uploaded";
pub const IMPORTING: &str = "importing";
/// Cancelled, with a release queued for the host, which may have the chat.
pub const CANCELLED: &str = "cancelled";

/// A chat's move while it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudMove {
    pub dialog_id: String,
    pub host: String,
    pub move_id: String,
    pub direction: String,
    pub step: String,
    pub started_at: i64,
}

/// Locks the chat for its move to `host`, with `move_id`: it and its
/// subagents' chats are `moving` from now on. Refused for a chat that isn't
/// here, or whose earlier move isn't over.
pub fn lock(conn: &mut Connection, dialog_id: &str, host: &str, move_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    match location::of_chat(&tx, dialog_id)? {
        Location::Here => {}
        Location::Moving(to) => return Err(format!("This chat is moving to {to} already.")),
        Location::Host(on) => return Err(format!("This chat is on {on}.")),
        Location::Released => return Err("This chat went back to its computer.".into()),
    }
    if of_chat(&tx, dialog_id)?.is_some() {
        return Err("This chat's last move isn't over: cancel it first.".into());
    }
    location::set_in(&tx, dialog_id, &Location::Moving(host.to_string()))?;
    tx.execute(
        "INSERT INTO cloud_moves (dialog_id, host, move_id, direction, step, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![dialog_id, host, move_id, THERE, LOCKED, now_ms()],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

/// The step the chat's move reached.
pub fn step(conn: &Connection, dialog_id: &str, step: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE cloud_moves SET step = ?2 WHERE dialog_id = ?1",
        params![dialog_id, step],
    )
    .map(drop)
    .map_err(err)
}

/// The chat's move, while it goes.
pub fn of_chat(conn: &Connection, dialog_id: &str) -> DbResult<Option<CloudMove>> {
    conn.query_row(
        "SELECT dialog_id, host, move_id, direction, step, started_at FROM cloud_moves
         WHERE dialog_id = ?1",
        params![dialog_id],
        from_row,
    )
    .optional()
    .map_err(err)
}

/// The moves to or from `host` that go.
pub fn with_host(conn: &Connection, host: &str) -> DbResult<Vec<CloudMove>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT dialog_id, host, move_id, direction, step, started_at FROM cloud_moves
             WHERE host = ?1 ORDER BY started_at",
        )
        .map_err(err)?;
    let rows = stmt.query_map(params![host], from_row).map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

/// The flip, the move's last step: the chat and its subagents' chats are on
/// `host`, its mirror at the import's revision `rev`, and its move over.
pub fn flip(conn: &mut Connection, dialog_id: &str, host: &str, rev: i64) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    location::set_in(&tx, dialog_id, &Location::Host(host.to_string()))?;
    tx.execute(
        "INSERT INTO mirror_state (dialog_id, host, rev) VALUES (?1, ?2, ?3)
         ON CONFLICT (dialog_id) DO UPDATE SET host = ?2, rev = ?3, imported_result = NULL,
           imported_seq = NULL, unreported_landing = NULL",
        params![dialog_id, host, rev],
    )
    .map_err(err)?;
    tx.execute(
        "DELETE FROM cloud_moves WHERE dialog_id = ?1",
        params![dialog_id],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

/// The move undone: the chat is here again, and its move over.
pub fn roll_back(conn: &mut Connection, dialog_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    location::set_in(&tx, dialog_id, &Location::Here)?;
    tx.execute(
        "DELETE FROM cloud_moves WHERE dialog_id = ?1",
        params![dialog_id],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

/// The move cancelled where its host may have the chat: it's here again,
/// and its row stays, `cancelled`, until the release reaches the host.
pub fn cancel(conn: &mut Connection, dialog_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    location::set_in(&tx, dialog_id, &Location::Here)?;
    step(&tx, dialog_id, CANCELLED)?;
    tx.commit().map_err(err)
}

/// The move's row goes: what it queued reached its host.
pub fn forget(conn: &Connection, dialog_id: &str) -> DbResult<()> {
    conn.execute(
        "DELETE FROM cloud_moves WHERE dialog_id = ?1",
        params![dialog_id],
    )
    .map(drop)
    .map_err(err)
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<CloudMove> {
    Ok(CloudMove {
        dialog_id: r.get(0)?,
        host: r.get(1)?,
        move_id: r.get(2)?,
        direction: r.get(3)?,
        step: r.get(4)?,
        started_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_db::store::{self, NewChildDialog, NewDialog};
    use crate::chat_db::{mirror, schema};

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::configure(&conn, true).unwrap();
        schema::migrate(&mut conn).unwrap();
        let dialog = NewDialog {
            id: "chat".into(),
            title: "Chat".into(),
            created_at: 1,
            updated_at: 1,
            project_id: None,
        };
        store::create_dialog(&mut conn, &dialog).unwrap();
        let kid = NewChildDialog {
            id: "kid".into(),
            parent_id: "chat".into(),
            parent_call_id: "c1".into(),
            kind: "scout".into(),
            title: "Scout".into(),
            created_at: 2,
        };
        store::create_child_dialog(&mut conn, &kid).unwrap();
        conn
    }

    fn at(conn: &Connection) -> (Location, Location) {
        let of = |chat| location::of_chat(conn, chat).unwrap();
        (of("chat"), of("kid"))
    }

    #[test]
    fn a_move_locks_the_chat_then_flips_it_to_its_host() {
        let mut conn = db();
        lock(&mut conn, "chat", "server", "m1").unwrap();
        let moving = Location::Moving("server".into());
        assert_eq!(at(&conn), (moving.clone(), moving));
        let fail = lock(&mut conn, "chat", "server", "m2").unwrap_err();
        assert!(fail.contains("moving to server"), "{fail}");
        step(&conn, "chat", IMPORTING).unwrap();
        let row = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!(
            (row.host.as_str(), row.move_id.as_str(), row.step.as_str()),
            ("server", "m1", IMPORTING)
        );
        assert_eq!(with_host(&conn, "server").unwrap(), [row]);

        flip(&mut conn, "chat", "server", 42).unwrap();
        let on = Location::Host("server".into());
        assert_eq!(at(&conn), (on.clone(), on));
        assert_eq!(mirror::state(&conn, "chat").unwrap().unwrap().rev, 42);
        assert!(of_chat(&conn, "chat").unwrap().is_none());
    }

    #[test]
    fn a_move_rolled_back_or_cancelled_leaves_the_chat_here() {
        let mut conn = db();
        lock(&mut conn, "chat", "server", "m1").unwrap();
        roll_back(&mut conn, "chat").unwrap();
        assert_eq!(at(&conn), (Location::Here, Location::Here));
        assert!(of_chat(&conn, "chat").unwrap().is_none());

        lock(&mut conn, "chat", "server", "m2").unwrap();
        cancel(&mut conn, "chat").unwrap();
        assert_eq!(at(&conn), (Location::Here, Location::Here));
        let queued = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!(
            (queued.step.as_str(), queued.move_id.as_str()),
            (CANCELLED, "m2")
        );
        // Not again until the release reached the host.
        assert!(lock(&mut conn, "chat", "server", "m3").is_err());
        forget(&conn, "chat").unwrap();
        lock(&mut conn, "chat", "server", "m3").unwrap();
    }
}

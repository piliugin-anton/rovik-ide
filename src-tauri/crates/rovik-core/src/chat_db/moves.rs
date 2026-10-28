//! A chat's move while it goes (6.8, K25, K51): its `cloud_moves` row, the
//! step the move reached, and the transactions that change where the chat
//! is along with it. The lock puts the chat and its subagents' chats at
//! `moving:<host>`, which nothing local changes; the flip puts them on the
//! host with the mirror's position, a rollback back here. A move cut short
//! is finished or undone from its row once its host answers. A cancelled
//! move whose host may have the chat keeps its row, step `cancelled`, until
//! the release it queued reaches that host.
//!
//! A move back leaves the chat on its host until its flip, which puts it
//! and its subagents' chats here and drops the mirror's row. One cut short
//! before the host released the chat is dropped, the chat still there; one
//! after is finished. One cut short while the release was asked, its
//! answer not heard, is settled by what the host says (`ChatOwner`). Its
//! row stays after the flip, step `forgetting`, until `ChatForget` reaches
//! the host.
//!
//! A remote chat deleted here leaves a tombstone, a move back's row at
//! step `deleted`, until its host deletes the chat too (`ChatDelete`).

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

/// The steps a move back reaches after `LOCKED` and `PAUSED`, in order:
/// its last result is in the chat's store here (`FETCHED`) before the host
/// releases the chat, after which the move can only be finished.
pub const FETCHED: &str = "fetched";
/// The release asked of the host, its answer not heard yet: the host may
/// have let the chat go, so the row stays until the host says.
pub const RELEASING: &str = "releasing";
pub const RELEASED: &str = "released";
/// The chat is here again, and its host is yet to forget it.
pub const FORGETTING: &str = "forgetting";
/// The chat was deleted here, and its host is yet to delete it.
pub const DELETED: &str = "deleted";

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
/// here, or whose earlier move isn't over. A move back whose host is yet to
/// forget the chat gives way: an import clears what the chat left there
/// (K36), and another host keeps it until its own cleanup.
pub fn lock(conn: &mut Connection, dialog_id: &str, host: &str, move_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    match location::of_chat(&tx, dialog_id)? {
        Location::Here => {}
        Location::Moving(to) => return Err(format!("This chat is moving to {to} already.")),
        Location::Host(on) => return Err(format!("This chat is on {on}.")),
        Location::Released => return Err("This chat went back to its computer.".into()),
    }
    match of_chat(&tx, dialog_id)? {
        Some(going) if going.step == FORGETTING => forget(&tx, dialog_id)?,
        Some(going) => return Err(unfinished(&going)),
        None => {}
    }
    location::set_in(&tx, dialog_id, &Location::Moving(host.to_string()))?;
    insert(&tx, dialog_id, host, move_id, THERE)?;
    tx.commit().map_err(err)
}

/// Locks the chat for its move back from `host`, with `move_id`. It stays
/// on the host until the flip; the app refuses its writes meanwhile. A move
/// back from `host` cut short goes on instead, its row as it was. Refused
/// for a chat that isn't on `host`, or whose move there isn't over.
pub fn lock_back(
    conn: &mut Connection,
    dialog_id: &str,
    host: &str,
    move_id: &str,
) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    match location::of_chat(&tx, dialog_id)? {
        Location::Host(on) if on == host => {}
        Location::Host(on) => return Err(format!("This chat is on {on}.")),
        Location::Here => return Err("This chat is on this computer already.".into()),
        Location::Moving(to) => return Err(format!("This chat is moving to {to}.")),
        Location::Released => return Err("This chat went back to its computer.".into()),
    }
    match of_chat(&tx, dialog_id)? {
        Some(going) if going.direction == BACK && going.host == host => {}
        Some(going) => return Err(unfinished(&going)),
        None => insert(&tx, dialog_id, host, move_id, BACK)?,
    }
    tx.commit().map_err(err)
}

fn insert(
    conn: &Connection,
    dialog_id: &str,
    host: &str,
    move_id: &str,
    way: &str,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO cloud_moves (dialog_id, host, move_id, direction, step, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![dialog_id, host, move_id, way, LOCKED, now_ms()],
    )
    .map(drop)
    .map_err(err)
}

/// Why a chat with a move going can't start another.
fn unfinished(going: &CloudMove) -> String {
    match going.direction.as_str() {
        BACK => format!("This chat's move back from {} isn't over yet.", going.host),
        _ => "This chat's last move isn't over: cancel it first.".into(),
    }
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

/// The flip of a move back: the chat and its subagents' chats are here,
/// the mirror's row goes, and the move waits for its host to forget them.
pub fn flip_back(conn: &mut Connection, dialog_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    location::set_in(&tx, dialog_id, &Location::Here)?;
    tx.execute(
        "DELETE FROM mirror_state WHERE dialog_id = ?1",
        params![dialog_id],
    )
    .map_err(err)?;
    step(&tx, dialog_id, FORGETTING)?;
    tx.commit().map_err(err)
}

/// A remote chat's tombstone, written before its rows go: its host
/// deletes the chat when it's next told, or forgets it if it let the chat
/// go already. It takes the place of a move back cut short.
pub fn bury(conn: &mut Connection, dialog_id: &str, host: &str, move_id: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    let step = match of_chat(&tx, dialog_id)? {
        Some(going) if going.step == RELEASED || going.step == FORGETTING => FORGETTING,
        _ => DELETED,
    };
    tx.execute(
        "INSERT INTO cloud_moves (dialog_id, host, move_id, direction, step, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (dialog_id) DO UPDATE SET host = ?2, direction = ?4, step = ?5",
        params![dialog_id, host, move_id, BACK, step, now_ms()],
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

/// The move's row goes: what it queued reached its host, or a move back
/// was undone before its host released the chat, which stays there.
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
    use crate::chat_db::store::{self, NewChildDialog, NewDialog, Place};
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
            folders: None,
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
    fn a_move_back_keeps_the_chat_on_its_host_until_its_flip() {
        let mut conn = db();
        let refused = lock_back(&mut conn, "chat", "server", "b1").unwrap_err();
        assert!(refused.contains("on this computer already"), "{refused}");
        lock(&mut conn, "chat", "server", "m1").unwrap();
        flip(&mut conn, "chat", "server", 42).unwrap();

        lock_back(&mut conn, "chat", "server", "b1").unwrap();
        let on = Location::Host("server".into());
        assert_eq!(at(&conn), (on.clone(), on.clone()));
        let row = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!((row.direction.as_str(), row.step.as_str()), (BACK, LOCKED));
        let elsewhere = lock_back(&mut conn, "chat", "other", "b2").unwrap_err();
        assert!(elsewhere.contains("on server"), "{elsewhere}");
        step(&conn, "chat", RELEASED).unwrap();
        // Cut short: it goes on from where it was.
        lock_back(&mut conn, "chat", "server", "b2").unwrap();
        let row = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!((row.move_id.as_str(), row.step.as_str()), ("b1", RELEASED));

        flip_back(&mut conn, "chat").unwrap();
        assert_eq!(at(&conn), (Location::Here, Location::Here));
        assert!(mirror::state(&conn, "chat").unwrap().is_none());
        assert_eq!(of_chat(&conn, "chat").unwrap().unwrap().step, FORGETTING);

        // A move there needn't wait for the host to forget the chat.
        lock(&mut conn, "chat", "server", "m2").unwrap();
        let row = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!(
            (row.direction.as_str(), row.move_id.as_str()),
            (THERE, "m2")
        );
    }

    #[test]
    fn a_deleted_chats_tombstone_says_what_its_host_is_to_do() {
        let mut conn = db();
        bury(&mut conn, "chat", "server", "d1").unwrap();
        let row = of_chat(&conn, "chat").unwrap().unwrap();
        assert_eq!(
            (row.direction.as_str(), row.step.as_str(), row.host.as_str()),
            (BACK, DELETED, "server")
        );
        // A chat its host let go already is forgotten there, not deleted.
        step(&conn, "chat", RELEASED).unwrap();
        bury(&mut conn, "chat", "server", "d2").unwrap();
        assert_eq!(of_chat(&conn, "chat").unwrap().unwrap().step, FORGETTING);
        assert_eq!(with_host(&conn, "server").unwrap().len(), 1);
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

    #[test]
    fn the_list_shows_where_a_chat_is_as_it_moves() {
        let mut conn = db();
        let place = |conn: &Connection| {
            let one = store::get_dialog(conn, "chat").unwrap().unwrap().place;
            let page = store::list_dialogs(conn, None, None, 10).unwrap();
            let listed = page.items.into_iter().find(|d| d.id == "chat").unwrap();
            assert_eq!(listed.place, one, "the list and the one chat agree");
            one
        };
        let server = || "server".to_string();
        assert_eq!(place(&conn), None);
        lock(&mut conn, "chat", "server", "m1").unwrap();
        assert_eq!(place(&conn), Some(Place::Moving { host: server() }));
        flip(&mut conn, "chat", "server", 42).unwrap();
        assert_eq!(place(&conn), Some(Place::Host { host: server() }));
        lock_back(&mut conn, "chat", "server", "b1").unwrap();
        assert_eq!(place(&conn), Some(Place::MovingBack { host: server() }));
        step(&conn, "chat", RELEASED).unwrap();
        assert_eq!(place(&conn), Some(Place::MovingBack { host: server() }));
        flip_back(&mut conn, "chat").unwrap();
        assert_eq!(place(&conn), None, "here, its host yet to forget it");
        // A cancelled move whose host may have the chat leaves it here.
        lock(&mut conn, "chat", "other", "m2").unwrap();
        cancel(&mut conn, "chat").unwrap();
        assert_eq!(place(&conn), None);
    }
}

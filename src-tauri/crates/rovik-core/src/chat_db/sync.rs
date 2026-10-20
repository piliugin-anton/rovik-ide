//! A chat's changes, for its mirror (6.3): what changed in a chat and its
//! subagents' chats since a revision, read a page at a time, and those
//! changes written into another database as the owner has them.
//!
//! - **Reading** (`changes_since`): every row of the chat's subtree whose
//!   revision is past the one asked from and no later than the owner's
//!   clock when the pull began (`ChangesCursor::upto`); what changes
//!   meanwhile comes with the next pull. Each page is read in one
//!   transaction. The chat rows come on the first page, parents first,
//!   then each chat's CLM state, CLM revisions and messages, in the order
//!   of their revisions: at most `PAGE_BYTES` of them a page, or one row
//!   when a row is bigger. The last page lists the CLM revisions the owner
//!   still has, since it deletes old ones.
//! - **Writing** (`apply_in`): each row as the owner has it, by its key,
//!   with the owner's numbering and times, and the search index kept as
//!   `store` keeps it. The chat's project and location stay this
//!   database's, and a new subagent's chat takes its parent's. A row that
//!   is already as the owner has it isn't written, so applying the same
//!   changes twice changes nothing, not even a revision.
//! - **A mirror's pull** (`apply_pulled`) writes only while the chat is
//!   still on that host, checked in the same transaction, so a late page
//!   can't overwrite a chat that moved back. The pull's last page moves the
//!   mirror's position (`mirror_state.rev`) in that transaction too.
//! - **A move's import** (`import_in`, 6.8) writes a chat's changes since 0
//!   on its host, owned there, once `check_bundle` has found them to be
//!   that chat's and its subagents' alone, and never over another chat's
//!   rows.

use super::attachments;
use super::location::{self, Location};
use super::store::{self, err, DbResult};
use super::DbState;
use rovik_protocol::sync::{
    ChangeSet, ChangedTable, ChangesCursor, ChangesPage, StoredClmState, StoredDialog, StoredExtra,
    StoredMessage, StoredRevision,
};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The most a page of changes carries (4.2): a row bigger than this goes
/// on a page of its own.
pub const PAGE_BYTES: usize = 512 * 1024;

/// Rows read at a time while a page fills.
const BATCH: i64 = 32;

/// The query for a chat's messages changed since a revision, after where
/// the last page stopped, oldest change first: `messages_by_rev` serves it.
pub(crate) const CHANGED_MESSAGES: &str = "SELECT id FROM messages
     WHERE dialog_id = ?1 AND rev > ?2 AND rev <= ?3 AND (rev, id) > (?4, ?5)
     ORDER BY rev, id LIMIT ?6";

const MESSAGE_COLUMNS: &str = "m.id, m.dialog_id, m.seq, m.role, m.body_external, m.body_bytes, \
     m.tool_calls, m.tool_call_id, m.meta, m.error, m.failed, m.finalized, m.created_at, m.content, \
     b.content, m.rev";

const DIALOG_COLUMNS: &str =
    "id, title, created_at, updated_at, message_count, preview, archived, \
     parent_id, parent_call_id, kind, context_mode, model_provider, model_id, effort, rev";

const CLM_STATE_COLUMNS: &str = "dialog_id, anchor_id, rendered_anchor_id, rendered_revision, \
     tokens, budget, nudged, updated_at, projection, rendered_messages, rendered, task, rev";

const REVISION_COLUMNS: &str = "dialog_id, number, kind, created_at, tokens, content, rev";

/*
 * ---------------------------------------------------------------------------
 * Reading
 * ---------------------------------------------------------------------------
 */

/// `changes_since` on a read connection, a page of at most `PAGE_BYTES`.
pub async fn pull(
    db: &DbState,
    dialog_id: String,
    since: i64,
    cursor: Option<ChangesCursor>,
) -> DbResult<ChangesPage> {
    db.read(move |c| changes_since(c, &dialog_id, since, cursor.as_ref(), PAGE_BYTES))
        .await
}

/// The rows of `root`'s subtree changed since revision `since`, from where
/// `cursor` stopped, at most `budget` bytes of them (one row when a row is
/// bigger).
pub fn changes_since(
    conn: &Connection,
    root: &str,
    since: i64,
    cursor: Option<&ChangesCursor>,
    budget: usize,
) -> DbResult<ChangesPage> {
    let tx = conn.unchecked_transaction().map_err(err)?;
    let upto = match cursor {
        Some(c) => c.upto,
        None => clock(&tx)?,
    };
    let mut chats = subtree(&tx, root)?;
    chats.sort();
    let mut changes = ChangeSet::default();
    let mut size = 0;
    if cursor.is_none() {
        changes.dialogs = changed_dialogs(&tx, &chats, since, upto)?;
        size += changes.dialogs.iter().map(dialog_size).sum::<usize>();
    }
    let page = |changes, next| ChangesPage {
        changes,
        next,
        rev: upto,
    };
    for chat in &chats {
        for table in [
            ChangedTable::ClmState,
            ChangedTable::ClmRevisions,
            ChangedTable::Messages,
        ] {
            // Rows of this stream after `after` (rev, key) only.
            let mut after = match cursor {
                Some(c) => match (chat.as_str(), table).cmp(&(c.chat.as_str(), c.table)) {
                    std::cmp::Ordering::Less => continue,
                    std::cmp::Ordering::Equal => Some((c.rev, c.key.clone())),
                    std::cmp::Ordering::Greater => None,
                },
                None => None,
            };
            loop {
                let rows = read_stream(&tx, chat, table, since, upto, after.as_ref())?;
                if rows.is_empty() {
                    break;
                }
                for (row, rev, key) in rows {
                    let bytes = row.size();
                    if size > 0 && size + bytes > budget {
                        let (rev, key) = after.unwrap_or((i64::MIN, String::new()));
                        let next = ChangesCursor {
                            upto,
                            chat: chat.clone(),
                            table,
                            rev,
                            key,
                        };
                        return Ok(page(changes, Some(next)));
                    }
                    size += bytes;
                    row.add_to(&mut changes);
                    after = Some((rev, key));
                }
            }
        }
    }
    changes.clm_kept = Some(kept_revisions(&tx, &chats)?);
    Ok(page(changes, None))
}

/// The database's revision clock.
pub fn clock(conn: &Connection) -> DbResult<i64> {
    conn.query_row("SELECT rev FROM sync_clock", [], |r| r.get(0))
        .map_err(err)
}

/// The chat and its subagents' chats that this database has.
fn subtree(conn: &Connection, root: &str) -> DbResult<Vec<String>> {
    let exists: bool = conn
        .query_row("SELECT 1 FROM dialogs WHERE id = ?1", params![root], |_| {
            Ok(())
        })
        .optional()
        .map_err(err)?
        .is_some();
    if !exists {
        return Ok(Vec::new());
    }
    let mut chats = store::descendants(conn, root)?;
    chats.push(root.to_string());
    Ok(chats)
}

/// The subtree's chat rows changed in (since, upto], parents first.
fn changed_dialogs(
    conn: &Connection,
    chats: &[String],
    since: i64,
    upto: i64,
) -> DbResult<Vec<StoredDialog>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {DIALOG_COLUMNS} FROM dialogs WHERE id = ?1 AND rev > ?2 AND rev <= ?3"
        ))
        .map_err(err)?;
    let mut changed = Vec::new();
    for chat in chats {
        let row = stmt
            .query_row(params![chat, since, upto], dialog_from_row)
            .optional()
            .map_err(err)?;
        changed.extend(row);
    }
    Ok(parents_first(changed))
}

/// Chat rows ordered so that each comes after its parent when both are
/// there.
fn parents_first(mut rows: Vec<StoredDialog>) -> Vec<StoredDialog> {
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let mut placed: HashSet<String> = HashSet::new();
    let pending: HashSet<String> = rows.iter().map(|d| d.id.clone()).collect();
    let mut out = Vec::with_capacity(rows.len());
    while !rows.is_empty() {
        let (ready, waiting): (Vec<_>, Vec<_>) = rows.into_iter().partition(|d| {
            d.parent_id
                .as_ref()
                .is_none_or(|p| !pending.contains(p) || placed.contains(p))
        });
        if ready.is_empty() {
            // Parent links that loop: as they are.
            out.extend(waiting);
            break;
        }
        placed.extend(ready.iter().map(|d| d.id.clone()));
        out.extend(ready);
        rows = waiting;
    }
    out
}

/// A row of a chat's stream, with its revision and its key there.
enum StreamRow {
    ClmState(StoredClmState),
    Revision(StoredRevision),
    Message(StoredMessage),
}

impl StreamRow {
    /// About the bytes it takes on a page.
    fn size(&self) -> usize {
        let text = |t: &Option<String>| t.as_ref().map_or(0, String::len);
        match self {
            StreamRow::ClmState(s) => {
                128 + text(&s.projection)
                    + text(&s.rendered_messages)
                    + text(&s.rendered)
                    + text(&s.task)
                    + text(&s.nudged)
            }
            StreamRow::Revision(r) => 128 + r.content.len(),
            StreamRow::Message(m) => message_size(m),
        }
    }

    fn add_to(self, changes: &mut ChangeSet) {
        match self {
            StreamRow::ClmState(s) => changes.clm_states.push(s),
            StreamRow::Revision(r) => changes.clm_revisions.push(r),
            StreamRow::Message(m) => changes.messages.push(m),
        }
    }
}

fn dialog_size(d: &StoredDialog) -> usize {
    256 + d.title.len() + d.preview.as_ref().map_or(0, String::len)
}

fn message_size(m: &StoredMessage) -> usize {
    let text = |t: &Option<String>| t.as_ref().map_or(0, String::len);
    256 + m.content.len()
        + text(&m.body)
        + text(&m.tool_calls)
        + text(&m.meta)
        + text(&m.error)
        + 80 * m.attachments.len()
        + m.extras
            .iter()
            .map(|e| 64 + e.extra_content.len())
            .sum::<usize>()
}

/// The next rows of one of a chat's streams, after `after`.
fn read_stream(
    conn: &Connection,
    chat: &str,
    table: ChangedTable,
    since: i64,
    upto: i64,
    after: Option<&(i64, String)>,
) -> DbResult<Vec<(StreamRow, i64, String)>> {
    let (after_rev, after_key) = after
        .map(|(rev, key)| (*rev, key.clone()))
        .unwrap_or((i64::MIN, String::new()));
    match table {
        ChangedTable::Dialogs => Ok(Vec::new()),
        ChangedTable::ClmState => {
            let state = conn
                .query_row(
                    &format!(
                        "SELECT {CLM_STATE_COLUMNS} FROM clm_state
                         WHERE dialog_id = ?1 AND rev > ?2 AND rev <= ?3"
                    ),
                    params![chat, since, upto],
                    |r| Ok((clm_state_from_row(r)?, r.get::<_, i64>(12)?)),
                )
                .optional()
                .map_err(err)?;
            Ok(state
                .filter(|(_, rev)| (*rev, chat.to_string()) > (after_rev, after_key.clone()))
                .map(|(s, rev)| vec![(StreamRow::ClmState(s), rev, chat.to_string())])
                .unwrap_or_default())
        }
        ChangedTable::ClmRevisions => {
            let after_number: i64 = after_key.parse().unwrap_or(i64::MIN);
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {REVISION_COLUMNS} FROM clm_revisions
                     WHERE dialog_id = ?1 AND rev > ?2 AND rev <= ?3 AND (rev, number) > (?4, ?5)
                     ORDER BY rev, number LIMIT ?6"
                ))
                .map_err(err)?;
            let rows = stmt
                .query_map(
                    params![chat, since, upto, after_rev, after_number, BATCH],
                    |r| Ok((revision_from_row(r)?, r.get::<_, i64>(6)?)),
                )
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            Ok(rows
                .into_iter()
                .map(|(revision, rev)| {
                    let key = revision.number.to_string();
                    (StreamRow::Revision(revision), rev, key)
                })
                .collect())
        }
        ChangedTable::Messages => {
            let mut stmt = conn.prepare_cached(CHANGED_MESSAGES).map_err(err)?;
            let ids: Vec<String> = stmt
                .query_map(
                    params![chat, since, upto, after_rev, after_key, BATCH],
                    |r| r.get(0),
                )
                .map_err(err)?
                .collect::<Result<_, _>>()
                .map_err(err)?;
            let mut rows = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some((message, rev)) = read_message(conn, &id)? {
                    rows.push((StreamRow::Message(message), rev, id));
                }
            }
            Ok(rows)
        }
    }
}

/// A message as stored, whole, with its revision.
fn read_message(conn: &Connection, id: &str) -> DbResult<Option<(StoredMessage, i64)>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages m
                 LEFT JOIN message_bodies b ON b.message_id = m.id WHERE m.id = ?1"
            ),
            params![id],
            |r| Ok((message_from_row(r)?, r.get::<_, i64>(15)?)),
        )
        .optional()
        .map_err(err)?;
    let Some((mut message, rev)) = row else {
        return Ok(None);
    };
    message.attachments = {
        let mut stmt = conn
            .prepare_cached(
                "SELECT attachment_id FROM message_attachments WHERE message_id = ?1 ORDER BY position",
            )
            .map_err(err)?;
        let ids = stmt
            .query_map(params![id], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        ids
    };
    message.extras = {
        let mut stmt = conn
            .prepare_cached(
                "SELECT call_id, extra_content, created_at FROM tool_call_extras
                 WHERE message_id = ?1 ORDER BY call_id",
            )
            .map_err(err)?;
        let extras = stmt
            .query_map(params![id], |r| {
                Ok(StoredExtra {
                    call_id: r.get(0)?,
                    extra_content: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        extras
    };
    Ok(Some((message, rev)))
}

/// Each chat's CLM revisions, by number.
fn kept_revisions(conn: &Connection, chats: &[String]) -> DbResult<BTreeMap<String, Vec<i64>>> {
    let mut stmt = conn
        .prepare_cached("SELECT number FROM clm_revisions WHERE dialog_id = ?1 ORDER BY number")
        .map_err(err)?;
    let mut kept = BTreeMap::new();
    for chat in chats {
        let numbers = stmt
            .query_map(params![chat], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(err)?;
        kept.insert(chat.clone(), numbers);
    }
    Ok(kept)
}

fn dialog_from_row(r: &Row) -> rusqlite::Result<StoredDialog> {
    Ok(StoredDialog {
        id: r.get(0)?,
        title: r.get(1)?,
        created_at: r.get(2)?,
        updated_at: r.get(3)?,
        message_count: r.get(4)?,
        preview: r.get(5)?,
        archived: r.get(6)?,
        parent_id: r.get(7)?,
        parent_call_id: r.get(8)?,
        kind: r.get(9)?,
        context_mode: r.get(10)?,
        model_provider: r.get(11)?,
        model_id: r.get(12)?,
        effort: r.get(13)?,
    })
}

fn message_from_row(r: &Row) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
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
        body: r.get(14)?,
        attachments: Vec::new(),
        extras: Vec::new(),
    })
}

fn clm_state_from_row(r: &Row) -> rusqlite::Result<StoredClmState> {
    Ok(StoredClmState {
        dialog_id: r.get(0)?,
        anchor_id: r.get(1)?,
        rendered_anchor_id: r.get(2)?,
        rendered_revision: r.get(3)?,
        tokens: r.get(4)?,
        budget: r.get(5)?,
        nudged: r.get(6)?,
        updated_at: r.get(7)?,
        projection: r.get(8)?,
        rendered_messages: r.get(9)?,
        rendered: r.get(10)?,
        task: r.get(11)?,
    })
}

fn revision_from_row(r: &Row) -> rusqlite::Result<StoredRevision> {
    Ok(StoredRevision {
        dialog_id: r.get(0)?,
        number: r.get(1)?,
        kind: r.get(2)?,
        created_at: r.get(3)?,
        tokens: r.get(4)?,
        content: r.get(5)?,
    })
}

/*
 * ---------------------------------------------------------------------------
 * Writing
 * ---------------------------------------------------------------------------
 */

/// What `apply_in` wrote: chats and messages new to this database, and the
/// ones it had that changed, for the events after a pull (6.3).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Applied {
    pub new_chats: Vec<String>,
    pub changed_chats: Vec<String>,
    /// (chat, message id), in the order written.
    pub added: Vec<(String, String)>,
    pub changed: Vec<(String, String)>,
}

impl Applied {
    pub fn is_empty(&self) -> bool {
        self.new_chats.is_empty()
            && self.changed_chats.is_empty()
            && self.added.is_empty()
            && self.changed.is_empty()
    }
}

/// Where a chat new to this database goes when it isn't a subagent's,
/// which goes where its parent is.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChats {
    pub project_id: Option<String>,
    pub location: Location,
}

/// A page of a pull into the mirror of `root`, on `host`, in one
/// transaction: nothing when the chat isn't on that host any more. The
/// pull's last page moves the mirror's position to its revision.
pub fn apply_pulled(
    conn: &mut Connection,
    root: &str,
    host: &str,
    page: &ChangesPage,
) -> DbResult<Option<Applied>> {
    let tx = conn.transaction().map_err(err)?;
    let on_host = Location::Host(host.to_string());
    if location::of_chat(&tx, root)? != on_host {
        return Ok(None);
    }
    let new = NewChats {
        project_id: None,
        location: on_host,
    };
    let applied = apply_in(&tx, &page.changes, &new)?;
    if page.next.is_none() {
        tx.execute(
            "UPDATE mirror_state SET rev = MAX(rev, ?2) WHERE dialog_id = ?1",
            params![root, page.rev],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    Ok(Some(applied))
}

/// "Re-sync this chat" (6.3): the mirror of `root`, on `host`, replaced in
/// one transaction by every row its owner has, which a pull from 0 read up
/// to the owner's revision `rev`. The chat's own row stays, with what hangs
/// from it here (the input's draft and its files, K45), and takes the
/// owner's fields. Nothing when the chat isn't on that host any more.
pub fn replace_mirror(
    conn: &mut Connection,
    root: &str,
    host: &str,
    rows: &ChangeSet,
    rev: i64,
) -> DbResult<Option<Applied>> {
    let tx = conn.transaction().map_err(err)?;
    let on_host = Location::Host(host.to_string());
    if location::of_chat(&tx, root)? != on_host {
        return Ok(None);
    }
    store::delete_below(&tx, root)?;
    let new = NewChats {
        project_id: None,
        location: on_host,
    };
    let applied = apply_in(&tx, rows, &new)?;
    // From the owner's clock as it is, even one behind the mirror's: a
    // daemon whose database was replaced.
    tx.execute(
        "UPDATE mirror_state SET rev = ?2 WHERE dialog_id = ?1",
        params![root, rev],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(Some(applied))
}

/// Whether `rows` are the chat `lead`'s and its subagents' alone (6.8):
/// the lead's row, which has no parent, each other chat under it, and
/// every other row in one of those chats. An import writes nothing else.
pub fn check_bundle(lead: &str, rows: &ChangeSet) -> Result<(), String> {
    let parents: HashMap<&str, Option<&str>> = rows
        .dialogs
        .iter()
        .map(|d| (d.id.as_str(), d.parent_id.as_deref()))
        .collect();
    if parents.len() != rows.dialogs.len() {
        return Err("The chat's rows have a chat twice.".into());
    }
    match parents.get(lead) {
        Some(None) => {}
        Some(Some(_)) => return Err("A subagent's chat doesn't move on its own.".into()),
        None => return Err("The chat's rows don't have the chat.".into()),
    }
    for chat in parents.keys() {
        let (mut at, mut steps) = (*chat, 0);
        while at != lead {
            match parents.get(at) {
                Some(&Some(parent)) if steps < parents.len() => at = parent,
                _ => return Err(format!("{chat} isn't one of the chat's subagents' chats.")),
            }
            steps += 1;
        }
    }
    let stray = (rows.clm_states.iter().map(|s| &s.dialog_id))
        .chain(rows.clm_revisions.iter().map(|r| &r.dialog_id))
        .chain(rows.messages.iter().map(|m| &m.dialog_id))
        .chain(rows.clm_kept.iter().flat_map(|kept| kept.keys()))
        .find(|chat| !parents.contains_key(chat.as_str()));
    match stray {
        Some(chat) => Err(format!(
            "The chat's rows have rows of {chat}, another chat."
        )),
        None => Ok(()),
    }
}

/// Writes a chat that moved here (6.8), with its subagents' chats, in the
/// caller's transaction: owned here, in `project_id`. Rows another chat
/// here has under the same ids are refused, never overwritten. Returns the
/// clock once the rows are in, where the chat's mirror goes on from.
pub fn import_in(
    conn: &Connection,
    lead: &str,
    rows: &ChangeSet,
    project_id: &str,
) -> DbResult<i64> {
    check_bundle(lead, rows)?;
    check_rows_free(conn, rows, &[])?;
    let new = NewChats {
        project_id: Some(project_id.to_string()),
        location: Location::Here,
    };
    apply_in(conn, rows, &new)?;
    clock(conn)
}

/// Whether the bundle's rows can be written here once the chat's earlier
/// stay, if any, is gone (6.8): no other chat here has a chat or a message
/// under the bundle's ids. Read before an import takes anything of the
/// earlier stay away, so an import it refuses leaves that stay whole.
pub fn check_import_free(conn: &Connection, lead: &str, rows: &ChangeSet) -> DbResult<()> {
    check_bundle(lead, rows)?;
    let mut own = store::descendants(conn, lead)?;
    own.push(lead.to_string());
    check_rows_free(conn, rows, &own)
}

/// Rows another chat here has under the bundle's ids are refused, never
/// overwritten; the chats in `own` are the bundle's own earlier rows.
fn check_rows_free(conn: &Connection, rows: &ChangeSet, own: &[String]) -> DbResult<()> {
    for dialog in &rows.dialogs {
        if own.contains(&dialog.id) {
            continue;
        }
        let here = conn
            .query_row(
                "SELECT 1 FROM dialogs WHERE id = ?1",
                params![dialog.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(err)?;
        if here.is_some() {
            return Err(format!("This server has another chat {}.", dialog.id));
        }
    }
    for message in &rows.messages {
        let here: Option<String> = conn
            .query_row(
                "SELECT dialog_id FROM messages WHERE id = ?1",
                params![message.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        if let Some(chat) = here.filter(|chat| !own.contains(chat)) {
            return Err(format!(
                "This server has a message {} in another chat, {chat}.",
                message.id
            ));
        }
    }
    Ok(())
}

/// Writes `changes` as the owner has them, in the caller's transaction.
pub fn apply_in(conn: &Connection, changes: &ChangeSet, new: &NewChats) -> DbResult<Applied> {
    let mut applied = Applied::default();
    for dialog in parents_first(changes.dialogs.clone()) {
        apply_dialog(conn, &dialog, new, &mut applied)?;
    }
    for state in &changes.clm_states {
        apply_clm_state(conn, state)?;
    }
    for revision in &changes.clm_revisions {
        apply_revision(conn, revision)?;
    }
    let mut children: HashMap<String, bool> = HashMap::new();
    for message in &changes.messages {
        let child = match children.get(&message.dialog_id) {
            Some(child) => *child,
            None => {
                let child = is_child(conn, &message.dialog_id)?;
                children.insert(message.dialog_id.clone(), child);
                child
            }
        };
        apply_message(conn, message, child, &mut applied)?;
    }
    if let Some(kept) = &changes.clm_kept {
        for (chat, numbers) in kept {
            delete_revisions_but(conn, chat, numbers)?;
        }
    }
    Ok(applied)
}

fn apply_dialog(
    conn: &Connection,
    dialog: &StoredDialog,
    new: &NewChats,
    applied: &mut Applied,
) -> DbResult<()> {
    let here = conn
        .query_row(
            &format!("SELECT {DIALOG_COLUMNS} FROM dialogs WHERE id = ?1"),
            params![dialog.id],
            dialog_from_row,
        )
        .optional()
        .map_err(err)?;
    let child = dialog.parent_id.is_some();
    match here {
        Some(here) if here == *dialog => return Ok(()),
        Some(here) => {
            conn.execute(
                "UPDATE dialogs SET title = ?2, created_at = ?3, updated_at = ?4, message_count = ?5,
                   preview = ?6, archived = ?7, parent_id = ?8, parent_call_id = ?9, kind = ?10,
                   context_mode = ?11, model_provider = ?12, model_id = ?13, effort = ?14
                 WHERE id = ?1",
                params![
                    dialog.id,
                    dialog.title,
                    dialog.created_at,
                    dialog.updated_at,
                    dialog.message_count,
                    dialog.preview,
                    dialog.archived,
                    dialog.parent_id,
                    dialog.parent_call_id,
                    dialog.kind,
                    dialog.context_mode,
                    dialog.model_provider,
                    dialog.model_id,
                    dialog.effort,
                ],
            )
            .map_err(err)?;
            if !child && here.title != dialog.title {
                store::index_title(conn, &dialog.id, &dialog.title)?;
            }
            applied.changed_chats.push(dialog.id.clone());
        }
        None => {
            // A subagent's chat goes where its parent is.
            let parent = match &dialog.parent_id {
                Some(parent) => conn
                    .query_row(
                        "SELECT project_id, location FROM dialogs WHERE id = ?1",
                        params![parent],
                        |r| {
                            Ok((
                                r.get::<_, Option<String>>(0)?,
                                r.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(err)?,
                None => None,
            };
            let (project_id, location) =
                parent.unwrap_or_else(|| (new.project_id.clone(), new.location.column()));
            conn.execute(
                "INSERT INTO dialogs (id, title, created_at, updated_at, message_count, preview, archived,
                   parent_id, parent_call_id, kind, context_mode, model_provider, model_id, effort,
                   project_id, location)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    dialog.id,
                    dialog.title,
                    dialog.created_at,
                    dialog.updated_at,
                    dialog.message_count,
                    dialog.preview,
                    dialog.archived,
                    dialog.parent_id,
                    dialog.parent_call_id,
                    dialog.kind,
                    dialog.context_mode,
                    dialog.model_provider,
                    dialog.model_id,
                    dialog.effort,
                    project_id,
                    location,
                ],
            )
            .map_err(err)?;
            if !child {
                store::index_title(conn, &dialog.id, &dialog.title)?;
            }
            applied.new_chats.push(dialog.id.clone());
        }
    }
    Ok(())
}

fn apply_clm_state(conn: &Connection, state: &StoredClmState) -> DbResult<()> {
    let here = conn
        .query_row(
            &format!("SELECT {CLM_STATE_COLUMNS} FROM clm_state WHERE dialog_id = ?1"),
            params![state.dialog_id],
            clm_state_from_row,
        )
        .optional()
        .map_err(err)?;
    if here.as_ref() == Some(state) {
        return Ok(());
    }
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
            state.dialog_id,
            state.anchor_id,
            state.rendered_anchor_id,
            state.rendered_revision,
            state.tokens,
            state.budget,
            state.nudged,
            state.updated_at,
            state.projection,
            state.rendered_messages,
            state.rendered,
            state.task,
        ],
    )
    .map_err(err)?;
    Ok(())
}

fn apply_revision(conn: &Connection, revision: &StoredRevision) -> DbResult<()> {
    let here = conn
        .query_row(
            &format!(
                "SELECT {REVISION_COLUMNS} FROM clm_revisions WHERE dialog_id = ?1 AND number = ?2"
            ),
            params![revision.dialog_id, revision.number],
            revision_from_row,
        )
        .optional()
        .map_err(err)?;
    if here.as_ref() == Some(revision) {
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO clm_revisions (dialog_id, number, kind, created_at, tokens, content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            revision.dialog_id,
            revision.number,
            revision.kind,
            revision.created_at,
            revision.tokens,
            revision.content,
        ],
    )
    .map_err(err)?;
    Ok(())
}

fn apply_message(
    conn: &Connection,
    message: &StoredMessage,
    child: bool,
    applied: &mut Applied,
) -> DbResult<()> {
    let here = read_message(conn, &message.id)?.map(|(m, _)| m);
    if here.as_ref() == Some(message) {
        return Ok(());
    }
    // A row of the chat's with this number, which came another way (a
    // run's event) numbered here: out of the way, at a number nothing has
    // (one parked before may hold -seq), until its own change brings the
    // owner's number.
    let lowest: i64 = conn
        .query_row(
            "SELECT COALESCE(MIN(seq), 0) FROM messages WHERE dialog_id = ?1",
            params![message.dialog_id],
            |r| r.get(0),
        )
        .map_err(err)?;
    conn.execute(
        "UPDATE messages SET seq = ?4 WHERE dialog_id = ?1 AND seq = ?2 AND id != ?3 AND seq > 0",
        params![
            message.dialog_id,
            message.seq,
            message.id,
            lowest.min(0) - 1
        ],
    )
    .map_err(err)?;
    let values = params![
        message.id,
        message.dialog_id,
        message.seq,
        message.role,
        message.body_external as i64,
        message.body_bytes,
        message.tool_calls,
        message.tool_call_id,
        message.meta,
        message.error,
        message.failed as i64,
        message.finalized as i64,
        message.created_at,
        message.content,
    ];
    if here.is_some() {
        conn.execute(
            "UPDATE messages SET dialog_id = ?2, seq = ?3, role = ?4, body_external = ?5, body_bytes = ?6,
               tool_calls = ?7, tool_call_id = ?8, meta = ?9, error = ?10, failed = ?11,
               finalized = ?12, created_at = ?13, content = ?14
             WHERE id = ?1",
            values,
        )
        .map_err(err)?;
    } else {
        conn.execute(
            "INSERT INTO messages (id, dialog_id, seq, role, body_external, body_bytes, tool_calls,
               tool_call_id, meta, error, failed, finalized, created_at, content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            values,
        )
        .map_err(err)?;
    }
    let old = here.unwrap_or_default();
    if old.body != message.body {
        match &message.body {
            Some(body) => conn.execute(
                "INSERT OR REPLACE INTO message_bodies (message_id, content) VALUES (?1, ?2)",
                params![message.id, body],
            ),
            None => conn.execute(
                "DELETE FROM message_bodies WHERE message_id = ?1",
                params![message.id],
            ),
        }
        .map_err(err)?;
    }
    if old.attachments != message.attachments {
        conn.execute(
            "DELETE FROM message_attachments WHERE message_id = ?1",
            params![message.id],
        )
        .map_err(err)?;
        for (position, attachment) in message.attachments.iter().enumerate() {
            // One the files didn't bring gets a row without a file (K44).
            attachments::add_unavailable(conn, attachment)?;
            conn.execute(
                "INSERT INTO message_attachments (message_id, position, attachment_id) VALUES (?1, ?2, ?3)",
                params![message.id, position as i64, attachment],
            )
            .map_err(err)?;
        }
    }
    if old.extras != message.extras {
        conn.execute(
            "DELETE FROM tool_call_extras WHERE message_id = ?1",
            params![message.id],
        )
        .map_err(err)?;
        for extra in &message.extras {
            conn.execute(
                "INSERT INTO tool_call_extras (message_id, call_id, extra_content, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    message.id,
                    extra.call_id,
                    extra.extra_content,
                    extra.created_at
                ],
            )
            .map_err(err)?;
        }
    }
    // The search index, as `store` keeps it for a final message.
    let searchable = message.finalized
        && !child
        && (message.role == "user" || message.role == "assistant")
        && message.error.is_none()
        && !message.failed
        && !message.content.trim().is_empty();
    if searchable {
        store::index_message(conn, &message.dialog_id, &message.id, &message.content)?;
    } else {
        store::remove_search_row(conn, &message.dialog_id, Some(&message.id))?;
    }
    let row = (message.dialog_id.clone(), message.id.clone());
    match old.id.is_empty() {
        true => applied.added.push(row),
        false => applied.changed.push(row),
    }
    Ok(())
}

fn is_child(conn: &Connection, dialog_id: &str) -> DbResult<bool> {
    conn.query_row(
        "SELECT parent_id IS NOT NULL FROM dialogs WHERE id = ?1",
        params![dialog_id],
        |r| r.get(0),
    )
    .optional()
    .map(|child| child.unwrap_or(false))
    .map_err(err)
}

/// Deletes the chat's CLM revisions but `numbers`, which the owner keeps.
fn delete_revisions_but(conn: &Connection, chat: &str, numbers: &[i64]) -> DbResult<()> {
    let here: Vec<i64> = {
        let mut stmt = conn
            .prepare_cached("SELECT number FROM clm_revisions WHERE dialog_id = ?1")
            .map_err(err)?;
        let numbers = stmt
            .query_map(params![chat], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        numbers
    };
    let kept: HashSet<i64> = numbers.iter().copied().collect();
    let gone: Vec<i64> = here.into_iter().filter(|n| !kept.contains(n)).collect();
    for chunk in gone.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql =
            format!("DELETE FROM clm_revisions WHERE dialog_id = ? AND number IN ({placeholders})");
        let values = std::iter::once(rusqlite::types::Value::Text(chat.to_string()))
            .chain(chunk.iter().map(|n| rusqlite::types::Value::Integer(*n)));
        conn.execute(&sql, params_from_iter(values)).map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;

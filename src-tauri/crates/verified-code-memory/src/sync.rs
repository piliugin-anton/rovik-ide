//! Record replication uses Lamport revisions, not host clocks. A deletion wins
//! over every edit of the same ID. Clearing advances a project epoch, so an
//! offline replica cannot bring the previous history back. Derived indexes and
//! host paths are never transported.

use crate::{claim_hash, MemoryRecord, MemoryStore, Result, SharedMemoryStore, VcmError};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum encoded page size, below the transport's call limit.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Maximum operations in one page.
pub const MAX_PAGE_RECORDS: usize = 64;
const MAX_COUNTER: i64 = i64::MAX / 2;

/// A logical revision, ordered independently of wall-clock differences.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    /// Monotonically increasing logical counter.
    pub counter: i64,
    /// Replica that issued the revision; empty only for the initial epoch.
    pub replica: String,
}

/// One record revision or durable deletion marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// Stable record ID, retained across hosts.
    pub id: Uuid,
    /// Revision that last changed this record.
    pub version: Stamp,
    /// Revision of the title/content, independent of later verification changes.
    pub claim: Stamp,
    /// A deletion marker, which cannot be replaced by an edit.
    pub deleted: bool,
    /// The complete record, with an empty repository field on the wire.
    pub record: Option<MemoryRecord>,
}

/// A bounded incremental page for one externally authorized project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// Wire schema version.
    pub format: u32,
    /// Durable identity of the sending database.
    pub replica: String,
    /// The most recent clear-history operation.
    pub epoch: Stamp,
    /// Sender's sequence through which this page has scanned.
    pub through: i64,
    /// Further changes remain after this page.
    pub more: bool,
    /// Record revisions and deletion markers.
    pub changes: Vec<Change>,
}

/// A rejected local revision. Persist its disposition before advancing the cursor.
#[derive(Debug)]
pub struct RejectedRevision {
    /// ID of the record that requires attention.
    pub id: Uuid,
    /// Sender-local change sequence of this specific revision.
    pub sequence: i64,
    /// Validation failure, without record content.
    pub reason: &'static str,
}

/// A bounded scan of the local change queue for a team link.
#[derive(Debug)]
pub struct ExportScan {
    /// Valid revisions and the cursor through all inspected revisions.
    pub page: Page,
    /// Invalid revisions included in the scanned range.
    pub rejected: Vec<RejectedRevision>,
}

/// Durable progress for one project and peer endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// The peer database identity; changes after replacement or loss of its DB.
    pub replica: String,
    /// Local changes acknowledged by that peer.
    pub sent: i64,
    /// Peer changes committed locally.
    pub received: i64,
}

pub(crate) fn initialize(c: &Connection) -> Result<()> {
    let transaction = c.unchecked_transaction()?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_sync_clock (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1), replica TEXT NOT NULL,
            counter INTEGER NOT NULL, sequence INTEGER NOT NULL, applying INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS memory_sync_projects (
            repo TEXT PRIMARY KEY, counter INTEGER NOT NULL, replica TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS memory_sync_changes (
            repo TEXT NOT NULL, id TEXT NOT NULL, sequence INTEGER NOT NULL,
            counter INTEGER NOT NULL, replica TEXT NOT NULL, deleted INTEGER NOT NULL,
            claim_counter INTEGER NOT NULL, claim_replica TEXT NOT NULL,
            PRIMARY KEY(repo,id));
         CREATE INDEX IF NOT EXISTS memory_sync_sequence ON memory_sync_changes(repo,sequence);
         CREATE TABLE IF NOT EXISTS memory_sync_peers (
            repo TEXT NOT NULL, peer TEXT NOT NULL, replica TEXT NOT NULL,
            sent INTEGER NOT NULL, received INTEGER NOT NULL, PRIMARY KEY(repo,peer));",
    )?;
    c.execute(
        "INSERT OR IGNORE INTO memory_sync_clock VALUES (1,?1,0,0,0)",
        [Uuid::new_v4().to_string()],
    )?;
    // Cover every writer, including bulk validation, supersession and pruning.
    // Retrieval counters/timestamps alone do not create replication traffic.
    let columns = [
        "kind",
        "verification",
        "applicability",
        "source_kind",
        "repo",
        "title",
        "content",
        "claim_hash",
        "identity",
        "validated_claim_hash",
        "files_json",
        "tags_json",
        "code_version",
        "source_episode",
        "evidence",
        "confidence",
        "content_updated_at",
        "last_validated_at",
    ];
    let changed = columns
        .iter()
        .map(|col| format!("old.{col} IS NOT new.{col}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    for (suffix, event, row, deleted, condition) in [
        ("insert", "INSERT", "new", 0, String::new()),
        ("update", "UPDATE", "new", 0, format!(" AND ({changed})")),
        ("delete", "DELETE", "old", 1, String::new()),
    ] {
        let unchanged = if event == "UPDATE" {
            "old.title IS new.title AND old.content IS new.content"
        } else {
            "0"
        };
        let claim_counter = format!("CASE WHEN {unchanged} THEN coalesce((SELECT claim_counter FROM memory_sync_changes WHERE repo={row}.repo AND id={row}.id),counter) ELSE counter END");
        let claim_replica = format!("CASE WHEN {unchanged} THEN coalesce((SELECT claim_replica FROM memory_sync_changes WHERE repo={row}.repo AND id={row}.id),replica) ELSE replica END");
        c.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS memory_sync_{suffix} AFTER {event} ON memories
             WHEN (SELECT applying FROM memory_sync_clock WHERE singleton=1)=0 {condition}
             BEGIN
               INSERT OR IGNORE INTO memory_sync_projects VALUES ({row}.repo,0,'');
               UPDATE memory_sync_clock SET counter=counter+1,sequence=sequence+1 WHERE singleton=1;
               INSERT INTO memory_sync_changes
                 SELECT {row}.repo,{row}.id,sequence,counter,replica,{deleted},{claim_counter},{claim_replica}
                 FROM memory_sync_clock WHERE singleton=1
               ON CONFLICT(repo,id) DO UPDATE SET sequence=excluded.sequence,
                 counter=excluded.counter,replica=excluded.replica,deleted=excluded.deleted,claim_counter=excluded.claim_counter,claim_replica=excluded.claim_replica;
             END;"
        ))?;
    }
    let legacy = c.prepare("SELECT repo,id FROM memories WHERE NOT EXISTS (SELECT 1 FROM memory_sync_changes s WHERE s.repo=memories.repo AND s.id=memories.id)")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (repo, id) in legacy {
        c.execute(
            "INSERT OR IGNORE INTO memory_sync_projects VALUES (?1,0,'')",
            [&repo],
        )?;
        c.execute(
            "UPDATE memory_sync_clock SET counter=counter+1,sequence=sequence+1 WHERE singleton=1",
            [],
        )?;
        c.execute("INSERT INTO memory_sync_changes SELECT ?1,?2,sequence,counter,replica,0,counter,replica FROM memory_sync_clock WHERE singleton=1", params![repo,id])?;
    }
    // Publish the replication schema and its version atomically. Old binaries
    // must not reopen this store and clear it without advancing its epoch.
    crate::team::initialize(c)?;
    c.pragma_update(None, "user_version", crate::store::SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

impl MemoryStore {
    /// Durable database identity, used to detect replaced peers.
    pub fn sync_replica(&self) -> Result<String> {
        Ok(self.connection.query_row(
            "SELECT replica FROM memory_sync_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn sync_epoch(&self, repo: &str) -> Result<Stamp> {
        Ok(self
            .connection
            .query_row(
                "SELECT counter,replica FROM memory_sync_projects WHERE repo=?1",
                [repo],
                |r| {
                    Ok(Stamp {
                        counter: r.get(0)?,
                        replica: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub(crate) fn sync_clear(&self, repo: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE memory_sync_clock SET counter=counter+1,sequence=sequence+1 WHERE singleton=1",
            [],
        )?;
        self.connection.execute("INSERT INTO memory_sync_projects SELECT ?1,counter,replica FROM memory_sync_clock WHERE singleton=1 ON CONFLICT(repo) DO UPDATE SET counter=excluded.counter,replica=excluded.replica", [repo])?;
        self.connection
            .execute("DELETE FROM memory_sync_changes WHERE repo=?1", [repo])?;
        Ok(())
    }

    /// Exports a bounded page after a local sequence. Host-specific repository
    /// paths are removed; the receiver supplies its authorized local scope.
    pub fn sync_export(&self, repo: &str, after: i64) -> Result<Page> {
        Ok(self.export_range(repo, after, false)?.page)
    }

    /// Scans at most one page of changes and reports individual invalid revisions.
    /// Commit the rejection list before acknowledging the returned cursor.
    pub fn sync_scan(&self, repo: &str, after: i64) -> Result<ExportScan> {
        self.export_range(repo, after, true)
    }

    fn export_range(&self, repo: &str, after: i64, report_invalid: bool) -> Result<ExportScan> {
        if !(0..=MAX_COUNTER).contains(&after) {
            return Err(VcmError::InvalidSync("sequence is out of range"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let replica = self.sync_replica()?;
        let epoch = self.sync_epoch(repo)?;
        let mut page = Page {
            format: 1,
            replica,
            epoch,
            through: after,
            more: false,
            changes: Vec::new(),
        };
        let rows = self.connection.prepare("SELECT id,sequence,counter,replica,deleted,claim_counter,claim_replica FROM memory_sync_changes WHERE repo=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?
            .query_map(params![repo,after,(MAX_PAGE_RECORDS+1) as i64], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,Stamp{counter:r.get(2)?,replica:r.get(3)?},r.get::<_,bool>(4)?,Stamp{counter:r.get(5)?,replica:r.get(6)?})))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        let overhead = serde_json::to_vec(&page)?.len() + 128;
        let mut bytes = overhead;
        let mut rejected = Vec::new();
        for (id, sequence, version, deleted, claim) in rows {
            if page.changes.len() + rejected.len() == MAX_PAGE_RECORDS {
                page.more = true;
                break;
            }
            let id =
                Uuid::parse_str(&id).map_err(|_| VcmError::InvalidSync("stored ID is invalid"))?;
            let record = if deleted {
                None
            } else {
                let mut record = self.get(id)?;
                record.repo.clear();
                Some(record)
            };
            let change = Change {
                id,
                version,
                claim,
                deleted,
                record,
            };
            let size = serde_json::to_vec(&change)?.len() + 1;
            if report_invalid {
                let validation = if overhead + size > MAX_PAGE_BYTES {
                    Err(VcmError::InvalidSync(
                        "one memory exceeds the page size limit",
                    ))
                } else {
                    change.validate()
                };
                if let Err(VcmError::InvalidSync(reason)) = validation {
                    rejected.push(RejectedRevision {
                        id,
                        sequence,
                        reason,
                    });
                    page.through = sequence;
                    continue;
                }
                validation?;
            }
            if bytes + size > MAX_PAGE_BYTES {
                if page.changes.is_empty() {
                    return Err(VcmError::InvalidSync(
                        "one memory exceeds the page size limit",
                    ));
                }
                page.more = true;
                break;
            }
            bytes += size;
            page.changes.push(change);
            page.through = sequence;
        }
        if !page.more {
            page.through = self.connection.query_row(
                "SELECT sequence FROM memory_sync_clock WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
        }
        transaction.commit()?;
        Ok(ExportScan { page, rejected })
    }

    /// Merges a page atomically. Deletes dominate edits, and newer clear epochs
    /// discard older history. Concurrent edits use logical revision ordering.
    /// Existing evidence binding is preserved; imported text gains no trust.
    pub fn sync_apply(&self, repo: &str, page: &Page) -> Result<()> {
        validate(page)?;
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        let transaction = self.connection.unchecked_transaction()?;
        self.merge_page(repo, page)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn merge_page(&self, repo: &str, page: &Page) -> Result<()> {
        let local_epoch = self.sync_epoch(repo)?;
        let greatest = page
            .changes
            .iter()
            .map(|c| c.version.counter)
            .chain([page.epoch.counter])
            .max()
            .unwrap_or(0);
        self.connection.execute(
            "UPDATE memory_sync_clock SET counter=max(counter,?1),applying=1 WHERE singleton=1",
            [greatest],
        )?;
        if page.epoch < local_epoch {
            self.connection.execute(
                "UPDATE memory_sync_clock SET applying=0 WHERE singleton=1",
                [],
            )?;
            return Ok(());
        }
        let mut invalidated = false;
        if page.epoch > local_epoch {
            self.connection
                .execute("DELETE FROM memories WHERE repo=?1", [repo])?;
            self.connection
                .execute("DELETE FROM memory_sync_changes WHERE repo=?1", [repo])?;
            invalidated = true;
        }
        self.connection.execute("INSERT INTO memory_sync_projects VALUES (?1,?2,?3) ON CONFLICT(repo) DO UPDATE SET counter=excluded.counter,replica=excluded.replica", params![repo,page.epoch.counter,page.epoch.replica])?;
        for change in &page.changes {
            let known=self.connection.query_row("SELECT counter,replica,deleted,claim_counter,claim_replica FROM memory_sync_changes WHERE repo=?1 AND id=?2", params![repo,change.id.to_string()], |r| Ok((Stamp{counter:r.get(0)?,replica:r.get(1)?},r.get::<_,bool>(2)?,Stamp{counter:r.get(3)?,replica:r.get(4)?}))).optional()?;
            if let Some((version, deleted, claim)) = &known {
                if (*deleted && !change.deleted)
                    || (*deleted && change.deleted && *version >= change.version)
                    || (!*deleted
                        && !change.deleted
                        && (claim, version) >= (&change.claim, &change.version))
                {
                    continue;
                }
            }
            let previous = match self.get(change.id) {
                Ok(r) => Some(r),
                Err(VcmError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if previous.as_ref().is_some_and(|r| r.repo != repo) {
                return Err(VcmError::InvalidSync(
                    "record ID belongs to another project",
                ));
            }
            if change.deleted {
                self.connection
                    .execute("DELETE FROM memories WHERE id=?1", [change.id.to_string()])?;
            } else if let Some(record) = &change.record {
                let mut record = record.clone();
                record.repo = repo.to_string();
                if let Some(old) = &previous {
                    record.use_count = record.use_count.max(old.use_count);
                    record.success_count = record
                        .success_count
                        .max(old.success_count)
                        .min(record.use_count);
                    record.last_retrieved_at = record.last_retrieved_at.max(old.last_retrieved_at);
                    self.update(&record)?;
                } else {
                    self.insert(&record)?;
                }
            }
            invalidated |= previous.is_some() || change.deleted;
            self.connection.execute(
                "UPDATE memory_sync_clock SET sequence=sequence+1 WHERE singleton=1",
                [],
            )?;
            self.connection.execute("INSERT INTO memory_sync_changes SELECT ?1,?2,sequence,?3,?4,?5,?6,?7 FROM memory_sync_clock WHERE singleton=1 ON CONFLICT(repo,id) DO UPDATE SET sequence=excluded.sequence,counter=excluded.counter,replica=excluded.replica,deleted=excluded.deleted,claim_counter=excluded.claim_counter,claim_replica=excluded.claim_replica", params![repo,change.id.to_string(),change.version.counter,change.version.replica,change.deleted,change.claim.counter,change.claim.replica])?;
        }
        if invalidated {
            self.bump_generation(repo)?;
        }
        self.connection.execute(
            "UPDATE memory_sync_clock SET applying=0 WHERE singleton=1",
            [],
        )?;
        Ok(())
    }

    /// Reads durable acknowledged progress for a project/endpoint pair.
    pub fn sync_progress(&self, repo: &str, peer: &str) -> Result<Progress> {
        Ok(self
            .connection
            .query_row(
                "SELECT replica,sent,received FROM memory_sync_peers WHERE repo=?1 AND peer=?2",
                params![repo, peer],
                |r| {
                    Ok(Progress {
                        replica: r.get(0)?,
                        sent: r.get(1)?,
                        received: r.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Endpoint names with durable progress, including hosts whose chats have
    /// since moved back. This lets reconnect discover their project replicas.
    pub fn sync_peers(&self, repo: &str) -> Result<Vec<String>> {
        Ok(self
            .connection
            .prepare("SELECT peer FROM memory_sync_peers WHERE repo=?1 ORDER BY peer")?
            .query_map([repo], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Saves progress only after the peer acknowledged the sent page and its
    /// returned page was committed. A changed replica resets both cursors.
    pub fn sync_ack(&self, repo: &str, peer: &str, progress: &Progress) -> Result<()> {
        if !(0..=MAX_COUNTER).contains(&progress.sent)
            || !(0..=MAX_COUNTER).contains(&progress.received)
            || Uuid::parse_str(&progress.replica).is_err()
        {
            return Err(VcmError::InvalidSync("invalid progress"));
        }
        self.connection.execute("INSERT INTO memory_sync_peers VALUES (?1,?2,?3,?4,?5) ON CONFLICT(repo,peer) DO UPDATE SET replica=excluded.replica,sent=CASE WHEN replica=excluded.replica THEN max(sent,excluded.sent) ELSE excluded.sent END,received=CASE WHEN replica=excluded.replica THEN max(received,excluded.received) ELSE excluded.received END", params![repo,peer,progress.replica,progress.sent,progress.received])?;
        Ok(())
    }
}

fn valid_stamp(stamp: &Stamp, initial: bool) -> bool {
    (0..=MAX_COUNTER).contains(&stamp.counter)
        && if stamp.counter == 0 {
            initial && stamp.replica.is_empty()
        } else {
            Uuid::parse_str(&stamp.replica).is_ok()
        }
}

pub(crate) fn validate(page: &Page) -> Result<()> {
    if page.format != 1
        || Uuid::parse_str(&page.replica).is_err()
        || !valid_stamp(&page.epoch, true)
        || !(0..=MAX_COUNTER).contains(&page.through)
        || page.changes.len() > MAX_PAGE_RECORDS
        || serde_json::to_vec(page)?.len() > MAX_PAGE_BYTES
    {
        return Err(VcmError::InvalidSync("malformed or oversized page"));
    }
    let mut ids = std::collections::HashSet::new();
    for change in &page.changes {
        if !ids.insert(change.id) {
            return Err(VcmError::InvalidSync("duplicate record revision"));
        }
        change.validate()?;
    }
    Ok(())
}

impl Change {
    /// Checks the revision, claim binding, record scope and relative file paths.
    pub fn validate(&self) -> Result<()> {
        if !valid_stamp(&self.version, false)
            || !valid_stamp(&self.claim, false)
            || self.claim.counter == 0
            || self.claim.counter > self.version.counter
            || self.version.counter == 0
            || self.deleted == self.record.is_some()
        {
            return Err(VcmError::InvalidSync("invalid record revision"));
        }
        if let Some(record) = &self.record {
            if record.id != self.id
                || !record.repo.is_empty()
                || record.title.trim().is_empty()
                || record.content.trim().is_empty()
                || record.claim_hash != claim_hash(&record.title, &record.content)
                || !record.confidence.is_finite()
                || !(0.0..=1.0).contains(&record.confidence)
                || record.success_count > record.use_count
            {
                return Err(VcmError::InvalidSync("invalid record or scope"));
            }
            if record.files.iter().any(|f| {
                f.starts_with('/')
                    || f.starts_with('\\')
                    || f.contains(':')
                    || f.replace('\\', "/").split('/').any(|part| part == "..")
            }) {
                return Err(VcmError::InvalidSync(
                    "source files must be project-relative",
                ));
            }
        }
        Ok(())
    }
}

impl SharedMemoryStore {
    /// See [`MemoryStore::sync_peers`].
    pub fn sync_peers(&self, repo: &str) -> Result<Vec<String>> {
        self.lock()?.sync_peers(repo)
    }
    /// See [`MemoryStore::sync_replica`].
    pub fn sync_replica(&self) -> Result<String> {
        self.lock()?.sync_replica()
    }
    /// See [`MemoryStore::sync_export`].
    pub fn sync_export(&self, repo: &str, after: i64) -> Result<Page> {
        self.lock()?.sync_export(repo, after)
    }
    /// See [`MemoryStore::sync_scan`].
    pub fn sync_scan(&self, repo: &str, after: i64) -> Result<ExportScan> {
        self.lock()?.sync_scan(repo, after)
    }
    /// See [`MemoryStore::sync_apply`].
    pub fn sync_apply(&self, repo: &str, page: &Page) -> Result<()> {
        self.lock()?.sync_apply(repo, page)
    }
    /// See [`MemoryStore::sync_progress`].
    pub fn sync_progress(&self, repo: &str, peer: &str) -> Result<Progress> {
        self.lock()?.sync_progress(repo, peer)
    }
    /// See [`MemoryStore::sync_ack`].
    pub fn sync_ack(&self, repo: &str, peer: &str, progress: &Progress) -> Result<()> {
        self.lock()?.sync_ack(repo, peer, progress)
    }
}

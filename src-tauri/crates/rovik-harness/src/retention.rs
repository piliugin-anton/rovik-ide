//! Conservative retention: all registered evidence stays; only aged orphan blobs may go.
use crate::{identity::Digest, ledger::Ledger, require, Error, Result};
use serde::Serialize;
use std::{collections::BTreeSet, fs, time::SystemTime};
/// Reviewable retention inventory. Registered evidence and ledger history are preserved.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionReport {
    /// Blobs protected by the durable registry.
    pub retained_registered: u64,
    /// Bytes protected by the durable registry.
    pub retained_bytes: u64,
    /// Unregistered blobs beyond the explicit crash-recovery grace period.
    pub orphan_digests: Vec<Digest>,
    /// True only after the requested orphan deletion committed.
    pub applied: bool,
}
impl Ledger {
    /// Inspect or remove unreferenced CAS blobs while excluding concurrent publication.
    /// A full day is the minimum grace; registered history is never pruned here.
    pub fn retention(&self, grace_seconds: u64, apply: bool) -> Result<RetentionReport> {
        require(
            grace_seconds >= 86_400,
            "retention grace must be at least one day",
        )?;
        // Acquire publication ownership first: a publisher registers its blob
        // under a short SQL transaction while still holding a shared lock.
        let publication = self.artifact_lock_file()?;
        publication.lock()?;
        // Publication owns all artifact registration. This exclusive lock keeps
        // the registry stable without holding SQLite's writer during file IO.
        self.verify_artifacts()?;
        let (count, bytes): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(byte_count),0) FROM artifacts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let registered = {
            let mut statement = self.connection.prepare("SELECT digest FROM artifacts")?;
            let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<BTreeSet<_>, _>>()?
        };
        let mut orphans = vec![];
        for entry in fs::read_dir(self.root.join("artifacts"))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(digest) = Digest::parse(&format!("sha256:{name}")) else {
                continue;
            };
            if registered.contains(digest.as_str()) {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path())?;
            require(
                meta.is_file() && !meta.file_type().is_symlink(),
                "retention encountered a nonregular artifact",
            )?;
            let age = SystemTime::now()
                .duration_since(meta.modified()?)
                .map_err(|_| Error::new("artifact modification time is in the future".into()))?;
            if age.as_secs() >= grace_seconds {
                orphans.push(digest);
            }
        }
        orphans.sort();
        if apply {
            for digest in &orphans {
                fs::remove_file(self.root.join("artifacts").join(digest.hex()))?;
            }
            #[cfg(unix)]
            fs::File::open(self.root.join("artifacts"))?.sync_all()?;
        }
        Ok(RetentionReport {
            retained_registered: count as u64,
            retained_bytes: bytes as u64,
            orphan_digests: orphans,
            applied: apply,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_retention_does_not_need_the_sqlite_writer_while_scanning() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("state");
        let ledger = Ledger::open(&root).unwrap();
        ledger
            .store("source", "text/plain", b"retained", 0)
            .unwrap();
        ledger
            .connection
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        let mut concurrent = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
        let transaction = concurrent
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let report = ledger.retention(86400, true).unwrap();
        assert_eq!(report.retained_registered, 1);
        transaction.rollback().unwrap();
    }
}

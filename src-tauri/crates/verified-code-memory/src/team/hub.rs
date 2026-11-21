use super::*;

impl MemoryStore {
    /// Applies an authorized editor's push and records its origin in the same
    /// SQLite transaction. The transport must bind principal, project and device
    /// before calling this method. Epoch changes use an administrative action.
    pub fn team_accept(
        &self,
        repo: &str,
        principal: &str,
        device_replica: &str,
        expected_hub: &str,
        page: &Page,
        readable: impl Fn(&str) -> bool,
    ) -> Result<()> {
        crate::sync::validate(page)?;
        if repo.is_empty() || principal.is_empty() || page.replica != device_replica {
            return Err(VcmError::InvalidSync("project or device identity mismatch"));
        }
        let tx = self.connection.unchecked_transaction()?;
        if self.sync_replica()? != expected_hub || self.sync_epoch(repo)? != page.epoch {
            return Err(VcmError::InvalidSync("hub identity or epoch changed"));
        }
        let clock: i64 = tx.query_row(
            "SELECT counter FROM memory_sync_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO team_devices VALUES (?1,?2,?3)",
            params![repo, device_replica, principal],
        )?;
        let owner: String = tx.query_row(
            "SELECT principal FROM team_devices WHERE repo=?1 AND replica=?2",
            params![repo, device_replica],
            |r| r.get(0),
        )?;
        if owner != principal {
            return Err(VcmError::InvalidSync(
                "device is bound to a different account",
            ));
        }
        let mut novel = page.clone();
        novel.changes.clear();
        for change in &page.changes {
            if self.team_was_purged(repo, change.id)? {
                return Err(VcmError::InvalidSync("record history was purged"));
            }
            let bytes = serde_json::to_string(change)?;
            let known: Option<String> = tx
                .query_row(
                    "SELECT payload FROM team_history
                WHERE repo=?1 AND id=?2 AND writer=?3 AND counter=?4",
                    params![
                        repo,
                        change.id.to_string(),
                        change.version.replica,
                        change.version.counter
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(known) = known {
                if known != bytes {
                    return Err(VcmError::InvalidSync(
                        "revision was reused for different content",
                    ));
                }
                continue;
            }
            if change.version.replica != device_replica
                || change.version.counter > clock.saturating_add(1_000_000).min(i64::MAX / 4)
            {
                return Err(VcmError::InvalidSync("unregistered or excessive revision"));
            }
            if let Some(record) = &change.record {
                if change.claim.replica == device_replica {
                    tx.execute(
                        "INSERT OR IGNORE INTO team_claims VALUES (?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            repo,
                            change.id.to_string(),
                            change.claim.replica,
                            change.claim.counter,
                            record.title,
                            record.content,
                            principal
                        ],
                    )?;
                }
                let matches:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM team_claims WHERE repo=?1 AND id=?2 AND writer=?3 AND counter=?4 AND title=?5 AND content=?6)",
                    params![repo,change.id.to_string(),change.claim.replica,change.claim.counter,record.title,record.content],|r|r.get(0))?;
                if !matches {
                    return Err(VcmError::InvalidSync("unknown or conflicting base claim"));
                }
                if project_record(record, &readable)? != *record {
                    return Err(VcmError::InvalidSync("record contains device-local fields"));
                }
            }
            tx.execute(
                "INSERT INTO team_history VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    repo,
                    change.id.to_string(),
                    change.version.replica,
                    change.version.counter,
                    principal,
                    bytes
                ],
            )?;
            novel.changes.push(change.clone());
        }
        self.merge_page(repo, &novel)?;
        tx.commit()?;
        Ok(())
    }
}

impl MemoryStore {
    /// Clears shared history once for an authenticated administrative request.
    /// The expected epoch prevents a delayed request from clearing newer work.
    pub fn team_admin_clear(
        &self,
        repo: &str,
        principal: &str,
        hub: &str,
        expected: &Stamp,
        operation: Uuid,
    ) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        if self.sync_replica()? != hub || principal.is_empty() || repo.is_empty() {
            return Err(VcmError::InvalidSync("invalid administrative request"));
        }
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT principal,epoch FROM team_admin WHERE repo=?1 AND operation=?2",
                params![repo, operation.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let epoch = serde_json::to_string(expected)?;
        if let Some(saved) = previous {
            return if saved == (principal.to_string(), epoch) {
                Ok(())
            } else {
                Err(VcmError::InvalidSync(
                    "administrative operation ID was reused",
                ))
            };
        }
        if self.sync_epoch(repo)? != *expected {
            return Err(VcmError::InvalidSync("team epoch changed"));
        }
        tx.execute(
            "UPDATE memory_sync_clock SET applying=1 WHERE singleton=1",
            [],
        )?;
        self.sync_clear(repo)?;
        tx.execute("DELETE FROM memories WHERE repo=?1", [repo])?;
        tx.execute(
            "UPDATE memory_sync_clock SET applying=0 WHERE singleton=1",
            [],
        )?;
        self.bump_generation(repo)?;
        tx.execute(
            "INSERT INTO team_admin VALUES (?1,?2,?3,?4)",
            params![repo, operation.to_string(), principal, epoch],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl MemoryStore {
    fn team_was_purged(&self, repo: &str, id: Uuid) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM team_purges WHERE repo=?1 AND id=?2)",
            params![repo, id.to_string()],
            |row| row.get(0),
        )?)
    }

    /// Removes a withdrawn record's historical payloads. A content-free receipt
    /// fences offline writers and identifies the administrator who performed the purge.
    pub fn team_purge(
        &self,
        repo: &str,
        principal: &str,
        hub: &str,
        epoch: &Stamp,
        id: Uuid,
    ) -> Result<()> {
        self.connection.pragma_update(None, "secure_delete", true)?;
        let tx = self.connection.unchecked_transaction()?;
        if principal.is_empty() || self.sync_replica()? != hub || self.sync_epoch(repo)? != *epoch {
            return Err(VcmError::InvalidSync("hub identity or epoch changed"));
        }
        if self.team_was_purged(repo, id)? {
            return Ok(());
        }
        let key = params![repo, id.to_string()];
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM memories WHERE repo=?1 AND id=?2)",
            key,
            |row| row.get(0),
        )?;
        if live {
            return Err(VcmError::InvalidSync(
                "withdraw the record before purging its history",
            ));
        }
        if tx.execute("DELETE FROM team_history WHERE repo=?1 AND id=?2", key)? == 0 {
            return Err(VcmError::InvalidSync("no revision history for this record"));
        }
        tx.execute("DELETE FROM team_claims WHERE repo=?1 AND id=?2", key)?;
        tx.execute(
            "INSERT INTO team_purges VALUES (?1,?2,?3)",
            params![repo, id.to_string(), principal],
        )?;
        tx.commit()?;
        Ok(())
    }
}

use super::*;

impl MemoryStore {
    /// Applies a hub reply and acknowledges it atomically in the isolated
    /// replica. Save unacknowledged local content before a remote clear or a
    /// competing revision can remove it from the latest-change table.
    pub fn team_receive(
        &self,
        repo: &str,
        peer: &str,
        progress: &crate::sync::Progress,
        sent: i64,
        page: &Page,
    ) -> Result<()> {
        self.team_receive_attributed(repo, peer, progress, sent, page, &[])
    }

    /// Commits incoming records, provenance and cursor together.
    pub fn team_receive_attributed(
        &self,
        repo: &str,
        peer: &str,
        progress: &crate::sync::Progress,
        sent: i64,
        page: &Page,
        authors: &[(Uuid, String, String)],
    ) -> Result<()> {
        crate::sync::validate(page)?;
        let tx = self.connection.unchecked_transaction()?;
        if self.sync_progress(repo, peer)? != *progress
            || (!progress.replica.is_empty() && progress.replica != page.replica)
            || page.through < progress.received
            || sent < progress.sent
            || page.epoch < self.sync_epoch(repo)?
        {
            return Err(VcmError::InvalidSync(
                "team progress changed; rediscover this replica",
            ));
        }
        if page.epoch.counter > i64::MAX / 4
            || page
                .changes
                .iter()
                .any(|c| c.version.counter > i64::MAX / 4)
        {
            return Err(VcmError::InvalidSync("excessive team counter"));
        }
        let clear = page.epoch > self.sync_epoch(repo)?;
        let mut pending=self.connection.prepare("SELECT id, counter FROM memory_sync_changes WHERE repo=?1 AND replica=?2 AND sequence>?3 AND deleted=0")?;
        let device = self.sync_replica()?;
        let rows = pending.query_map(params![repo, device, progress.sent], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, counter) = row?;
            let id = Uuid::parse_str(&id)
                .map_err(|_| VcmError::InvalidSync("invalid pending record ID"))?;
            if clear
                || page
                    .changes
                    .iter()
                    .any(|c| c.id == id && c.version.replica != device)
            {
                let record = self.get(id)?;
                self.connection.execute(
                    "INSERT OR IGNORE INTO team_recovery VALUES (?1,?2,?3,?4,?5)",
                    params![
                        repo,
                        id.to_string(),
                        counter,
                        Uuid::new_v4().to_string(),
                        serde_json::to_string(&record)?
                    ],
                )?;
            }
        }
        self.merge_page(repo, page)?;
        for (id, author, editor) in authors {
            if let Some(change) = page.changes.iter().find(|c| c.id == *id) {
                if let Some(record) = &change.record {
                    let applied:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM memory_sync_changes WHERE repo=?1 AND id=?2 AND counter=?3 AND replica=?4)",params![repo,id.to_string(),change.version.counter,change.version.replica],|r|r.get(0))?;
                    if applied {
                        self.save_attribution(repo, *id, author, editor, &record.claim_hash)?;
                    }
                }
            }
        }
        self.sync_ack(
            repo,
            peer,
            &crate::sync::Progress {
                replica: page.replica.clone(),
                sent,
                received: page.through,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Copies preserved edits into personal memory. A saved recovery UUID makes
    /// a retry after either database commit idempotent.
    pub fn team_restore_pending(
        &self,
        scope: Uuid,
        personal: &MemoryStore,
        repo: &str,
    ) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let target = personal.connection.unchecked_transaction()?;
        let mut query =
            tx.prepare("SELECT recovery_id,payload FROM team_recovery WHERE repo=?1")?;
        let rows = query.query_map([scope.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, payload) = row?;
            let mut record: MemoryRecord = serde_json::from_str(&payload)?;
            let original = record.id;
            record.id =
                Uuid::parse_str(&id).map_err(|_| VcmError::InvalidSync("invalid recovery ID"))?;
            record.repo = repo.to_string();
            let fresh = target.execute(
                "INSERT OR IGNORE INTO team_receipts VALUES (?1,?2)",
                params![scope.to_string(), id],
            )? != 0;
            if fresh {
                target.execute(
                    "INSERT OR IGNORE INTO team_conflicts VALUES (?1,?2,?3,?4)",
                    params![repo, scope.to_string(), original.to_string(), id],
                )?;
                personal.insert(&record)?;
            }
        }
        drop(query);
        target.commit()?;
        tx.execute(
            "DELETE FROM team_recovery WHERE repo=?1",
            [scope.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl MemoryStore {
    /// Whether this replica has verified a complete bootstrap for the peer.
    pub fn team_bootstrapped(&self, repo: &str, peer: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM team_bootstrap WHERE repo=?1 AND peer=?2)",
            params![repo, peer],
            |r| r.get(0),
        )?)
    }

    /// Records completion after the streamed trailer agrees with durable progress.
    pub fn team_finish_bootstrap(
        &self,
        repo: &str,
        peer: &str,
        replica: &str,
        through: i64,
    ) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let p = self.sync_progress(repo, peer)?;
        if p.replica != replica || p.received != through {
            return Err(VcmError::InvalidSync("bootstrap progress changed"));
        }
        tx.execute(
            "INSERT OR REPLACE INTO team_bootstrap VALUES (?1,?2)",
            params![repo, peer],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl MemoryStore {
    /// Reconciles old mappings against a fully bootstrapped replacement before
    /// replaying its pages. Missing or divergent personal copies remain visible
    /// for review, including changes the restored server may once have accepted.
    pub fn team_rebind(&self, repo: &str, link: Uuid, replacement: &MemoryStore) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let scope = link.to_string();
        let mut q =
            tx.prepare("SELECT team_id,local_id FROM team_mappings WHERE repo=?1 AND link=?2")?;
        let pairs = q
            .query_map(params![repo, scope], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(q);
        for (id, local) in pairs {
            let parse = |v: &str| {
                Uuid::parse_str(v).map_err(|_| VcmError::InvalidSync("invalid team mapping"))
            };
            let current = match self.get(parse(&local)?) {
                Ok(r) => Some(r),
                Err(VcmError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            let new = match replacement.get(parse(&id)?) {
                Ok(r) => Some(r),
                Err(VcmError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            let matches = match (current.as_ref(), new.as_ref()) {
                (Some(personal), Some(remote)) if personal.repo == repo && remote.repo == scope => {
                    project_record(personal, |_| true).is_ok_and(|mut p| {
                        p.id = remote.id;
                        p.repo = scope.clone();
                        p.same_context_claim(remote)
                    })
                }
                // A deliberate personal deletion is still suppressed.
                (None, Some(_)) => true,
                _ => false,
            };
            if !matches {
                if current.is_some() {
                    tx.execute(
                        "INSERT OR IGNORE INTO team_conflicts VALUES (?1,?2,?3,?4)",
                        params![repo, scope, id, local],
                    )?;
                }
                tx.execute(
                    "DELETE FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
                    params![repo, scope, id],
                )?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO team_imports VALUES (?1,?2,?3,?4,0)",
            params![
                repo,
                scope,
                replacement.sync_replica()?,
                serde_json::to_string(&replacement.sync_epoch(&scope)?)?
            ],
        )?;
        tx.execute(
            "DELETE FROM team_outbox WHERE repo=?1 AND link=?2",
            params![repo, scope],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Changes only the server identity after restoring a stopped hub's
    /// database from backup. Clients then reconcile rather than trusting an old cursor.
    pub fn team_rotate_restored_hub(&self) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection.execute(
            "UPDATE memory_sync_clock SET replica=?1 WHERE singleton=1",
            [&id],
        )?;
        Ok(id)
    }
}

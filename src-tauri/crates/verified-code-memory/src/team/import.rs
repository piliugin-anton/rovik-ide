use super::*;

impl MemoryStore {
    /// Sequence already materialized from the local team replica.
    pub fn team_import_cursor(&self, repo: &str, link: Uuid) -> Result<i64> {
        Ok(self
            .connection
            .query_row(
                "SELECT through FROM team_imports WHERE repo=?1 AND link=?2",
                params![repo, link.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Lists personal records preserved when a competing team change arrived.
    pub fn team_conflicts(&self, repo: &str, link: Uuid) -> Result<Vec<String>> {
        let mut q=self.connection.prepare("SELECT c.local_id FROM team_conflicts c JOIN memories m ON m.repo=c.repo AND m.id=c.local_id WHERE c.repo=?1 AND c.link=?2 ORDER BY c.local_id")?;
        let rows = q
            .query_map(params![repo, link.to_string()], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Marks a preserved edit reviewed. Its personal record remains available.
    pub fn team_resolve(&self, repo: &str, link: Uuid, local: Uuid) -> Result<()> {
        self.connection.execute(
            "DELETE FROM team_conflicts WHERE repo=?1 AND link=?2 AND local_id=?3",
            params![repo, link.to_string(), local.to_string()],
        )?;
        Ok(())
    }

    /// Imports one contiguous page from a trusted, policy-filtered local team
    /// replica. The link identifies a particular hub project and local replica.
    /// Replace a lost replica through explicit reconciliation before resuming.
    pub fn team_import(&self, repo: &str, link: Uuid, after: i64, page: &Page) -> Result<()> {
        self.team_import_localized(repo, link, after, page, &std::collections::HashMap::new())
    }

    /// Imports a team page using applicability computed outside the transaction.
    pub fn team_import_localized(
        &self,
        repo: &str,
        link: Uuid,
        after: i64,
        page: &Page,
        applicability: &std::collections::HashMap<Uuid, Applicability>,
    ) -> Result<()> {
        self.team_import_attributed(
            repo,
            link,
            after,
            page,
            applicability,
            &std::collections::HashMap::new(),
        )
    }

    /// Materializes authenticated attribution and the import cursor in one transaction.
    pub fn team_import_attributed(
        &self,
        repo: &str,
        link: Uuid,
        after: i64,
        page: &Page,
        applicability: &std::collections::HashMap<Uuid, Applicability>,
        authors: &std::collections::HashMap<Uuid, (String, String)>,
    ) -> Result<()> {
        crate::sync::validate(page)?;
        if repo.trim().is_empty() || after < 0 || page.through < after {
            return Err(VcmError::InvalidSync("invalid team import scope or cursor"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let link_id = link;
        let link = link.to_string();
        let saved: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT replica,epoch,through FROM team_imports WHERE repo=?1 AND link=?2",
                params![repo, link],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let mut previous = Stamp::default();
        let mut cursor = 0;
        let previously_linked = saved.is_some();
        if let Some((replica, epoch, through)) = saved {
            if replica != page.replica {
                return Err(VcmError::InvalidSync(
                    "team replica changed; reconcile the link",
                ));
            }
            previous = serde_json::from_str(&epoch)?;
            cursor = through;
        }
        if cursor != after || page.epoch < previous {
            return Err(VcmError::InvalidSync(
                "team import progress changed; rescan",
            ));
        }
        if previously_linked && page.epoch > previous {
            let mut statement =
                tx.prepare("SELECT team_id FROM team_mappings WHERE repo=?1 AND link=?2")?;
            let ids = statement
                .query_map(params![repo, link], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in ids {
                self.import_record(repo, &link, &id, None, Applicability::Unknown)?;
            }
        }
        for change in &page.changes {
            self.import_record(
                repo,
                &link,
                &change.id.to_string(),
                change.record.as_ref(),
                applicability.get(&change.id).copied().unwrap_or_default(),
            )?;
        }
        for change in &page.changes {
            if let (Some(record), Some((author, editor)), Some(local)) = (
                &change.record,
                authors.get(&change.id),
                self.team_local_id(repo, link_id, change.id)?,
            ) {
                self.save_attribution(repo, local, author, editor, &record.claim_hash)?;
            }
        }
        tx.execute(
            "INSERT INTO team_imports VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(repo,link) DO UPDATE SET epoch=excluded.epoch,through=excluded.through",
            params![
                repo,
                link,
                page.replica,
                serde_json::to_string(&page.epoch)?,
                page.through
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn import_record(
        &self,
        repo: &str,
        link: &str,
        team_id: &str,
        incoming: Option<&MemoryRecord>,
        applicability: Applicability,
    ) -> Result<()> {
        let mapping: Option<(String, String)> = self.connection.query_row(
            "SELECT local_id,baseline FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
            params![repo, link, team_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let mut local = None;
        if let Some((id, baseline)) = mapping {
            let id = Uuid::parse_str(&id)
                .map_err(|_| VcmError::InvalidSync("invalid mapped memory ID"))?;
            match self.get(id) {
                Ok(record) => {
                    if record.repo != repo {
                        return Err(VcmError::InvalidSync(
                            "mapped memory belongs to another project",
                        ));
                    }
                    let mut comparison = record.clone();
                    normalize_local_fields(&mut comparison);
                    let base: MemoryRecord = serde_json::from_str(&baseline)?;
                    let pending:bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM team_outbox WHERE repo=?1 AND link=?2 AND team_id=?3)",params![repo,link,team_id], |r|r.get(0))?;
                    if comparison.same_context_claim(&base) && !pending {
                        local = Some(record);
                    } else {
                        // Preserve the user's pending version as a normal local memory.
                        self.connection.execute(
                            "INSERT OR IGNORE INTO team_conflicts VALUES (?1,?2,?3,?4)",
                            params![repo, link, team_id, id.to_string()],
                        )?;
                    }
                }
                Err(VcmError::NotFound(_)) => {
                    // A personal deletion stays personal. Do not recreate it on update.
                    if incoming.is_some() {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(incoming) = incoming {
            let mut record = incoming.clone();
            record.id = local.as_ref().map_or_else(Uuid::new_v4, |old| old.id);
            record.repo = repo.to_owned();
            record.source_episode = None;
            normalize_local_fields(&mut record);
            if let Some(old) = &local {
                record.identity.clone_from(&old.identity);
                record.source_episode = old.source_episode;
            }
            let baseline = serde_json::to_string(&record)?;
            record.applicability = applicability;
            if let Some(old) = local {
                record.tags.extend(
                    old.tags
                        .iter()
                        .filter(|tag| tag.starts_with("chat:"))
                        .cloned(),
                );
                record.last_retrieved_at = old.last_retrieved_at;
                record.use_count = old.use_count;
                record.success_count = old.success_count;
                if old != record {
                    self.update(&record)?;
                    self.bump_generation(repo)?;
                }
            } else {
                self.insert(&record)?;
            }
            self.connection.execute("INSERT INTO team_mappings VALUES (?1,?2,?3,?4,?5)
                ON CONFLICT(repo,link,team_id) DO UPDATE SET local_id=excluded.local_id,baseline=excluded.baseline",
                params![repo, link, team_id, record.id.to_string(), baseline])?;
        } else {
            if let Some(record) = local {
                self.connection
                    .execute("DELETE FROM memories WHERE id=?1", [record.id.to_string()])?;
                self.bump_generation(repo)?;
            }
            self.connection.execute(
                "DELETE FROM team_outbox WHERE repo=?1 AND link=?2 AND team_id=?3",
                params![repo, link, team_id],
            )?;
            self.connection.execute(
                "DELETE FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
                params![repo, link, team_id],
            )?;
        }
        Ok(())
    }
}

impl SharedMemoryStore {
    /// See [`MemoryStore::team_import_cursor`].
    pub fn team_import_cursor(&self, repo: &str, link: Uuid) -> Result<i64> {
        self.lock()?.team_import_cursor(repo, link)
    }

    /// See [`MemoryStore::team_import`].
    pub fn team_import(&self, repo: &str, link: Uuid, after: i64, page: &Page) -> Result<()> {
        self.lock()?.team_import(repo, link, after, page)
    }
}

impl MemoryStore {
    /// Authenticated publisher of a live claim and submitter of its latest revision.
    pub fn team_provenance(&self, repo: &str, change: &Change) -> Result<Option<(String, String)>> {
        Ok(self.connection.query_row("SELECT c.principal,h.principal FROM team_claims c JOIN team_history h ON h.repo=c.repo AND h.id=c.id
            WHERE c.repo=?1 AND c.id=?2 AND c.writer=?3 AND c.counter=?4 AND h.writer=?5 AND h.counter=?6",
            params![repo,change.id.to_string(),change.claim.replica,change.claim.counter,change.version.replica,change.version.counter],
            |r|Ok((r.get(0)?,r.get(1)?))).optional()?)
    }
    /// Saves authenticated attribution for a team record. Never infer it from a chat tag.
    pub(super) fn save_attribution(
        &self,
        repo: &str,
        id: Uuid,
        author: &str,
        editor: &str,
        hash: &str,
    ) -> Result<()> {
        self.connection.execute("INSERT INTO team_attribution VALUES (?1,?2,?3,?4,?5) ON CONFLICT(repo,id) DO UPDATE SET author=excluded.author,editor=excluded.editor,claim_hash=excluded.claim_hash",
            params![repo,id.to_string(),author,editor,hash])?;
        Ok(())
    }

    /// Returns the authors recorded by the hub for an imported local record.
    pub fn team_authors(&self, repo: &str, id: Uuid) -> Result<Option<(String, String)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT a.author,a.editor FROM team_attribution a JOIN memories m ON m.repo=a.repo AND m.id=a.id AND m.claim_hash=a.claim_hash WHERE a.repo=?1 AND a.id=?2",
                params![repo, id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Local ID bound to the shared memory within this link.
    pub fn team_local_id(&self, repo: &str, link: Uuid, id: Uuid) -> Result<Option<Uuid>> {
        self.connection
            .query_row(
                "SELECT local_id FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
                params![repo, link.to_string(), id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| {
                Uuid::parse_str(&s).map_err(|_| VcmError::InvalidSync("invalid mapped memory ID"))
            })
            .transpose()
    }
}

impl SharedMemoryStore {
    /// Whether the personal record is mapped to team memory and can be withdrawn.
    pub fn team_can_withdraw(&self, repo: &str, id: Uuid) -> Result<bool> {
        Ok(self.lock()?.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM team_mappings m WHERE repo=?1 AND local_id=?2
             AND NOT EXISTS(SELECT 1 FROM team_withdrawals w WHERE w.repo=m.repo
             AND w.link=m.link AND w.local_id=m.local_id))",
            params![repo, id.to_string()],
            |row| row.get(0),
        )?)
    }

    /// See [`MemoryStore::team_authors`].
    pub fn team_authors(&self, repo: &str, id: Uuid) -> Result<Option<(String, String)>> {
        self.lock()?.team_authors(repo, id)
    }
}

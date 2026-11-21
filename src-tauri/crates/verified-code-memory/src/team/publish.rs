use super::*;

/// Produces the exact content for a publication preview, with local fields removed.
/// Supply the project's existing restricted-file read check for each file.
pub fn project_record(
    record: &MemoryRecord,
    readable: impl Fn(&str) -> bool,
) -> Result<MemoryRecord> {
    if record.kind == MemoryKind::Episode
        || record.verification == Verification::Deprecated
        || (!matches!(
            record.source_kind,
            SourceKind::UserAssertion | SourceKind::CodeObservation | SourceKind::TestResult
        ) && record.verification != Verification::Verified)
    {
        return Err(VcmError::InvalidSync(
            "this kind or source is local by default",
        ));
    }
    if record.verification == Verification::Verified && !record.evidence_matches_claim() {
        return Err(VcmError::InvalidSync(
            "verification belongs to a different claim",
        ));
    }
    if record
        .tags
        .iter()
        .any(|t| t.eq_ignore_ascii_case("private") || t.eq_ignore_ascii_case("local-only"))
    {
        return Err(VcmError::InvalidSync("memory is private"));
    }
    if !record.files.iter().all(|f| readable(f)) {
        return Err(VcmError::InvalidSync("memory references a restricted file"));
    }
    let mut out = record.clone();
    out.repo.clear();
    out.identity.clear();
    out.source_episode = None;
    normalize_local_fields(&mut out);
    out.updated_at = out
        .last_validated_at
        .unwrap_or(out.content_updated_at)
        .max(out.content_updated_at);
    let stamp = Stamp {
        counter: 1,
        replica: out.id.to_string(),
    };
    let change = Change {
        id: out.id,
        version: stamp.clone(),
        claim: stamp,
        deleted: false,
        record: Some(out.clone()),
    };
    change.validate()?;
    if out.files.iter().any(|f| {
        f.trim().is_empty()
            || f.contains(char::is_control)
            || f.split(['/', '\\']).any(|p| p.is_empty() || p == ".")
    }) {
        return Err(VcmError::InvalidSync("invalid team source path"));
    }
    let encoded = serde_json::to_vec(&out)?;
    if encoded.len() > 64 * 1024 {
        return Err(VcmError::InvalidSync("team memory exceeds 64 KiB"));
    }
    static SECRETS: std::sync::OnceLock<std::result::Result<regex::Regex, regex::Error>> =
        std::sync::OnceLock::new();
    let pattern = SECRETS.get_or_init(|| regex::Regex::new(
        r"(?i)-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----|gh[pousr]_[a-z0-9_]{16,}|github_pat_[a-z0-9_]+|glpat-[a-z0-9_-]{16,}|eyJ[a-z0-9_-]+\.eyJ[a-z0-9_-]+\.[a-z0-9_-]{12,}|sk-[a-z0-9_-]{16,}|AKIA[0-9A-Z]{16}|xox[baprs]-[a-z0-9-]+|bearer\s+\S+|(?:api[_-]?key|token|password|secret)\s*[:=]\s*\S+"
    )).as_ref().map_err(|_| VcmError::InvalidSync("credential scan unavailable"))?;
    if std::iter::once(&out.title)
        .chain([&out.content])
        .chain(out.evidence.iter())
        .chain(out.tags.iter())
        .chain(out.files.iter())
        .chain(out.code_version.iter())
        .any(|text| pattern.is_match(text))
    {
        return Err(VcmError::InvalidSync(
            "memory contains a possible credential",
        ));
    }
    Ok(out)
}

impl MemoryStore {
    /// Queues initial publication only if the approved preview still matches the
    /// current personal record. Mapping and intent survive interruption together.
    pub fn team_queue_publication(
        &self,
        repo: &str,
        link: Uuid,
        preview: &MemoryRecord,
        readable: impl Fn(&str) -> bool,
    ) -> Result<Uuid> {
        let tx = self.connection.unchecked_transaction()?;
        let personal = self.get(preview.id)?;
        if personal.repo != repo || project_record(&personal, readable)? != *preview {
            return Err(VcmError::InvalidSync(
                "memory changed; review the publication again",
            ));
        }
        let link = link.to_string();
        tx.execute(
            "INSERT OR IGNORE INTO team_consent VALUES (?1,?2,?3)",
            params![repo, link, personal.id.to_string()],
        )?;
        let withdrawn: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM team_withdrawals WHERE repo=?1 AND link=?2 AND local_id=?3)",
            params![repo,link,personal.id.to_string()], |r|r.get(0))?;
        if withdrawn {
            return Err(VcmError::InvalidSync("this memory was withdrawn"));
        }
        let existing: Option<(String,String)> = tx.query_row(
            "SELECT team_id,baseline FROM team_mappings WHERE repo=?1 AND link=?2 AND local_id=?3",
            params![repo,link,personal.id.to_string()], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (id, mut expected) = if let Some((id, baseline)) = existing {
            let id = Uuid::parse_str(&id).map_err(|_| VcmError::InvalidSync("invalid team ID"))?;
            let baseline: MemoryRecord = serde_json::from_str(&baseline)?;
            let mut projected = project_record(&baseline, |_| true)?;
            projected.id = id;
            (id, Some(serde_json::to_string(&projected)?))
        } else {
            (Uuid::new_v4(), None)
        };
        let mut outgoing = preview.clone();
        outgoing.id = id;
        let payload = serde_json::to_string(&outgoing)?;
        // Re-approval replaces an unsent intent while retaining its original base.
        if let Some(previous) = tx
            .query_row(
                "SELECT expected FROM team_outbox WHERE repo=?1 AND link=?2 AND team_id=?3",
                params![repo, link, id.to_string()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
        {
            expected = previous;
        } else if expected.as_ref() == Some(&payload) {
            tx.commit()?;
            return Ok(id);
        }
        let mut baseline = personal;
        normalize_local_fields(&mut baseline);
        tx.execute(
            "INSERT INTO team_mappings VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(repo,link,team_id) DO UPDATE SET baseline=excluded.baseline",
            params![
                repo,
                link,
                id.to_string(),
                baseline.id.to_string(),
                serde_json::to_string(&baseline)?
            ],
        )?;
        tx.execute("INSERT INTO team_outbox VALUES (?1,?2,?3,?4,?5,?6)
            ON CONFLICT(repo,link,team_id) DO UPDATE SET operation=excluded.operation,payload=excluded.payload,expected=excluded.expected",
            params![repo,link,id.to_string(),Uuid::new_v4().to_string(),payload,expected])?;
        tx.commit()?;
        Ok(id)
    }

    /// Queues a team withdrawal while retaining the personal record and a
    /// durable marker so future scans cannot re-publish it automatically.
    pub fn team_withdraw(&self, repo: &str, link: Uuid, id: Uuid) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let link = link.to_string();
        let key = id.to_string();
        let (team, baseline): (String, String) = tx.query_row(
            "SELECT team_id,baseline FROM team_mappings WHERE repo=?1 AND link=?2 AND local_id=?3",
            params![repo, link, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut expected = project_record(&serde_json::from_str(&baseline)?, |_| true)?;
        expected.id =
            Uuid::parse_str(&team).map_err(|_| VcmError::InvalidSync("invalid mapped team ID"))?;
        tx.execute(
            "INSERT OR IGNORE INTO team_withdrawals VALUES (?1,?2,?3,?4)",
            params![repo, link, key, team],
        )?;
        tx.execute(
            "INSERT INTO team_outbox VALUES (?1,?2,?3,?4,NULL,?5)
            ON CONFLICT(repo,link,team_id) DO UPDATE SET operation=excluded.operation,payload=NULL",
            params![
                repo,
                link,
                team,
                Uuid::new_v4().to_string(),
                serde_json::to_string(&expected)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Drains one approved intent into an isolated link store. The receiving
    /// transaction creates a local team stamp and a replay receipt together.
    pub fn team_flush_publication(
        &self,
        repo: &str,
        link: Uuid,
        replica: &MemoryStore,
        readable: impl Fn(&str) -> bool,
    ) -> Result<bool> {
        if self.sync_replica()? == replica.sync_replica()? {
            return Err(VcmError::InvalidSync("team link needs a separate replica"));
        }
        let source = self.connection.unchecked_transaction()?;
        struct Publication {
            operation: String,
            payload: Option<String>,
            expected: Option<String>,
            local_id: String,
            team_id: String,
        }
        let next = source.query_row(
            "SELECT o.operation,o.payload,o.expected,m.local_id,m.team_id FROM team_outbox o JOIN team_mappings m USING(repo,link,team_id)
             WHERE o.repo=?1 AND o.link=?2 ORDER BY o.rowid LIMIT 1",
            params![repo,link.to_string()], |row| Ok(Publication {
                operation: row.get(0)?, payload: row.get(1)?, expected: row.get(2)?,
                local_id: row.get(3)?, team_id: row.get(4)?,
            }),
        ).optional()?;
        let Some(Publication {
            operation,
            payload,
            expected,
            local_id,
            team_id,
        }) = next
        else {
            return Ok(false);
        };
        let target = replica.connection.unchecked_transaction()?;
        let scope = link.to_string();
        let fresh = target.execute(
            "INSERT OR IGNORE INTO team_receipts VALUES (?1,?2)",
            params![scope, operation],
        )? != 0;
        let applied = (|| -> Result<()> {
            if fresh {
                let team_id = Uuid::parse_str(&team_id)
                    .map_err(|_| VcmError::InvalidSync("invalid mapped team ID"))?;
                let old = match replica.get(team_id) {
                    Ok(mut r) => {
                        if r.repo != scope {
                            return Err(VcmError::InvalidSync(
                                "team record belongs to another link",
                            ));
                        }
                        r.repo.clear();
                        Some(serde_json::to_string(&r)?)
                    }
                    Err(VcmError::NotFound(_)) => None,
                    Err(e) => return Err(e),
                };
                if old != expected {
                    return Err(VcmError::InvalidSync(
                        "team revision changed; resolve the competing edit",
                    ));
                }
                if let Some(data) = &payload {
                    let mut outgoing: MemoryRecord = serde_json::from_str(data)?;
                    let id = Uuid::parse_str(&local_id)
                        .map_err(|_| VcmError::InvalidSync("invalid mapped memory ID"))?;
                    let current = self.get(id)?;
                    let mut projection = project_record(&current, readable)?;
                    projection.id = team_id;
                    if current.repo != repo || projection != outgoing {
                        return Err(VcmError::InvalidSync(
                            "queued memory changed; review it again",
                        ));
                    }
                    outgoing.repo = scope.clone();
                    if old.is_some() {
                        replica.update(&outgoing)?;
                    } else {
                        replica.insert(&outgoing)?;
                    }
                } else if old.is_some() {
                    target.execute(
                        "DELETE FROM memories WHERE repo=?1 AND id=?2",
                        params![scope, team_id.to_string()],
                    )?;
                }
            }
            Ok(())
        })();
        let blocked = match applied {
            Ok(()) => {
                target.commit()?;
                None
            }
            Err(VcmError::InvalidSync(reason)) => Some(reason),
            Err(VcmError::NotFound(_)) => Some("personal record was deleted"),
            Err(e) => return Err(e),
        };
        if let Some(reason) = blocked {
            let id = Uuid::parse_str(&local_id)
                .map_err(|_| VcmError::InvalidSync("invalid mapped memory ID"))?;
            self.team_block(repo, link, id, reason)?;
            // An initial publication has no remote base. Retire its mapping so
            // a later explicit approval starts cleanly.
            if expected.is_none() {
                source.execute(
                    "DELETE FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
                    params![repo, scope, team_id],
                )?;
            } else {
                source.execute(
                    "INSERT OR IGNORE INTO team_conflicts VALUES (?1,?2,?3,?4)",
                    params![repo, scope, team_id, local_id],
                )?;
            }
        }
        if payload.is_none() && blocked.is_none() {
            source.execute(
                "DELETE FROM team_mappings WHERE repo=?1 AND link=?2 AND team_id=?3",
                params![repo, scope, team_id],
            )?;
        }
        source.execute("DELETE FROM team_outbox WHERE operation=?1", [operation])?;
        source.commit()?;
        Ok(true)
    }
}

impl MemoryStore {
    /// Rechecks record consent and current policy immediately before a network
    /// push. Local deletions do not authorize publishing team tombstones.
    pub fn team_check_push(
        &self,
        repo: &str,
        link: Uuid,
        change: &Change,
        readable: impl Fn(&str) -> bool,
    ) -> Result<()> {
        if change.deleted {
            let withdrawn:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM team_withdrawals WHERE repo=?1 AND link=?2 AND team_id=?3)",params![repo,link.to_string(),change.id.to_string()],|r|r.get(0))?;
            return if withdrawn {
                Ok(())
            } else {
                Err(VcmError::InvalidSync("withdrawal requires consent"))
            };
        }
        let local:Option<String>=self.connection.query_row("SELECT m.local_id FROM team_mappings m JOIN team_consent c USING(repo,link,local_id) WHERE m.repo=?1 AND m.link=?2 AND m.team_id=?3",
            params![repo,link.to_string(),change.id.to_string()],|r|r.get(0)).optional()?;
        let id =
            Uuid::parse_str(&local.ok_or(VcmError::InvalidSync("publication has no consent"))?)
                .map_err(|_| VcmError::InvalidSync("invalid mapped memory ID"))?;
        let mut projection = project_record(&self.get(id)?, readable)?;
        projection.id = change.id;
        if Some(&projection) != change.record.as_ref() {
            return Err(VcmError::InvalidSync(
                "publication no longer matches the local memory",
            ));
        }
        Ok(())
    }

    /// Saves a blocked record ID and a content-free reason for Settings.
    pub fn team_block(&self, repo: &str, link: Uuid, id: Uuid, reason: &str) -> Result<()> {
        self.connection.execute("INSERT INTO team_blocked VALUES (?1,?2,?3,?4) ON CONFLICT(repo,link,id) DO UPDATE SET reason=excluded.reason",params![repo,link.to_string(),id.to_string(),reason])?;
        Ok(())
    }

    /// Most recent blocked IDs and their reasons, bounded for Settings.
    pub fn team_blocked(&self, repo: &str, link: Uuid) -> Result<Vec<(String, String)>> {
        let mut q=self.connection.prepare("SELECT id,reason FROM team_blocked WHERE repo=?1 AND link=?2 ORDER BY rowid DESC LIMIT 50")?;
        let rows = q
            .query_map(params![repo, link.to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Captures later desktop edits of explicitly approved IDs. An edit from
    /// a personal host requires fresh approval even if the record was shared.
    pub fn team_scan_approved(
        &self,
        repo: &str,
        link: Uuid,
        readable: impl Fn(&str) -> bool,
    ) -> Result<bool> {
        let cursor: i64 = self
            .connection
            .query_row(
                "SELECT through FROM team_scan WHERE repo=?1 AND link=?2",
                params![repo, link.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let export = self.sync_scan(repo, cursor)?;
        let writer = self.sync_replica()?;
        for rejected in export.rejected {
            self.team_block(repo, link, rejected.id, rejected.reason)?;
        }
        for change in export.page.changes {
            let Some(mut r) = change.record else { continue };
            let consent:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM team_consent c JOIN team_mappings m USING(repo,link,local_id) WHERE c.repo=?1 AND c.link=?2 AND c.local_id=?3 AND NOT EXISTS(SELECT 1 FROM team_withdrawals w WHERE w.repo=c.repo AND w.link=c.link AND w.local_id=c.local_id))",params![repo,link.to_string(),r.id.to_string()],|r|r.get(0))?;
            if !consent {
                continue;
            }
            if change.version.replica != writer {
                self.team_block(repo, link, r.id, "personal-host change requires review")?;
                continue;
            }
            r.repo = repo.to_string();
            match project_record(&r, &readable)
                .and_then(|preview| self.team_queue_publication(repo, link, &preview, &readable))
            {
                Ok(_) => {
                    self.connection.execute(
                        "DELETE FROM team_blocked WHERE repo=?1 AND link=?2 AND id=?3",
                        params![repo, link.to_string(), r.id.to_string()],
                    )?;
                }
                Err(VcmError::InvalidSync(reason)) => self.team_block(repo, link, r.id, reason)?,
                Err(VcmError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        self.connection.execute("INSERT INTO team_scan VALUES (?1,?2,?3) ON CONFLICT(repo,link) DO UPDATE SET through=max(through,excluded.through)",params![repo,link.to_string(),export.page.through])?;
        Ok(export.page.more)
    }
}

impl MemoryStore {
    /// Rescans consent after a policy change. Requeue blocked team revisions
    /// with their original stamps so an accepted retry remains idempotent.
    pub fn team_retry_blocked(&self, repo: &str, link: Uuid, replica: &MemoryStore) -> Result<()> {
        let scope = link.to_string();
        let target = replica.connection.unchecked_transaction()?;
        let source = self.connection.unchecked_transaction()?;
        let mut ids = std::collections::BTreeSet::new();
        for (db, scope_key) in [(self, repo), (replica, scope.as_str())] {
            let mut q = db
                .connection
                .prepare("SELECT id FROM team_blocked WHERE repo=?1 AND link=?2")?;
            for id in q.query_map(params![scope_key, scope], |r| r.get::<_, String>(0))? {
                ids.insert(id?);
            }
        }
        for id in ids {
            target.execute(
                "UPDATE memory_sync_clock SET sequence=sequence+1 WHERE singleton=1",
                [],
            )?;
            target.execute("UPDATE memory_sync_changes SET sequence=(SELECT sequence FROM memory_sync_clock WHERE singleton=1) WHERE repo=?1 AND id=?2",params![scope,id])?;
        }
        target.execute(
            "DELETE FROM team_blocked WHERE repo=?1 AND link=?2",
            params![scope, scope],
        )?;
        target.commit()?;
        source.execute(
            "DELETE FROM team_scan WHERE repo=?1 AND link=?2",
            params![repo, scope],
        )?;
        source.execute(
            "DELETE FROM team_blocked WHERE repo=?1 AND link=?2",
            params![repo, scope],
        )?;
        source.commit()?;
        Ok(())
    }
}

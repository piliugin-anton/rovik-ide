use crate::error::{Result, VcmError};
use crate::model::{
    claim_hash, Applicability, MemoryKind, MemoryRecord, MemoryStats, NewMemory, RetrievalQuery,
    RetrievedMemory, SourceKind, Verification,
};
use crate::retrieval::{
    bm25_relevance, build_fts_query, collect_ranked_candidates, compare_memories,
    deduplicate_memories, estimate_tokens, identifier_fts_query, normalize_path,
    RetrievalTelemetry, RetrievalWeights,
};
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use uuid::Uuid;

const MAX_RETRIEVAL_LIMIT: usize = 32;
// Older writers do not advance replication epochs when clearing history.
// They must refuse to reopen a database that now participates in sync.
pub(crate) const SCHEMA_VERSION: i64 = 4;

/// How many distinct admissible full-text claims one query retains before
/// reranking. Rejected rows and exact copies do not consume this allowance.
const LEXICAL_POOL: usize = 80;

/// How many distinct admissible file claims one query retains.
const FILE_POOL: usize = 40;

/// SQLite-backed memory store with FTS5 lexical retrieval.
#[derive(Debug)]
pub struct MemoryStore {
    pub(crate) connection: Connection,
    weights: RetrievalWeights,
}

impl MemoryStore {
    /// Opens or creates a store.
    ///
    /// # Errors
    /// Returns an error if SQLite cannot be opened or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::initialize(Connection::open(path)?)
    }

    /// Opens an in-memory store.
    ///
    /// # Errors
    /// Returns an error if SQLite cannot be initialized.
    pub fn open_in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(connection: Connection) -> Result<Self> {
        let schema: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema > SCHEMA_VERSION {
            return Err(VcmError::UnsupportedSchema {
                actual: schema,
                supported: SCHEMA_VERSION,
            });
        }
        // The records and the file index first, the full-text index after the
        // columns are settled: FTS5 keeps an external-content index in step
        // with the table through these triggers, and a trigger that fires
        // against an index which never had the row is what SQLite reports as
        // `SQLITE_CORRUPT_VTAB`. So an older file is brought up to date before
        // anything can fire (see `rebuild_full_text`).
        connection.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                verification TEXT NOT NULL,
                applicability TEXT NOT NULL DEFAULT '"unknown"',
                source_kind TEXT NOT NULL DEFAULT '"unknown"',
                repo TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                claim_hash TEXT NOT NULL DEFAULT '',
                identity TEXT NOT NULL DEFAULT '',
                validated_claim_hash TEXT,
                files_json TEXT NOT NULL,
                tags_json TEXT NOT NULL,
                code_version TEXT,
                source_episode TEXT,
                evidence TEXT,
                confidence REAL NOT NULL,
                created_at TEXT NOT NULL,
                content_updated_at TEXT,
                updated_at TEXT NOT NULL,
                last_validated_at TEXT,
                last_retrieved_at TEXT,
                use_count INTEGER NOT NULL DEFAULT 0,
                success_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_memories_repo ON memories(repo);
            CREATE INDEX IF NOT EXISTS idx_memories_verification ON memories(verification);
            CREATE INDEX IF NOT EXISTS idx_memories_kind ON memories(kind);
            CREATE INDEX IF NOT EXISTS idx_memories_updated ON memories(updated_at);
            CREATE INDEX IF NOT EXISTS idx_memories_repo_version ON memories(repo, code_version);
            -- One counter per project, moved whenever its memory is cut down
            -- (a record deleted, or the lot cleared). A write that read the
            -- counter before that — a queued job, a retried step, a run that
            -- started earlier — is refused, so deletion cannot be undone by a
            -- late writer.
            CREATE TABLE IF NOT EXISTS memory_generations (
                repo TEXT PRIMARY KEY,
                generation INTEGER NOT NULL
            ) WITHOUT ROWID;

            -- Every file a memory names, one row each, as `normalize_path`
            -- writes it: asking which memories are about a path is then a
            -- lookup, not a scan that parses every record's JSON. The store
            -- writes these rows with the record (`insert`, `update`); the
            -- trigger keeps them from outliving it.
            CREATE TABLE IF NOT EXISTS memory_files (
                memory_id TEXT NOT NULL,
                path TEXT NOT NULL,
                PRIMARY KEY (memory_id, path)
            ) WITHOUT ROWID;

            CREATE INDEX IF NOT EXISTS idx_memory_files_path ON memory_files(path);

            CREATE TRIGGER IF NOT EXISTS memories_files_ad AFTER DELETE ON memories BEGIN
                DELETE FROM memory_files WHERE memory_id = old.id;
            END;
            "#,
        )?;

        add_missing_columns(&connection)?;
        // After the columns: an older file gains `identity` there, and an index
        // over a column that isn't there yet is an error, not a no-op.
        connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_identity ON memories(repo, identity)",
            [],
        )?;
        backfill_claim_hashes(&connection)?;
        backfill_files(&connection)?;
        create_full_text(&connection)?;
        rebuild_full_text(&connection)?;
        crate::sync::initialize(&connection)?;

        Ok(Self {
            connection,
            weights: RetrievalWeights::default(),
        })
    }

    /// Sets retrieval weights.
    #[must_use]
    pub fn with_weights(mut self, weights: RetrievalWeights) -> Self {
        self.weights = weights;
        self
    }

    /// Creates a provisional memory.
    ///
    /// # Errors
    /// Returns validation or database errors.
    pub fn remember(&self, memory: NewMemory) -> Result<MemoryRecord> {
        validate_new_memory(&memory)?;

        let now = Utc::now();
        let record = MemoryRecord {
            id: Uuid::new_v4(),
            kind: memory.kind,
            verification: Verification::Provisional,
            applicability: Applicability::Unknown,
            source_kind: memory.source_kind,
            repo: memory.repo,
            claim_hash: claim_hash(&memory.title, &memory.content),
            identity: memory.identity,
            validated_claim_hash: None,
            title: memory.title,
            content: memory.content,
            files: memory.files,
            tags: memory.tags,
            code_version: memory.code_version,
            source_episode: memory.source_episode,
            evidence: memory.evidence,
            confidence: memory.confidence,
            created_at: now,
            content_updated_at: now,
            updated_at: now,
            last_validated_at: None,
            last_retrieved_at: None,
            use_count: 0,
            success_count: 0,
        };

        self.insert(&record)?;
        Ok(record)
    }

    pub(crate) fn insert(&self, record: &MemoryRecord) -> Result<()> {
        let transaction = if self.connection.is_autocommit() {
            Some(self.connection.unchecked_transaction()?)
        } else {
            None
        };
        self.connection.execute(
            r#"INSERT INTO memories (id,kind,verification,applicability,source_kind,repo,title,content,claim_hash,identity,validated_claim_hash,files_json,tags_json,code_version,source_episode,evidence,confidence,created_at,content_updated_at,updated_at,last_validated_at,last_retrieved_at,use_count,success_count) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)"#,
            params![
                record.id.to_string(),
                serde_json::to_string(&record.kind)?,
                serde_json::to_string(&record.verification)?,
                serde_json::to_string(&record.applicability)?,
                serde_json::to_string(&record.source_kind)?,
                record.repo,
                record.title,
                record.content,
                record.claim_hash,
                record.identity,
                record.validated_claim_hash,
                serde_json::to_string(&record.files)?,
                serde_json::to_string(&record.tags)?,
                record.code_version,
                record.source_episode.map(|id| id.to_string()),
                record.evidence,
                record.confidence,
                record.created_at.to_rfc3339(),
                record.content_updated_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
                record.last_validated_at.map(|at| at.to_rfc3339()),
                record.last_retrieved_at.map(|at| at.to_rfc3339()),
                record.use_count,
                record.success_count,
            ],
        )?;
        self.index_files(record.id, &record.files)?;
        if let Some(transaction) = transaction {
            transaction.commit()?;
        }
        Ok(())
    }

    /// Points the file index at the files a memory names, replacing the ones it
    /// had: the record and its rows go in one transaction, so a memory is never
    /// left unfindable by its files.
    ///
    /// The rows hold `normalize_path`'s form, which is what
    /// [`crate::retrieval::path_overlap`] compares — so the query in
    /// `file_candidates` is that comparison in SQL, not an approximation of it.
    fn index_files(&self, id: Uuid, files: &[String]) -> Result<()> {
        self.connection.execute(
            "DELETE FROM memory_files WHERE memory_id = ?1",
            params![id.to_string()],
        )?;

        let mut statement = self
            .connection
            .prepare("INSERT OR IGNORE INTO memory_files (memory_id, path) VALUES (?1, ?2)")?;
        for file in files {
            let path = normalize_path(file);
            if path.is_empty() {
                continue;
            }
            statement.execute(params![id.to_string(), path])?;
        }
        Ok(())
    }

    /// Fetches a memory.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown ID.
    pub fn get(&self, id: Uuid) -> Result<MemoryRecord> {
        self.connection
            .query_row(
                "SELECT * FROM memories WHERE id = ?1",
                params![id.to_string()],
                Self::row_to_record,
            )
            .optional()?
            .ok_or(VcmError::NotFound(id))
    }

    /// Confirms a memory's claim with evidence about that claim: the tests or
    /// review that spoke to what it says, at the text it says now.
    ///
    /// What this records is a fact about this text — the claim hash as it is
    /// stored — so a later edit cannot keep the trust it earned: the evidence
    /// stays, for audit, and stops being evidence about the new text
    /// ([`MemoryRecord::evidence_matches_claim`]).
    ///
    /// # Errors
    /// Returns an error for unknown IDs, empty evidence, or deprecated records.
    pub fn verify(&self, id: Uuid, evidence: impl Into<String>) -> Result<MemoryRecord> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "evidence" });
        }

        let mut record = self.get(id)?;
        if record.verification == Verification::Deprecated {
            return Err(VcmError::Deprecated(id));
        }

        let now = Utc::now();
        record.verification = Verification::Verified;
        record.applicability = Applicability::Current;
        record.evidence = Some(evidence);
        record.validated_claim_hash = Some(record.claim_hash.clone());
        record.last_validated_at = Some(now);
        record.updated_at = now;
        self.update(&record)?;
        Ok(record)
    }

    /// Deprecates a memory: its claim was contradicted, not merely left
    /// unchecked. Being unsure of one is [`Self::mark_needs_revalidation`].
    ///
    /// # Errors
    /// Returns an error for unknown IDs or an empty reason.
    pub fn deprecate(&self, id: Uuid, reason: impl Into<String>) -> Result<MemoryRecord> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "reason" });
        }

        let mut record = self.get(id)?;
        record.verification = Verification::Deprecated;
        record.applicability = Applicability::NeedsRevalidation;
        record.evidence = Some(reason);
        record.updated_at = Utc::now();
        self.update(&record)?;
        Ok(record)
    }

    /// Marks a memory to be checked again, keeping what it says and the
    /// evidence it had.
    ///
    /// This is the response to something that makes a claim doubtable without
    /// contradicting it: the code it names changed, a check failed nearby, its
    /// text was edited. The record keeps its claim and its history; only its
    /// applicability changes, so a run reads it as something to check rather
    /// than something to rely on.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown ID.
    pub fn mark_needs_revalidation(&self, id: Uuid) -> Result<MemoryRecord> {
        let mut record = self.get(id)?;
        // A memory the project already dropped is not asked to be checked
        // again: it is not retrieved, so nothing in a run can make it worth
        // re-examining, and a run's own copy of it may be older than this.
        if record.verification == Verification::Deprecated {
            return Ok(record);
        }
        if record.applicability != Applicability::NeedsRevalidation {
            record.applicability = Applicability::NeedsRevalidation;
            record.updated_at = Utc::now();
            self.update(&record)?;
        }
        Ok(record)
    }

    /// Marks the memories tied to an older repository revision to be checked
    /// again.
    ///
    /// Nothing is deprecated: a different commit is not a contradiction, and a
    /// record about a file the commit didn't touch may still hold. Memories
    /// without a code version are left alone, as their tie to a revision cannot
    /// be read at all.
    ///
    /// # Errors
    /// Returns database or validation errors.
    pub fn mark_stale_for_version(&self, repo: &str, current_version: &str) -> Result<usize> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        if current_version.trim().is_empty() {
            return Err(VcmError::EmptyField {
                field: "current_version",
            });
        }

        let changed = self.connection.execute(
            r#"
            UPDATE memories
            SET applicability = ?3, updated_at = ?4
            WHERE repo = ?1
              AND code_version IS NOT NULL
              AND code_version != ?2
              AND verification != '"deprecated"'
              AND applicability != ?3
            "#,
            params![
                repo,
                current_version,
                serde_json::to_string(&Applicability::NeedsRevalidation)?,
                Utc::now().to_rfc3339(),
            ],
        )?;

        Ok(changed)
    }

    /// The project's records with this identity, newest first: what a later
    /// record with the same identity revises.
    ///
    /// A lookup, not a search: identity is exact, and a claim is
    /// consolidated against all of it however old the records are.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for an empty repository or identity, or
    /// database errors.
    pub fn find_by_identity(&self, repo: &str, identity: &str) -> Result<Vec<MemoryRecord>> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        if identity.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "identity" });
        }

        let mut statement = self.connection.prepare(
            r#"
            SELECT * FROM memories
            WHERE repo = ?1 AND identity = ?2
            ORDER BY content_updated_at DESC, id
            "#,
        )?;
        let rows = statement.query_map(params![repo, identity], Self::row_to_record)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replaces one record with another: the older ones are deprecated in the
    /// same transaction that leaves the newer one as it is, so no reader sees a
    /// project where both the old and the new say the same thing.
    ///
    /// Only records of `kept`'s kind that a later record actually supersedes are
    /// touched; a contradiction is [`Self::resolve_conflict`]'s business.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown `kept` id, or database
    /// errors.
    pub fn supersede(
        &self,
        kept: Uuid,
        older: &[Uuid],
        reason: impl Into<String>,
    ) -> Result<usize> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "reason" });
        }
        // The kept record is read first, so an unknown id fails before anything
        // is written, and its project with it.
        let repo = self.get(kept)?.repo;

        let transaction = self.connection.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut deprecated = 0;
        for id in older {
            if *id == kept {
                continue;
            }
            deprecated += self.connection.execute(
                r#"
                UPDATE memories
                SET verification = ?3, applicability = ?4, evidence = ?5, updated_at = ?6
                WHERE id = ?1 AND repo = ?2 AND verification != ?3
                "#,
                params![
                    id.to_string(),
                    repo,
                    serde_json::to_string(&Verification::Deprecated)?,
                    serde_json::to_string(&Applicability::NeedsRevalidation)?,
                    reason.as_str(),
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(deprecated)
    }

    /// The project's counter of how much its memory has been cut down: it moves
    /// when a record is deleted or the project is cleared, so a write that read
    /// it earlier can tell that what it was written from is gone.
    ///
    /// # Errors
    /// Returns database errors.
    pub fn generation(&self, repo: &str) -> Result<i64> {
        let generation: Option<i64> = self
            .connection
            .query_row(
                "SELECT generation FROM memory_generations WHERE repo = ?1",
                params![repo],
                |row| row.get(0),
            )
            .optional()?;
        Ok(generation.unwrap_or(0))
    }

    /// Records a memory only if the project's memory has not been cut down since
    /// `generation` was read: `Ok(None)` when it has.
    ///
    /// This is what keeps a late or retried writer — a queued job, a run that
    /// started before the user deleted its record — from putting back what was
    /// deleted.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::remember`].
    pub fn remember_if_current(
        &self,
        memory: NewMemory,
        generation: i64,
    ) -> Result<Option<MemoryRecord>> {
        let transaction = self.connection.unchecked_transaction()?;
        let repo = memory.repo.clone();
        if self.generation(&repo)? != generation {
            return Ok(None);
        }
        // Idempotency is checked in the same transaction as admission, so two
        // writers cannot both insert the same claim after a separate lookup.
        if !memory.identity.is_empty() {
            if let Some(existing) = self
                .find_by_identity(&repo, &memory.identity)?
                .into_iter()
                .find(|r| r.verification != Verification::Deprecated && memory.matches_capture(r))
            {
                transaction.commit()?;
                return Ok(Some(existing));
            }
        }
        let record = self.remember(memory)?;
        transaction.commit()?;
        Ok(Some(record))
    }

    /// Confirms a record only if the project's memory has not been cut down
    /// since `generation` was read. `Ok(None)` when it has: the record the
    /// caller meant may be a different one by now.
    ///
    /// # Errors
    /// See [`Self::verify`].
    pub fn verify_if_current(
        &self,
        id: Uuid,
        evidence: impl Into<String>,
        generation: i64,
    ) -> Result<Option<MemoryRecord>> {
        if self.generation(&self.repo_of(id)?)? != generation {
            return Ok(None);
        }
        self.verify(id, evidence).map(Some)
    }

    /// Keeps the project's memory bounded: the newest `keep` episodes stay, and
    /// older ones go.
    ///
    /// What a run *did* is provenance, worth keeping for a while and not
    /// forever; what the project *knows* — its procedures and facts — is never
    /// pruned. Says how many went.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for an empty repository, or database
    /// errors.
    pub fn prune_episodes(&self, repo: &str, keep: usize) -> Result<usize> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        let kind = serde_json::to_string(&MemoryKind::Episode)?;
        let removed = self.connection.execute(
            r#"
            DELETE FROM memories
            WHERE repo = ?1
              AND kind = ?2
              AND id NOT IN (
                SELECT id FROM memories
                WHERE repo = ?1 AND kind = ?2
                ORDER BY content_updated_at DESC, id
                LIMIT ?3
              )
            "#,
            params![repo, kind, i64::try_from(keep).unwrap_or(i64::MAX)],
        )?;
        Ok(removed)
    }

    /// The project whose record this is.
    fn repo_of(&self, id: Uuid) -> Result<String> {
        Ok(self.get(id)?.repo)
    }

    /// Keeps one verified memory and deprecates contradictory records.
    ///
    /// # Errors
    /// Returns an error if the kept memory is unknown, not verified, or a
    /// deprecated target is unknown.
    pub fn resolve_conflict(
        &self,
        keep_id: Uuid,
        deprecated_ids: &[Uuid],
        reason: impl Into<String>,
    ) -> Result<MemoryRecord> {
        let reason = reason.into();
        let kept = self.get(keep_id)?;

        if kept.verification != Verification::Verified {
            return Err(VcmError::Deprecated(keep_id));
        }

        for id in deprecated_ids {
            if *id == keep_id {
                continue;
            }
            self.deprecate(*id, reason.as_str())?;
        }

        Ok(kept)
    }

    /// Records that a run had these memories in hand: their retrieval time, and
    /// nothing about what they say.
    ///
    /// Kept apart from the content's own timestamps on purpose: a record that is
    /// recalled often is not thereby newer, and must not climb the ranking for
    /// being looked at ([`crate::retrieval`] reads recency from
    /// `content_updated_at`).
    ///
    /// An empty list is a no-op: a run that recalled nothing has no retrieval to
    /// note.
    ///
    /// # Errors
    /// Returns database errors.
    pub fn record_retrieval(&self, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut statement = self
            .connection
            .prepare("UPDATE memories SET last_retrieved_at = ?2 WHERE id = ?1")?;
        for id in ids {
            statement.execute(params![id.to_string(), now])?;
        }
        Ok(())
    }

    /// Notes how a run that had this memory in hand ended.
    ///
    /// An association, not a verdict on the memory: the run's checks passing
    /// says something happened in a run where the record was present, not that
    /// the record caused it. Nothing about the claim, its trust or its freshness
    /// changes here.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown ID.
    pub fn record_outcome(&self, id: Uuid, succeeded: bool) -> Result<MemoryRecord> {
        let mut record = self.get(id)?;
        record.use_count = record.use_count.saturating_add(1);

        if succeeded {
            record.success_count = record.success_count.saturating_add(1);
        }

        self.update(&record)?;
        Ok(record)
    }

    /// Counts one project's memories, by trust state.
    ///
    /// The column holds the state as JSON writes it (`"verified"`); a row a
    /// still older store wrote bare counts too.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for an empty repository, or database
    /// errors.
    pub fn stats(&self, repo: &str) -> Result<MemoryStats> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }

        let mut statement = self.connection.prepare(
            "SELECT verification, COUNT(*) FROM memories WHERE repo = ?1 GROUP BY verification",
        )?;
        let rows = statement.query_map(params![repo], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut stats = MemoryStats::default();
        for row in rows {
            let (state, count) = row?;
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            stats.total = stats.total.saturating_add(count);
            // Added, not assigned: the same state can come back twice, in
            // both forms.
            match state.trim_matches('"') {
                "verified" => stats.verified = stats.verified.saturating_add(count),
                "provisional" => stats.provisional = stats.provisional.saturating_add(count),
                "deprecated" => stats.deprecated = stats.deprecated.saturating_add(count),
                _ => {}
            }
        }
        Ok(stats)
    }

    /// Lists one project's memories, newest first.
    ///
    /// `query` is found in part of a title, the text, a tag or a file name;
    /// empty, it matches every memory. Every trust state is listed, deprecated
    /// ones included: the page that shows them is where stale ones go.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for an empty repository, or database
    /// errors.
    pub fn list(&self, repo: &str, query: &str) -> Result<Vec<MemoryRecord>> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }

        let mut statement = self.connection.prepare(
            r#"
            SELECT * FROM memories
            WHERE repo = ?1
              AND (title LIKE ?2 ESCAPE '\'
                OR content LIKE ?2 ESCAPE '\'
                OR tags_json LIKE ?2 ESCAPE '\'
                OR files_json LIKE ?2 ESCAPE '\')
            ORDER BY updated_at DESC
            "#,
        )?;

        let rows = statement.query_map(params![repo, containing(query)], Self::row_to_record)?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Lists at most 50 records after a stable (update time, id) cursor.
    /// All trust states remain visible to the manager. A cursor is scoped to
    /// this project; the encoded boundary survives anchor updates or deletion.
    pub fn list_page(
        &self,
        repo: &str,
        query: &str,
        before: Option<&str>,
    ) -> Result<Vec<MemoryRecord>> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        let cursor = if let Some(before) = before {
            let parts: Vec<_> = before.split('|').collect();
            if parts.len() != 3
                || parts[0] != claim_hash(repo, "memory-page-v1")
                || chrono::DateTime::parse_from_rfc3339(parts[1]).is_err()
                || Uuid::parse_str(parts[2]).is_err()
            {
                return Err(VcmError::InvalidCursor);
            }
            Some((parts[1], parts[2]))
        } else {
            None
        };
        let mut statement = self.connection.prepare(
            r#"
            SELECT * FROM memories WHERE repo=?1
            AND (title LIKE ?2 ESCAPE '\' OR content LIKE ?2 ESCAPE '\'
              OR tags_json LIKE ?2 ESCAPE '\' OR files_json LIKE ?2 ESCAPE '\')
            AND (?3 IS NULL OR (updated_at,id) < (?3,?4))
            ORDER BY updated_at DESC,id DESC LIMIT 50
        "#,
        )?;
        let rows = statement.query_map(
            params![
                repo,
                containing(query),
                cursor.map(|c| c.0),
                cursor.map(|c| c.1)
            ],
            Self::row_to_record,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replaces a memory's title and text: the two its user corrects.
    ///
    /// The correction is the user's assertion, and it carries no evidence: the
    /// record keeps the evidence it had, for audit, but that evidence was about
    /// the text it replaced, so the record stops being one a run may rely on
    /// and is marked to be checked again
    /// ([`MemoryRecord::evidence_matches_claim`]). Correcting a memory cannot
    /// launder an unverified claim into a verified one, nor carry a verified
    /// one across a change of what it says.
    ///
    /// Its tags, its files and its kind stay as the agent left them. The update
    /// trigger reindexes the row for search.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for a blank title or text, or
    /// [`VcmError::NotFound`] for an unknown ID.
    pub fn edit(&self, id: Uuid, title: &str, content: &str) -> Result<MemoryRecord> {
        let title = title.trim();
        if title.is_empty() {
            return Err(VcmError::EmptyField { field: "title" });
        }
        let content = content.trim();
        if content.is_empty() {
            return Err(VcmError::EmptyField { field: "content" });
        }

        let mut record = self.get(id)?;
        let claim_hash = crate::model::claim_hash(title, content);
        if claim_hash != record.claim_hash {
            record.title = title.to_string();
            record.content = content.to_string();
            record.claim_hash = claim_hash;
            record.applicability = Applicability::NeedsRevalidation;
            record.content_updated_at = Utc::now();
            record.source_kind = SourceKind::UserAssertion;
        }
        record.updated_at = Utc::now();
        self.update(&record)?;
        Ok(record)
    }

    /// Removes a memory for good.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown ID, or database errors.
    pub fn delete(&self, id: Uuid) -> Result<()> {
        let repo = self.get(id)?.repo;
        let transaction = self.connection.unchecked_transaction()?;
        self.connection.execute(
            "DELETE FROM memories WHERE id = ?1",
            params![id.to_string()],
        )?;
        self.bump_generation(&repo)?;
        transaction.commit()?;
        Ok(())
    }

    /// Moves the project's generation: what was read of its memory before this
    /// may not be written back ([`Self::remember_if_current`]).
    pub(crate) fn bump_generation(&self, repo: &str) -> Result<i64> {
        self.connection.execute(
            r#"
            INSERT INTO memory_generations (repo, generation) VALUES (?1, 1)
            ON CONFLICT(repo) DO UPDATE SET generation = generation + 1
            "#,
            params![repo],
        )?;
        self.generation(repo)
    }

    /// Removes every memory of one project, and says how many went.
    ///
    /// # Errors
    /// Returns [`VcmError::EmptyField`] for an empty repository, or database
    /// errors.
    pub fn clear(&self, repo: &str) -> Result<usize> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        let transaction = self.connection.unchecked_transaction()?;
        self.sync_clear(repo)?;
        let removed = self
            .connection
            .execute("DELETE FROM memories WHERE repo = ?1", params![repo])?;
        // Whatever was read of this project's memory is gone: what was queued
        // from it must not come back.
        self.bump_generation(repo)?;
        transaction.commit()?;
        Ok(removed)
    }

    /// Retrieves a ranked context bundle with telemetry.
    ///
    /// Two independent candidate pools feed it, and neither stands in for the
    /// other: the memories whose words the task or the failure shares
    /// (SQLite's full-text index), and the memories that name one of the
    /// query's files. Both are filtered by the query's own limits — the
    /// project, the trust states the caller accepts and the tags it excludes —
    /// before the results are cut to `limit`, so a record the caller may not
    /// see can't take a slot from one it may.
    ///
    /// A memory is a candidate only if it is relevant to the query: a shared
    /// word, one of its files, or a term of the failure. Trust and recency
    /// order the relevant ones and admit none, so a query about something the
    /// project has no memory of answers with nothing.
    ///
    /// # Errors
    /// Returns [`VcmError::InvalidLimit`] for limits outside 1..=32.
    pub fn retrieve_with_telemetry(
        &self,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievedMemory>, RetrievalTelemetry)> {
        self.retrieve_filtered_with_telemetry(query, |_| true)
    }

    /// Applies caller visibility/applicability before ranking consumes result slots.
    pub fn retrieve_filtered_with_telemetry(
        &self,
        query: &RetrievalQuery,
        accept: impl Fn(&MemoryRecord) -> bool,
    ) -> Result<(Vec<RetrievedMemory>, RetrievalTelemetry)> {
        let started = Instant::now();

        if query.limit == 0 || query.limit > MAX_RETRIEVAL_LIMIT {
            return Err(VcmError::InvalidLimit(query.limit));
        }
        if query.repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }

        let now = Utc::now();
        let mut telemetry = RetrievalTelemetry::default();
        let mut candidates = self.lexical_candidates(query, &accept, now, &mut telemetry)?;
        for hit in self.file_candidates(query, &accept, now, &mut telemetry)? {
            if !candidates
                .iter()
                .any(|seen| seen.record.id == hit.record.id)
            {
                candidates.push(hit);
            }
        }

        // Keep ordinary lexical and explicit-file behavior intact. A compound
        // code name can name the words of a prose claim without sharing its
        // literal FTS token; require all its meaningful components together.
        if candidates.is_empty() {
            let fallback =
                identifier_fts_query(&[&query.task, query.failure.as_deref().unwrap_or("")]);
            candidates =
                self.full_text_candidates(query, &accept, now, &mut telemetry, fallback)?;
        }

        // Equal scores fall to the newer claim, then to the id: the same query
        // over the same memories answers the same way every time. "Newer" is
        // the claim's own stamp, as the recency component reads it — not when
        // the record was last touched by a run.
        candidates.sort_by(compare_memories);
        deduplicate_memories(&mut candidates);
        candidates.truncate(query.limit);

        telemetry.used_full_text_search = telemetry.fts_matches > 0;
        telemetry.returned = candidates.len();
        telemetry.elapsed_ms = started.elapsed().as_millis();

        Ok((candidates, telemetry))
    }

    /// The project's memories that share a specific word with the task or the
    /// failure, best match first, each with the strength of its match.
    fn lexical_candidates(
        &self,
        query: &RetrievalQuery,
        accept: &impl Fn(&MemoryRecord) -> bool,
        now: DateTime<Utc>,
        telemetry: &mut RetrievalTelemetry,
    ) -> Result<Vec<RetrievedMemory>> {
        let failure = query.failure.as_deref().unwrap_or("");
        let fts_query = build_fts_query(&[query.task.as_str(), failure]);
        self.full_text_candidates(query, accept, now, telemetry, fts_query)
    }

    fn full_text_candidates(
        &self,
        query: &RetrievalQuery,
        accept: &impl Fn(&MemoryRecord) -> bool,
        now: DateTime<Utc>,
        telemetry: &mut RetrievalTelemetry,
        fts_query: String,
    ) -> Result<Vec<RetrievedMemory>> {
        let Some(states) = states_sql(query) else {
            return Ok(Vec::new());
        };
        // No word of the query names anything (it is all "fix the tests"), so
        // there is nothing for the index to look up: only a file can make a
        // memory relevant to this query.
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }

        let sql = format!(
            r#"
            SELECT m.*, bm25(memories_fts) AS lexical_rank
            FROM memories_fts
            JOIN memories m ON m.rowid = memories_fts.rowid
            WHERE memories_fts MATCH ?
              AND m.repo = ?
              AND {states}
              {tags}
            ORDER BY lexical_rank, m.id
            "#,
            tags = tags_sql(&query.excluded_tags),
        );
        let mut statement = self.connection.prepare(&sql)?;
        let mut values = vec![Value::Text(fts_query), Value::Text(query.repo.clone())];
        values.extend(tag_needles(&query.excluded_tags));

        let rows = statement.query_map(params_from_iter(values), |row| {
            let rank: f64 = row.get("lexical_rank")?;
            Ok((Self::row_to_record(row)?, Some(bm25_relevance(rank))))
        })?;
        let scanned = telemetry.candidates_scanned;
        let found = collect_ranked_candidates(
            rows.map(|row| row.map_err(Into::into)),
            query,
            accept,
            now,
            &self.weights,
            LEXICAL_POOL,
            telemetry,
        )?;
        telemetry.fts_matches += telemetry.candidates_scanned - scanned;
        Ok(found)
    }

    /// The project's memories that name one of the query's files, newest
    /// first.
    ///
    /// Looked up through the file index, not by reading every record: a memory
    /// about `src/queue.rs` is no less about it for being old, so the search is
    /// over every memory of the project, and the index is what makes that a
    /// lookup.
    fn file_candidates(
        &self,
        query: &RetrievalQuery,
        accept: &impl Fn(&MemoryRecord) -> bool,
        now: DateTime<Utc>,
        telemetry: &mut RetrievalTelemetry,
    ) -> Result<Vec<RetrievedMemory>> {
        let wanted: Vec<String> = query
            .files
            .iter()
            .map(|file| normalize_path(file))
            .filter(|file| !file.is_empty())
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let Some(states) = states_sql(query) else {
            return Ok(Vec::new());
        };

        // [`crate::retrieval::path_overlap`] as SQL: the same path, a path that
        // is its tail, or the path from above. Both sides are stored
        // normalized, so this is that comparison and not an approximation.
        // `instr` rather than `LIKE`, so that a `_` or a `%` in a name is the
        // character it is.
        const ONE_FILE: &str = "(f.path = ?
                   OR instr(f.path, '/' || ?) = length(f.path) - length(?)
                   OR instr(?, '/' || f.path) = length(?) - length(f.path))";
        let conditions = vec![ONE_FILE; wanted.len()].join("\n                OR ");

        let sql = format!(
            r#"
            SELECT m.* FROM memories m
            WHERE m.repo = ?
              AND {states}
              {tags}
              AND EXISTS (
                SELECT 1 FROM memory_files f
                WHERE f.memory_id = m.id
                  AND ({conditions})
              )
            ORDER BY m.updated_at DESC, m.id
            "#,
            tags = tags_sql(&query.excluded_tags),
        );
        let mut values = vec![Value::Text(query.repo.clone())];
        values.extend(tag_needles(&query.excluded_tags));
        for path in &wanted {
            // The clause names the path five times: once for equality and
            // twice in each of the two ends.
            for _ in 0..5 {
                values.push(Value::Text(path.clone()));
            }
        }

        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((Self::row_to_record(row)?, None))
        })?;
        let scanned = telemetry.candidates_scanned;
        let found = collect_ranked_candidates(
            rows.map(|row| row.map_err(Into::into)),
            query,
            accept,
            now,
            &self.weights,
            FILE_POOL,
            telemetry,
        )?;
        telemetry.file_matches = telemetry.candidates_scanned - scanned;
        Ok(found)
    }

    /// Retrieves a ranked context bundle.
    ///
    /// # Errors
    /// See [`Self::retrieve_with_telemetry`].
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievedMemory>> {
        Ok(self.retrieve_with_telemetry(query)?.0)
    }

    /// Renders a compact prompt context block, within no bound.
    #[must_use]
    pub fn render_context(&self, memories: &[RetrievedMemory]) -> String {
        self.render_packet(memories, usize::MAX)
    }

    /// Renders the prompt's memory packet within the token allowance.
    #[must_use]
    pub fn render_packet(&self, memories: &[RetrievedMemory], tokens: usize) -> String {
        self.render_packet_traced(memories, tokens).0
    }

    /// Renders the prompt's memory packet, and says which records it emitted:
    /// what a request carried is not what was retrieved, and only the packet
    /// itself knows which ones the allowance left out.
    #[must_use]
    pub fn render_packet_traced(
        &self,
        memories: &[RetrievedMemory],
        tokens: usize,
    ) -> (String, Vec<Uuid>) {
        let mut output = String::from("## Relevant prior work\n\n");
        if estimate_tokens(&output) > tokens {
            return (String::new(), Vec::new());
        }

        if memories.is_empty() {
            let empty = "No sufficiently relevant prior memory.\n";
            if estimate_tokens(&output) + estimate_tokens(empty) <= tokens {
                output.push_str(empty);
            }
            return (output, Vec::new());
        }

        let mut emitted = Vec::new();
        let mut seen: Vec<&MemoryRecord> = Vec::new();
        let mut used = estimate_tokens(&output);
        let mut omitted = Vec::new();
        for memory in memories {
            if seen
                .iter()
                .any(|record| record.same_context_claim(&memory.record))
            {
                continue;
            }
            seen.push(&memory.record);
            let block = self.render_memory(memory);
            let cost = estimate_tokens(&block);
            if used + cost <= tokens {
                output.push_str(&block);
                used += cost;
                emitted.push(memory.record.id);
                continue;
            }
            // A long source episode must not hide shorter claims later in the
            // ranked pool. Complete claims get space before any fetch hints.
            omitted.push(memory);
        }

        for memory in &omitted {
            let head = format!(
                "- [{}] {} — (cut to fit the memory allowance; `memory_get` {} for the rest)\n",
                memory.record.verification, memory.record.title, memory.record.id
            );
            if used + estimate_tokens(&head) <= tokens {
                output.push_str(&head);
                used += estimate_tokens(&head);
                emitted.push(memory.record.id);
            }
        }

        let footer = "(The memory packet was cut to its allowance.)\n";
        if !omitted.is_empty() && used + estimate_tokens(footer) <= tokens {
            output.push_str(footer);
        }
        (output, emitted)
    }

    /// One record as the packet shows it.
    fn render_memory(&self, memory: &RetrievedMemory) -> String {
        // A record whose claim was edited or whose code moved on is evidence of
        // what was once true, not an instruction: it says so, so the model
        // checks it against the code instead of trusting it.
        let standing = match memory.record.applicability {
            Applicability::Current => "",
            Applicability::NeedsRevalidation => {
                " — to be checked again: this claim or the code it names changed"
            }
            Applicability::Unknown => " — never checked in its current form",
        };
        format!(
            "- [{}]{} {} — {}\n  id: {}\n  Evidence: {}\n  Files: {}\n  Match: {}\n",
            memory.record.verification,
            standing,
            memory.record.title,
            memory.record.content,
            memory.record.id,
            memory.record.evidence.as_deref().unwrap_or("none"),
            if memory.record.files.is_empty() {
                "none".to_string()
            } else {
                memory.record.files.join(", ")
            },
            memory.reason,
        )
    }

    pub(crate) fn update(&self, record: &MemoryRecord) -> Result<()> {
        let transaction = if self.connection.is_autocommit() {
            Some(self.connection.unchecked_transaction()?)
        } else {
            None
        };
        let changed = self.connection.execute(
            r#"UPDATE memories SET kind=?2,verification=?3,applicability=?4,source_kind=?5,repo=?6,title=?7,content=?8,claim_hash=?9,identity=?10,validated_claim_hash=?11,files_json=?12,tags_json=?13,code_version=?14,source_episode=?15,evidence=?16,confidence=?17,content_updated_at=?18,updated_at=?19,last_validated_at=?20,last_retrieved_at=?21,use_count=?22,success_count=?23 WHERE id=?1"#,
            params![
                record.id.to_string(),
                serde_json::to_string(&record.kind)?,
                serde_json::to_string(&record.verification)?,
                serde_json::to_string(&record.applicability)?,
                serde_json::to_string(&record.source_kind)?,
                record.repo,
                record.title,
                record.content,
                record.claim_hash,
                record.identity,
                record.validated_claim_hash,
                serde_json::to_string(&record.files)?,
                serde_json::to_string(&record.tags)?,
                record.code_version,
                record.source_episode.map(|id| id.to_string()),
                record.evidence,
                record.confidence,
                record.content_updated_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
                record.last_validated_at.map(|at| at.to_rfc3339()),
                record.last_retrieved_at.map(|at| at.to_rfc3339()),
                record.use_count,
                record.success_count,
            ],
        )?;

        if changed == 0 {
            return Err(VcmError::NotFound(record.id));
        }

        self.index_files(record.id, &record.files)?;
        if let Some(transaction) = transaction {
            transaction.commit()?;
        }
        Ok(())
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
        let id: String = row.get("id")?;
        let kind: String = row.get("kind")?;
        let verification: String = row.get("verification")?;
        let applicability: String = row.get("applicability")?;
        let source_kind: String = row.get("source_kind")?;
        let claim_hash: String = row.get("claim_hash")?;
        let files: String = row.get("files_json")?;
        let tags: String = row.get("tags_json")?;
        let created_at: String = row.get("created_at")?;
        let updated_at: String = row.get("updated_at")?;
        // A row written before these were kept keeps what it has: the content
        // was as old as the record, and nobody knows when it was last checked.
        let content_updated_at: Option<String> = row.get("content_updated_at")?;

        Ok(MemoryRecord {
            id: parse_uuid(&id)?,
            kind: serde_json::from_str(&kind).map_err(rusqlite_conversion_error)?,
            verification: serde_json::from_str(&verification).map_err(rusqlite_conversion_error)?,
            applicability: serde_json::from_str(&applicability)
                .map_err(rusqlite_conversion_error)?,
            source_kind: serde_json::from_str(&source_kind).map_err(rusqlite_conversion_error)?,
            repo: row.get("repo")?,
            title: row.get("title")?,
            content: row.get("content")?,
            claim_hash,
            identity: row.get("identity")?,
            validated_claim_hash: row.get("validated_claim_hash")?,
            files: serde_json::from_str(&files).map_err(rusqlite_conversion_error)?,
            tags: serde_json::from_str(&tags).map_err(rusqlite_conversion_error)?,
            code_version: row.get("code_version")?,
            source_episode: row
                .get::<_, Option<String>>("source_episode")?
                .map(|value| parse_uuid(&value))
                .transpose()?,
            evidence: row.get("evidence")?,
            confidence: row.get("confidence")?,
            created_at: parse_timestamp(&created_at)?,
            content_updated_at: match content_updated_at {
                Some(at) => parse_timestamp(&at)?,
                None => parse_timestamp(&created_at)?,
            },
            updated_at: parse_timestamp(&updated_at)?,
            last_validated_at: row
                .get::<_, Option<String>>("last_validated_at")?
                .map(|at| parse_timestamp(&at))
                .transpose()?,
            last_retrieved_at: row
                .get::<_, Option<String>>("last_retrieved_at")?
                .map(|at| parse_timestamp(&at))
                .transpose()?,
            use_count: row.get("use_count")?,
            success_count: row.get("success_count")?,
        })
    }
}

/// Thread-safe shared store.
#[derive(Debug, Clone)]
pub struct SharedMemoryStore {
    inner: Arc<Mutex<MemoryStore>>,
}

impl SharedMemoryStore {
    /// Wraps a store.
    #[must_use]
    pub fn new(store: MemoryStore) -> Self {
        Self {
            inner: Arc::new(Mutex::new(store)),
        }
    }

    /// Opens a shared SQLite-backed store.
    ///
    /// # Errors
    /// See [`MemoryStore::open`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::new(MemoryStore::open(path)?))
    }

    /// Opens a shared in-memory store.
    ///
    /// # Errors
    /// See [`MemoryStore::open_in_memory`].
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(MemoryStore::open_in_memory()?))
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, MemoryStore>> {
        self.inner.lock().map_err(|_| VcmError::LockPoisoned)
    }

    /// See [`MemoryStore::remember`].
    ///
    /// # Errors
    /// See [`MemoryStore::remember`].
    pub fn remember(&self, memory: NewMemory) -> Result<MemoryRecord> {
        self.lock()?.remember(memory)
    }

    /// See [`MemoryStore::verify`].
    ///
    /// # Errors
    /// See [`MemoryStore::verify`].
    pub fn verify(&self, id: Uuid, evidence: impl Into<String>) -> Result<MemoryRecord> {
        self.lock()?.verify(id, evidence)
    }

    /// See [`MemoryStore::deprecate`].
    ///
    /// # Errors
    /// See [`MemoryStore::deprecate`].
    pub fn deprecate(&self, id: Uuid, reason: impl Into<String>) -> Result<MemoryRecord> {
        self.lock()?.deprecate(id, reason)
    }

    /// See [`MemoryStore::find_by_identity`].
    ///
    /// # Errors
    /// See [`MemoryStore::find_by_identity`].
    pub fn find_by_identity(&self, repo: &str, identity: &str) -> Result<Vec<MemoryRecord>> {
        self.lock()?.find_by_identity(repo, identity)
    }

    /// See [`MemoryStore::supersede`].
    ///
    /// # Errors
    /// See [`MemoryStore::supersede`].
    pub fn supersede(
        &self,
        kept: Uuid,
        older: &[Uuid],
        reason: impl Into<String>,
    ) -> Result<usize> {
        self.lock()?.supersede(kept, older, reason)
    }

    /// See [`MemoryStore::generation`].
    ///
    /// # Errors
    /// See [`MemoryStore::generation`].
    pub fn generation(&self, repo: &str) -> Result<i64> {
        self.lock()?.generation(repo)
    }

    /// See [`MemoryStore::remember_if_current`].
    ///
    /// # Errors
    /// See [`MemoryStore::remember_if_current`].
    pub fn remember_if_current(
        &self,
        memory: NewMemory,
        generation: i64,
    ) -> Result<Option<MemoryRecord>> {
        self.lock()?.remember_if_current(memory, generation)
    }

    /// See [`MemoryStore::verify_if_current`].
    ///
    /// # Errors
    /// See [`MemoryStore::verify_if_current`].
    pub fn verify_if_current(
        &self,
        id: Uuid,
        evidence: impl Into<String>,
        generation: i64,
    ) -> Result<Option<MemoryRecord>> {
        self.lock()?.verify_if_current(id, evidence, generation)
    }

    /// See [`MemoryStore::prune_episodes`].
    ///
    /// # Errors
    /// See [`MemoryStore::prune_episodes`].
    pub fn prune_episodes(&self, repo: &str, keep: usize) -> Result<usize> {
        self.lock()?.prune_episodes(repo, keep)
    }

    /// See [`MemoryStore::resolve_conflict`].
    ///
    /// # Errors
    /// See [`MemoryStore::resolve_conflict`].
    pub fn resolve_conflict(
        &self,
        keep_id: Uuid,
        deprecated_ids: &[Uuid],
        reason: impl Into<String>,
    ) -> Result<MemoryRecord> {
        self.lock()?
            .resolve_conflict(keep_id, deprecated_ids, reason)
    }

    /// See [`MemoryStore::stats`].
    ///
    /// # Errors
    /// See [`MemoryStore::stats`].
    pub fn stats(&self, repo: &str) -> Result<MemoryStats> {
        self.lock()?.stats(repo)
    }

    /// See [`MemoryStore::list`].
    ///
    /// # Errors
    /// See [`MemoryStore::list`].
    pub fn list(&self, repo: &str, query: &str) -> Result<Vec<MemoryRecord>> {
        self.lock()?.list(repo, query)
    }

    /// Lists a bounded manager page; see [`MemoryStore::list_page`].
    pub fn list_page(
        &self,
        repo: &str,
        query: &str,
        before: Option<&str>,
    ) -> Result<Vec<MemoryRecord>> {
        self.lock()?.list_page(repo, query, before)
    }

    /// See [`MemoryStore::edit`].
    ///
    /// # Errors
    /// See [`MemoryStore::edit`].
    pub fn edit(&self, id: Uuid, title: &str, content: &str) -> Result<MemoryRecord> {
        self.lock()?.edit(id, title, content)
    }

    /// See [`MemoryStore::delete`].
    ///
    /// # Errors
    /// See [`MemoryStore::delete`].
    pub fn delete(&self, id: Uuid) -> Result<()> {
        self.lock()?.delete(id)
    }

    /// See [`MemoryStore::clear`].
    ///
    /// # Errors
    /// See [`MemoryStore::clear`].
    pub fn clear(&self, repo: &str) -> Result<usize> {
        self.lock()?.clear(repo)
    }

    /// See [`MemoryStore::mark_needs_revalidation`].
    ///
    /// # Errors
    /// See [`MemoryStore::mark_needs_revalidation`].
    pub fn mark_needs_revalidation(&self, id: Uuid) -> Result<MemoryRecord> {
        self.lock()?.mark_needs_revalidation(id)
    }

    /// See [`MemoryStore::mark_stale_for_version`].
    ///
    /// # Errors
    /// See [`MemoryStore::mark_stale_for_version`].
    pub fn mark_stale_for_version(&self, repo: &str, current_version: &str) -> Result<usize> {
        self.lock()?.mark_stale_for_version(repo, current_version)
    }

    /// See [`MemoryStore::record_retrieval`].
    ///
    /// # Errors
    /// See [`MemoryStore::record_retrieval`].
    pub fn record_retrieval(&self, ids: &[Uuid]) -> Result<()> {
        self.lock()?.record_retrieval(ids)
    }

    /// See [`MemoryStore::record_outcome`].
    ///
    /// # Errors
    /// See [`MemoryStore::record_outcome`].
    pub fn record_outcome(&self, id: Uuid, succeeded: bool) -> Result<MemoryRecord> {
        self.lock()?.record_outcome(id, succeeded)
    }

    /// See [`MemoryStore::get`].
    ///
    /// # Errors
    /// See [`MemoryStore::get`].
    pub fn get(&self, id: Uuid) -> Result<MemoryRecord> {
        self.lock()?.get(id)
    }

    /// See [`MemoryStore::retrieve`].
    ///
    /// # Errors
    /// See [`MemoryStore::retrieve`].
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievedMemory>> {
        self.lock()?.retrieve(query)
    }

    /// Retrieves with caller visibility/applicability filtering before limiting.
    pub fn retrieve_filtered(
        &self,
        query: &RetrievalQuery,
        accept: impl Fn(&MemoryRecord) -> bool,
    ) -> Result<Vec<RetrievedMemory>> {
        Ok(self
            .lock()?
            .retrieve_filtered_with_telemetry(query, accept)?
            .0)
    }

    /// See [`MemoryStore::retrieve_with_telemetry`].
    ///
    /// # Errors
    /// See [`MemoryStore::retrieve_with_telemetry`].
    pub fn retrieve_with_telemetry(
        &self,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievedMemory>, RetrievalTelemetry)> {
        self.lock()?.retrieve_with_telemetry(query)
    }

    /// See [`MemoryStore::render_packet`].
    #[must_use]
    pub fn render_packet(&self, memories: &[RetrievedMemory], tokens: usize) -> String {
        self.render_packet_traced(memories, tokens).0
    }

    /// See [`MemoryStore::render_packet_traced`].
    #[must_use]
    pub fn render_packet_traced(
        &self,
        memories: &[RetrievedMemory],
        tokens: usize,
    ) -> (String, Vec<Uuid>) {
        self.lock().map_or_else(
            |_| ("Memory store unavailable.".to_string(), Vec::new()),
            |store| store.render_packet_traced(memories, tokens),
        )
    }

    /// See [`MemoryStore::render_context`].
    #[must_use]
    pub fn render_context(&self, memories: &[RetrievedMemory]) -> String {
        self.lock()
            .map(|store| store.render_context(memories))
            .unwrap_or_else(|_| "Memory store unavailable.".to_string())
    }
}

/// Creates the full-text index and the triggers that keep it in step with the
/// table, if they are not there yet.
///
/// Called once the columns are settled: an FTS5 external-content index whose
/// triggers fire against a row the index never held is what SQLite reports as
/// `SQLITE_CORRUPT_VTAB`, so the index is created only when nothing is going
/// to update a memory before `rebuild_full_text` has filled it.
fn create_full_text(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            title,
            content,
            tags,
            content='memories',
            content_rowid='rowid',
            tokenize='porter unicode61 remove_diacritics 2'
        );

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, title, content, tags)
            VALUES (new.rowid, new.title, new.content, new.tags_json);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, title, content, tags)
            VALUES ('delete', old.rowid, old.title, old.content, old.tags_json);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, title, content, tags)
            VALUES ('delete', old.rowid, old.title, old.content, old.tags_json);
            INSERT INTO memories_fts(rowid, title, content, tags)
            VALUES (new.rowid, new.title, new.content, new.tags_json);
        END;
        "#,
    )?;
    Ok(())
}

///
/// A file written by an earlier build of this crate has the records but not
/// these columns, so they are added when it opens: the records it holds are
/// kept, and each column's default says what is honestly known about them (no
/// one knows when they were last checked, so `last_validated_at` is null).
const ADDED_COLUMNS: &[(&str, &str)] = &[
    ("applicability", "TEXT NOT NULL DEFAULT '\"unknown\"'"),
    ("source_kind", "TEXT NOT NULL DEFAULT '\"unknown\"'"),
    ("claim_hash", "TEXT NOT NULL DEFAULT ''"),
    ("identity", "TEXT NOT NULL DEFAULT ''"),
    ("validated_claim_hash", "TEXT"),
    ("content_updated_at", "TEXT"),
    ("last_validated_at", "TEXT"),
    ("last_retrieved_at", "TEXT"),
];

/// Adds any of [`ADDED_COLUMNS`] an existing table lacks. A table with all of
/// them — every table this version creates — is left alone.
fn add_missing_columns(connection: &Connection) -> Result<()> {
    let present: Vec<String> = {
        let mut statement = connection.prepare("PRAGMA table_info(memories)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>("name"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if present.is_empty() {
        // No table yet: `initialize` has just created this version's.
        return Ok(());
    }

    for (column, definition) in ADDED_COLUMNS {
        if present.iter().any(|name| name == column) {
            continue;
        }
        connection.execute(
            &format!("ALTER TABLE memories ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

/// Gives a claim hash to the records of a file written before claims were
/// hashed: without one, an edit could not tell that the text changed, and the
/// evidence an older record carries would look like evidence about whatever
/// text it holds now. Fills only rows with no hash, so a store that has them is
/// left alone.
fn backfill_claim_hashes(connection: &Connection) -> Result<()> {
    let unhashed: Vec<(String, String, String)> = {
        let mut statement = connection.prepare(
            "SELECT id, title, content FROM memories WHERE claim_hash = '' OR claim_hash IS NULL",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if unhashed.is_empty() {
        return Ok(());
    }

    let mut statement = connection.prepare("UPDATE memories SET claim_hash = ?2 WHERE id = ?1")?;
    for (id, title, content) in unhashed {
        statement.execute(params![id, claim_hash(&title, &content)])?;
    }
    Ok(())
}

fn backfill_files(connection: &Connection) -> Result<()> {
    let indexed: i64 =
        connection.query_row("SELECT COUNT(*) FROM memory_files", [], |row| row.get(0))?;
    if indexed > 0 {
        return Ok(());
    }

    let records: Vec<(String, String)> = {
        let mut statement = connection.prepare("SELECT id, files_json FROM memories")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if records.is_empty() {
        return Ok(());
    }

    let mut insert = connection
        .prepare("INSERT OR IGNORE INTO memory_files (memory_id, path) VALUES (?1, ?2)")?;
    for (id, files) in records {
        // A row whose files can't be read has no files to index; its words
        // still find it.
        let files: Vec<String> = serde_json::from_str(&files).unwrap_or_default();
        for file in files {
            let path = normalize_path(&file);
            if path.is_empty() {
                continue;
            }
            insert.execute(params![id, path])?;
        }
    }
    Ok(())
}

/// Rebuilds the full-text index when it holds nothing and the store does: FTS5
/// keeps an external-content index (`content='memories'`) in step with the
/// table only through the triggers, so the records a store already had when the
/// index was created are in the table and not in the index. Nothing finds them
/// by their words until it is refilled — and words are the only way retrieval
/// reaches a memory, so such a store would silently recall nothing.
///
/// FTS5's own `'rebuild'` is no use here: it reads the content row's columns by
/// the index's names, and `tags` is `tags_json` in the table. This writes the
/// values the triggers write, by the names that exist, and reading `*_docsize`
/// rather than the index's columns asks how many documents are indexed without
/// reading a memory's text.
fn rebuild_full_text(connection: &Connection) -> Result<()> {
    let indexed: Option<i64> = connection
        .query_row("SELECT COUNT(*) FROM memories_fts_docsize", [], |row| {
            row.get(0)
        })
        .ok();
    if let Some(indexed) = indexed {
        if indexed > 0 {
            return Ok(());
        }
    }
    let stored: i64 =
        connection.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    if stored == 0 {
        return Ok(());
    }
    connection.execute(
        r#"
        INSERT INTO memories_fts (rowid, title, content, tags)
        SELECT m.rowid, m.title, m.content, m.tags_json FROM memories m
        "#,
        [],
    )?;
    Ok(())
}

fn validate_new_memory(memory: &NewMemory) -> Result<()> {
    if memory.repo.trim().is_empty() {
        return Err(VcmError::EmptyField { field: "repo" });
    }
    if memory.title.trim().is_empty() {
        return Err(VcmError::EmptyField { field: "title" });
    }
    if memory.content.trim().is_empty() {
        return Err(VcmError::EmptyField { field: "content" });
    }
    if !(0.0..=1.0).contains(&memory.confidence) {
        return Err(VcmError::OutOfRange {
            field: "confidence",
            minimum: 0.0,
            maximum: 1.0,
            actual: memory.confidence as f64,
        });
    }
    Ok(())
}

/// The pattern `LIKE` finds `query` by: `%…%`, with the characters `LIKE`
/// keeps for itself (`\`, `%`, `_`) escaped, so a memory that really holds a
/// `%` is the one a `%` finds.
fn containing(query: &str) -> String {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// The SQL that keeps a query's trust limits: the states it accepts, in both
/// forms a row can hold them in — this store writes JSON (`"verified"`), an
/// older one wrote it bare — and never a deprecated one, since a deprecated
/// memory is history and `list` is where history shows.
///
/// `None` when the query accepts no retrievable state at all: then nothing may
/// be returned, and a clause that admitted everything would be the opposite of
/// what was asked.
fn states_sql(query: &RetrievalQuery) -> Option<String> {
    let mut states: Vec<&str> = Vec::new();
    for verification in &query.verifications {
        let state = match verification {
            Verification::Provisional => "provisional",
            Verification::Verified => "verified",
            Verification::Deprecated => continue,
        };
        if !states.contains(&state) {
            states.push(state);
        }
    }
    if query.verifications.is_empty() {
        states.push("provisional");
        states.push("verified");
    }
    if states.is_empty() {
        return None;
    }

    Some(format!(
        "TRIM(m.verification, '\"') IN ({})",
        states
            .iter()
            .map(|state| format!("'{state}'"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The SQL that keeps the records carrying an excluded tag out of the query,
/// and the values it needs to say so.
///
/// A tag is compared as the whole element the stored array holds: `instr` looks
/// for its JSON encoding, quotes and escapes included, so `chat:abc` isn't
/// found inside `chat:abcdef`.
fn tags_sql(excluded: &[String]) -> String {
    excluded
        .iter()
        .map(|_| "AND instr(m.tags_json, ?) = 0".to_string())
        .collect::<Vec<_>>()
        .join("\n              ")
}

fn tag_needles(excluded: &[String]) -> Vec<Value> {
    excluded
        .iter()
        .map(|tag| Value::Text(serde_json::to_string(tag).unwrap_or_else(|_| format!("\"{tag}\""))))
        .collect()
}

fn parse_uuid(value: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_timestamp(value: &str) -> rusqlite::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn rusqlite_conversion_error<E>(error: E) -> rusqlite::Error
where
    E: std::fmt::Display + std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

/// Returns a normalized database path.
#[must_use]
pub fn database_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}

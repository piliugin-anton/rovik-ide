use crate::embedding::{MemoryEmbedder, VectorIndex};
use crate::error::{Result, VcmError};
use crate::model::{MemoryRecord, MemoryStats, NewMemory, RetrievalQuery, RetrievedMemory, Verification};
use crate::retrieval::{build_fts_query, score_memory, RetrievalTelemetry, RetrievalWeights};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use uuid::Uuid;

const MAX_RETRIEVAL_LIMIT: usize = 32;
const MAX_FTS_CANDIDATES: usize = 200;

/// SQLite-backed memory store with FTS5 lexical retrieval.
#[derive(Debug)]
pub struct MemoryStore {
    connection: Connection,
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
                repo TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                files_json TEXT NOT NULL,
                tags_json TEXT NOT NULL,
                code_version TEXT,
                source_episode TEXT,
                evidence TEXT,
                confidence REAL NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                use_count INTEGER NOT NULL DEFAULT 0,
                success_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_memories_repo ON memories(repo);
            CREATE INDEX IF NOT EXISTS idx_memories_verification ON memories(verification);
            CREATE INDEX IF NOT EXISTS idx_memories_kind ON memories(kind);
            CREATE INDEX IF NOT EXISTS idx_memories_updated ON memories(updated_at);
            CREATE INDEX IF NOT EXISTS idx_memories_repo_version ON memories(repo, code_version);

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
            repo: memory.repo,
            title: memory.title,
            content: memory.content,
            files: memory.files,
            tags: memory.tags,
            code_version: memory.code_version,
            source_episode: memory.source_episode,
            evidence: memory.evidence,
            confidence: memory.confidence,
            created_at: now,
            updated_at: now,
            use_count: 0,
            success_count: 0,
        };

        self.insert(&record)?;
        Ok(record)
    }

    fn insert(&self, record: &MemoryRecord) -> Result<()> {
        self.connection.execute(
            r#"INSERT INTO memories (id,kind,verification,repo,title,content,files_json,tags_json,code_version,source_episode,evidence,confidence,created_at,updated_at,use_count,success_count) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"#,
            params![
                record.id.to_string(),
                serde_json::to_string(&record.kind)?,
                serde_json::to_string(&record.verification)?,
                record.repo,
                record.title,
                record.content,
                serde_json::to_string(&record.files)?,
                serde_json::to_string(&record.tags)?,
                record.code_version,
                record.source_episode.map(Uuid::to_string),
                record.evidence,
                record.confidence,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
                record.use_count,
                record.success_count,
            ],
        )?;
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

    /// Promotes a memory after evidence is available.
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

        record.verification = Verification::Verified;
        record.evidence = Some(evidence);
        record.updated_at = Utc::now();
        self.update(&record)?;
        Ok(record)
    }

    /// Deprecates a memory.
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
        record.evidence = Some(reason);
        record.updated_at = Utc::now();
        self.update(&record)?;
        Ok(record)
    }

    /// Deprecates memories tied to an older repository revision.
    ///
    /// Memories without a code version are retained because their applicability
    /// cannot be determined from revision metadata alone.
    ///
    /// # Errors
    /// Returns database or validation errors.
    pub fn invalidate_stale_for_version(
        &self,
        repo: &str,
        current_version: &str,
        reason: impl Into<String>,
    ) -> Result<usize> {
        if repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }
        if current_version.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "current_version" });
        }

        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "reason" });
        }

        let now = Utc::now().to_rfc3339();
        let changed = self.connection.execute(
            r#"
            UPDATE memories
            SET verification = 'deprecated', evidence = ?3, updated_at = ?4
            WHERE repo = ?1
              AND code_version IS NOT NULL
              AND code_version != ?2
              AND verification != 'deprecated'
            "#,
            params![repo, current_version, reason, now],
        )?;

        Ok(changed)
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

    /// Records whether a memory produced a successful outcome.
    ///
    /// # Errors
    /// Returns [`VcmError::NotFound`] for an unknown ID.
    pub fn record_outcome(&self, id: Uuid, succeeded: bool) -> Result<MemoryRecord> {
        let mut record = self.get(id)?;
        record.use_count = record.use_count.saturating_add(1);

        if succeeded {
            record.success_count = record.success_count.saturating_add(1);
        }

        record.updated_at = Utc::now();
        self.update(&record)?;
        Ok(record)
    }

    /// Counts one project's memories, by trust state.
    ///
    /// The column holds the state as JSON writes it (`"verified"`); a state
    /// written by a plain `UPDATE` (`invalidate_stale_for_version`) has no
    /// quotes, so both forms count.
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

    /// Retrieves a ranked context bundle with telemetry.
    ///
    /// # Errors
    /// Returns [`VcmError::InvalidLimit`] for limits outside 1..=32.
    pub fn retrieve_with_telemetry(
        &self,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievedMemory>, RetrievalTelemetry)> {
        let started = Instant::now();

        if query.limit == 0 || query.limit > MAX_RETRIEVAL_LIMIT {
            return Err(VcmError::InvalidLimit(query.limit));
        }
        if query.repo.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "repo" });
        }

        let fts_query = build_fts_query(&[
            query.task.as_str(),
            query.failure.as_deref().unwrap_or(""),
        ]);

        let mut telemetry = RetrievalTelemetry::default();
        let mut candidates = Vec::new();

        if !fts_query.is_empty() {
            let mut statement = self.connection.prepare(
                r#"
                SELECT m.*, bm25(memories_fts) AS lexical_rank
                FROM memories_fts
                JOIN memories m ON m.rowid = memories_fts.rowid
                WHERE memories_fts MATCH ?1
                  AND m.repo = ?2
                  AND m.verification != 'deprecated'
                ORDER BY lexical_rank
                LIMIT ?3
                "#,
            )?;

            let rows = statement.query_map(
                params![fts_query, query.repo, MAX_FTS_CANDIDATES as i64],
                |row| {
                    Ok((
                        Self::row_to_record(row)?,
                        row.get::<_, f64>("lexical_rank")?,
                    ))
                },
            )?;

            for row in rows {
                let (record, rank) = row?;
                telemetry.fts_matches += 1;

                let lexical = normalize_bm25(rank);
                if let Some(scored) = score_memory(
                    &record,
                    &query.task,
                    &query.files,
                    query.failure.as_deref(),
                    Some(lexical),
                    Utc::now(),
                    &self.weights,
                ) {
                    candidates.push(scored);
                }
            }
        }

        if candidates.is_empty() {
            let mut statement = self.connection.prepare(
                r#"
                SELECT * FROM memories
                WHERE repo = ?1 AND verification != 'deprecated'
                ORDER BY updated_at DESC LIMIT ?2
                "#,
            )?;

            let rows = statement.query_map(
                params![query.repo, MAX_FTS_CANDIDATES as i64],
                Self::row_to_record,
            )?;

            for record in rows {
                let record = record?;
                if let Some(scored) = score_memory(
                    &record,
                    &query.task,
                    &query.files,
                    query.failure.as_deref(),
                    None,
                    Utc::now(),
                    &self.weights,
                ) {
                    candidates.push(scored);
                }
            }
        }

        telemetry.candidates_scanned = candidates.len();
        telemetry.used_full_text_search = telemetry.fts_matches > 0;
        candidates.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(query.limit);

        telemetry.returned = candidates.len();
        telemetry.elapsed_ms = started.elapsed().as_millis();

        Ok((candidates, telemetry))
    }

    /// Retrieves a ranked context bundle.
    ///
    /// # Errors
    /// See [`Self::retrieve_with_telemetry`].
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievedMemory>> {
        Ok(self.retrieve_with_telemetry(query)?.0)
    }

    /// Renders a compact prompt context block.
    #[must_use]
    pub fn render_context(&self, memories: &[RetrievedMemory]) -> String {
        let mut output = String::from("## Relevant verified memory\n\n");

        if memories.is_empty() {
            output.push_str("No sufficiently relevant prior memory.\n");
            return output;
        }

        for memory in memories {
            output.push_str(&format!(
                "- [{}] {} — {}\n  Evidence: {}\n  Files: {}\n  Match: {}\n",
                memory.record.verification,
                memory.record.title,
                memory.record.content,
                memory.record.evidence.as_deref().unwrap_or("none"),
                if memory.record.files.is_empty() {
                    "none".to_string()
                } else {
                    memory.record.files.join(", ")
                },
                memory.reason,
            ));
        }

        output
    }

    fn update(&self, record: &MemoryRecord) -> Result<()> {
        let changed = self.connection.execute(
            r#"UPDATE memories SET kind=?2,verification=?3,repo=?4,title=?5,content=?6,files_json=?7,tags_json=?8,code_version=?9,source_episode=?10,evidence=?11,confidence=?12,updated_at=?13,use_count=?14,success_count=?15 WHERE id=?1"#,
            params![
                record.id.to_string(),
                serde_json::to_string(&record.kind)?,
                serde_json::to_string(&record.verification)?,
                record.repo,
                record.title,
                record.content,
                serde_json::to_string(&record.files)?,
                serde_json::to_string(&record.tags)?,
                record.code_version,
                record.source_episode.map(Uuid::to_string),
                record.evidence,
                record.confidence,
                record.updated_at.to_rfc3339(),
                record.use_count,
                record.success_count,
            ],
        )?;

        if changed == 0 {
            return Err(VcmError::NotFound(record.id));
        }

        Ok(())
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
        let id: String = row.get("id")?;
        let kind: String = row.get("kind")?;
        let verification: String = row.get("verification")?;
        let files: String = row.get("files_json")?;
        let tags: String = row.get("tags_json")?;
        let created_at: String = row.get("created_at")?;
        let updated_at: String = row.get("updated_at")?;

        Ok(MemoryRecord {
            id: parse_uuid(&id)?,
            kind: serde_json::from_str(&kind).map_err(rusqlite_conversion_error)?,
            verification: serde_json::from_str(&verification)
                .map_err(rusqlite_conversion_error)?,
            repo: row.get("repo")?,
            title: row.get("title")?,
            content: row.get("content")?,
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
            updated_at: parse_timestamp(&updated_at)?,
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

    fn lock(&self) -> Result<MutexGuard<'_, MemoryStore>> {
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
        self.lock()?.resolve_conflict(keep_id, deprecated_ids, reason)
    }

    /// See [`MemoryStore::stats`].
    ///
    /// # Errors
    /// See [`MemoryStore::stats`].
    pub fn stats(&self, repo: &str) -> Result<MemoryStats> {
        self.lock()?.stats(repo)
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

    /// See [`MemoryStore::render_context`].
    #[must_use]
    pub fn render_context(&self, memories: &[RetrievedMemory]) -> String {
        self.lock()
            .map(|store| store.render_context(memories))
            .unwrap_or_else(|_| "Memory store unavailable.".to_string())
    }
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

fn normalize_bm25(rank: f64) -> f32 {
    (1.0 / (1.0 + rank.abs().max(0.0))) as f32
}

fn parse_uuid(value: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
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
    E: std::fmt::Display + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(error),
    )
}

/// Returns a normalized database path.
#[must_use]
pub fn database_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}

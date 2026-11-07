//! Rebuildable vectors in SQLite. The authoritative claim and its hash are
//! checked on every read and write; model changes never reuse old vectors.
use crate::error::{Result, VcmError};
use crate::{
    MemoryEmbedder, MemoryRecord, RetrievalQuery, RetrievedMemory, SharedMemoryStore, Verification,
};
use rusqlite::params;
use std::collections::HashMap;

/// A semantic index bound to an exact model revision and dimension.
#[derive(Clone, Debug)]
pub struct SemanticIndex {
    store: SharedMemoryStore,
    model: String,
    dimensions: usize,
}

impl SemanticIndex {
    /// Opens a derived index. Different model revisions coexist until rebuilt.
    pub fn new(store: SharedMemoryStore, model: &str, dimensions: usize) -> Result<Self> {
        if model.trim().is_empty() {
            return Err(VcmError::EmptyField { field: "model" });
        }
        if dimensions == 0 || dimensions > 65536 {
            return Err(VcmError::EmbeddingDimensionMismatch {
                expected: 1,
                actual: dimensions,
            });
        }
        store.lock()?.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_vectors (
            memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
            model TEXT NOT NULL, claim_hash TEXT NOT NULL, vector_json TEXT NOT NULL,
            PRIMARY KEY(memory_id, model)) WITHOUT ROWID;",
        )?;
        Ok(Self {
            store,
            model: model.to_owned(),
            dimensions,
        })
    }

    /// Embeds missing or changed records without holding the store lock.
    /// A concurrent edit, delete or clear causes the late result to be rejected.
    pub fn backfill(&self, repo: &str, embedder: &dyn MemoryEmbedder) -> Result<usize> {
        self.backfill_limited(repo, embedder, usize::MAX)
    }

    /// Backfills at most `limit` changed claims in one background batch.
    pub fn backfill_limited(
        &self,
        repo: &str,
        embedder: &dyn MemoryEmbedder,
        limit: usize,
    ) -> Result<usize> {
        let generation = self.store.generation(repo)?;
        let records = self.store.list(repo, "")?;
        let mut written = 0;
        for record in records {
            if written >= limit {
                break;
            }
            if record.verification == Verification::Deprecated {
                continue;
            }
            let current: bool = self.store.lock()?.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_vectors WHERE memory_id=?1 AND model=?2 AND claim_hash=?3)",
                params![record.id.to_string(), self.model, record.claim_hash], |row| row.get(0))?;
            if current {
                continue;
            }
            let vector = embedder.embed(&format!("{}\n{}", record.title, record.content))?;
            validate(&vector, self.dimensions)?;
            let guard = self.store.lock()?;
            if guard.generation(repo)? != generation {
                break;
            }
            written += guard.connection.execute("INSERT INTO memory_vectors(memory_id,model,claim_hash,vector_json)
                SELECT id, ?2, claim_hash, ?4 FROM memories WHERE id=?1 AND repo=?5 AND claim_hash=?3
                ON CONFLICT(memory_id,model) DO UPDATE SET claim_hash=excluded.claim_hash, vector_json=excluded.vector_json",
                params![record.id.to_string(), self.model, record.claim_hash, serde_json::to_string(&vector)?, repo])?;
        }
        Ok(written)
    }

    /// Removes this model's vectors for a project; the next backfill rebuilds them.
    pub fn clear(&self, repo: &str) -> Result<usize> {
        Ok(self.store.lock()?.connection.execute("DELETE FROM memory_vectors WHERE model=?1 AND memory_id IN (SELECT id FROM memories WHERE repo=?2)", params![self.model, repo])?)
    }

    /// Searches exact cosine similarity within the query's repository and filters.
    /// Thresholds must be calibrated for the configured model, independently of rank fusion.
    pub fn search(
        &self,
        query: &RetrievalQuery,
        vector: &[f32],
        minimum: f32,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        self.search_filtered(query, vector, minimum, |_| true)
    }

    /// Searches after applying caller visibility/applicability to each candidate.
    pub fn search_filtered(
        &self,
        query: &RetrievalQuery,
        vector: &[f32],
        minimum: f32,
        accept: impl Fn(&MemoryRecord) -> bool,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        validate(vector, self.dimensions)?;
        if !minimum.is_finite() || !(0.0..=1.0).contains(&minimum) {
            return Err(VcmError::OutOfRange {
                field: "similarity",
                minimum: 0.0,
                maximum: 1.0,
                actual: minimum as f64,
            });
        }
        if query.limit == 0 || query.limit > 32 {
            return Err(VcmError::InvalidLimit(query.limit));
        }
        let guard = self.store.lock()?;
        let mut statement = guard.connection.prepare("SELECT v.memory_id, v.vector_json FROM memory_vectors v JOIN memories m ON m.id=v.memory_id WHERE m.repo=?1 AND v.model=?2 AND v.claim_hash=m.claim_hash")?;
        let rows = statement.query_map(params![query.repo, self.model], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (id, json) = row?;
            let id = uuid::Uuid::parse_str(&id)
                .map_err(|_| VcmError::EmptyField { field: "memory_id" })?;
            let record = guard.get(id)?;
            if !accept(&record) {
                continue;
            }
            if record.verification == Verification::Deprecated
                || (!query.verifications.is_empty()
                    && !query.verifications.contains(&record.verification))
                || record.tags.iter().any(|t| query.excluded_tags.contains(t))
            {
                continue;
            }
            let stored: Vec<f32> = serde_json::from_str(&json)?;
            validate(&stored, self.dimensions)?;
            let similarity = cosine(vector, &stored);
            if similarity >= minimum {
                found.push((record, similarity));
            }
        }
        found.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        crate::retrieval::retain_distinct(&mut found, |(record, _)| record);
        found.truncate(query.limit);
        Ok(found)
    }

    /// Fuses already-admitted lexical and semantic pools by reciprocal ranks.
    /// The caller applies its visibility policy before rendering the result.
    pub fn retrieve(
        &self,
        query: &RetrievalQuery,
        embedder: &dyn MemoryEmbedder,
        minimum: f32,
    ) -> Result<Vec<RetrievedMemory>> {
        self.retrieve_filtered(query, embedder, minimum, |_| true)
    }

    /// Hybrid retrieval with caller eligibility applied before either pool is limited.
    pub fn retrieve_filtered(
        &self,
        query: &RetrievalQuery,
        embedder: &dyn MemoryEmbedder,
        minimum: f32,
        accept: impl Fn(&MemoryRecord) -> bool,
    ) -> Result<Vec<RetrievedMemory>> {
        let lexical = self.store.retrieve_filtered(query, &accept)?;
        let vector = embedder.embed(&format!(
            "{}\n{}",
            query.task,
            query.failure.as_deref().unwrap_or("")
        ))?;
        let semantic = self.search_filtered(query, &vector, minimum, &accept)?;
        let mut fused = HashMap::new();
        for (rank, mut hit) in lexical.into_iter().enumerate() {
            hit.score = 1.0 / (61 + rank) as f32;
            fused.insert(hit.record.id, hit);
        }
        for (rank, (record, similarity)) in semantic.into_iter().enumerate() {
            let score = 1.0 / (61 + rank) as f32;
            // The two pools may choose different IDs for the same exact copy.
            // Fuse their evidence of relevance into one claim, not two slots.
            if let Some(hit) = fused
                .values_mut()
                .find(|hit| hit.record.same_context_claim(&record))
            {
                hit.score += score;
                hit.reason.push_str(", semantic match");
            } else {
                fused.insert(
                    record.id,
                    RetrievedMemory {
                        record,
                        score,
                        reason: format!("semantic cosine {similarity:.3}"),
                        components: Default::default(),
                    },
                );
            }
        }
        let mut found: Vec<_> = fused.into_values().collect();
        found.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.record.id.cmp(&b.record.id))
        });
        crate::retrieval::deduplicate_memories(&mut found);
        found.truncate(query.limit);
        Ok(found)
    }
}

fn validate(vector: &[f32], expected: usize) -> Result<()> {
    if vector.len() != expected {
        return Err(VcmError::EmbeddingDimensionMismatch {
            expected,
            actual: vector.len(),
        });
    }
    if vector.iter().any(|x| !x.is_finite()) || vector.iter().all(|x| *x == 0.0) {
        return Err(VcmError::OutOfRange {
            field: "embedding",
            minimum: -1.0,
            maximum: 1.0,
            actual: f64::NAN,
        });
    }
    Ok(())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f64 = a.iter().zip(b).map(|(a, b)| *a as f64 * *b as f64).sum();
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    (dot / (norm(a) * norm(b))).clamp(-1.0, 1.0) as f32
}

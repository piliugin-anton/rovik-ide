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
    short_query: Option<(usize, f32)>,
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
            PRIMARY KEY(memory_id, model)) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS memory_vector_conclusions (
            memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
            model TEXT NOT NULL, claim_hash TEXT NOT NULL, vector_json TEXT NOT NULL,
            PRIMARY KEY(memory_id, model)) WITHOUT ROWID;",
        )?;
        Ok(Self {
            store,
            model: model.to_owned(),
            dimensions,
            short_query: None,
        })
    }

    /// Calibrates similarity separately for short, distinctive queries. `scale`
    /// multiplies the normal cutoff; strict vector searches remain unchanged.
    pub fn with_short_query_scale(mut self, words: usize, scale: f32) -> Result<Self> {
        if words == 0 || words > 16 {
            return Err(VcmError::OutOfRange {
                field: "short_query_words",
                minimum: 1.0,
                maximum: 16.0,
                actual: words as f64,
            });
        }
        if !scale.is_finite() || !(0.0..=1.0).contains(&scale) {
            return Err(VcmError::OutOfRange {
                field: "short_query_scale",
                minimum: 0.0,
                maximum: 1.0,
                actual: scale as f64,
            });
        }
        self.short_query = Some((words, scale));
        Ok(self)
    }

    /// Embeds missing or changed records without holding the store lock.
    /// Truncated records may also get one bounded conclusion vector.
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
        self.backfill_controlled(repo, embedder, limit, || true)
    }

    /// Completes the pending snapshot while `should_continue` permits it.
    /// The predicate runs without the store lock, before inference and before
    /// committing its result, so disabling memory also rejects in-flight work.
    pub fn backfill_while(
        &self,
        repo: &str,
        embedder: &dyn MemoryEmbedder,
        should_continue: impl Fn() -> bool,
    ) -> Result<usize> {
        self.backfill_controlled(repo, embedder, usize::MAX, should_continue)
    }

    fn backfill_controlled(
        &self,
        repo: &str,
        embedder: &dyn MemoryEmbedder,
        limit: usize,
        should_continue: impl Fn() -> bool,
    ) -> Result<usize> {
        let generation = self.store.generation(repo)?;
        let records = self.store.list(repo, "")?;
        let mut written = 0;
        for record in records {
            if written >= limit || self.store.generation(repo)? != generation {
                break;
            }
            if record.verification == Verification::Deprecated {
                continue;
            }
            let current: bool = self.store.lock()?.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_vectors WHERE memory_id=?1 AND model=?2 AND claim_hash=?3)",
                params![record.id.to_string(), self.model, record.claim_hash], |row| row.get(0))?;
            let document = format!("{}\n{}", record.title, record.content);
            let conclusion = overflow_conclusion(&document, embedder.input_character_limit());
            let conclusion_current: bool = conclusion.is_none() || self.store.lock()?.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_vector_conclusions WHERE memory_id=?1 AND model=?2 AND claim_hash=?3)",
                params![record.id.to_string(), self.model, record.claim_hash], |row| row.get(0))?;
            if current && conclusion_current {
                continue;
            }
            if !should_continue() {
                break;
            }
            let vector = if current {
                None
            } else {
                let vector = embedder.embed(&document)?;
                validate(&vector, self.dimensions)?;
                Some(vector)
            };
            if !should_continue() {
                break;
            }
            let conclusion_vector = if conclusion_current {
                None
            } else {
                conclusion.and_then(|text| {
                    match embedder.embed(&text).and_then(|vector| {
                        validate(&vector, self.dimensions)?;
                        Ok(vector)
                    }) {
                        Ok(vector) => Some(vector),
                        Err(error) => {
                            eprintln!("memory: conclusion embedding unavailable; preserving primary index: {error}");
                            None
                        }
                    }
                })
            };
            if !should_continue() {
                break;
            }
            let mut guard = self.store.lock()?;
            if guard.generation(repo)? != generation {
                break;
            }
            let transaction = guard.connection.transaction()?;
            let mut changed = 0;
            if let Some(vector) = vector {
                changed += transaction.execute("INSERT INTO memory_vectors(memory_id,model,claim_hash,vector_json)
                SELECT id, ?2, claim_hash, ?4 FROM memories WHERE id=?1 AND repo=?5 AND claim_hash=?3
                ON CONFLICT(memory_id,model) DO UPDATE SET claim_hash=excluded.claim_hash, vector_json=excluded.vector_json",
                params![record.id.to_string(), self.model, record.claim_hash, serde_json::to_string(&vector)?, repo])?;
            }
            if let Some(vector) = conclusion_vector {
                changed += transaction.execute("INSERT INTO memory_vector_conclusions(memory_id,model,claim_hash,vector_json)
                SELECT id, ?2, claim_hash, ?4 FROM memories WHERE id=?1 AND repo=?5 AND claim_hash=?3
                ON CONFLICT(memory_id,model) DO UPDATE SET claim_hash=excluded.claim_hash, vector_json=excluded.vector_json",
                params![record.id.to_string(), self.model, record.claim_hash, serde_json::to_string(&vector)?, repo])?;
            }
            transaction.commit()?;
            written += usize::from(changed > 0);
        }
        Ok(written)
    }

    /// Removes this model's vectors for a project; the next backfill rebuilds them.
    pub fn clear(&self, repo: &str) -> Result<usize> {
        let mut guard = self.store.lock()?;
        let transaction = guard.connection.transaction()?;
        transaction.execute("DELETE FROM memory_vector_conclusions WHERE model=?1 AND memory_id IN (SELECT id FROM memories WHERE repo=?2)", params![self.model, repo])?;
        let removed = transaction.execute("DELETE FROM memory_vectors WHERE model=?1 AND memory_id IN (SELECT id FROM memories WHERE repo=?2)", params![self.model, repo])?;
        transaction.commit()?;
        Ok(removed)
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
        self.search_view_filtered(query, vector, minimum, accept, false)
    }

    fn search_view_filtered(
        &self,
        query: &RetrievalQuery,
        vector: &[f32],
        minimum: f32,
        accept: impl Fn(&MemoryRecord) -> bool,
        conclusion: bool,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        validate(vector, self.dimensions)?;
        validate_threshold(minimum)?;
        if query.limit == 0 || query.limit > 32 {
            return Err(VcmError::InvalidLimit(query.limit));
        }
        let guard = self.store.lock()?;
        let table = if conclusion {
            "memory_vector_conclusions"
        } else {
            "memory_vectors"
        };
        let mut statement = guard.connection.prepare(&format!("SELECT v.memory_id, v.vector_json FROM {table} v JOIN memories m ON m.id=v.memory_id WHERE m.repo=?1 AND v.model=?2 AND v.claim_hash=m.claim_hash"))?;
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
        validate_threshold(minimum)?;
        let lexical = self.store.retrieve_filtered(query, &accept)?;
        let vector = embedder.embed(&format!(
            "{}\n{}",
            query.task,
            query.failure.as_deref().unwrap_or("")
        ))?;
        let text = format!("{} {}", query.task, query.failure.as_deref().unwrap_or(""));
        let words = text.split_whitespace().count();
        let admission = self
            .short_query
            .filter(|(limit, _)| words > 0 && words <= *limit)
            .filter(|_| !crate::retrieval::build_fts_query(&[&text]).is_empty())
            .map_or(minimum, |(_, scale)| minimum * scale);
        let semantic = self.search_filtered(query, &vector, admission, &accept)?;
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
        // The separator also consumes one character of the encoder window.
        // Keep incumbent ranks; only unused slots receive diagnostic evidence.
        let failure = query
            .failure
            .as_deref()
            .filter(|text| !text.trim().is_empty());
        let truncated = embedder.input_character_limit().is_some_and(|limit| {
            limit > 0 && query.task.chars().count().saturating_add(1) >= limit
        });
        let mut diagnostic_vector = None;
        if let Some(failure) = failure.filter(|_| truncated && found.len() < query.limit) {
            let supplementary = embedder.embed(failure).and_then(|diagnostic| {
                let hits = self.search_filtered(query, &diagnostic, minimum, |record| {
                    accept(record)
                        && !found
                            .iter()
                            .any(|hit| hit.record.same_context_claim(record))
                })?;
                diagnostic_vector = Some(diagnostic);
                Ok(hits)
            });
            match supplementary {
                Ok(hits) => {
                    let last_score = found.last().map_or(1.0 / 61.0, |hit| hit.score);
                    for (rank, (record, similarity)) in hits.into_iter().enumerate() {
                        found.push(RetrievedMemory {
                            record,
                            score: last_score / (rank + 2) as f32,
                            reason: format!("diagnostic cosine {similarity:.3}"),
                            components: Default::default(),
                        });
                        if found.len() == query.limit {
                            break;
                        }
                    }
                }
                // A busy encoder or an unavailable supplementary view must not
                // discard answers from the successful original lookup.
                Err(error) => eprintln!(
                    "memory: diagnostic lookup unavailable; preserving current results: {error}"
                ),
            }
        }
        let exact_title = found.iter().any(|hit| {
            hit.record.title.eq_ignore_ascii_case(query.task.trim())
                || query
                    .failure
                    .as_deref()
                    .is_some_and(|failure| hit.record.title.eq_ignore_ascii_case(failure.trim()))
        });
        if found.len() < query.limit && !exact_title {
            match self.store.identifier_candidates_filtered(query, |record| {
                accept(record)
                    && !found
                        .iter()
                        .any(|hit| hit.record.same_context_claim(record))
            }) {
                Ok(hits) => {
                    let last_score = found.last().map_or(1.0 / 61.0, |hit| hit.score);
                    for (rank, mut hit) in
                        hits.into_iter().take(query.limit - found.len()).enumerate()
                    {
                        hit.score = last_score / (rank + 2) as f32;
                        hit.reason.push_str(", supplementary complete symbol match");
                        found.push(hit);
                    }
                }
                Err(error) => eprintln!(
                    "memory: symbol lookup unavailable; preserving current results: {error}"
                ),
            }
        }
        if found.len() < query.limit
            && !exact_title
            && embedder
                .input_character_limit()
                .is_some_and(|limit| limit > 0)
        {
            // Preserve ordinary conclusion matches before trying a diagnostic
            // that was hidden by the task window. Its inference already ran.
            for (view, reason) in std::iter::once((&vector, "conclusion")).chain(
                diagnostic_vector
                    .as_ref()
                    .map(|vector| (vector, "diagnostic conclusion")),
            ) {
                if found.len() == query.limit {
                    break;
                }
                match self.search_view_filtered(
                    query,
                    view,
                    minimum,
                    |record| {
                        accept(record)
                            && !found
                                .iter()
                                .any(|hit| hit.record.same_context_claim(record))
                    },
                    true,
                ) {
                    Ok(hits) => {
                        let last_score = found.last().map_or(1.0 / 61.0, |hit| hit.score);
                        for (rank, (record, similarity)) in
                            hits.into_iter().take(query.limit - found.len()).enumerate()
                        {
                            found.push(RetrievedMemory {
                                record,
                                score: last_score / (rank + 2) as f32,
                                reason: format!("{reason} cosine {similarity:.3}"),
                                components: Default::default(),
                            });
                        }
                    }
                    Err(error) => eprintln!(
                        "memory: {reason} lookup unavailable; preserving current results: {error}"
                    ),
                }
            }
        }
        // A terse request can name one exact compound symbol while broad
        // context fills the pool. Preserve the leaders and broader requests.
        if found.len() == query.limit
            && query.limit > 1
            && query.files.is_empty()
            && query.failure.is_none()
            && !exact_title
            && focused_identifier(&query.task)
        {
            // Check incumbents in their own filtered scan: an already supplied
            // match can rank below the separate supplementary pool's bound.
            let supplementary = self
                .store
                .identifier_candidates_filtered(query, |record| {
                    accept(record)
                        && found
                            .iter()
                            .any(|hit| hit.record.same_context_claim(record))
                })
                .and_then(|represented| {
                    if represented.is_empty() {
                        Ok(self
                            .store
                            .identifier_candidates_filtered(query, &accept)?
                            .into_iter()
                            .next())
                    } else {
                        Ok(None)
                    }
                });
            match supplementary {
                Ok(Some(mut candidate)) => {
                    found.pop();
                    candidate.score = found.last().map_or(1.0 / 61.0, |hit| hit.score) / 2.0;
                    candidate.reason.push_str(", focused complete symbol match");
                    found.push(candidate);
                }
                Ok(None) => {}
                Err(error) => eprintln!("memory: focused symbol lookup unavailable; preserving current results: {error}"),
            }
        }
        if found.len() == query.limit && query.limit > 1 && query.failure.is_none() && !exact_title
        {
            let minimum_names = if query.files.is_empty() { 2 } else { 1 };
            if let Some(names) = focused_identifiers(&query.task, minimum_names) {
                let supplementary = (|| -> Result<_> {
                    let mut protected = vec![false; found.len()];
                    protected[0] = true;
                    let mut anchored_leader = names.iter().any(|name| {
                        crate::retrieval::identifier_title_matches(name, &found[0].record.title)
                    });
                    for (i, hit) in found.iter().enumerate() {
                        // A file hint is independent relevance evidence. Keep
                        // every matching incumbent, even at the pool's tail.
                        if query.files.iter().any(|file| {
                            hit.record
                                .files
                                .iter()
                                .any(|stored| crate::retrieval::path_overlap(file, stored))
                        }) || focused_words(&query.task).any(|word| {
                            hit.record.title.eq_ignore_ascii_case(word)
                                || hit.record.title.eq_ignore_ascii_case(
                                    word.trim_matches(|c: char| !c.is_alphanumeric()),
                                )
                        }) {
                            protected[i] = true;
                            anchored_leader |= i == 0;
                        }
                    }
                    let mut additions: Vec<RetrievedMemory> = Vec::new();
                    for name in &names {
                        let mut lookup = query.clone();
                        lookup.task = (*name).to_owned();
                        let represented =
                            self.store
                                .identifier_candidates_filtered(&lookup, |record| {
                                    accept(record)
                                        && (found
                                            .iter()
                                            .any(|hit| hit.record.same_context_claim(record))
                                            || additions
                                                .iter()
                                                .any(|hit| hit.record.same_context_claim(record)))
                                })?;
                        for (i, hit) in found.iter().enumerate() {
                            if represented
                                .iter()
                                .any(|r| r.record.same_context_claim(&hit.record))
                            {
                                protected[i] = true;
                                anchored_leader |= i == 0;
                            }
                        }
                        if represented.is_empty() {
                            let Some(mut candidate) = self
                                .store
                                .identifier_candidates_filtered(&lookup, &accept)?
                                .into_iter()
                                .next()
                            else {
                                return Ok(None);
                            };
                            candidate
                                .reason
                                .push_str(", requested complete symbol match");
                            additions.push(candidate);
                        }
                    }
                    let mut slots: Vec<_> =
                        (1..found.len()).rev().filter(|i| !protected[*i]).collect();
                    if additions.len() > slots.len() {
                        return Ok(None);
                    }
                    // File coverage must not cancel the successful name plan.
                    // Keep its slot assignments; use only remaining positions.
                    let file_coverage = (|| -> Result<_> {
                        let mut files: Vec<RetrievedMemory> = Vec::new();
                        for file in &query.files {
                            if found.iter().chain(&additions).chain(&files).any(|hit| {
                                hit.record
                                    .files
                                    .iter()
                                    .any(|stored| crate::retrieval::path_overlap(file, stored))
                            }) {
                                continue;
                            }
                            let mut lookup = query.clone();
                            lookup.files = vec![file.clone()];
                            if let Some(mut hit) = self
                                .store
                                .file_candidates_filtered(&lookup, &accept)?
                                .into_iter()
                                .next()
                            {
                                hit.reason.push_str(", requested file coverage");
                                files.push(hit);
                            }
                        }
                        Ok(files)
                    })();
                    match file_coverage {
                        Ok(files) if additions.len() + files.len() <= slots.len() => {
                            additions.extend(files);
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!(
                            "memory: file coverage unavailable; preserving name plan: {error}"
                        ),
                    }
                    if let Some(budget) = query.packet_budget {
                        let title_coverage = (|| -> Result<_> {
                            let mut titles: Vec<RetrievedMemory> = Vec::new();
                            for name in &names {
                                let name = *name;
                                if found.iter().chain(&additions).chain(&titles).any(|hit| {
                                    crate::retrieval::identifier_title_matches(
                                        name,
                                        &hit.record.title,
                                    )
                                        // Keep oversized incumbents in place, but they
                                        // cannot supply complete title coverage.
                                        && crate::retrieval::estimate_tokens(&hit.record.content)
                                            <= budget
                                }) {
                                    continue;
                                }
                                let mut lookup = query.clone();
                                lookup.task = name.to_owned();
                                if let Some(mut hit) = self
                                    .store
                                    .identifier_candidates_filtered(&lookup, |record| {
                                        accept(record)
                                            && crate::retrieval::identifier_title_matches(
                                                name,
                                                &record.title,
                                            )
                                            // A claim that exceeds the entire allowance
                                            // cannot become a complete title addition.
                                            // Reject it before the collector's bound so
                                            // a shorter eligible claim can be considered.
                                            && crate::retrieval::estimate_tokens(&record.content)
                                                <= budget
                                    })?
                                    .into_iter()
                                    .next()
                                {
                                    hit.reason = "requested symbol title".to_owned();
                                    titles.push(hit);
                                }
                            }
                            Ok(titles)
                        })();
                        match title_coverage {
                            Ok(titles)
                                if !titles.is_empty()
                                    && additions.len() + titles.len() <= slots.len() + 1 =>
                            {
                                // Preserve the complete protected claims from the exact
                                // legacy plan, including names/files added to that plan.
                                let mut legacy = found.clone();
                                for (slot, hit) in slots.iter().copied().zip(&additions) {
                                    let mut hit = hit.clone();
                                    hit.score = legacy[slot].score;
                                    legacy[slot] = hit;
                                }
                                let before = self.store.render_packet_with_evidence(&legacy, budget);
                                let protected_ids: Vec<_> = legacy
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| {
                                        protected[*i]
                                            || slots.iter().take(additions.len()).any(|slot| slot == i)
                                    })
                                    .map(|(_, hit)| hit.record.id)
                                    .filter(|id| before.complete_ids.contains(id))
                                    .collect();
                                // Keep successful legacy placements. An unanchored
                                // leader may supply one slot only when every explicit
                                // subject is then delivered in full.
                                for release_leader in [false, true] {
                                    if release_leader
                                        && (anchored_leader
                                            || names.len() + query.files.len() + 1 < query.limit)
                                    {
                                        continue;
                                    }
                                    let mut trial_slots = slots.clone();
                                    if release_leader {
                                        trial_slots.insert(additions.len(), 0);
                                    }
                                    if additions.len() + titles.len() > trial_slots.len() {
                                        continue;
                                    }
                                    let mut trial = legacy.clone();
                                    for (slot, hit) in trial_slots
                                        .iter()
                                        .copied()
                                        .skip(additions.len())
                                        .zip(&titles)
                                    {
                                        let mut hit = hit.clone();
                                        hit.score = trial[slot].score;
                                        trial[slot] = hit;
                                    }
                                    let after = self.store.render_packet_with_evidence(&trial, budget);
                                    if protected_ids
                                        .iter()
                                        .filter(|id| !release_leader || **id != legacy[0].record.id)
                                        .all(|id| after.complete_ids.contains(id))
                                        && titles
                                            .iter()
                                            .all(|hit| after.complete_ids.contains(&hit.record.id))
                                        && (!release_leader
                                            || complete_focused_coverage(
                                                query,
                                                &names,
                                                &trial,
                                                &after.complete_ids,
                                            ))
                                    {
                                        slots = trial_slots;
                                        additions.extend(titles);
                                        break;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("memory: title coverage unavailable; preserving legacy plan: {error}"),
                        }
                    }
                    Ok(Some((slots, additions)))
                })();
                match supplementary {
                    Ok(Some((slots, additions))) => {
                        for (slot, mut hit) in slots.into_iter().zip(additions) {
                            hit.score = found[slot].score;
                            found[slot] = hit;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!(
                        "memory: multi-symbol lookup unavailable; preserving results: {error}"
                    ),
                }
            }
        }
        Ok(found)
    }
}

// A leader may yield only to a complete, fully subscribed explicit request.
// Canonical aliases and overlapping file hints do not create extra subjects.
fn complete_focused_coverage(
    query: &RetrievalQuery,
    names: &[&str],
    memories: &[RetrievedMemory],
    complete_ids: &[uuid::Uuid],
) -> bool {
    let complete: Vec<_> = memories
        .iter()
        .filter(|hit| complete_ids.contains(&hit.record.id))
        .map(|hit| &hit.record)
        .collect();
    let mut identifiers = Vec::new();
    for name in names {
        let clause = crate::retrieval::identifier_fts_query(&[name]);
        if !identifiers.contains(&clause) {
            if !complete
                .iter()
                .any(|record| crate::retrieval::identifier_title_matches(name, &record.title))
            {
                return false;
            }
            identifiers.push(clause);
        }
    }
    let mut files: Vec<&str> = Vec::new();
    for file in &query.files {
        if files
            .iter()
            .any(|prior| crate::retrieval::path_overlap(prior, file))
        {
            continue;
        }
        if !complete.iter().any(|record| {
            record
                .files
                .iter()
                .any(|stored| crate::retrieval::path_overlap(stored, file))
        }) {
            return false;
        }
        files.push(file);
    }
    let mut literals = Vec::new();
    for word in focused_words(&query.task) {
        if !crate::retrieval::identifier_fts_query(&[word]).is_empty() {
            continue;
        }
        let literal = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_ascii_lowercase();
        if !crate::retrieval::is_distinctive(&literal) || literals.contains(&literal) {
            continue;
        }
        if !complete
            .iter()
            .any(|record| record.title.eq_ignore_ascii_case(&literal))
        {
            return false;
        }
        literals.push(literal);
    }
    identifiers.len() + files.len() + literals.len() >= query.limit
}

fn focused_identifiers(task: &str, minimum_names: usize) -> Option<Vec<&str>> {
    let words: Vec<_> = focused_words(task).collect();
    if !(minimum_names..=5).contains(&words.len()) || !focused_scope(task) {
        return None;
    }
    let mut names = Vec::new();
    let mut context = 0;
    for word in words {
        let clause = crate::retrieval::identifier_fts_query(&[word]);
        if clause.is_empty() {
            context += 1;
        } else if clause.contains(" OR ") {
            return None;
        } else if !names.contains(&word) {
            names.push(word);
        }
    }
    (context <= 1 && (minimum_names..=4).contains(&names.len())).then_some(names)
}

fn focused_words(task: &str) -> impl Iterator<Item = &str> {
    task.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';'))
        .filter(|word| !word.is_empty())
}

fn focused_identifier(task: &str) -> bool {
    if !(1..=2).contains(&task.split_whitespace().count()) || !focused_scope(task) {
        return false;
    }
    let clause = crate::retrieval::identifier_fts_query(&[task]);
    !clause.is_empty() && !clause.contains(" OR ")
}

fn focused_scope(task: &str) -> bool {
    if task.contains(['*', '?'])
        || task.chars().any(char::is_numeric)
        || task.split(|c: char| !c.is_ascii_alphabetic()).any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "all"
                    | "every"
                    | "each"
                    | "both"
                    | "either"
                    | "any"
                    | "everything"
                    | "entire"
                    | "whole"
                    | "many"
                    | "several"
                    | "multiple"
            )
        })
    {
        return false;
    }
    true
}

fn overflow_conclusion(document: &str, limit: Option<usize>) -> Option<String> {
    let limit = limit.filter(|limit| *limit > 0)?;
    let document = document.trim_end();
    if document.chars().count() <= limit {
        return None;
    }
    let document = document.replace("\r\n", "\n");
    // Require a complete paragraph or sentence beyond the primary window.
    // Arbitrary suffixes can discard the context of a claim.
    let (prefix, paragraph) = if let Some(parts) = document.rsplit_once("\n\n") {
        parts
    } else {
        // A complete sentence retains its linguistic boundary; an arbitrary
        // suffix can turn a fragment into an unrelated semantic match.
        let (prefix, sentence) = document.rsplit_once(". ")?;
        let previous = prefix.split_whitespace().last()?;
        if previous.len() < 3
            || !previous.chars().all(char::is_alphabetic)
            || !sentence.chars().next()?.is_uppercase()
            || !matches!(sentence.chars().last()?, '.' | '!' | '?')
            || sentence.chars().count() > limit
        {
            return None;
        }
        (prefix, sentence)
    };
    if prefix.chars().count().saturating_add(2) < limit {
        return None;
    }
    let paragraph = paragraph.trim();
    Some(
        paragraph
            .chars()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    )
}

fn validate_threshold(minimum: f32) -> Result<()> {
    if !minimum.is_finite() || !(0.0..=1.0).contains(&minimum) {
        return Err(VcmError::OutOfRange {
            field: "similarity",
            minimum: 0.0,
            maximum: 1.0,
            actual: minimum as f64,
        });
    }
    Ok(())
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

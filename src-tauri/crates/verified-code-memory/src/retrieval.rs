use crate::model::{MemoryKind, MemoryRecord, RetrievedMemory, Verification};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::cmp::Ordering;

pub(crate) fn compare_memories(left: &RetrievedMemory, right: &RetrievedMemory) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| {
            right
                .record
                .content_updated_at
                .cmp(&left.record.content_updated_at)
        })
        .then_with(|| left.record.id.cmp(&right.record.id))
}

/// Fills a bounded pool with admissible, distinct claims rather than raw rows.
/// The index still controls scan order; rejected rows and exact copies cannot
/// hide the next independent claim. Keep the best representative of a copy.
pub(crate) fn collect_ranked_candidates(
    rows: impl Iterator<Item = crate::Result<(MemoryRecord, Option<f32>)>>,
    query: &crate::RetrievalQuery,
    accept: &impl Fn(&MemoryRecord) -> bool,
    now: DateTime<Utc>,
    weights: &RetrievalWeights,
    limit: usize,
    telemetry: &mut RetrievalTelemetry,
) -> crate::Result<Vec<RetrievedMemory>> {
    let mut found: Vec<RetrievedMemory> = Vec::new();
    for row in rows {
        let (record, lexical) = row?;
        telemetry.candidates_scanned += 1;
        if !accept(&record) {
            telemetry.rejected += 1;
            continue;
        }
        let Some(hit) = score_memory(
            &record,
            &query.task,
            &query.files,
            query.failure.as_deref(),
            lexical,
            now,
            weights,
        ) else {
            telemetry.rejected += 1;
            continue;
        };
        if let Some(copy) = found
            .iter_mut()
            .find(|seen| seen.record.same_context_claim(&hit.record))
        {
            if compare_memories(&hit, copy).is_lt() {
                *copy = hit;
            }
        } else {
            found.push(hit);
            if found.len() >= limit {
                break;
            }
        }
    }
    Ok(found)
}

/// Removes interchangeable copies from ranked results, retaining the first
/// (highest-ranked) representative. Stored records and their evidence remain
/// untouched; distinct scopes, sources and claims always remain eligible.
pub fn deduplicate_memories(memories: &mut Vec<RetrievedMemory>) {
    retain_distinct(memories, |hit| &hit.record);
}

pub(crate) fn retain_distinct<T>(items: &mut Vec<T>, record: impl Fn(&T) -> &MemoryRecord) {
    let mut unique = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        if !unique
            .iter()
            .any(|seen| record(&item).same_context_claim(record(seen)))
        {
            unique.push(item);
        }
    }
    *items = unique;
}

/// Retrieval weights.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalWeights {
    /// Weight for FTS5/BM25 relevance.
    pub lexical: f32,
    /// Weight for task-term overlap.
    pub text_match: f32,
    /// Weight for file overlap.
    pub file_match: f32,
    /// Weight for failure overlap.
    pub failure_match: f32,
    /// Verified-memory bonus.
    pub verified: f32,
    /// Procedure/project-fact bonus.
    pub kind_bonus: f32,
    /// Recency weight.
    pub recency: f32,
    /// Historical success weight.
    pub success_rate: f32,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            lexical: 2.0,
            text_match: 1.0,
            file_match: 1.5,
            failure_match: 2.0,
            verified: 1.25,
            kind_bonus: 0.5,
            recency: 0.75,
            // A successful run is an association, not claim-specific proof.
            // Keep the counter for audit but do not reward it by default.
            success_rate: 0.0,
        }
    }
}

/// Score components.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct MemoryScore {
    /// FTS5 and task-term relevance.
    pub lexical: f32,
    /// File overlap.
    pub file: f32,
    /// Failure overlap.
    pub failure: f32,
    /// Verification and kind trust.
    pub trust: f32,
    /// Recency.
    pub recency: f32,
    /// Historical success.
    pub outcome: f32,
}

impl MemoryScore {
    /// Total score.
    #[must_use]
    pub fn total(&self) -> f32 {
        self.lexical + self.file + self.failure + self.trust + self.recency + self.outcome
    }
}

/// Retrieval telemetry: what the query looked at, and what it did with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RetrievalTelemetry {
    /// Candidate records considered, from every pool.
    pub candidates_scanned: usize,
    /// Full-text candidates before reranking.
    pub fts_matches: usize,
    /// File candidates before reranking.
    pub file_matches: usize,
    /// Whether full-text search found anything.
    pub used_full_text_search: bool,
    /// Candidates the relevance gate turned down.
    pub rejected: usize,
    /// Records returned.
    pub returned: usize,
    /// Latency in milliseconds.
    pub elapsed_ms: u128,
}

/// Scores a memory, and decides whether the task is about it at all.
///
/// Relevance is what admits a memory: it shares the task's words, names one of
/// its files, or shares the failure's words. Trust, kind, recency and a past
/// success only order the memories that already belong to the task — they
/// cannot make an unrelated record a weak match, which is how a query that
/// matches nothing used to return the project's most trusted memory.
///
/// `lexical_score` is the strength of the record's full-text match, larger for
/// a better match ([`bm25_relevance`]); `None` when the record came from
/// another candidate pool. A caller offering a record with no relevance signal
/// at all gets `None` back.
#[must_use]
pub fn score_memory(
    record: &MemoryRecord,
    task: &str,
    files: &[String],
    failure: Option<&str>,
    lexical_score: Option<f32>,
    now: DateTime<Utc>,
    weights: &RetrievalWeights,
) -> Option<RetrievedMemory> {
    if record.verification == Verification::Deprecated {
        return None;
    }

    let task_terms = terms(task);
    let mut content_terms = terms(&record.title);
    content_terms.extend(terms(&record.content));
    content_terms.extend(record.tags.iter().map(|tag| tag.to_lowercase()));

    let mut components = MemoryScore::default();
    let mut reasons = Vec::new();

    if let Some(score) = lexical_score {
        components.lexical = score * weights.lexical;
        reasons.push("BM25 lexical match".to_string());
    }

    let text_hits = task_terms
        .iter()
        .filter(|term| {
            content_terms
                .iter()
                .any(|candidate| candidate.contains(*term))
        })
        .count();

    if text_hits > 0 {
        components.lexical +=
            (text_hits as f32 / task_terms.len().max(1) as f32) * weights.text_match;
        reasons.push(format!("{text_hits} task-term matches"));
    }

    let file_hits = files
        .iter()
        .filter(|file| record.files.iter().any(|stored| path_overlap(file, stored)))
        .count();

    if file_hits > 0 {
        components.file = (file_hits as f32).min(3.0) * weights.file_match;
        reasons.push(format!("{file_hits} file matches"));
    }

    let failure_hits = failure
        .map(|failure| {
            terms(failure)
                .into_iter()
                .filter(|term| is_distinctive(term))
                .filter(|term| {
                    content_terms
                        .iter()
                        .any(|candidate| candidate.contains(term))
                })
                .count()
        })
        .unwrap_or(0);

    if failure_hits > 0 {
        components.failure = (failure_hits as f32).min(3.0) * weights.failure_match;
        reasons.push(format!("{failure_hits} failure-term matches"));
    }

    // The gate: no shared words, no file, no failure term — no relevance, so
    // no result, whatever the record's trust and however recent it is.
    if lexical_score.is_none() && file_hits == 0 && failure_hits == 0 {
        return None;
    }

    let verification_bonus = match record.verification {
        Verification::Verified => weights.verified,
        Verification::Provisional => weights.verified * 0.35,
        Verification::Deprecated => return None,
    };

    let kind_bonus = match record.kind {
        MemoryKind::Procedure => weights.kind_bonus,
        MemoryKind::ProjectFact => weights.kind_bonus * 0.8,
        MemoryKind::Episode => 0.0,
    };

    components.trust = verification_bonus + kind_bonus;

    // Recency reads when the claim last changed, not when the record was last
    // touched: being retrieved, linked or counted says nothing about how new
    // what it says is.
    let age_days = (now - record.content_updated_at).num_days().max(0) as f32;
    components.recency = (1.0 / (1.0 + age_days / 30.0)) * weights.recency;
    components.outcome =
        (record.success_count as f32 / record.use_count.max(1) as f32) * weights.success_rate;

    let score = components.total();

    Some(RetrievedMemory {
        record: record.clone(),
        score,
        reason: if reasons.is_empty() {
            "weak contextual match".to_string()
        } else {
            reasons.join(", ")
        },
        components,
    })
}

/// Turns SQLite's `bm25()` rank into a relevance in `0..1`, larger for a
/// better match.
///
/// SQLite's rank is negative, and the better a row matches the more negative
/// it is (`-4.0` beats `-0.5`). Ranking on its magnitude, as this crate once
/// did, orders the weaker match first: `1 / (1 + |rank|)` gives the worse row
/// the larger number. The relevance keeps the direction and the bound.
#[must_use]
pub fn bm25_relevance(rank: f64) -> f32 {
    let matched = (-rank).max(0.0);
    (matched / (1.0 + matched)) as f32
}

/// Words that name no particular subject: the ones that appear in most task
/// sentences and in memories about something else entirely.
///
/// They are left out of the full-text query ([`build_fts_query`]), which makes
/// a full-text hit itself evidence that the memory shares something specific
/// with the task, rather than a fact that both texts contain "the". Words of
/// two letters or fewer are dropped earlier, by [`raw_terms`].
const GENERIC_TERMS: &[&str] = &[
    // Length alone excluded these before literal two-letter suffixes existed.
    "an", "as", "at", "be", "by", "do", "he", "if", "in", "is", "it", "me", "my", "no", "of", "on",
    "or", "so", "to", "up", "us", "we", "add", "added", "adds", "after", "again", "all", "also",
    "and", "any", "are", "before", "but", "can", "change", "changed", "changes", "code",
    "codebase", "could", "did", "does", "done", "each", "every", "file", "files", "fix", "fixed",
    "fixes", "for", "from", "get", "gets", "got", "had", "has", "have", "help", "helps", "here",
    "how", "into", "its", "just", "keep", "keeps", "like", "line", "lines", "make", "makes",
    "making", "more", "most", "need", "needs", "not", "now", "off", "only", "other", "our", "out",
    "same", "should", "some", "still", "task", "tasks", "test", "tests", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "thing", "things", "this", "those", "too",
    "update", "updated", "updates", "use", "used", "using", "very", "want", "wants", "was", "way",
    "ways", "were", "what", "when", "where", "which", "while", "why", "will", "with", "work",
    "works", "would", "you", "your",
];

/// Whether `term` names something a memory and a task can share.
#[must_use]
pub fn is_distinctive(term: &str) -> bool {
    term.len() > 2 && !GENERIC_TERMS.contains(&term)
}

/// A rough token count of `text`: four characters to a token, rounded up,
/// which is the estimate a prompt budget can be planned with without running
/// the model's own tokenizer.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Builds a safe FTS5 query from free text: every distinctive term quoted, the
/// terms joined by `OR`, or the empty string when the text holds none.
///
/// Any term may match: a task's text rarely shares every word with the memory
/// about it ("fix the retry race" against "Retry race"), and BM25 ranks the
/// rows that share more of them first. An empty query matches nothing, which
/// is what a request of only generic words deserves — it names no subject to
/// look a memory up by.
#[must_use]
pub fn build_fts_query(inputs: &[&str]) -> String {
    let mut all_terms = Vec::new();

    for input in inputs {
        for term in raw_terms(input) {
            if is_distinctive(&term) && !all_terms.contains(&term) {
                all_terms.push(term);
            }
        }
    }

    all_terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// A conservative fallback for code names whose spelling hides prose words.
/// Every distinct component must match. A literal two-letter suffix needs two
/// distinct longer anchors; other short or generic components reject the name.
pub(crate) fn identifier_fts_query(inputs: &[&str]) -> String {
    let mut names = Vec::new();
    for input in inputs {
        for name in input.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            let bytes = name.as_bytes();
            if bytes.len() > 128 {
                continue;
            }
            let mut parts: Vec<String> = if name.contains('_') {
                let components: Vec<_> = name.split('_').collect();
                if components.iter().any(|part| {
                    part.is_empty() || !part.as_bytes().iter().all(u8::is_ascii_alphabetic)
                }) {
                    continue;
                }
                components
                    .into_iter()
                    .map(str::to_ascii_lowercase)
                    .collect()
            } else {
                if !bytes.iter().all(u8::is_ascii_alphabetic) {
                    continue;
                }
                let mut starts = vec![0];
                for i in 1..bytes.len() {
                    if bytes[i].is_ascii_uppercase()
                        && (bytes[i - 1].is_ascii_lowercase()
                            || (bytes[i - 1].is_ascii_uppercase()
                                && bytes.get(i + 1).is_some_and(u8::is_ascii_lowercase)))
                    {
                        starts.push(i);
                    }
                }
                starts.push(bytes.len());
                starts
                    .windows(2)
                    .map(|pair| name[pair[0]..pair[1]].to_ascii_lowercase())
                    .collect()
            };
            let anchored_suffix = parts.split_last().is_some_and(|(suffix, anchors)| {
                suffix.len() == 2
                    && !GENERIC_TERMS.contains(&suffix.as_str())
                    && anchors.len() >= 2
                    && anchors.iter().all(|part| is_distinctive(part))
                    && anchors.iter().any(|part| part != &anchors[0])
            });
            if !(2..=8).contains(&parts.len())
                || (!anchored_suffix && parts.iter().any(|part| !is_distinctive(part)))
            {
                continue;
            }
            parts.sort();
            parts.dedup();
            if parts.len() < 2 {
                continue;
            }
            let clause = format!(
                "({})",
                parts
                    .iter()
                    .map(|part| format!("\"{part}\""))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            );
            if !names.contains(&clause) {
                names.push(clause);
            }
            if names.len() == 4 {
                return names.join(" OR ");
            }
        }
    }
    names.join(" OR ")
}

pub(crate) fn identifier_title_matches(name: &str, title: &str) -> bool {
    let expected = identifier_fts_query(&[name]);
    if expected.is_empty() || expected.contains(" OR ") {
        return false;
    }
    let spelling = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    identifier_fts_query(&[&spelling]) == expected
}

fn raw_terms(input: &str) -> Vec<String> {
    input
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"'
                )
        })
        .map(|term| {
            term.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| term.len() > 2)
        .collect()
}

pub(crate) fn terms(input: &str) -> Vec<String> {
    raw_terms(input)
}

/// A path as both this crate and the run compare them: `/` separators, no
/// `.` or `..` segments, no leading or trailing separator.
pub(crate) fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    segments.join("/")
}

/// Whether two paths name the same file, or one names the other as seen from a
/// repository root: `src/queue.rs` and `queue.rs` are the same file to a
/// memory, `src/notfoo.rs` and `foo.rs` are not.
///
/// Whole segments only. Comparing the ends of the strings, as this crate once
/// did, made every `foo.rs` match every `notfoo.rs`, and gave the memory a file
/// bonus for a file it says nothing about. Both sides are normalized first, so
/// the separators and the `./` a tool prints don't decide it.
#[must_use]
pub fn path_overlap(left: &str, right: &str) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right || left.ends_with(&format!("/{right}")) || right.ends_with(&format!("/{left}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_identifiers_require_all_distinct_meaningful_words() {
        assert_eq!(
            identifier_fts_query(&["crate::SharedMemoryStore"]),
            "(\"memory\" AND \"shared\" AND \"store\")"
        );
        assert_eq!(
            identifier_fts_query(&["httpRequest", "HTTPRequest"]),
            "(\"http\" AND \"request\")"
        );
        for text in [
            "FixTheTests",
            "MakeTheChange",
            "GetMemoryStore",
            "HttpIOState",
            "MemoryMemory",
            "SharedMemoryStore2",
            "MEMORY",
            "",
            "AlphaBravoCharlieDeltaEchoFoxtrotGolfHotelIndigo",
        ] {
            assert!(identifier_fts_query(&[text]).is_empty(), "{text}");
        }
    }

    #[test]
    fn short_terminal_components_remain_literal_with_two_distinct_long_anchors() {
        for name in ["OnnxMiniLm", "onnx_mini_lm", "crate::ONNXMiniLM"] {
            assert_eq!(
                identifier_fts_query(&[name]),
                "(\"lm\" AND \"mini\" AND \"onnx\")"
            );
        }
        for name in [
            "OnnxLm",
            "OnnxLmCache",
            "OnnxMiniTo",
            "OnnxMiniIn",
            "OnnxMiniOr",
            "OnnxMiniX",
            "OnnxMiniLm2",
            "MemoryMemoryDb",
            "get_memory_db",
            "HttpIOState",
            "http_io_state",
        ] {
            assert!(identifier_fts_query(&[name]).is_empty(), "{name}");
        }
    }

    #[test]
    fn snake_identifiers_use_complete_words_without_fts_phrase_order_constraints() {
        let expected = "(\"memory\" AND \"shared\" AND \"store\")";
        // Snake names were previously unsupported by this helper. Their ordinary
        // exact-phrase lookup still runs first; fallback now requires all words.
        for name in [
            "shared_memory_store",
            "store_memory_shared",
            "STORE_MEMORY_SHARED",
            "crate::store_memory_shared",
        ] {
            assert_eq!(identifier_fts_query(&[name]), expected);
        }
        for name in [
            "_shared_memory_store",
            "shared_memory_store_",
            "shared__memory_store",
            "shared_memory_2store",
            "get_memory_store",
            "fix_the_tests",
            "memory_memory",
            "http_io_state",
            "alpha_bravo_charlie_delta_echo_foxtrot_golf_hotel_india",
        ] {
            assert!(identifier_fts_query(&[name]).is_empty(), "{name}");
        }
        assert_eq!(
            identifier_fts_query(&["shared_memory_store", "SharedMemoryStore"]),
            expected
        );
    }

    #[test]
    fn identifier_expansion_is_bounded_and_cannot_introduce_fts_operators() {
        let query = identifier_fts_query(&[
            "AlphaBravo CharlieDelta EchoFoxtrot GolfHotel IndiaJuliet \" OR *",
        ]);
        assert_eq!(query.matches(" AND ").count(), 4);
        assert!(!query.contains("india"));
        assert!(!query.contains('*'));
        assert!(identifier_fts_query(&[&"aB".repeat(100)]).is_empty());
    }

    #[test]
    fn fts_query_is_safe_and_quoted() {
        assert_eq!(
            build_fts_query(&["retry \"race\" OR; drop"]),
            "\"retry\" OR \"race\" OR \"drop\""
        );
    }

    #[test]
    fn paths_overlap_across_separators() {
        assert!(path_overlap("src\\a\\b.py", "src/a/b.py"));
    }

    #[test]
    fn a_pool_keeps_the_highest_ranked_copy_including_custom_outcome_weights() {
        let store = crate::MemoryStore::open_in_memory().unwrap();
        let record = store
            .remember(crate::NewMemory::procedure("p", "Retry", "Bound retries."))
            .unwrap();
        let mut better = record.clone();
        better.id = uuid::Uuid::new_v4();
        better.use_count = 1;
        better.success_count = 1;
        let query = crate::RetrievalQuery::new("p", "retry");
        let mut telemetry = RetrievalTelemetry::default();
        let weights = RetrievalWeights {
            success_rate: 3.0,
            ..Default::default()
        };
        let found = collect_ranked_candidates(
            [Ok((record, Some(1.0))), Ok((better.clone(), Some(1.0)))].into_iter(),
            &query,
            &|_| true,
            Utc::now(),
            &weights,
            80,
            &mut telemetry,
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].record.id, better.id);
        assert_eq!(telemetry.candidates_scanned, 2);
    }
}
#[test]
fn canonical_titles_require_the_entire_name_without_incidental_context_words() {
    assert!(identifier_title_matches(
        "MemorySettings",
        "Memory settings"
    ));
    assert!(identifier_title_matches(
        "memory_settings",
        "Settings memory"
    ));
    assert!(identifier_title_matches(
        "crate::OnnxMiniLm",
        "ONNX Mini LM"
    ));
    assert!(!identifier_title_matches("MemorySettings", "Memory entry"));
    assert!(!identifier_title_matches(
        "MemorySettings",
        "Memory settings reference"
    ));
    assert!(!identifier_title_matches("OnnxMiniLm", "ONNX Mini DB"));
}

use crate::model::{MemoryKind, MemoryRecord, RetrievedMemory, Verification};
use chrono::{DateTime, Utc};
use serde::Serialize;

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
            success_rate: 1.0,
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
    "add", "added", "adds", "after", "again", "all", "also", "and", "any", "are", "before", "but",
    "can", "change", "changed", "changes", "code", "codebase", "could", "did", "does", "done",
    "each", "every", "file", "files", "fix", "fixed", "fixes", "for", "from", "get", "gets", "got",
    "had", "has", "have", "help", "helps", "here", "how", "into", "its", "just", "keep", "keeps",
    "like", "line", "lines", "make", "makes", "making", "more", "most", "need", "needs", "not",
    "now", "off", "only", "other", "our", "out", "same", "should", "some", "still", "task",
    "tasks", "test", "tests", "than", "that", "the", "their", "them", "then", "there", "these",
    "they", "thing", "things", "this", "those", "too", "update", "updated", "updates", "use",
    "used", "using", "very", "want", "wants", "was", "way", "ways", "were", "what", "when",
    "where", "which", "while", "why", "will", "with", "work", "works", "would", "you", "your",
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
}

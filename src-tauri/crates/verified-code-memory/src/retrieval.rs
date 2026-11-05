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

/// Retrieval telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RetrievalTelemetry {
    /// Candidate records considered.
    pub candidates_scanned: usize,
    /// FTS5 matches before reranking.
    pub fts_matches: usize,
    /// Whether FTS5 was used.
    pub used_full_text_search: bool,
    /// Records returned.
    pub returned: usize,
    /// Latency in milliseconds.
    pub elapsed_ms: u128,
}

/// Scores a memory.
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
        .filter(|term| content_terms.iter().any(|candidate| candidate.contains(*term)))
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

    if let Some(failure) = failure {
        let failure_terms = terms(failure);
        let hits = failure_terms
            .iter()
            .filter(|term| content_terms.iter().any(|candidate| candidate.contains(*term)))
            .count();

        if hits > 0 {
            components.failure = (hits as f32).min(3.0) * weights.failure_match;
            reasons.push(format!("{hits} failure-term matches"));
        }
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

    let age_days = (now - record.updated_at).num_days().max(0) as f32;
    components.recency = (1.0 / (1.0 + age_days / 30.0)) * weights.recency;
    components.outcome =
        (record.success_count as f32 / record.use_count.max(1) as f32) * weights.success_rate;

    let score = components.total();
    if score <= 0.05 {
        return None;
    }

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

/// Builds a safe FTS5 query from free text.
#[must_use]
pub fn build_fts_query(inputs: &[&str]) -> String {
    let mut all_terms = Vec::new();

    for input in inputs {
        for term in raw_terms(input) {
            if !all_terms.contains(&term) {
                all_terms.push(term);
            }
        }
    }

    all_terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
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

pub(crate) fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

pub(crate) fn path_overlap(left: &str, right: &str) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    left == right || left.ends_with(&right) || right.ends_with(&left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_is_safe_and_quoted() {
        assert_eq!(
            build_fts_query(&["retry \"race\" OR; drop"]),
            "\"retry\" \"race\" \"drop\""
        );
    }

    #[test]
    fn paths_overlap_across_separators() {
        assert!(path_overlap("src\\a\\b.py", "src/a/b.py"));
    }
}

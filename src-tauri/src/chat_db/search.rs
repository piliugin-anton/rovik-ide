//! Ranked chat search on FTS5.
//!
//! Same rules as the old in-memory search: every word must appear somewhere
//! in the chat (title or any message), titles outweigh messages, word-start
//! matches count, recency breaks ties, and typo tolerance is a title-only
//! fallback. FTS5 matches rows (one message or one title), so each word is
//! queried separately and the results are intersected per chat.

use super::store::{err, get_dialog, now_ms, DbResult, DialogSummary};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

const PER_TERM_ROW_CAP: i64 = 50_000;
const RANK_POOL: usize = 200;
const HITS_CAP: usize = 6;
const HIT_WEIGHT: f64 = 0.5;
const PHRASE_BONUS: f64 = 6.0;
const FUZZY_SCORE: f64 = 6.0;
const RECENCY_MAX: f64 = 2.0;
const TITLE_WEIGHT: f64 = 10.0;
const FUZZY_CANDIDATES: i64 = 500;

/// Text with highlight ranges in UTF-16 code units (JavaScript string indexes).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Highlighted {
    pub text: String,
    pub ranges: Vec<[usize; 2]>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub dialog: DialogSummary,
    pub score: f64,
    /// The best-matching message, for jumping straight to it.
    pub message_id: Option<String>,
    pub snippet: Option<Highlighted>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub text: String,
    pub phrase: bool,
}

/// Words, plus "quoted phrases" as single terms; lowercased, deduplicated,
/// terms under 2 characters dropped.
pub fn parse_query(query: &str) -> Vec<Term> {
    let mut terms: Vec<Term> = Vec::new();
    let mut rest = query.trim();
    while !rest.is_empty() {
        let (text, phrase, next) = if let Some(after) = rest.strip_prefix('"') {
            match after.find('"') {
                Some(end) => (&after[..end], true, &after[end + 1..]),
                None => (after, true, ""),
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (&rest[..end], false, &rest[end..])
        };
        let text = text.trim().to_lowercase();
        if text.chars().count() >= 2 && !terms.iter().any(|t| t.text == text) {
            terms.push(Term { text, phrase });
        }
        rest = next.trim_start();
    }
    terms
}

/// One term as an FTS5 query string: quoted (so punctuation is literal),
/// with a prefix star for single words.
fn fts_string(term: &Term) -> String {
    let quoted = format!("\"{}\"", term.text.replace('"', "\"\""));
    if term.phrase {
        quoted
    } else {
        format!("{quoted}*")
    }
}

fn recency_bonus(updated_at: i64) -> f64 {
    let age_days = ((now_ms() - updated_at).max(0) as f64) / 86_400_000.0;
    RECENCY_MAX * (-age_days / 30.0).exp()
}

/// Per chat: best (lowest) bm25 and number of matching rows.
fn term_matches(conn: &Connection, term: &Term) -> DbResult<HashMap<String, (f64, usize)>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT dialog_id, bm25(search_index, ?2, 1.0) FROM search_index
             WHERE search_index MATCH ?1 LIMIT ?3",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![fts_string(term), TITLE_WEIGHT, PER_TERM_ROW_CAP], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .map_err(err)?;
    let mut by_dialog: HashMap<String, (f64, usize)> = HashMap::new();
    for row in rows {
        let (dialog, rank) = row.map_err(err)?;
        let entry = by_dialog.entry(dialog).or_insert((f64::INFINITY, 0));
        entry.0 = entry.0.min(rank);
        entry.1 += 1;
    }
    Ok(by_dialog)
}

/// Converts FTS5 snippet markers (\x02 … \x03) into ranges, collapsing
/// whitespace so the excerpt reads as one line.
fn snippet_to_highlighted(raw: &str) -> Highlighted {
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut utf16 = 0usize;
    let mut open: Option<usize> = None;
    let mut last_space = false;
    for ch in raw.chars() {
        match ch {
            '\u{2}' => open = Some(utf16),
            '\u{3}' => {
                if let Some(start) = open.take() {
                    if utf16 > start {
                        ranges.push([start, utf16]);
                    }
                }
            }
            c if c.is_whitespace() => {
                if !last_space && !text.is_empty() {
                    text.push(' ');
                    utf16 += 1;
                }
                last_space = true;
            }
            c => {
                text.push(c);
                utf16 += c.len_utf16();
                last_space = false;
            }
        }
    }
    Highlighted { text, ranges }
}

fn best_snippet(conn: &Connection, dialog_id: &str, terms: &[Term]) -> DbResult<Option<(String, Highlighted)>> {
    let any = terms.iter().map(fts_string).collect::<Vec<_>>().join(" OR ");
    let mut stmt = conn
        .prepare_cached(
            "SELECT message_id, snippet(search_index, 1, char(2), char(3), '…', 16)
             FROM search_index
             WHERE search_index MATCH ?1 AND dialog_id = ?2 AND message_id IS NOT NULL
             ORDER BY bm25(search_index, ?3, 1.0) LIMIT 1",
        )
        .map_err(err)?;
    let mut rows = stmt.query(params![any, dialog_id, TITLE_WEIGHT]).map_err(err)?;
    match rows.next().map_err(err)? {
        Some(row) => {
            let message_id: String = row.get(0).map_err(err)?;
            let raw: String = row.get(1).map_err(err)?;
            Ok(Some((message_id, snippet_to_highlighted(&raw))))
        }
        None => Ok(None),
    }
}

pub fn search(conn: &Connection, query: &str, limit: usize) -> DbResult<Vec<SearchHit>> {
    let terms = parse_query(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 200);

    // 1. Per term, then intersect: every term somewhere in the chat.
    let mut per_term = Vec::with_capacity(terms.len());
    for term in &terms {
        per_term.push(term_matches(conn, term)?);
    }
    let mut candidates: Vec<(String, f64)> = per_term[0]
        .keys()
        .filter(|d| per_term.iter().all(|m| m.contains_key(*d)))
        .map(|d| {
            let text_score: f64 = per_term
                .iter()
                .map(|m| {
                    let (best, hits) = m[d];
                    -best + HIT_WEIGHT * hits.min(HITS_CAP) as f64
                })
                .sum();
            (d.clone(), text_score)
        })
        .collect();

    if candidates.is_empty() {
        return fuzzy_title_search(conn, &terms, limit);
    }

    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    candidates.truncate(RANK_POOL);

    // 2. Phrase bonus: all terms in one message or title.
    let together: HashSet<String> = if terms.len() > 1 {
        let all = terms.iter().map(fts_string).collect::<Vec<_>>().join(" ");
        let mut stmt = conn
            .prepare_cached("SELECT DISTINCT dialog_id FROM search_index WHERE search_index MATCH ?1")
            .map_err(err)?;
        let rows = stmt.query_map(params![all], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)?
    } else {
        HashSet::new()
    };

    // 3. Recency, then the final order.
    let mut ranked: Vec<(DialogSummary, f64)> = Vec::with_capacity(candidates.len());
    for (dialog_id, text_score) in candidates {
        if let Some(dialog) = get_dialog(conn, &dialog_id)? {
            let bonus = if together.contains(&dialog_id) { PHRASE_BONUS } else { 0.0 };
            let score = text_score + bonus + recency_bonus(dialog.updated_at);
            ranked.push((dialog, score));
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.updated_at.cmp(&a.0.updated_at)));
    ranked.truncate(limit);

    // 4. Snippets, unless the title alone already shows every term.
    let mut hits = Vec::with_capacity(ranked.len());
    for (dialog, score) in ranked {
        let title = dialog.title.to_lowercase();
        let title_covers_all = terms.iter().all(|t| title.contains(&t.text));
        let (message_id, snippet) = if title_covers_all {
            (None, None)
        } else {
            match best_snippet(conn, &dialog.id, &terms)? {
                Some((id, s)) => (Some(id), Some(s)),
                None => (None, None),
            }
        };
        hits.push(SearchHit { dialog, score, message_id, snippet });
    }
    Ok(hits)
}

/*
 * ---------------------------------------------------------------------------
 * Typo fallback (titles only)
 * ---------------------------------------------------------------------------
 */

fn max_typos(term: &str) -> usize {
    match term.chars().count() {
        n if n >= 8 => 2,
        n if n >= 4 => 1,
        _ => 0,
    }
}

/// Optimal-string-alignment distance, giving up once it exceeds `max`.
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev2: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        let mut row_min = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur.push(v);
            row_min = row_min.min(v);
        }
        if row_min > max {
            return max + 1;
        }
        prev2 = prev;
        prev = cur;
    }
    prev[b.len()]
}

/// A title word matches when it, or its prefix of the term's length, is
/// within the term's typo budget ("tauir" matches "tauri-ide").
fn title_matches_fuzzy(title: &str, term: &str) -> bool {
    let budget = max_typos(term);
    if budget == 0 {
        return false;
    }
    let term_len = term.chars().count();
    title
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .any(|word| {
            let prefix: String = word.chars().take(term_len).collect();
            edit_distance(term, word, budget) <= budget
                || (word.chars().count() > term_len && edit_distance(term, &prefix, budget) <= budget)
        })
}

fn trigram_query(term: &str) -> Option<String> {
    let chars: Vec<char> = term.chars().collect();
    if chars.len() < 3 {
        return None;
    }
    let grams: Vec<String> = chars
        .windows(3)
        .map(|w| format!("\"{}\"", w.iter().collect::<String>().replace('"', "\"\"")))
        .collect();
    Some(grams.join(" OR "))
}

fn fuzzy_title_search(conn: &Connection, terms: &[Term], limit: usize) -> DbResult<Vec<SearchHit>> {
    if terms.iter().any(|t| t.phrase || max_typos(&t.text) == 0) {
        return Ok(Vec::new());
    }
    // Candidates: titles sharing a trigram with any term; then every term
    // must match some word within its typo budget.
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut stmt = conn
        .prepare_cached("SELECT dialog_id, title FROM title_trigrams WHERE title_trigrams MATCH ?1 LIMIT ?2")
        .map_err(err)?;
    for term in terms {
        let Some(q) = trigram_query(&term.text) else { continue };
        let rows = stmt
            .query_map(params![q, FUZZY_CANDIDATES], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for row in rows {
            let (id, title) = row.map_err(err)?;
            titles.insert(id, title.to_lowercase());
        }
    }

    let mut hits = Vec::new();
    for (dialog_id, title) in titles {
        if terms.iter().all(|t| title_matches_fuzzy(&title, &t.text)) {
            if let Some(dialog) = get_dialog(conn, &dialog_id)? {
                let score = FUZZY_SCORE * terms.len() as f64 + recency_bonus(dialog.updated_at);
                hits.push(SearchHit { dialog, score, message_id: None, snippet: None });
            }
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    Ok(hits)
}

//! The history view: recent commits, searched by what they say, touch and
//! change, what one commit changed, and a file as it was at a commit.

use super::{blob_text, delta_path, diff_line_stats, inside_repo, open_repo, tree_blob, ErrText};
use crate::git::stash::{delta_status, GitStashFile, GitStashFilePreview};
use crate::util::blocking;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitCommitEntry {
    pub oid: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    /// Commit time, seconds since the epoch.
    pub time: i64,
    /// Total files changed in this commit.
    #[serde(default)]
    pub file_count: usize,
    /// Files matching the active history search (`None` when no query).
    #[serde(default)]
    pub matched_file_count: Option<usize>,
}

impl GitCommitEntry {
    fn of(commit: &git2::Commit, file_count: usize, matched_file_count: Option<usize>) -> Self {
        let full = commit.id().to_string();
        GitCommitEntry {
            short: full.chars().take(7).collect(),
            oid: full,
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or("(no message)")
                .to_string(),
            author: commit.author().name().unwrap_or("?").to_string(),
            time: commit.time().seconds(),
            file_count,
            matched_file_count,
        }
    }
}

/// Recent commits on the current HEAD, newest first, for the history view.
///
/// With `query`, only commits matching the search are returned (smart
/// history search). Matching is case-insensitive; the query is split into
/// whitespace-separated terms and EVERY term must match somewhere in the
/// commit (AND semantics). Each term matches when it appears in:
/// - the message, author, or oid (prefix or substring), or
/// - a changed file's path: full path, basename, or with `_`/`-`/etc.
///   ignored so `get_user` also matches `getUserName` (compact form), or
/// - the patch itself: added/removed lines, raw or compacted the same way,
///   so function/variable/class names (or any identifier, partial) match.
///   Single-character terms never scan patch content: they match
///   metadata/filenames only (a 1-char content scan matches almost every
///   commit and costs a full diff walk for no signal).
///
/// Approach (researched `git log -S/-G` pickaxe, `git log --name-status`,
/// and `git grep $(git rev-list …)`): rather than shelling out, walk with
/// revwalk and diff parent→commit via libgit2 in one backend call. Cheap
/// metadata (message/author/oid) is checked first so most non-matching
/// commits never pay for a diff; filenames come from the same diff's
/// deltas; content comes from the diff's added/deleted lines only
/// (context lines excluded), which is exactly what `-S`/`-G` semantics
/// care about. Full tree-sitter symbol extraction per historic blob was
/// considered and rejected: substring + compact-form matching on patch
/// lines already covers partial identifier matches in every language at a
/// fraction of the cost.
///
/// Pagination: `offset` counts raw revwalk commits already scanned
/// (not results), so a search page can resume where the previous one
/// stopped without re-scanning. Offsets count from `tip`: the first page
/// walks from HEAD and returns it as `tip`, and later pages pass it back,
/// so commits made meanwhile (a terminal, another tool) can't shift the
/// offsets and repeat or skip a commit. A tip that no longer exists
/// (history rewritten and pruned) falls back to HEAD; the reply's `tip`
/// then differs from the one sent, and the UI starts over. Each call scans
/// at most `GIT_LOG_SCAN_CAP` commits and returns at most `limit` matches;
/// `has_more` tells the UI whether older commits remain, and `next_offset`
/// is the `offset` for the next page. The walk is capped per call so
/// typing stays interactive on large histories; "load more / search
/// further" pages further back instead of raising the cap.
#[tauri::command]
pub async fn git_log(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
    tip: Option<String>,
) -> Result<GitLogPage, String> {
    // Each call supersedes the ones before it for the same repository (its
    // section shows only the latest reply): an older search still scanning
    // stops at its next commit instead of finishing hundreds of diffs nobody
    // will see. Another repository's section reads its own history meanwhile.
    let generation = GIT_LOG_LATEST.begin(&path);
    let key = path.clone();
    blocking(move || {
        let superseded = || !GIT_LOG_LATEST.is_latest(&key, generation);
        git_log_scan(path, limit, query, offset, tip.as_deref(), &superseded)
    })
    .await
}

/// The latest `git_log` call of each repository (see `git_log`), by the
/// folder its section asks from: one per repository.
static GIT_LOG_LATEST: Generations = Generations::new();

/// A count of calls per key: each call takes the next number, and learns
/// whether a newer call for its key has come since.
pub(super) struct Generations(Mutex<BTreeMap<String, u64>>);

impl Generations {
    pub(super) const fn new() -> Generations {
        Generations(Mutex::new(BTreeMap::new()))
    }

    /// A new call for `key`: the ones before it are superseded.
    pub(super) fn begin(&self, key: &str) -> u64 {
        let mut latest = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let next = latest.get(key).copied().unwrap_or(0) + 1;
        latest.insert(key.to_string(), next);
        next
    }

    /// Whether `generation` is still `key`'s latest call.
    pub(super) fn is_latest(&self, key: &str, generation: u64) -> bool {
        let latest = self.0.lock().unwrap_or_else(|e| e.into_inner());
        latest.get(key) == Some(&generation)
    }
}

/// One page of history: the matches plus the cursor for the next page.
/// `offset`/`next_offset` count scanned (raw) commits, so pages resume
/// the revwalk without re-scanning. `has_more` is true when the walk
/// still has older commits the cap stopped us from reaching — a query
/// can return fewer than `limit` matches and still have more history
/// worth searching.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitLogPage {
    pub commits: Vec<GitCommitEntry>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub next_offset: usize,
    #[serde(default)]
    pub scanned: usize,
    /// The commit this page's walk started from (offsets count from it);
    /// pass it back with the next page. None for an empty repository.
    #[serde(default)]
    pub tip: Option<String>,
}

/// Commits scanned for one page before stopping (results still capped
/// to `limit`; newest first, so this bounds latency, not correctness of
/// the top matches — the UI pages further with `offset`).
const GIT_LOG_SCAN_CAP: usize = 400;

/// Added/deleted content lines examined per commit before stopping the
/// content scan (a match past this is missed; 20k lines is far beyond a
/// typical commit and keeps one giant generated-file commit from hanging
/// a keystroke).
const GIT_LOG_LINES_PER_COMMIT_CAP: usize = 20_000;

/// Per-file share of the content budget: one giant generated file must
/// not eat the whole per-commit budget and starve the other files.
const GIT_LOG_LINES_PER_FILE_CAP: usize = 5_000;

/// Binary deltas never match content (their patch has no usable text).
/// Filename/message matches still count.
fn delta_is_binary(diff: &git2::Diff, i: usize) -> bool {
    diff.get_delta(i)
        .is_some_and(|d| d.new_file().is_binary() || d.old_file().is_binary())
}

/// Lowercase alphanumeric only: `get_user-name` → `getusername`, so
/// snake_case, kebab-case and camelCase match each other partially.
/// Only used for short strings (query terms, filenames). Patch lines use
/// `compact_contains` instead to avoid allocating per line.
fn compact_ident(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Compact-substring check without allocating the compacted haystack:
/// true when `needle_compact` (already lowercase alnum-only) appears in
/// `haystack_lower` after ignoring non-alphanumeric characters — i.e.
/// `compact(haystack).contains(needle)` without building the string.
/// Byte-oriented; both sides are lowercased before calling.
pub(super) fn compact_contains(haystack_lower: &str, needle_compact: &str) -> bool {
    if needle_compact.is_empty() {
        return false;
    }
    let needle = needle_compact.as_bytes();
    let hay = haystack_lower.as_bytes();
    // Streaming match: needle bytes must appear in order, skipping
    // non-alphanumeric haystack bytes (which `compact()` would drop).
    // Comparison is ASCII case-insensitive so callers may pass a raw
    // (non-lowered) haystack; the needle is already compact+lowercase.
    let mut ni = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < hay.len() {
        let b = hay[i].to_ascii_lowercase();
        if !b.is_ascii_alphanumeric() {
            // Separator: skippable mid-match, never a match itself.
            i += 1;
            continue;
        }
        if b == needle[ni] {
            if ni == 0 {
                start = i;
            }
            ni += 1;
            if ni == needle.len() {
                return true;
            }
            i += 1;
        } else if ni > 0 {
            // Mismatch mid-match: restart just after the attempt's start.
            ni = 0;
            i = start + 1;
        } else {
            i += 1;
        }
    }
    false
}

/// One normalized search term: raw lowercase plus compact form.
struct SearchTerm {
    raw: String,
    compact: String,
}

/// Compact characters a term needs before its compact form is matched:
/// `c++` compacts to `c`, which would match nearly every path and line.
const MIN_COMPACT_CHARS: usize = 2;

impl SearchTerm {
    /// Whether the compact (separator-blind) form is worth matching.
    fn compact_usable(&self) -> bool {
        self.compact.len() >= MIN_COMPACT_CHARS
    }

    /// Whether this term may scan patch content. Single-character terms
    /// match metadata/filenames only: a 1-char content scan hits nearly
    /// every commit and costs a full patch walk for no signal.
    fn scans_content(&self) -> bool {
        self.raw.chars().count() > 1
    }

    /// Whether the term is in the file's path or basename, raw or compact.
    fn matches_file(&self, f: &FileName) -> bool {
        f.path.contains(&self.raw)
            || f.base.contains(&self.raw)
            || (self.compact_usable()
                && (compact_contains(&f.path, &self.compact)
                    || compact_contains(&f.base, &self.compact)))
    }

    /// Added/removed line matches when the raw line or its compact form
    /// contains the term (raw or compact). Covers identifiers, strings,
    /// comments — anything textual in the patch.
    fn matches_line(&self, line_lower: &str) -> bool {
        line_lower.contains(&self.raw)
            || (self.compact_usable() && compact_contains(line_lower, &self.compact))
    }
}

/// A changed file's path and basename, lowercased once per file: what the
/// terms are matched against.
struct FileName {
    path: String,
    base: String,
}

impl FileName {
    fn of(rel: &str) -> Self {
        FileName {
            path: rel.to_lowercase(),
            base: rel.rsplit('/').next().unwrap_or(rel).to_lowercase(),
        }
    }
}

/// The search terms of `query`: none without a query, or with a blank one.
fn query_terms(query: Option<&str>) -> Option<Vec<SearchTerm>> {
    let terms: Vec<SearchTerm> = query?
        .split_whitespace()
        .map(|t| SearchTerm {
            raw: t.to_lowercase(),
            compact: compact_ident(t),
        })
        .filter(|t| !t.raw.is_empty())
        .collect();
    (!terms.is_empty()).then_some(terms)
}

/// No context lines: only added/removed lines are ever scanned, and the
/// stats (added/removed counts) don't need context either.
fn changes_only() -> git2::DiffOptions {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    opts.interhunk_lines(0);
    opts
}

/// A commit's change: its first parent's tree against its own (the
/// initial commit against nothing), with renames detected, so a moved
/// file is one rename instead of a full delete plus a full add (whose
/// every line would match a content search).
fn commit_diff<'r>(
    repo: &'r Repository,
    commit: &git2::Commit,
    diff_opts: &mut git2::DiffOptions,
) -> Result<git2::Diff<'r>, git2::Error> {
    let tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(diff_opts))?;
    let mut find = git2::DiffFindOptions::new();
    find.renames(true);
    // Best effort: without detection the diff is still right, just coarser.
    let _ = diff.find_similar(Some(&mut find));
    Ok(diff)
}

/// The added and removed lines of file `i` of `diff` (never context, and
/// never git's "\ No newline at end of file" markers), lowercased, each to
/// `each` until it says Break, and at most `cap` of them. Returns how many
/// lines were read; a blank one counts, but isn't shown.
fn change_lines(
    diff: &git2::Diff,
    i: usize,
    cap: usize,
    mut each: impl FnMut(&str) -> ControlFlow<()>,
) -> usize {
    let Ok(Some(patch)) = git2::Patch::from_diff(diff, i) else {
        return 0;
    };
    let mut read = 0;
    for h in 0..patch.num_hunks() {
        let Ok(lines) = patch.num_lines_in_hunk(h) else {
            continue;
        };
        for l in 0..lines {
            if read >= cap {
                return read;
            }
            let Ok(line) = patch.line_in_hunk(h, l) else {
                continue;
            };
            if !matches!(line.origin(), '+' | '-') {
                continue;
            }
            read += 1;
            let text = String::from_utf8_lossy(line.content()).to_lowercase();
            if text.trim().is_empty() {
                continue;
            }
            if each(&text).is_break() {
                return read;
            }
        }
    }
    read
}

/// Per changed file: (filename matches a term, added/removed lines match a
/// term). The one definition of a matching file, for the list's badge
/// (`git_log`) and the expanded file list (`git_commit_show`), so the two
/// always agree. Binary files never match by content; single-character
/// terms never scan content; each file's scan is capped.
fn file_match_flags(diff: &git2::Diff, terms: &[SearchTerm]) -> Vec<(bool, bool)> {
    let scanning: Vec<&SearchTerm> = terms.iter().filter(|t| t.scans_content()).collect();
    (0..diff.deltas().len())
        .map(|i| {
            let file_hit = delta_path(diff, i)
                .map(|rel| FileName::of(&rel))
                .is_some_and(|name| terms.iter().any(|t| t.matches_file(&name)));
            let mut content_hit = false;
            if !scanning.is_empty() && !delta_is_binary(diff, i) {
                change_lines(diff, i, GIT_LOG_LINES_PER_FILE_CAP, |text| {
                    content_hit = scanning.iter().any(|t| t.matches_line(text));
                    if content_hit {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                });
            }
            (file_hit, content_hit)
        })
        .collect()
}

/// `git_log` without cancellation (tests).
#[cfg(test)]
pub(super) fn git_log_blocking(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
) -> Result<GitLogPage, String> {
    git_log_scan(path, limit, query, offset, None, &|| false)
}

/// A history walk, newest first, from `tip` when it's still a commit in
/// the repository, else from HEAD. Returns the walk and the commit it
/// starts from; None when the repository has no commits yet.
fn history_walk<'r>(
    repo: &'r Repository,
    tip: Option<&str>,
) -> Result<Option<(git2::Revwalk<'r>, git2::Oid)>, String> {
    let mut walk = repo.revwalk().err_text()?;
    // Newest first, but never a parent before its child (git's
    // --date-order): commits made in the same second (rebases, scripts,
    // quick amends) otherwise come out in any order.
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .err_text()?;
    let pinned = tip
        .and_then(|t| t.parse::<git2::Oid>().ok())
        .filter(|oid| repo.find_commit(*oid).is_ok());
    let start = match pinned {
        Some(oid) => oid,
        None => match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(c) => c.id(),
            // No commits yet.
            None => return Ok(None),
        },
    };
    walk.push(start).err_text()?;
    Ok(Some((walk, start)))
}

/// `git_log_blocking`, stopping early once `superseded` says a newer call
/// replaced this one (the partial page is never shown).
pub(super) fn git_log_scan(
    path: String,
    limit: Option<u32>,
    query: Option<String>,
    offset: Option<usize>,
    tip: Option<&str>,
    superseded: &dyn Fn() -> bool,
) -> Result<GitLogPage, String> {
    let repo = open_repo(&path)?;
    let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
    let skip = offset.unwrap_or(0);
    let terms = query_terms(query.as_deref());
    // No commits yet: empty history, not an error.
    let Some((walk, start)) = history_walk(&repo, tip)? else {
        return Ok(GitLogPage {
            commits: Vec::new(),
            has_more: false,
            next_offset: skip,
            scanned: 0,
            tip: None,
        });
    };
    // A tip that no longer resolves fell back to HEAD: the offset counted
    // from the old tip means nothing here, so this page starts over.
    let skip = if tip.is_some_and(|t| t != start.to_string()) {
        0
    } else {
        skip
    };
    let scan_cap = if terms.is_some() {
        GIT_LOG_SCAN_CAP
    } else {
        limit
    };
    let mut diff_opts = changes_only();
    let mut entries = Vec::new();
    let mut scanned = 0usize;
    let mut exhausted = true;
    for oid in walk.skip(skip).take(scan_cap + 1) {
        // The +1 probe: there is at least one more commit beyond this
        // page's budget. Or a newer call replaced this one.
        if scanned >= scan_cap || superseded() {
            exhausted = false;
            break;
        }
        let oid = oid.err_text()?;
        scanned += 1;
        let commit = repo.find_commit(oid).err_text()?;
        let (file_count, matched_file_count) = match &terms {
            Some(terms) => match commit_match_details(&repo, &commit, terms, &mut diff_opts) {
                Some((total, matched)) => (total, Some(matched)),
                None => continue,
            },
            // No query: cheap file count via diff without content scan.
            None => (commit_file_count(&repo, &commit, &mut diff_opts), None),
        };
        entries.push(GitCommitEntry::of(&commit, file_count, matched_file_count));
        if entries.len() >= limit {
            // Page is full. Older commits may still hold (more) matches;
            // whether anything remains is resolved by the probe below.
            break;
        }
    }
    // If we stopped because the page filled up, we don't yet know
    // whether older commits remain. The loop above breaks out of the
    // iterator, so re-open a cheap walk to probe: skip(skip+scanned)
    // and check for one more oid. Bounded to a single step.
    let mut has_more = !exhausted;
    if !has_more && entries.len() >= limit {
        if let Some((mut probe, _)) = history_walk(&repo, Some(&start.to_string()))? {
            has_more = probe.nth(skip + scanned).is_some();
        }
    }
    Ok(GitLogPage {
        commits: entries,
        has_more,
        next_offset: skip + scanned,
        scanned,
        tip: Some(start.to_string()),
    })
}

/// Total files changed in a commit (parent→commit diff, no content scan).
fn commit_file_count(
    repo: &Repository,
    commit: &git2::Commit,
    diff_opts: &mut git2::DiffOptions,
) -> usize {
    commit_diff(repo, commit, diff_opts).map_or(0, |d| d.deltas().len())
}

/// Detailed match for one commit: `None` when the commit doesn't satisfy
/// the query (AND semantics), otherwise `(total_files, matched_files)`
/// where `matched_files` counts files matching ANY term via filename or
/// content. Message-only matches yield `matched_files == 0` — the caller
/// still shows the commit, and the file list shows all files unfiltered.
///
/// Two phases: metadata + filenames first (one diff, no patch text), then
/// patch content only for terms still unmatched. Single-character terms
/// never reach the content phase (see `SearchTerm::scans_content`).
fn commit_match_details(
    repo: &Repository,
    commit: &git2::Commit,
    terms: &[SearchTerm],
    diff_opts: &mut git2::DiffOptions,
) -> Option<(usize, usize)> {
    // The whole message, body included (not just the summary line).
    let msg_lower = commit.message().unwrap_or("").to_lowercase();
    let author_lower = commit.author().name().unwrap_or("?").to_lowercase();
    let oid_str = commit.id().to_string();
    let short = oid_str.chars().take(7).collect::<String>();

    // Cheap raw checks first; the compact (streaming, allocation-free)
    // check runs only when the raw substring missed.
    let pending: Vec<&SearchTerm> = terms
        .iter()
        .filter(|term| {
            !(oid_str.contains(&term.raw)
                || short.starts_with(&term.raw)
                || msg_lower.contains(&term.raw)
                || author_lower.contains(&term.raw)
                || (term.compact_usable() && compact_contains(&msg_lower, &term.compact)))
        })
        .collect();

    let Ok(diff) = commit_diff(repo, commit, diff_opts) else {
        return pending.is_empty().then_some((0, 0));
    };
    let n = diff.deltas().len();
    if n == 0 {
        return pending.is_empty().then_some((0, 0));
    }

    // Which pending terms are satisfied by a filename somewhere?
    let mut satisfied = vec![false; pending.len()];
    for name in (0..n).filter_map(|i| delta_path(&diff, i)) {
        let name = FileName::of(&name);
        for (pi, term) in pending.iter().enumerate() {
            if term.matches_file(&name) {
                satisfied[pi] = true;
            }
        }
    }
    // Terms still needing content. Single-char terms never scan content:
    // unmatched by now, they fail the commit.
    let mut need_content: Vec<(usize, &SearchTerm)> = Vec::new();
    for (pi, term) in pending.iter().enumerate() {
        if satisfied[pi] {
            continue;
        }
        if !term.scans_content() {
            return None;
        }
        need_content.push((pi, term));
    }
    // Content scan per file (pickaxe-style, added/removed lines only).
    // Binary files are skipped (no usable text); each file gets its own
    // budget share so one generated file can't starve the rest; the loop
    // stops as soon as every needed term has hit somewhere.
    let mut remaining = need_content.len();
    let mut examined = 0usize;
    for i in 0..n {
        if examined >= GIT_LOG_LINES_PER_COMMIT_CAP || remaining == 0 {
            break;
        }
        if delta_is_binary(&diff, i) {
            continue;
        }
        let cap = GIT_LOG_LINES_PER_FILE_CAP.min(GIT_LOG_LINES_PER_COMMIT_CAP - examined);
        examined += change_lines(&diff, i, cap, |text| {
            for (pi, term) in &need_content {
                if !satisfied[*pi] && term.matches_line(text) {
                    satisfied[*pi] = true;
                    remaining -= 1;
                    if remaining == 0 {
                        return ControlFlow::Break(());
                    }
                }
            }
            ControlFlow::Continue(())
        });
    }
    // Overall AND: every term must be meta-hit, filename-hit, or content-hit.
    if remaining > 0 {
        return None;
    }
    // The badge: files matching any term, by the same rule (and the same
    // full scan) as the expanded file list in `git_commit_show`. The scan
    // above stops early to decide the match; this one runs only for
    // commits that matched, at most a page's worth.
    let matched_files = file_match_flags(&diff, terms)
        .iter()
        .filter(|(f, c)| *f || *c)
        .count();
    Some((n, matched_files))
}

/// The commit `oid` names.
fn find_commit<'r>(repo: &'r Repository, oid: &str) -> Result<git2::Commit<'r>, String> {
    let oid = oid
        .parse::<git2::Oid>()
        .map_err(|_| format!("'{oid}' is not a valid commit id"))?;
    repo.find_commit(oid).err_text()
}

/// What a commit changed: the files it touched, from diffing its first
/// parent against the commit tree (no parent = initial commit, everything
/// is added). Same shape as `git_stash_show` so the history view reuses
/// the stash file list UI.
///
/// With `query`, each file is flagged with `matched` / `match_kind`
/// ("filename", "content", "both") against the same smart-search terms
/// used by `git_log`, so the UI can pin matching files to the top and
/// show "N of M files match". Without a query every file is `matched`.
#[tauri::command]
pub async fn git_commit_show(
    path: String,
    oid: String,
    query: Option<String>,
) -> Result<Vec<GitStashFile>, String> {
    blocking(move || git_commit_show_blocking(path, oid, query)).await
}

pub fn git_commit_show_blocking(
    path: String,
    oid: String,
    query: Option<String>,
) -> Result<Vec<GitStashFile>, String> {
    let repo = open_repo(&path)?;
    let commit = find_commit(&repo, &oid)?;
    let diff = commit_diff(&repo, &commit, &mut changes_only()).err_text()?;
    let stats = diff_line_stats(&diff);
    // Per file: (filename hit, content hit), the same rule as the badge.
    let flags = query_terms(query.as_deref()).map(|terms| file_match_flags(&diff, &terms));
    let mut out = Vec::new();
    for (i, delta) in diff.deltas().enumerate() {
        let Some(rel) = delta_path(&diff, i) else {
            continue;
        };
        let (additions, deletions) = stats.get(&rel).copied().unwrap_or((0, 0));
        let (matched, match_kind) = match &flags {
            None => (true, None),
            Some(flags) => {
                let (file_hit, content_hit) = flags[i];
                let kind = match (file_hit, content_hit) {
                    (true, true) => Some("both"),
                    (true, false) => Some("filename"),
                    (false, true) => Some("content"),
                    (false, false) => None,
                };
                (file_hit || content_hit, kind.map(str::to_string))
            }
        };
        out.push(GitStashFile {
            path: rel,
            status: delta_status(delta.status()).to_string(),
            additions,
            deletions,
            matched,
            match_kind,
        });
    }
    // Matched files first (when filtering), then by path — so the answer
    // to "which file contains it" is at the top even with 100 files.
    out.sort_by(|a, b| (b.matched.cmp(&a.matched)).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

/// One file's content at a commit plus the version it changed from, in a
/// single call: enough to open a read-only editor tab with change marks.
/// `content`: the file at the commit (None: deleted in the commit);
/// `base`: the version at the first parent (None: new in the commit, or
/// the initial commit). Same shape as the stash preview.
#[tauri::command]
pub async fn git_commit_file_preview(
    path: String,
    oid: String,
    file: String,
) -> Result<GitStashFilePreview, String> {
    blocking(move || git_commit_file_preview_blocking(path, oid, file)).await
}

pub fn git_commit_file_preview_blocking(
    path: String,
    oid: String,
    file: String,
) -> Result<GitStashFilePreview, String> {
    let repo = open_repo(&path)?;
    let rel = inside_repo(&file)?;
    let commit = find_commit(&repo, &oid)?;
    let tree = commit.tree().err_text()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let content = tree_blob(Some(&tree), rel)
        .map(|id| blob_text(&repo, id, &file))
        .transpose()?;
    // Not in the parent under this name: moved (or copied) there by the
    // commit, its change is against where it was. As a new file, every
    // line of a plain rename showed as added.
    let base = tree_blob(parent_tree.as_ref(), rel)
        .or_else(|| tree_blob(parent_tree.as_ref(), &renamed_from(&repo, &commit, rel)?))
        .map(|id| blob_text(&repo, id, &file))
        .transpose()?;
    Ok(GitStashFilePreview { content, base })
}

/// Where a file of `commit` was in its parent, when the commit moved or
/// copied it there (git's rename detection, as the commit's file list).
fn renamed_from(repo: &Repository, commit: &git2::Commit, path: &Path) -> Option<PathBuf> {
    let diff = commit_diff(repo, commit, &mut git2::DiffOptions::new()).ok()?;
    let delta = diff.deltas().find(|d| {
        matches!(d.status(), git2::Delta::Renamed | git2::Delta::Copied)
            && d.new_file().path() == Some(path)
    })?;
    delta.old_file().path().map(Path::to_path_buf)
}

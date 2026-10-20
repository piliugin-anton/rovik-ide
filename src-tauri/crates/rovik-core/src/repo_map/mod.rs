//! The repository map: the project's most relevant declarations for the
//! request at hand, with their files and line ranges, small enough to go in
//! the system prompt (Aider's repo map, on our outlines).
//!
//! 1. Every source file is parsed once (outline::tags): what it declares and
//!    which names it uses. Results are cached by file, until it changes.
//! 2. A file that uses a name another file declares points to it. The
//!    weight: how often it's used (square root, so one file can't drown the
//!    rest), times 10 for a name the request mentions or a long specific
//!    one, divided by 10 for a private (`_x`) name or one declared in many
//!    files (`new`, `get`).
//! 3. PageRank over those links, starting from the request's focus: files
//!    it names, files the chat read or edited. Every file's rank flows to
//!    the declarations it uses.
//! 4. The best-ranked declarations that fit the budget, grouped by file,
//!    with their enclosing declarations for context.
//!
//! The walk respects .gitignore and the agent's denylist, skips large
//! files, and stops at a time and file limit: a huge project gets a map of
//! what was parsed by then, never a wait.

#[cfg(test)]
mod tests;

use crate::outline::{self, Symbol};
use crate::read_symbol::ancestors;
use ignore::{WalkBuilder, WalkState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Files larger than this aren't parsed.
const MAX_FILE_BYTES: u64 = 1_000_000;
/// Names shorter than this are too common to link files by.
const MIN_NAME_CHARS: usize = 3;
/// A name declared in more files than this says little about any of them.
const COMMON_NAME_FILES: usize = 5;
const PAGERANK_DAMPING: f64 = 0.85;
const PAGERANK_ITERATIONS: usize = 60;
/// Share of a file's rank its own declarations get, so a file with no
/// incoming links still shows; a file the request is about keeps more.
const SELF_SHARE: f64 = 0.15;
const FOCUS_SELF_SHARE: f64 = 0.5;
/// Test code and fixtures matter less, unless the request is about them.
const TEST_WEIGHT: f64 = 0.1;
/// A declaration inside a function (a local) can't be used from another file.
const LOCAL_WEIGHT: f64 = 0.1;
/// The least a name in every file still counts.
const MIN_RARITY: f64 = 0.02;
/// A declaration the request names, compared with its neighbours (a plain
/// word, maybe not meant as a name, gets its square root).
const MENTIONED_BOOST: f64 = 10.0;
/// How much a plain word (`stop`) the request uses counts as a starting
/// point, against a specific name (`execute_tool_calls`).
const PLAIN_WORD: f64 = 0.05;
/// Projects tracked in the cache at once.
const CACHED_ROOTS: usize = 2;
/// Outlined languages left out of the map: data and markup.
const NOT_CODE: &[&str] = &[
    "JSON", "YAML", "TOML", "XML", "HTML", "CSS", "Markdown", "KDL",
];

/// What the request is about.
#[derive(Debug, Default, Clone)]
pub struct Focus {
    /// Project-relative paths: named in the request, read or edited in the chat.
    pub files: Vec<String>,
    /// Words the request uses (identifiers, maybe).
    pub words: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The map's size in characters, at most.
    pub max_chars: usize,
    /// Time for finding and parsing files.
    pub time_limit: Duration,
    /// Files parsed, at most.
    pub max_files: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_chars: 6_000,
            time_limit: Duration::from_secs(4),
            max_files: 20_000,
        }
    }
}

/// One file's tags, as cached.
#[derive(Debug)]
struct FileTags {
    symbols: Vec<Symbol>,
    identifiers: HashMap<String, u32>,
}

/// Parsed files by absolute path: modified time and size, and their tags.
type Cache = HashMap<PathBuf, (SystemTime, u64, Arc<FileTags>)>;

/// A file the walk parsed (or found cached): project-relative and absolute
/// path, modified time, size, tags.
type Found = (String, PathBuf, SystemTime, u64, Arc<FileTags>);

/// Per project root, most recently used last.
static CACHE: LazyLock<Mutex<Vec<(PathBuf, Cache)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Forgets the cached tags for one file under `root`, if cached. Call after
/// the agent writes the file: mtime granularity (and same-size rewrites)
/// can otherwise keep serving the old parse, leaving the mid-run map stale.
pub fn invalidate_path(root: &Path, abs: &Path) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    for (cached_root, files) in cache.iter_mut() {
        if cached_root != root {
            continue;
        }
        files.remove(abs);
        // The walk may have stored the canonical path while the caller
        // holds a symlinked one (macOS /tmp), or vice versa.
        if let Ok(canon) = abs.canonicalize() {
            files.remove(&canon);
        }
    }
}

/// The map for `root`, or None when there's nothing to show (no source
/// files with declarations).
pub fn repo_map(root: &Path, focus: &Focus, options: &Options) -> Option<String> {
    let files = collect(root, options);
    let body = render_map(
        &files,
        focus,
        options.max_chars.saturating_sub(HEADER.len() + 1),
    )?;
    Some(format!("{HEADER}\n{body}"))
}

const HEADER: &str = "## Repository map\n\
The project's most relevant files for this request and their declarations, with line ranges (`start-end`), \
nested ones indented under their parent. It's a starting point, not the whole project: \
read what you need with read by symbol (a declaration's name) or start_line/end_line, search for anything not shown, \
and query the structural graph with neighbours (files a file uses / is used by) and callers (a symbol's declarations and users).";

/// The project's source files and their tags, by project-relative path.
fn collect(root: &Path, options: &Options) -> BTreeMap<String, Arc<FileTags>> {
    let deadline = Instant::now() + options.time_limit;
    let cached: Cache = {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .iter()
            .find(|(r, _)| r == root)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    };
    let cached = &cached;
    let found: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    let count = std::sync::atomic::AtomicUsize::new(0);

    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .filter_entry(|e| !crate::agent_tools::is_denylisted_name(&e.file_name().to_string_lossy()))
        .build_parallel()
        .run(|| {
            let (found, count) = (&found, &count);
            Box::new(move |entry| {
                if Instant::now() > deadline {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                // Cheap first: a file no grammar knows isn't read, nor data
                // and markup (their keys and headings aren't used by name).
                match outline::language_for(&rel, "") {
                    Some(lang) if NOT_CODE.contains(&lang.name) => return WalkState::Continue,
                    None if !is_ambiguous(&rel) => return WalkState::Continue,
                    _ => {}
                }
                if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= options.max_files {
                    return WalkState::Quit;
                }
                let Ok(meta) = entry.metadata() else {
                    return WalkState::Continue;
                };
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let size = meta.len();
                let tags = match cached.get(path) {
                    Some((m, s, tags)) if *m == modified && *s == size => tags.clone(),
                    _ => {
                        let Ok(content) = std::fs::read_to_string(path) else {
                            return WalkState::Continue;
                        };
                        match outline::tags(&rel, &content) {
                            Some(t) => Arc::new(FileTags {
                                symbols: t.symbols,
                                identifiers: t.identifiers,
                            }),
                            None => return WalkState::Continue,
                        }
                    }
                };
                if let Ok(mut f) = found.lock() {
                    f.push((rel, path.to_path_buf(), modified, size, tags));
                }
                WalkState::Continue
            })
        });

    let found = found.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut fresh: Cache = HashMap::with_capacity(found.len());
    let mut files = BTreeMap::new();
    for (rel, abs, modified, size, tags) in found {
        fresh.insert(abs, (modified, size, tags.clone()));
        files.insert(rel, tags);
    }
    // The cache keeps what this walk saw: deleted files drop out.
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(r, _)| r != root);
    cache.push((root.to_path_buf(), fresh));
    while cache.len() > CACHED_ROOTS {
        cache.remove(0);
    }
    files
}

/// `.h` and `.m` need their content to tell the language: always read.
fn is_ambiguous(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".h") || lower.ends_with(".m")
}

/// How much a name says about a file using it, from how many of the
/// project's `files` use it (`used_in`): 1 for a name one file uses, near 0
/// for one every file does (`path`, `text`, `new`). Like IDF in search.
fn rarity(used_in: usize, files: usize) -> f64 {
    if files <= 1 {
        return 1.0;
    }
    let used_in = used_in.clamp(1, files) as f64;
    let files = files as f64;
    ((files / used_in).ln() / files.ln()).max(MIN_RARITY)
}

/// `parse_config`, `parseConfig`: long and made of parts, so it hardly
/// means anything else. (A leading `_` marks a private name, not a specific
/// one.)
fn is_specific(name: &str) -> bool {
    let bare = name.trim_start_matches('_');
    let mixed_case =
        bare.chars().any(|c| c.is_uppercase()) && bare.chars().any(|c| c.is_lowercase());
    bare.chars().count() >= 8 && (bare.contains('_') || mixed_case)
}

/// A declaration's weight as a link between files.
fn name_weight(name: &str, declared_in: usize, mentioned: &HashSet<&str>) -> f64 {
    let mut w = 1.0;
    if mentioned.contains(name) {
        w *= 10.0;
    }
    if is_specific(name) {
        w *= 10.0;
    }
    if name.starts_with('_') {
        w *= 0.1;
    }
    if declared_in > COMMON_NAME_FILES {
        w *= 0.1;
    }
    w
}

/// Ranks the declarations and renders the best that fit in `max_chars`.
/// None when no file declares anything.
fn render_map(
    files: &BTreeMap<String, Arc<FileTags>>,
    focus: &Focus,
    max_chars: usize,
) -> Option<String> {
    let paths: Vec<&String> = files.keys().collect();
    let index: HashMap<&str, usize> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();
    let n = paths.len();
    if n == 0 {
        return None;
    }

    // Who declares what: names of every declaration, by file.
    let scopes: Vec<Vec<Scope>> = paths.iter().map(|p| scopes(&files[*p].symbols)).collect();
    let mut declared_by: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, path) in paths.iter().enumerate() {
        let mut names: Vec<&str> = files[*path]
            .symbols
            .iter()
            .zip(&scopes[i])
            .filter(|(_, scope)| !scope.local)
            .map(|(s, _)| s.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            if name.chars().count() >= MIN_NAME_CHARS {
                declared_by.entry(name).or_default().push(i);
            }
        }
    }
    if declared_by.is_empty() && files.values().all(|f| f.symbols.is_empty()) {
        return None;
    }
    let mentioned: HashSet<&str> = focus.words.iter().map(String::as_str).collect();
    // How many files use each declared name.
    let mut used_in: HashMap<&str, usize> = HashMap::new();
    for tags in files.values() {
        for name in tags.identifiers.keys() {
            if let Some((&declared, _)) = declared_by.get_key_value(name.as_str()) {
                *used_in.entry(declared).or_default() += 1;
            }
        }
    }

    // Links: user file → declaring file, through a name, with its weight.
    // `uses[(from, to, name)]`, in a deterministic order.
    let mut links: BTreeMap<(usize, usize, &str), f64> = BTreeMap::new();
    for (from, path) in paths.iter().enumerate() {
        let tags = &files[*path];
        let mut used: Vec<(&String, &u32)> = tags.identifiers.iter().collect();
        used.sort();
        for (name, &count) in used {
            let Some(declarers) = declared_by.get(name.as_str()) else {
                continue;
            };
            // A name the file declares itself is, there, its own.
            if tags.symbols.iter().any(|s| &s.name == name) {
                continue;
            }
            let uses = count;
            let weight = name_weight(name, declarers.len(), &mentioned)
                * rarity(used_in.get(name.as_str()).copied().unwrap_or(1), n)
                * f64::from(uses).sqrt();
            for &to in declarers {
                if to != from {
                    links.insert((from, to, name.as_str()), weight);
                }
            }
        }
    }

    // Where the walk starts: the focus, else everywhere alike.
    let mut start = vec![0.0; n];
    let mut focused = vec![false; n];
    for f in &focus.files {
        if let Some(&i) = index.get(f.as_str()) {
            start[i] += 1.0;
            focused[i] = true;
        }
    }
    // A file whose name a focus word is (`chatStore` → chatStore.ts) counts too.
    for (i, path) in paths.iter().enumerate() {
        let stem = Path::new(path.as_str())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !stem.is_empty() && mentioned.contains(stem) {
            start[i] += 0.5;
        }
    }
    // And so does a file declaring a name the request mentions: much more
    // for `execute_tool_calls` than for a plain word like `stop`.
    let mut mentioned_decls: HashSet<(usize, &str)> = HashSet::new();
    for word in &focus.words {
        let Some(declarers) = declared_by.get(word.as_str()) else {
            continue;
        };
        if declarers.len() > COMMON_NAME_FILES {
            continue;
        }
        let strength = if is_specific(word) { 1.0 } else { PLAIN_WORD } / declarers.len() as f64;
        for &i in declarers {
            start[i] += strength;
            mentioned_decls.insert((i, word.as_str()));
        }
    }
    let total: f64 = start.iter().sum();
    if total > 0.0 {
        start.iter_mut().for_each(|s| *s /= total);
    } else {
        start.iter_mut().for_each(|s| *s = 1.0 / n as f64);
    }

    let rank = pagerank(n, &links, &start);

    // Each file's rank goes to what it uses, in proportion; a share to its own.
    let mut out_weight = vec![0.0; n];
    for (&(from, _, _), &w) in &links {
        out_weight[from] += w;
    }
    let self_share = |i: usize| {
        if focused[i] {
            FOCUS_SELF_SHARE
        } else {
            SELF_SHARE
        }
    };
    let mut def_rank: HashMap<(usize, &str), f64> = HashMap::new();
    for (&(from, to, name), &w) in &links {
        *def_rank.entry((to, name)).or_default() +=
            rank[from] * (1.0 - self_share(from)) * w / out_weight[from];
    }
    // A file's own share: outer declarations first (a quarter less per
    // level), locals hardly.
    let mut own: Vec<Vec<f64>> = Vec::with_capacity(n);
    for (i, path) in paths.iter().enumerate() {
        let weights: Vec<f64> = files[*path]
            .symbols
            .iter()
            .zip(&scopes[i])
            .map(|(s, scope)| {
                0.25f64.powi(s.depth as i32) * if scope.local { LOCAL_WEIGHT } else { 1.0 }
            })
            .collect();
        let total: f64 = weights.iter().sum();
        // Without outgoing links, all of its rank.
        let share = if out_weight[i] > 0.0 {
            self_share(i)
        } else {
            1.0
        };
        own.push(
            weights
                .iter()
                .map(|w| {
                    if total > 0.0 {
                        rank[i] * share * w / total
                    } else {
                        0.0
                    }
                })
                .collect(),
        );
    }

    // Declarations, best first (ties: path, line). A name declared twice in
    // a file shares its links' rank.
    let mut ranked: Vec<(f64, usize, usize)> = Vec::new(); // (rank, file, symbol index)
    for (i, path) in paths.iter().enumerate() {
        let test_file = !focused[i] && is_test_path(path);
        for (j, s) in files[*path].symbols.iter().enumerate() {
            let scope = &scopes[i][j];
            let linked = if scope.local {
                0.0
            } else {
                def_rank.get(&(i, s.name.as_str())).copied().unwrap_or(0.0)
            };
            let mut r = linked + own[i][j];
            if !scope.local && mentioned_decls.contains(&(i, s.name.as_str())) {
                r *= if is_specific(&s.name) {
                    MENTIONED_BOOST
                } else {
                    MENTIONED_BOOST.sqrt()
                };
            }
            if test_file || (scope.test && !focused[i]) {
                r *= TEST_WEIGHT;
            }
            if r > 0.0 {
                ranked.push((r, i, j));
            }
        }
    }
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    if ranked.is_empty() {
        return None;
    }

    // As many as fit: the largest count whose rendering fits.
    let render_top = |k: usize| render(files, &paths, &ranked[..k], &focused);
    let (mut lo, mut hi) = (0usize, ranked.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if render_top(mid).chars().count() <= max_chars {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (lo > 0).then(|| render_top(lo))
}

/// Personalized PageRank over the file links: `start` is where walks
/// begin and restart, and where dangling files send their rank.
fn pagerank(n: usize, links: &BTreeMap<(usize, usize, &str), f64>, start: &[f64]) -> Vec<f64> {
    let mut out: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (&(from, to, _), &w) in links {
        out[from].push((to, w));
    }
    let totals: Vec<f64> = out.iter().map(|o| o.iter().map(|(_, w)| w).sum()).collect();
    let mut rank = start.to_vec();
    for _ in 0..PAGERANK_ITERATIONS {
        let mut next: Vec<f64> = start.iter().map(|s| (1.0 - PAGERANK_DAMPING) * s).collect();
        let mut dangling = 0.0;
        for from in 0..n {
            if totals[from] > 0.0 {
                for &(to, w) in &out[from] {
                    next[to] += PAGERANK_DAMPING * rank[from] * w / totals[from];
                }
            } else {
                dangling += rank[from];
            }
        }
        for (i, s) in start.iter().enumerate() {
            next[i] += PAGERANK_DAMPING * dangling * s;
        }
        let change: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
        rank = next;
        if change < 1e-9 {
            break;
        }
    }
    rank
}

/*
 * ---------------------------------------------------------------------------
 * Graph queries: the agent's `neighbours` and `callers` tools
 * ---------------------------------------------------------------------------
 *
 * The map above computes a file↔file graph and throws the edges away; these
 * queries keep them. Both collect the same tags (same cache, same limits)
 * and rebuild the same links, but with no request focus: the query itself
 * is the focus, so link weights are structural only.
 */

/// Cap on either tool's output, in characters.
const GRAPH_MAX_CHARS: usize = 6_000;
/// Files listed per direction (uses / used-by) or per section.
const GRAPH_MAX_FILES: usize = 20;
/// Shared names shown per neighbouring file.
const GRAPH_MAX_NAMES_PER_FILE: usize = 8;
/// Declaring files shown for a symbol.
const GRAPH_MAX_DECLARERS: usize = 10;
/// Using files shown per symbol.
const GRAPH_MAX_USERS: usize = 30;
/// Candidates listed when a path or symbol doesn't resolve.
const GRAPH_MAX_CANDIDATES: usize = 12;

/// The file↔file graph behind the map, with owned names: paths in order,
/// which files declare each name, and user-file → declaring-file links.
struct Graph {
    paths: Vec<String>,
    declared_by: HashMap<String, Vec<usize>>,
    links: BTreeMap<(usize, usize, String), f64>,
}

fn build_graph(files: &BTreeMap<String, Arc<FileTags>>) -> Graph {
    let paths: Vec<String> = files.keys().cloned().collect();
    let n = paths.len();
    let scopes: Vec<Vec<Scope>> = paths.iter().map(|p| scopes(&files[p].symbols)).collect();
    let mut declared_by: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, path) in paths.iter().enumerate() {
        let mut names: Vec<&str> = files[path]
            .symbols
            .iter()
            .zip(&scopes[i])
            .filter(|(_, scope)| !scope.local)
            .map(|(s, _)| s.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            if name.chars().count() >= MIN_NAME_CHARS {
                declared_by.entry(name.to_string()).or_default().push(i);
            }
        }
    }
    // How many files use each declared name (for rarity, as in the map).
    let empty: HashSet<&str> = HashSet::new();
    let mut used_in: HashMap<&str, usize> = HashMap::new();
    for tags in files.values() {
        for name in tags.identifiers.keys() {
            if let Some((declared, _)) = declared_by.get_key_value(name.as_str()) {
                *used_in.entry(declared.as_str()).or_default() += 1;
            }
        }
    }
    let mut links: BTreeMap<(usize, usize, String), f64> = BTreeMap::new();
    for (from, path) in paths.iter().enumerate() {
        let tags = &files[path];
        let mut used: Vec<(&String, &u32)> = tags.identifiers.iter().collect();
        used.sort();
        for (name, &count) in used {
            let Some(declarers) = declared_by.get(name.as_str()) else {
                continue;
            };
            if tags.symbols.iter().any(|s| &s.name == name) {
                continue;
            }
            let weight = name_weight(name, declarers.len(), &empty)
                * rarity(used_in.get(name.as_str()).copied().unwrap_or(1), n)
                * f64::from(count).sqrt();
            for &to in declarers {
                if to != from {
                    links.insert((from, to, name.clone()), weight);
                }
            }
        }
    }
    Graph {
        paths,
        declared_by,
        links,
    }
}

/// A query path as the walk stores it: forward slashes, no leading `./`.
fn normalize_query_path(input: &str) -> String {
    let mut p = input.trim().replace('\\', "/");
    // `path/to/file.ts:42` → the file.
    if !p.contains("::") {
        if let Some((before, _)) = p.split_once(':') {
            p = before.to_string();
        }
    }
    p = p.trim().to_string();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    while p.starts_with('/') && p.len() > 1 {
        p = p[1..].to_string();
    }
    p.trim_end_matches('/').to_string()
}

fn resolve_graph_file(paths: &[String], input: &str) -> Result<usize, String> {
    let norm = normalize_query_path(input);
    if norm.is_empty() {
        return Err(
            "Error: \"path\" is empty: give a file path from the project, e.g. \"src/lib/foo.ts\"."
                .into(),
        );
    }
    if let Some(i) = paths.iter().position(|p| p == &norm) {
        return Ok(i);
    }
    // `chatStore.ts` for `src/lib/chatStore.ts`: a unique suffix still finds it.
    let suffixes: Vec<usize> = paths
        .iter()
        .enumerate()
        .filter(|(_, p)| *p == &norm || p.ends_with(&format!("/{norm}")))
        .map(|(i, _)| i)
        .collect();
    match suffixes.as_slice() {
        [one] => return Ok(*one),
        [_, _, ..] => {
            let mut out = vec![format!("Error: \"{input}\" matches several files; give the full path from the project root:")];
            for i in suffixes.into_iter().take(GRAPH_MAX_CANDIDATES) {
                out.push(format!("- {}", paths[i]));
            }
            return Err(out.join("\n"));
        }
        [] => {}
    }
    // Last resort: files containing it, to point the model at search instead.
    let mut near: Vec<&String> = paths
        .iter()
        .filter(|p| {
            p.contains(&norm) || norm.split('/').next_back().is_some_and(|f| p.ends_with(f))
        })
        .collect();
    near.sort();
    near.truncate(GRAPH_MAX_CANDIDATES);
    let mut out = vec![format!(
        "Error: \"{input}\" is not a source file in the project graph."
    )];
    if !near.is_empty() {
        out.push("Did you mean:".into());
        for p in near {
            out.push(format!("- {p}"));
        }
    }
    out.push("List folders with list_dir or search for the file with search_code.".into());
    Err(out.join("\n"))
}

/// The one-hop structural neighbourhood of a file: what it uses and what
/// uses it, through shared symbol names. Names are static (tree-sitter):
/// dynamic uses (getattr, reflection) don't appear.
pub fn neighbours(root: &Path, path: &str, options: &Options) -> Result<String, String> {
    let files = collect(root, options);
    if files.is_empty() {
        return Err("Error: no source files found for the project graph.".into());
    }
    let graph = build_graph(&files);
    let me = resolve_graph_file(&graph.paths, path)?;
    let mine = &graph.paths[me];

    // Per neighbouring file, in each direction: the shared names, heaviest first.
    let mut uses: HashMap<usize, Vec<(&str, f64)>> = HashMap::new();
    let mut used_by: HashMap<usize, Vec<(&str, f64)>> = HashMap::new();
    for ((from, to, name), &w) in &graph.links {
        if *from == me {
            uses.entry(*to).or_default().push((name.as_str(), w));
        } else if *to == me {
            used_by.entry(*from).or_default().push((name.as_str(), w));
        }
    }
    let mut uses: Vec<(usize, Vec<(&str, f64)>)> = uses.into_iter().collect();
    let mut used_by: Vec<(usize, Vec<(&str, f64)>)> = used_by.into_iter().collect();
    for (_, names) in uses.iter_mut().chain(used_by.iter_mut()) {
        names.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        names.truncate(GRAPH_MAX_NAMES_PER_FILE);
    }
    let total = |names: &[Vec<(&str, f64)>]| {
        names
            .iter()
            .flat_map(|v| v.iter().map(|(_, w)| w))
            .sum::<f64>()
    };
    uses.sort_by(|a, b| {
        total(std::slice::from_ref(&b.1))
            .total_cmp(&total(std::slice::from_ref(&a.1)))
            .then(graph.paths[a.0].cmp(&graph.paths[b.0]))
    });
    used_by.sort_by(|a, b| {
        total(std::slice::from_ref(&b.1))
            .total_cmp(&total(std::slice::from_ref(&a.1)))
            .then(graph.paths[a.0].cmp(&graph.paths[b.0]))
    });
    uses.truncate(GRAPH_MAX_FILES);
    used_by.truncate(GRAPH_MAX_FILES);

    let mut out = vec![format!("## Neighbours of {mine}")];
    // The file's own top-level declarations, so the model knows what it offers.
    let own: Vec<String> = files[mine]
        .symbols
        .iter()
        .filter(|s| s.depth == 0)
        .take(GRAPH_MAX_FILES)
        .map(|s| {
            let lines = if s.end_line > s.start_line {
                format!("{}-{}", s.start_line, s.end_line)
            } else {
                s.start_line.to_string()
            };
            format!("- {lines} {}", s.signature)
        })
        .collect();
    if !own.is_empty() {
        out.push(format!("Declares:\n{}", own.join("\n")));
    }
    let render_side = |title: &str, side: &[(usize, Vec<(&str, f64)>)], verb: &str| {
        if side.is_empty() {
            return format!("{title}:\n(none found by name{verb})");
        }
        let mut lines = vec![title.to_string()];
        for (other, names) in side {
            let via = names
                .iter()
                .map(|(n, _)| n.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- {} (via {via})", graph.paths[*other]));
        }
        lines.join("\n")
    };
    let uses_total: usize = graph.links.keys().filter(|(f, _, _)| *f == me).count();
    let used_total: usize = graph.links.keys().filter(|(_, t, _)| *t == me).count();
    out.push(render_side(
        &format!(
            "{mine} uses ({} file{})",
            uses.len(),
            if uses.len() == 1 { "" } else { "s" }
        ),
        &uses,
        "",
    ));
    out.push(render_side(
        &format!(
            "Used by ({} file{})",
            used_by.len(),
            if used_by.len() == 1 { "" } else { "s" }
        ),
        &used_by,
        "; it may be an entry point or used dynamically",
    ));
    let _ = (uses_total, used_total);
    out.push("Read a file with read; find exact use sites with search_code.".into());
    let mut text = out.join("\n\n");
    if text.chars().count() > GRAPH_MAX_CHARS {
        let mut cut: String = text.chars().take(GRAPH_MAX_CHARS).collect();
        if let Some(i) = cut.rfind('\n') {
            cut.truncate(i);
        }
        text = format!("{cut}\n[…truncated]");
    }
    Ok(text)
}

/// A symbol query split into parts: `Parser::parse`, `Parser > parse` and
/// `Parser.parse` all mean outer `Parser`, name `parse`.
fn split_symbol_query(query: &str) -> Vec<String> {
    query
        .split(" > ")
        .flat_map(|p| p.split("::"))
        .flat_map(|p| p.split(['.', '/', '#']))
        .map(|p| {
            let mut s = p.trim();
            s = s.trim_start_matches('#').trim();
            s = s.trim_matches(|c| c == '"' || c == '\'' || c == '`');
            let s = match s.find(['<', '(', '[', ':']) {
                Some(0) | None => s,
                Some(i) => &s[..i],
            };
            s.trim().trim_end_matches(':').trim().to_string()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// Indexes of the declarations `query` names in one file's symbols (as
/// `read` by symbol resolves them: whole name first, else the last part
/// with its outers among the ancestors).
fn match_in_file(symbols: &[Symbol], query: &str) -> Vec<usize> {
    let has_separator =
        query.contains("::") || query.contains(" > ") || query.contains(['.', '/', '#']);
    if has_separator {
        let whole: Vec<usize> = (0..symbols.len())
            .filter(|&i| symbols[i].name.trim() == query.trim())
            .collect();
        if !whole.is_empty() {
            return whole;
        }
    } else {
        let want = query
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == '`');
        let hit: Vec<usize> = (0..symbols.len())
            .filter(|&i| symbols[i].name.trim() == want)
            .collect();
        if !hit.is_empty() {
            return hit;
        }
    }
    let parts = split_symbol_query(query);
    let Some((last, outer)) = parts.split_last() else {
        return Vec::new();
    };
    (0..symbols.len())
        .filter(|&i| symbols[i].name.trim() == *last)
        .filter(|&i| {
            let chain: Vec<String> = ancestors(symbols, i)
                .iter()
                .rev()
                .map(|&a| symbols[a].name.trim().to_string())
                .collect();
            let mut want = outer.iter();
            let mut next = want.next();
            for name in &chain {
                if Some(name) == next {
                    next = want.next();
                }
            }
            next.is_none()
        })
        .collect()
}

/// Where a symbol is declared and which files use the name: the static
/// callers/dependents the graph sees. For exact use sites (lines), follow
/// up with search_code.
pub fn callers(root: &Path, symbol: &str, options: &Options) -> Result<String, String> {
    let query = symbol
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if query.is_empty() {
        return Err("Error: \"symbol\" is empty: give a declaration's name, e.g. \"parse\" or \"Parser::parse\".".into());
    }
    let files = collect(root, options);
    if files.is_empty() {
        return Err("Error: no source files found for the project graph.".into());
    }
    let graph = build_graph(&files);
    let parts = split_symbol_query(query);
    let Some((bare, _)) = parts.split_last() else {
        return Err(format!("Error: \"{symbol}\" names no symbol."));
    };
    let Some(declarers) = graph.declared_by.get(bare.as_str()) else {
        // Suggest names containing it, most-declared first for stability.
        let mut near: Vec<(&String, usize)> = graph
            .declared_by
            .iter()
            .filter(|(n, _)| n.contains(bare.as_str()))
            .map(|(n, d)| (n, d.len()))
            .collect();
        near.sort_by(|a, b| a.0.cmp(b.0));
        near.truncate(GRAPH_MAX_CANDIDATES);
        let mut out = vec![format!(
            "Error: no declaration named \"{query}\" in the project graph."
        )];
        if !near.is_empty() {
            out.push("Similar names:".into());
            for (n, _) in near {
                out.push(format!("- {n}"));
            }
        }
        out.push(
            "Search for it with search_code (it may be dynamic or outside parsed sources).".into(),
        );
        return Err(out.join("\n"));
    };
    // The declarations themselves, with lines and signatures.
    let mut shown: Vec<(usize, usize)> = Vec::new(); // (file, symbol index)
    for &i in declarers {
        for j in match_in_file(&files[&graph.paths[i]].symbols, query) {
            shown.push((i, j));
        }
    }
    if shown.is_empty() {
        // Qualified but the nesting never matched: fall back to bare matches.
        for &i in declarers {
            for (j, s) in files[&graph.paths[i]].symbols.iter().enumerate() {
                if s.name.trim() == bare.as_str() {
                    shown.push((i, j));
                }
            }
        }
    }
    shown.sort();
    // Files with a matching declaration, before and after the cap: the ones
    // cut by the cap are "not shown"; declaring files that didn't match a
    // qualified query (`Parser::parse` vs another `parse`) are another thing.
    let distinct =
        |v: &[(usize, usize)]| v.iter().map(|(i, _)| *i).collect::<HashSet<usize>>().len();
    let matched_files = distinct(&shown);
    shown.truncate(GRAPH_MAX_DECLARERS * 2);
    let shown_files = distinct(&shown);
    // Using files, heaviest first.
    let mut users: HashMap<usize, (f64, u32)> = HashMap::new();
    for ((from, to, name), &w) in &graph.links {
        if name == bare && declarers.contains(to) {
            let e = users.entry(*from).or_insert((0.0, 0));
            e.0 += w;
            e.1 += files[&graph.paths[*from]]
                .identifiers
                .get(bare.as_str())
                .copied()
                .unwrap_or(1);
        }
    }
    let mut users: Vec<(usize, f64, u32)> =
        users.into_iter().map(|(f, (w, c))| (f, w, c)).collect();
    users.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(graph.paths[a.0].cmp(&graph.paths[b.0]))
    });
    let users_total = users.len();
    users.truncate(GRAPH_MAX_USERS);

    let mut out = vec![format!("## Callers of `{query}`")];
    if shown.is_empty() {
        out.push(format!(
            "Declared in:\n(declared as `{bare}` in {} file{}; no outline entry matched)",
            declarers.len(),
            if declarers.len() == 1 { "" } else { "s" }
        ));
    } else {
        let mut lines = vec!["Declared in:".to_string()];
        for (i, j) in &shown {
            let s = &files[&graph.paths[*i]].symbols[*j];
            let range = if s.end_line > s.start_line {
                format!("{}-{}", s.start_line, s.end_line)
            } else {
                s.start_line.to_string()
            };
            lines.push(format!("- {} [{range}]: {}", graph.paths[*i], s.signature));
        }
        let hidden = matched_files - shown_files;
        if hidden > 0 {
            lines.push(format!(
                "(…and {hidden} more declaring file{} not shown)",
                if hidden == 1 { "" } else { "s" }
            ));
        }
        let other = declarers.len().saturating_sub(matched_files);
        if other > 0 {
            lines.push(format!(
                "(`{bare}` is also declared in {other} other file{} that don't match `{query}`)",
                if other == 1 { "" } else { "s" }
            ));
        }
        out.push(lines.join("\n"));
    }
    if users.is_empty() {
        out.push(format!("Used by:\n(no other file uses `{bare}` by name; it may be an entry point or used dynamically)"));
    } else {
        let mut lines = vec![format!(
            "Used by ({} file{}):",
            users_total,
            if users_total == 1 { "" } else { "s" }
        )];
        for (f, _, count) in &users {
            let times = if *count == 1 {
                "1 use".to_string()
            } else {
                format!("{count} uses")
            };
            lines.push(format!("- {} ({times} via `{bare}`)", graph.paths[*f]));
        }
        if users_total > users.len() {
            lines.push(format!(
                "(…and {} more not shown)",
                users_total - users.len()
            ));
        }
        out.push(lines.join("\n"));
    }
    out.push(format!("Find exact use sites with search_code for `{bare}`; read a declaration with read by symbol."));
    let mut text = out.join("\n\n");
    if text.chars().count() > GRAPH_MAX_CHARS {
        let mut cut: String = text.chars().take(GRAPH_MAX_CHARS).collect();
        if let Some(i) = cut.rfind('\n') {
            cut.truncate(i);
        }
        text = format!("{cut}\n[…truncated]");
    }
    Ok(text)
}

/// The chosen declarations grouped by file (the request's files first, then
/// by their best declaration; declarations by line), each with the
/// declarations it's nested in, so a method shows under its class.
fn render(
    files: &BTreeMap<String, Arc<FileTags>>,
    paths: &[&String],
    chosen: &[(f64, usize, usize)],
    focused: &[bool],
) -> String {
    let mut by_file: Vec<(usize, Vec<usize>)> = Vec::new(); // in order of each file's best
    for &(_, file, sym) in chosen {
        match by_file.iter_mut().find(|(f, _)| *f == file) {
            Some((_, syms)) => syms.push(sym),
            None => by_file.push((file, vec![sym])),
        }
    }
    // The files the request is about first, in their order of rank.
    by_file.sort_by_key(|(file, _)| !focused[*file]);
    let mut out = String::new();
    for (file, syms) in by_file {
        let symbols = &files[paths[file]].symbols;
        let mut shown: Vec<usize> = Vec::new();
        for &j in &syms {
            shown.push(j);
            shown.extend(ancestors(symbols, j));
        }
        shown.sort_unstable();
        shown.dedup();
        out.push_str(paths[file]);
        out.push_str(":\n");
        for j in shown {
            let s = &symbols[j];
            let lines = if s.end_line > s.start_line {
                format!("{}-{}", s.start_line, s.end_line)
            } else {
                s.start_line.to_string()
            };
            out.push_str(&format!(
                "{}{lines} {}\n",
                "  ".repeat(s.depth + 1),
                s.signature
            ));
        }
    }
    out.trim_end().to_string()
}

/// Where a declaration sits.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Scope {
    /// Inside a function (or a variable's value): not usable elsewhere.
    local: bool,
    /// Inside a `tests` module or the like.
    test: bool,
}

/// Node kinds whose insides are local.
fn is_callable_kind(kind: &str) -> bool {
    [
        "function",
        "method",
        "lambda",
        "closure",
        "subroutine",
        "procedure",
    ]
    .iter()
    .any(|k| kind.contains(k))
        || matches!(
            kind,
            "function_item"
                | "lexical_declaration"
                | "variable_declaration"
                | "let_binding"
                | "value_definition"
                | "value_declaration"
                | "fun_decl"
                | "defun"
                | "proc_declaration"
                | "func_declaration"
        )
}

/// Each symbol's scope, from its ancestors.
fn scopes(symbols: &[Symbol]) -> Vec<Scope> {
    (0..symbols.len())
        .map(|j| {
            let mut scope = Scope::default();
            // `mod x;` only says a module exists: nothing to read there.
            let s = &symbols[j];
            if s.kind == "mod_item" && s.signature.ends_with(';') {
                scope.local = true;
            }
            for a in ancestors(symbols, j) {
                scope.local |= is_callable_kind(&symbols[a].kind);
                scope.test |= matches!(
                    symbols[a].name.as_str(),
                    "tests" | "test" | "spec" | "specs"
                );
            }
            scope
        })
        .collect()
}

/// Test code and fixtures, by path: `tests/`, `__tests__/`, `testdata/`,
/// `fixtures/`, `foo_test.go`, `foo.test.ts`, `foo.spec.js`, `test_foo.py`,
/// `e2e_tests.rs`.
fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let file = parts.pop().unwrap_or("");
    if parts.iter().any(|d| {
        matches!(
            *d,
            "test"
                | "tests"
                | "__tests__"
                | "testdata"
                | "test_data"
                | "fixtures"
                | "__fixtures__"
                | "spec"
                | "specs"
                | "e2e"
        )
    }) {
        return true;
    }
    let stem = file.split('.').next().unwrap_or(file);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.ends_with("_spec")
        || stem == "tests"
        || stem == "conftest"
        || file.contains(".test.")
        || file.contains(".spec.")
}

/// What a request is about, from its text and the paths the chat touched:
/// the project files it names, and its identifier-like words.
pub fn focus_from(root: &Path, request: &str, touched: &[String]) -> Focus {
    let mut files: Vec<String> = Vec::new();
    let mut push = |rel: String| {
        if !files.contains(&rel) {
            files.push(rel);
        }
    };
    for t in touched {
        let rel = relative(root, t);
        if root.join(&rel).is_file() {
            push(rel);
        }
    }
    let mut words: Vec<String> = Vec::new();
    for token in request.split(|c: char| c.is_whitespace() || "`'\"()[]{}<>,;!?|".contains(c)) {
        let token = token.trim_matches(|c: char| ".:#".contains(c));
        if token.is_empty() {
            continue;
        }
        // `src/lib/chatStore.ts`, `chatStore.ts:42`: a file, if it exists.
        if token.contains('/') || token.contains('.') {
            let path = token.split(':').next().unwrap_or(token);
            let rel = relative(root, path);
            if !rel.is_empty() && !rel.contains("..") && root.join(&rel).is_file() {
                push(rel);
            }
        }
        for word in token.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.chars().count() >= MIN_NAME_CHARS
                && word
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                && !words.iter().any(|w| w == word)
            {
                words.push(word.to_string());
            }
        }
    }
    Focus { files, words }
}

/// `path` relative to `root`, with forward slashes.
fn relative(root: &Path, path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let root_str = root.to_string_lossy().replace('\\', "/");
    let rel = p
        .strip_prefix(&format!("{}/", root_str.trim_end_matches('/')))
        .unwrap_or(&p);
    rel.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// Forgets the cached tags (tests). A test that panicked holding the
/// cache doesn't fail the others.
#[cfg(test)]
fn clear_cache() {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

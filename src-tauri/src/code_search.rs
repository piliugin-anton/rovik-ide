//! Project text search on ripgrep's engine.
//!
//! - Walking: `ignore` (ripgrep's walker), in parallel; .gitignore, global
//!   gitignore and .git/info/exclude respected even outside a git repo;
//!   hidden files skipped; denylisted directories pruned, not just filtered.
//! - Globs: ripgrep `-g` semantics via `ignore::overrides`: "*.ts" matches
//!   at any depth, "src/**/*.ts" is relative to the root, "!" excludes.
//! - Matching: `grep-searcher` + `grep-regex`. Literal or regex, case-
//!   insensitive unless asked; binary files skipped (NUL byte), non-UTF-8
//!   text still searched (lossy), files streamed rather than read whole.
//! - The result limit: every match in the project is counted, and the ones
//!   returned are the first `max_results` in path order, so the same search
//!   always returns the same matches whatever order the parallel walk ran
//!   in. Only matches that can still be among those are kept in memory.
//!   Counting stops at COUNT_LIMIT matches; the totals then say so.

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{overrides::OverrideBuilder, WalkBuilder, WalkState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

const MAX_LINE_CHARS: usize = 300;
const MAX_FILE_BYTES: u64 = 2_000_000;
/// Counting all matches stops here: a query like "e" in a large project
/// would otherwise read all of it.
const COUNT_LIMIT: usize = 200_000;

pub struct SearchOptions {
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Match only at word boundaries.
    pub whole_word: bool,
    /// ripgrep-style globs; a leading "!" excludes.
    pub globs: Vec<String>,
    pub max_results: usize,
    pub context_lines: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchLine {
    pub path: String,
    pub line: u64,
    pub text: String,
    /// false for context lines around a match.
    pub is_match: bool,
    /// Match positions within `text`, in UTF-16 units (JavaScript indexes).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<[usize; 2]>,
    /// 1-based UTF-16 column of the first match in the original line, for
    /// placing the editor cursor (`text` is trimmed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// The matches returned (the first `max_results` in path order) and
    /// their context lines.
    pub lines: Vec<SearchLine>,
    /// Matches and files in `lines`.
    pub match_count: usize,
    pub file_count: usize,
    /// More matches exist than are returned.
    pub truncated: bool,
    /// Matches and files in the whole project, returned or not.
    pub total_matches: usize,
    pub total_files: usize,
    /// false when counting stopped at COUNT_LIMIT: the totals are at least
    /// this many, and the matches returned are the first in path order among
    /// the files searched by then.
    pub totals_complete: bool,
}

struct FileHits {
    lines: Vec<SearchLine>,
    /// Matches in `lines`.
    matches: usize,
}

/// The files whose matches can still be among the first `cap` in path
/// order. A file that sorts after enough matches is dropped.
struct Kept {
    files: BTreeMap<String, FileHits>,
    matches: usize,
}

impl Kept {
    fn add(&mut self, path: String, hits: FileHits, cap: usize) {
        if self.matches >= cap
            && self
                .files
                .last_key_value()
                .is_some_and(|(last, _)| &path > last)
        {
            return; // enough matches before it already
        }
        self.matches += hits.matches;
        self.files.insert(path, hits);
        while let Some((_, last)) = self.files.last_key_value() {
            if self.matches - last.matches < cap {
                break;
            }
            self.matches -= last.matches;
            self.files.pop_last();
        }
    }
}

/// Collects one file's matches and context lines: all of them counted, at
/// most `cap` matches kept (no file can show more).
struct Collector<'a> {
    matcher: &'a RegexMatcher,
    path: String,
    lines: Vec<SearchLine>,
    /// Matches in the file, and how many of them are in `lines`.
    matches: usize,
    kept: usize,
    cap: usize,
    total: &'a AtomicUsize,
    count_limit: usize,
    stop: &'a AtomicBool,
}

fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.trim().chars().take(MAX_LINE_CHARS).collect()
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Match ranges inside the cleaned text, and the first match's column in
/// the original line.
fn locate(matcher: &RegexMatcher, raw: &[u8], text: &str) -> (Vec<[usize; 2]>, Option<usize>) {
    let mut ranges = Vec::new();
    let _ = matcher.find_iter(text.as_bytes(), |m| {
        if text.is_char_boundary(m.start()) && text.is_char_boundary(m.end()) && m.end() > m.start()
        {
            ranges.push([utf16_len(&text[..m.start()]), utf16_len(&text[..m.end()])]);
        }
        true
    });
    let full = String::from_utf8_lossy(raw);
    let full = full.trim_end_matches(['\r', '\n']);
    let lead = utf16_len(&full[..full.len() - full.trim_start().len()]);
    let column = ranges.first().map(|r| lead + r[0] + 1);
    (ranges, column)
}

impl Sink for Collector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, m: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let first = m.line_number().unwrap_or(0);
        for (i, line) in m.lines().enumerate() {
            if self.total.fetch_add(1, Ordering::SeqCst) >= self.count_limit {
                self.stop.store(true, Ordering::SeqCst);
                return Ok(false);
            }
            self.matches += 1;
            if self.kept >= self.cap {
                continue; // counted, not kept
            }
            self.kept += 1;
            let text = clean(line);
            let (ranges, column) = locate(self.matcher, line, &text);
            self.lines.push(SearchLine {
                path: self.path.clone(),
                line: first + i as u64,
                text,
                is_match: true,
                ranges,
                column,
            });
        }
        Ok(true)
    }

    fn context(&mut self, _: &Searcher, c: &SinkContext<'_>) -> Result<bool, Self::Error> {
        if self.kept >= self.cap && self.matches > self.kept {
            return Ok(true); // around matches that aren't kept
        }
        self.lines.push(SearchLine {
            path: self.path.clone(),
            line: c.line_number().unwrap_or(0),
            text: clean(c.bytes()),
            is_match: false,
            ranges: Vec::new(),
            column: None,
        });
        Ok(true)
    }
}

/// A comma-separated glob list ("*.ts, !*.test.ts"). Commas inside braces
/// belong to the glob: "src/**/*.{ts,tsx}" is one glob, not two halves.
pub fn split_globs(list: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in list.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                globs.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    globs.push(&list[start..]);
    globs
        .into_iter()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn search(
    root: &Path,
    opts: &SearchOptions,
    skip_name: fn(&str) -> bool,
) -> Result<SearchResult, String> {
    search_counting_to(root, opts, skip_name, COUNT_LIMIT)
}

fn search_counting_to(
    root: &Path,
    opts: &SearchOptions,
    skip_name: fn(&str) -> bool,
    count_limit: usize,
) -> Result<SearchResult, String> {
    if opts.query.is_empty() {
        return Err("query must not be empty".into());
    }

    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!opts.case_sensitive)
        .fixed_strings(!opts.regex)
        .word(opts.whole_word)
        .line_terminator(Some(b'\n'))
        .build(&opts.query)
        .map_err(|e| format!("Invalid regex: {e}"))?;

    let mut overrides = OverrideBuilder::new(root);
    for glob in &opts.globs {
        overrides
            .add(glob)
            .map_err(|e| format!("Invalid glob \"{glob}\": {e}"))?;
    }
    let overrides = overrides
        .build()
        .map_err(|e| format!("Invalid glob: {e}"))?;

    let mut searcher = SearcherBuilder::new();
    searcher
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .before_context(opts.context_lines)
        .after_context(opts.context_lines);

    let cap = opts.max_results.max(1);
    let total = AtomicUsize::new(0);
    let total_files = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let kept = Mutex::new(Kept {
        files: BTreeMap::new(),
        matches: 0,
    });
    let context_lines = opts.context_lines as u64;

    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .overrides(overrides)
        .filter_entry(move |e| !skip_name(&e.file_name().to_string_lossy()))
        .build_parallel()
        .run(|| {
            let matcher = matcher.clone();
            let mut searcher = searcher.build();
            let (total, total_files, stop, kept) = (&total, &total_files, &stop, &kept);
            Box::new(move |entry| {
                if stop.load(Ordering::SeqCst) {
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
                    .into_owned();
                // `/` on every platform, like the other tools' paths.
                let rel = if cfg!(windows) {
                    rel.replace('\\', "/")
                } else {
                    rel
                };
                let mut sink = Collector {
                    matcher: &matcher,
                    path: rel.clone(),
                    lines: Vec::new(),
                    matches: 0,
                    kept: 0,
                    cap,
                    total,
                    count_limit,
                    stop,
                };
                // Unreadable files are skipped, like ripgrep does.
                let _ = searcher.search_path(&matcher, path, &mut sink);
                if sink.matches > 0 {
                    total_files.fetch_add(1, Ordering::SeqCst);
                    let mut lines = sink.lines;
                    lines.sort_by_key(|l| l.line);
                    if sink.matches > sink.kept {
                        trim_context(&mut lines, context_lines);
                    }
                    if let Ok(mut k) = kept.lock() {
                        k.add(
                            rel,
                            FileHits {
                                lines,
                                matches: sink.kept,
                            },
                            cap,
                        );
                    }
                }
                if stop.load(Ordering::SeqCst) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });

    drop(matcher);
    let kept = kept.into_inner().map_err(|e| e.to_string())?;
    let totals_complete = !stop.load(Ordering::SeqCst);
    let total_matches = total.load(Ordering::SeqCst).min(count_limit);
    let total_files = total_files.load(Ordering::SeqCst);

    // The first `cap` matches in path order: whole files, then part of the
    // last one.
    let mut lines = Vec::new();
    let mut match_count = 0;
    let mut file_count = 0;
    for (_, file) in kept.files {
        let room = cap - match_count;
        if room == 0 {
            break;
        }
        let mut file_lines = file.lines;
        if file.matches > room {
            let mut seen = 0;
            let end = file_lines
                .iter()
                .position(|l| {
                    if l.is_match {
                        seen += 1;
                    }
                    seen > room
                })
                .unwrap_or(file_lines.len());
            file_lines.truncate(end);
            trim_context(&mut file_lines, context_lines);
        }
        match_count += file.matches.min(room);
        file_count += 1;
        lines.extend(file_lines);
    }
    Ok(SearchResult {
        lines,
        match_count,
        file_count,
        truncated: match_count < total_matches || !totals_complete,
        total_matches,
        total_files,
        totals_complete,
    })
}

/// Drops context lines after the last match's own `context` lines: the
/// lines before a match that isn't kept.
fn trim_context(lines: &mut Vec<SearchLine>, context: u64) {
    let Some(last) = lines.iter().rposition(|l| l.is_match) else {
        lines.clear();
        return;
    };
    let limit = lines[last].line + context;
    while lines.last().is_some_and(|l| !l.is_match && l.line > limit) {
        lines.pop();
    }
}

/// Search for the Files tab: larger result limit, include/exclude globs,
/// whole-word matching. Runs off the main thread.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_in_files(
    root: String,
    query: String,
    regex: Option<bool>,
    case_sensitive: Option<bool>,
    whole_word: Option<bool>,
    include: Option<String>,
    exclude: Option<String>,
    max_results: Option<usize>,
) -> Result<SearchResult, String> {
    let root = std::fs::canonicalize(&root).map_err(|e| format!("{root}: {e}"))?;
    let split = |list: Option<String>| split_globs(&list.unwrap_or_default());
    let mut globs = split(include);
    globs.extend(
        split(exclude)
            .into_iter()
            .map(|g| format!("!{}", g.trim_start_matches('!'))),
    );
    let opts = SearchOptions {
        query,
        regex: regex.unwrap_or(false),
        case_sensitive: case_sensitive.unwrap_or(false),
        whole_word: whole_word.unwrap_or(false),
        globs,
        max_results: max_results.unwrap_or(2000).clamp(1, 10_000),
        context_lines: 0,
    };
    crate::rt::spawn_blocking(move || search(&root, &opts, crate::agent_tools::is_denylisted_name))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn skip(name: &str) -> bool {
        name == "node_modules" || name.ends_with(".pem")
    }

    fn fixture() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "code-search-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let w = |p: &str, c: &[u8]| {
            let path = root.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        };
        w(
            "src/app.ts",
            b"import x\nconst fooBar = 1;\n// later\nfunction FOOBAR() {}\n",
        );
        w("src/deep/util.ts", b"export const foo_baz = 2;\n");
        w("src/deep/util.test.ts", b"test('foo')\n");
        w("README.md", b"About foo\n");
        w("node_modules/lib/index.js", b"foo everywhere\n");
        w("secret.pem", b"foo key\n");
        w("ignored.txt", b"foo ignored\n");
        w(".gitignore", b"ignored.txt\n");
        w("image.bin", b"foo\x00\x01\x02 binary");
        w("latin1.txt", b"caf\xe9 foo\n"); // not valid UTF-8
        root
    }

    fn opts(query: &str) -> SearchOptions {
        SearchOptions {
            query: query.into(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            globs: vec![],
            max_results: 100,
            context_lines: 0,
        }
    }

    fn paths(r: &SearchResult) -> Vec<String> {
        let mut p: Vec<String> = r
            .lines
            .iter()
            .filter(|l| l.is_match)
            .map(|l| l.path.replace('\\', "/"))
            .collect();
        p.dedup();
        p
    }

    #[test]
    fn finds_text_respecting_ignores_denylist_and_binaries() {
        let root = fixture();
        let r = search(&root, &opts("foo"), skip).unwrap();
        assert_eq!(
            paths(&r),
            [
                "README.md",
                "latin1.txt",
                "src/app.ts",
                "src/deep/util.test.ts",
                "src/deep/util.ts"
            ],
            "no node_modules, .pem, gitignored or binary files; non-UTF-8 text still searched"
        );
        assert_eq!(r.match_count, 6);
        assert_eq!(r.file_count, 5);
        assert!(!r.truncated);
        let latin = r.lines.iter().find(|l| l.path == "latin1.txt").unwrap();
        assert!(latin.text.contains("foo"));
    }

    #[test]
    fn case_regex_and_literal_modes() {
        let root = fixture();
        let mut o = opts("FOOBAR");
        o.case_sensitive = true;
        let r = search(&root, &o, skip).unwrap();
        assert_eq!(r.match_count, 1);
        assert_eq!(r.lines[0].line, 4);

        let mut o = opts(r"foo_?ba[rz]");
        o.regex = true;
        assert_eq!(search(&root, &o, skip).unwrap().match_count, 3);

        // Literal mode: regex syntax is just text.
        assert_eq!(search(&root, &opts("foo.*"), skip).unwrap().match_count, 0);

        let mut o = opts("(unclosed");
        o.regex = true;
        assert!(search(&root, &o, skip)
            .unwrap_err()
            .starts_with("Invalid regex"));
    }

    #[test]
    fn ripgrep_style_globs() {
        let root = fixture();
        let with = |globs: &[&str]| {
            let mut o = opts("foo");
            o.globs = globs.iter().map(|g| g.to_string()).collect();
            paths(&search(&root, &o, skip).unwrap())
        };
        assert_eq!(
            with(&["*.ts"]),
            ["src/app.ts", "src/deep/util.test.ts", "src/deep/util.ts"],
            "any depth"
        );
        assert_eq!(
            with(&["src/deep/**"]),
            ["src/deep/util.test.ts", "src/deep/util.ts"],
            "relative to root"
        );
        assert_eq!(
            with(&["*.ts", "!*.test.ts"]),
            ["src/app.ts", "src/deep/util.ts"],
            "exclusion"
        );
        assert_eq!(
            with(&["src/**/*.{ts,rs}", "!*.test.ts"]),
            ["src/app.ts", "src/deep/util.ts"],
            "braces"
        );
    }

    #[test]
    fn glob_lists_split_outside_braces_only() {
        assert_eq!(split_globs("*.ts, !*.test.ts"), ["*.ts", "!*.test.ts"]);
        assert_eq!(split_globs("src/**/*.{ts,tsx}"), ["src/**/*.{ts,tsx}"]);
        assert_eq!(
            split_globs("{src,lib}/**/*.{ts,tsx},!*.d.ts"),
            ["{src,lib}/**/*.{ts,tsx}", "!*.d.ts"]
        );
        assert_eq!(split_globs(" , *.rs ,"), ["*.rs"]);
        assert!(split_globs("").is_empty());
        // Unbalanced: kept whole, so the error names the glob as written.
        assert_eq!(split_globs("*.{ts,tsx"), ["*.{ts,tsx"]);
    }

    #[test]
    fn context_lines_and_cap() {
        let root = fixture();
        let mut o = opts("later");
        o.context_lines = 1;
        let r = search(&root, &o, skip).unwrap();
        let shape: Vec<(u64, bool)> = r.lines.iter().map(|l| (l.line, l.is_match)).collect();
        assert_eq!(shape, [(2, false), (3, true), (4, false)]);

        let mut o = opts("foo");
        o.max_results = 2;
        let r = search(&root, &o, skip).unwrap();
        assert_eq!(r.match_count, 2);
        assert!(r.truncated);
    }

    #[test]
    fn the_limit_keeps_the_first_matches_by_path_and_counts_them_all() {
        let root = fixture();
        let mut o = opts("foo");
        o.max_results = 2;
        let r = search(&root, &o, skip).unwrap();
        assert_eq!(
            paths(&r),
            ["README.md", "latin1.txt"],
            "the first files by path, not the first found"
        );
        assert_eq!((r.match_count, r.file_count), (2, 2));
        assert_eq!(
            (r.total_matches, r.total_files),
            (6, 5),
            "every match counted"
        );
        assert!(r.truncated && r.totals_complete);

        // Within the limit: everything, and the totals agree.
        let r = search(&root, &opts("foo"), skip).unwrap();
        assert_eq!(
            (r.match_count, r.total_matches, r.file_count, r.total_files),
            (6, 6, 5, 5)
        );
        assert!(!r.truncated);
    }

    /// A project of `n` files in nested folders, file i holding i % 5 + 1
    /// matches.
    fn many_files(n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "code-search-many-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        for i in 0..n {
            let path = root.join(format!("d{}/e{}/f{i:03}.txt", i % 7, i % 3));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body: String = (0..i % 5 + 1)
                .map(|j| format!("needle {j}\nfiller\n"))
                .collect();
            fs::write(path, body).unwrap();
        }
        root
    }

    #[test]
    fn the_same_search_returns_the_same_matches_every_time() {
        let root = many_files(300);
        let mut o = opts("needle");
        o.max_results = 37;
        let first = search(&root, &o, skip).unwrap();
        for _ in 0..25 {
            let again = search(&root, &o, skip).unwrap();
            assert_eq!(
                again.lines, first.lines,
                "the parallel walk's order must not matter"
            );
        }
        assert_eq!(first.match_count, 37);
        assert_eq!(
            first.total_matches,
            (0..300).map(|i| i % 5 + 1).sum::<usize>()
        );
        assert_eq!(first.total_files, 300);
        // And they're the first in path order.
        let mut all = o;
        all.max_results = 10_000;
        let everything = search(&root, &all, skip).unwrap();
        let expected: Vec<&SearchLine> = everything
            .lines
            .iter()
            .filter(|l| l.is_match)
            .take(37)
            .collect();
        let got: Vec<&SearchLine> = first.lines.iter().filter(|l| l.is_match).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn the_limit_can_fall_inside_a_file_and_its_context_stays_tidy() {
        let root = std::env::temp_dir().join(format!(
            "code-search-one-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // Matches on lines 2, 5, 8, 11, 14.
        let body: String = (1..=15)
            .map(|n| {
                if n % 3 == 2 {
                    format!("hit {n}\n")
                } else {
                    format!("other {n}\n")
                }
            })
            .collect();
        fs::write(root.join("a.txt"), body).unwrap();
        let mut o = opts("hit");
        o.context_lines = 1;
        o.max_results = 2;
        let r = search(&root, &o, skip).unwrap();
        let shape: Vec<(u64, bool)> = r.lines.iter().map(|l| (l.line, l.is_match)).collect();
        // No context of line 8's match (line 7) without the match itself.
        assert_eq!(
            shape,
            [
                (1, false),
                (2, true),
                (3, false),
                (4, false),
                (5, true),
                (6, false)
            ]
        );
        assert_eq!((r.match_count, r.total_matches), (2, 5));
    }

    #[test]
    fn a_file_with_many_matches_keeps_only_what_can_be_shown() {
        let root = std::env::temp_dir().join(format!(
            "code-search-dense-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("dense.txt"), "x\n".repeat(5_000)).unwrap();
        let mut o = opts("x");
        o.max_results = 5;
        let r = search(&root, &o, skip).unwrap();
        assert_eq!(r.lines.len(), 5);
        assert_eq!(r.lines.last().unwrap().line, 5);
        assert_eq!((r.total_matches, r.total_files), (5_000, 1));
    }

    #[test]
    fn counting_stops_at_its_limit_and_says_so() {
        let root = many_files(50);
        let mut o = opts("needle");
        o.max_results = 10;
        let r = search_counting_to(&root, &o, skip, 20).unwrap();
        assert!(!r.totals_complete);
        assert!(r.truncated);
        assert_eq!(r.total_matches, 20, "at least this many");
        assert!(r.match_count <= 10);
    }

    #[test]
    fn kept_files_are_pruned_to_what_can_still_be_shown() {
        let hits = |n: usize| FileHits {
            lines: (0..n as u64)
                .map(|i| SearchLine {
                    path: String::new(),
                    line: i + 1,
                    text: String::new(),
                    is_match: true,
                    ranges: vec![],
                    column: None,
                })
                .collect(),
            matches: n,
        };
        let mut kept = Kept {
            files: BTreeMap::new(),
            matches: 0,
        };
        // Arriving in any order; cap 4.
        for (path, n) in [("m", 2), ("z", 3), ("b", 1), ("y", 5), ("a", 2), ("c", 1)] {
            kept.add(path.to_string(), hits(n), 4);
        }
        // a(2) + b(1) + c(1) = 4: nothing after them can be shown.
        assert_eq!(kept.files.keys().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(kept.matches, 4);
        // A file sorting after a full set isn't even stored.
        kept.add("q".into(), hits(9), 4);
        assert_eq!(kept.files.len(), 3);
        // One sorting before it pushes the last out.
        kept.add("0".into(), hits(3), 4);
        assert_eq!(kept.files.keys().collect::<Vec<_>>(), ["0", "a"]);
    }

    #[test]
    fn whole_word_and_match_positions() {
        let root = fixture();
        let mut o = opts("foo");
        o.whole_word = true;
        let r = search(&root, &o, skip).unwrap();
        // "fooBar", "FOOBAR", "foo_baz" are not the word "foo".
        assert_eq!(
            paths(&r),
            ["README.md", "latin1.txt", "src/deep/util.test.ts"]
        );

        let r = search(&root, &opts("bar"), skip).unwrap();
        let line = r
            .lines
            .iter()
            .find(|l| l.path.ends_with("app.ts") && l.line == 2)
            .unwrap();
        assert_eq!(line.text, "const fooBar = 1;");
        assert_eq!(line.ranges, [[9, 12]]);
        assert_eq!(line.column, Some(10));

        // Positions count UTF-16 units and include the trimmed indentation.
        std::fs::write(root.join("src/ru.ts"), "    // Привет, мир: найти здесь\n").unwrap();
        let r = search(&root, &opts("найти"), skip).unwrap();
        let l = &r.lines[0];
        assert_eq!(
            &l.text.encode_utf16().collect::<Vec<_>>()[l.ranges[0][0]..l.ranges[0][1]],
            "найти".encode_utf16().collect::<Vec<_>>().as_slice()
        );
        assert_eq!(l.column, Some(4 + l.ranges[0][0] + 1));
    }
}

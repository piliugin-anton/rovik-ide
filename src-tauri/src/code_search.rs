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

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{overrides::OverrideBuilder, WalkBuilder, WalkState};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

const MAX_LINE_CHARS: usize = 300;
const MAX_FILE_BYTES: u64 = 2_000_000;

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
    pub lines: Vec<SearchLine>,
    pub match_count: usize,
    pub file_count: usize,
    /// Stopped at `max_results`; more matches exist.
    pub truncated: bool,
}

struct FileHits {
    path: String,
    lines: Vec<SearchLine>,
}

/// Collects one file's matches and context lines.
struct Collector<'a> {
    matcher: &'a RegexMatcher,
    path: String,
    lines: Vec<SearchLine>,
    total: &'a AtomicUsize,
    cap: usize,
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
        if text.is_char_boundary(m.start()) && text.is_char_boundary(m.end()) && m.end() > m.start() {
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
            if self.total.fetch_add(1, Ordering::SeqCst) >= self.cap {
                self.stop.store(true, Ordering::SeqCst);
                return Ok(false);
            }
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

pub fn search(root: &Path, opts: &SearchOptions, skip_name: fn(&str) -> bool) -> Result<SearchResult, String> {
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
        overrides.add(glob).map_err(|e| format!("Invalid glob \"{glob}\": {e}"))?;
    }
    let overrides = overrides.build().map_err(|e| format!("Invalid glob: {e}"))?;

    let mut searcher = SearcherBuilder::new();
    searcher
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .before_context(opts.context_lines)
        .after_context(opts.context_lines);

    let cap = opts.max_results.max(1);
    let total = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let files: Mutex<Vec<FileHits>> = Mutex::new(Vec::new());

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
            let (total, stop, files) = (&total, &stop, &files);
            Box::new(move |entry| {
                if stop.load(Ordering::SeqCst) {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else { return WalkState::Continue };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
                let mut sink = Collector { matcher: &matcher, path: rel.clone(), lines: Vec::new(), total, cap, stop };
                // Unreadable files are skipped, like ripgrep does.
                let _ = searcher.search_path(&matcher, path, &mut sink);
                if sink.lines.iter().any(|l| l.is_match) {
                    if let Ok(mut f) = files.lock() {
                        f.push(FileHits { path: rel, lines: sink.lines });
                    }
                }
                if stop.load(Ordering::SeqCst) { WalkState::Quit } else { WalkState::Continue }
            })
        });

    drop(matcher);
    let mut files = files.into_inner().map_err(|e| e.to_string())?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let truncated = stop.load(Ordering::SeqCst);
    let mut lines = Vec::new();
    let mut match_count = 0;
    let file_count = files.len();
    for file in files {
        let mut file_lines = file.lines;
        file_lines.sort_by_key(|l| l.line);
        match_count += file_lines.iter().filter(|l| l.is_match).count();
        lines.extend(file_lines);
    }
    Ok(SearchResult { lines, match_count, file_count, truncated })
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
    let split = |list: Option<String>| -> Vec<String> {
        list.unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut globs = split(include);
    globs.extend(split(exclude).into_iter().map(|g| format!("!{}", g.trim_start_matches('!'))));
    let opts = SearchOptions {
        query,
        regex: regex.unwrap_or(false),
        case_sensitive: case_sensitive.unwrap_or(false),
        whole_word: whole_word.unwrap_or(false),
        globs,
        max_results: max_results.unwrap_or(2000).clamp(1, 10_000),
        context_lines: 0,
    };
    tauri::async_runtime::spawn_blocking(move || search(&root, &opts, crate::agent_tools::is_denylisted_name))
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
        let root = std::env::temp_dir().join(format!("code-search-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&root);
        let w = |p: &str, c: &[u8]| {
            let path = root.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        };
        w("src/app.ts", b"import x\nconst fooBar = 1;\n// later\nfunction FOOBAR() {}\n");
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
        let mut p: Vec<String> = r.lines.iter().filter(|l| l.is_match).map(|l| l.path.replace('\\', "/")).collect();
        p.dedup();
        p
    }

    #[test]
    fn finds_text_respecting_ignores_denylist_and_binaries() {
        let root = fixture();
        let r = search(&root, &opts("foo"), skip).unwrap();
        assert_eq!(
            paths(&r),
            ["README.md", "latin1.txt", "src/app.ts", "src/deep/util.test.ts", "src/deep/util.ts"],
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
        assert!(search(&root, &o, skip).unwrap_err().starts_with("Invalid regex"));
    }

    #[test]
    fn ripgrep_style_globs() {
        let root = fixture();
        let with = |globs: &[&str]| {
            let mut o = opts("foo");
            o.globs = globs.iter().map(|g| g.to_string()).collect();
            paths(&search(&root, &o, skip).unwrap())
        };
        assert_eq!(with(&["*.ts"]), ["src/app.ts", "src/deep/util.test.ts", "src/deep/util.ts"], "any depth");
        assert_eq!(with(&["src/deep/**"]), ["src/deep/util.test.ts", "src/deep/util.ts"], "relative to root");
        assert_eq!(with(&["*.ts", "!*.test.ts"]), ["src/app.ts", "src/deep/util.ts"], "exclusion");
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
    fn whole_word_and_match_positions() {
        let root = fixture();
        let mut o = opts("foo");
        o.whole_word = true;
        let r = search(&root, &o, skip).unwrap();
        // "fooBar", "FOOBAR", "foo_baz" are not the word "foo".
        assert_eq!(paths(&r), ["README.md", "latin1.txt", "src/deep/util.test.ts"]);

        let r = search(&root, &opts("bar"), skip).unwrap();
        let line = r.lines.iter().find(|l| l.path.ends_with("app.ts") && l.line == 2).unwrap();
        assert_eq!(line.text, "const fooBar = 1;");
        assert_eq!(line.ranges, [[9, 12]]);
        assert_eq!(line.column, Some(10));

        // Positions count UTF-16 units and include the trimmed indentation.
        std::fs::write(root.join("src/ru.ts"), "    // Привет, мир: найти здесь\n").unwrap();
        let r = search(&root, &opts("найти"), skip).unwrap();
        let l = &r.lines[0];
        assert_eq!(&l.text.encode_utf16().collect::<Vec<_>>()[l.ranges[0][0]..l.ranges[0][1]], "найти".encode_utf16().collect::<Vec<_>>().as_slice());
        assert_eq!(l.column, Some(4 + l.ranges[0][0] + 1));
    }
}

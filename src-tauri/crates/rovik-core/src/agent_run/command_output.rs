//! Shapes command output for the model: what it needs to act on, in as few
//! tokens as possible. Used for bash results and for the
//! verification gate's failure reports.
//!
//!   - `cap_output`: long output keeps its start and end (errors and
//!     summaries come last), whole lines only.
//!   - `focus_failures`: a failing build or test run keeps its error blocks
//!     and final summary, not thousands of lines of passing tests.
//!   - `error_locations`: the files the errors point at, so a report can say
//!     whether they're files the agent changed.
//!   - `failure_keys`: a stable key for each failure (an error, a failing
//!     test), the same when only line numbers moved, so the verification
//!     gate can tell progress from churn between rounds (repair.rs).
//!   - `tests_ran`: how many tests a test run ran, from its summary.
//!   - `environment_failure`: a failed check that says its environment
//!     failed (a program or a module that isn't installed), not the code.
//!
//! Plain text heuristics that work across tsc, eslint, cargo, rustc, jest,
//! vitest, pytest and go test, without parsing any one tool's format.

use regex::Regex;
use std::sync::LazyLock;

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").unwrap());

/// Color and cursor codes. Commands run with NO_COLOR, but not all obey.
pub fn strip_ansi(text: &str) -> String {
    ANSI.replace_all(text, "").into_owned()
}

/// Plain text with "\n" line ends: no color codes, and the "\r" of CRLF and
/// of progress bars that redraw in place made line breaks, so a progress
/// run isn't one huge line.
pub fn clean_output(text: &str) -> String {
    strip_ansi(text).replace("\r\n", "\n").replace('\r', "\n")
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The start and the end of long output, whole lines. Errors and summaries
/// are usually at the end; the start shows what ran.
pub fn cap_output(text: &str, head_lines: usize, tail_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let out = if lines.len() > head_lines + tail_lines {
        let omitted = format!(
            "[… {} lines omitted …]",
            lines.len() - head_lines - tail_lines
        );
        let mut kept: Vec<&str> = lines[..head_lines].to_vec();
        kept.push(&omitted);
        kept.extend_from_slice(&lines[lines.len() - tail_lines..]);
        kept.join("\n")
    } else {
        text.to_string()
    };
    if chars(&out) <= max_chars {
        return out;
    }
    // Very long lines: keep the end, where the summary is.
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in lines.iter().rev() {
        if size + chars(line) >= max_chars {
            break;
        }
        kept.push(line);
        size += chars(line) + 1;
    }
    if kept.is_empty() {
        // The last line alone is over the limit (minified code, a JSON
        // blob): keep its end rather than nothing.
        let last = lines[lines.len() - 1];
        let end: String = last
            .chars()
            .skip(chars(last).saturating_sub(max_chars))
            .collect();
        return format!(
            "[… output shortened to the last {} characters of its last line …]\n{end}",
            chars(&end)
        );
    }
    kept.reverse();
    format!(
        "[… output shortened to its last {} lines …]\n{}",
        kept.len(),
        kept.join("\n")
    )
}

/// A line that starts or marks a failure. Deliberately broad: a few extra
/// lines kept cost little; a missed error makes the report useless.
static FAILURE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(error|errors|fail|failed|failure|failures|failing|panicked|exception|traceback|assertion|assert)\b|^\s*(✗|✕|×|●|E\s{2,}|FAIL\b|---- .+ ----$|--- FAIL)|^\s*>\s+\d+\s*\|",
    )
    .unwrap()
});

/// Lines that say it worked, which FAILURE_LINE's words also match ("0 failed").
static ALL_CLEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b0 (failed|failures|errors?)\b|\bno (errors|failures)\b").unwrap()
});

/// Lines from the end that are always kept: the run's summary.
const SUMMARY_LINES: usize = 15;
/// How far an error block runs on after its first line (until a blank line).
const BLOCK_LINES: usize = 30;
/// Lines kept before an error line (the code it points at, a test name).
const CONTEXT_BEFORE: usize = 2;

/// A failing run's output, cut down to its error blocks and its closing
/// summary. Short output is returned whole. Output with no recognizable
/// error lines falls back to its start and end (`cap_output`).
pub fn focus_failures(text: &str, max_chars: usize) -> String {
    let clean = clean_output(text);
    let clean = clean.trim_end();
    let lines: Vec<&str> = clean.split('\n').collect();
    if lines.len() <= 80 && chars(clean) <= max_chars {
        return clean.to_string();
    }

    // Which lines to keep: each failure line, a little before it, and the
    // block after it up to a blank line.
    let mut keep = vec![false; lines.len()];
    let mut blocks = 0;
    for i in 0..lines.len() {
        if !FAILURE_LINE.is_match(lines[i]) || ALL_CLEAR.is_match(lines[i]) {
            continue;
        }
        if !keep[i] {
            blocks += 1;
        }
        for j in i.saturating_sub(CONTEXT_BEFORE)..i {
            if !lines[j].trim().is_empty() {
                keep[j] = true;
            }
        }
        for j in i..lines.len().min(i + BLOCK_LINES) {
            if j > i && lines[j].trim().is_empty() {
                break;
            }
            keep[j] = true;
        }
    }
    if blocks == 0 {
        return cap_output(clean, 15, 120, max_chars);
    }

    let summary_from = lines.len().saturating_sub(SUMMARY_LINES);
    for k in keep.iter_mut().skip(summary_from) {
        *k = true;
    }

    // Assemble: kept lines in order, gaps marked. The first errors usually
    // cause the rest, so over budget the later ones go, never the summary.
    let summary = lines[summary_from..].join("\n");
    let budget = max_chars as isize - chars(&summary) as isize - 200;
    let mut out: Vec<String> = Vec::new();
    let mut size: isize = 0;
    let mut omitted = 0;
    let mut gap_start: Option<usize> = None;
    let close_gap = |out: &mut Vec<String>, gap_start: &mut Option<usize>, at: usize| {
        let Some(start) = gap_start.take() else {
            return;
        };
        let gap = at - start;
        // A short gap costs less than its marker: keep its lines.
        if gap <= 2 {
            out.extend(lines[start..at].iter().map(|l| l.to_string()));
        } else {
            out.push(format!("[… {gap} lines …]"));
        }
    };
    for i in 0..summary_from {
        if !keep[i] {
            if gap_start.is_none() && omitted == 0 {
                gap_start = Some(i);
            }
            continue;
        }
        if size > budget {
            omitted += 1;
            continue;
        }
        close_gap(&mut out, &mut gap_start, i);
        out.push(lines[i].to_string());
        size += chars(lines[i]) as isize + 1;
    }
    if omitted > 0 {
        out.push(format!("[… {omitted} more error lines omitted …]"));
    } else {
        close_gap(&mut out, &mut gap_start, summary_from);
    }
    out.push(summary);
    out.join("\n")
}

static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)(?:^|[\s"'(`\[])((?:\.{1,2}/|/)?(?:[A-Za-z0-9_@.+-]+/)*[A-Za-z0-9_@.+-]+\.[A-Za-z][A-Za-z0-9]{0,5})(?::(\d+)(?::\d+)?\b|\((\d+),\d+\))"#,
    )
    .unwrap()
});
static PY_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"File "([^"]+\.[A-Za-z0-9]{1,6})", line \d+"#).unwrap());
static NOT_SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(https?|node|file):|^\d+(\.\d+)+$|(^|/)node_modules/|\.min\.js$").unwrap()
});

/// Source files the output points at: "src/a.ts:12:5", "src/a.ts(12,5)",
/// "--> src/main.rs:3:1", 'File "app/x.py", line 9'. Returned as written
/// (relative paths), each once, in order of appearance.
pub fn error_locations(text: &str) -> Vec<String> {
    let clean = strip_ansi(text);
    let mut found: Vec<String> = Vec::new();
    for re in [&*LOCATION, &*PY_LOCATION] {
        for m in re.captures_iter(&clean) {
            let path = m[1].strip_prefix("./").unwrap_or(&m[1]).to_string();
            // Not source files: URLs, versions, node internals, minified bundles.
            if NOT_SOURCE.is_match(&path) || found.contains(&path) {
                continue;
            }
            found.push(path);
        }
    }
    found
}

/// Splits the files errors point at into ones the agent changed in this run
/// and others. Paths are compared by their tail: tools print them relative
/// to where they ran (cargo in src-tauri/ prints "src/x.rs" for
/// "src-tauri/src/x.rs").
pub fn split_by_changed(
    locations: &[String],
    changed_files: &[String],
) -> (Vec<String>, Vec<String>) {
    locations
        .iter()
        .cloned()
        .partition(|loc| changed_files.iter().any(|c| same_file_by_tail(loc, c)))
}

/// Whether a path a tool printed (absolute, or relative to where it ran)
/// is a project file: compared by their tail (cargo in src-tauri/ prints
/// "src/x.rs" for "src-tauri/src/x.rs"; eslint prints "/home/me/p/src/a.ts").
pub fn same_file_by_tail(printed: &str, file: &str) -> bool {
    let norm = |p: &str| {
        let p = p.replace('\\', "/");
        let p = p.strip_prefix("./").unwrap_or(&p);
        p.trim_start_matches('/').to_string()
    };
    let (l, c) = (norm(printed), norm(file));
    !l.is_empty()
        && !c.is_empty()
        && (c == l || c.ends_with(&format!("/{l}")) || l.ends_with(&format!("/{c}")))
}

/*
 * ---------------------------------------------------------------------------
 * Failure keys
 * ---------------------------------------------------------------------------
 */

/// Most keys kept for one check: past this, the rest are one key.
pub const MAX_KEYS: usize = 50;
const MAX_MESSAGE_CHARS: usize = 160;

/// tsc: `src/a.ts(12,5): error TS2322: …` and `src/a.ts:12:5 - error TS2322: …`.
static TSC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?P<file>[^\s(:]+\.[A-Za-z0-9]+)(?:\(\d+,\d+\)|:\d+:\d+)\s*[:-]\s*error\s+(?P<code>TS\d+)\s*:\s*(?P<msg>.+)$").unwrap()
});
/// rustc and other compilers: `error[E0308]: mismatched types` (its file on
/// a later `--> src/main.rs:3:5` line).
static RUST_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error(?:\[(?P<code>[A-Z]+\d+)\])?:\s*(?P<msg>.+)$").unwrap());
static RUST_ARROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*-->\s*(?P<file>[^\s:]+\.[A-Za-z0-9]+):\d+").unwrap());
/// Summary lines that say it failed, not what did.
static RUST_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(could not compile|aborting due to|test failed|\d+ previous errors?|build failed|failed to run|Some errors have detailed|\d+ targets? failed|doctest failed|process didn't exit successfully)").unwrap()
});
/// `path:12:5: error: msg` (gcc, clang), `path:12:5: F401 msg` (ruff),
/// `./a.go:3:2: msg` (go vet).
static GENERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?P<file>(?:\.{1,2}/)?[A-Za-z0-9_@./+-]+\.[A-Za-z][A-Za-z0-9]{0,5}):\d+(?P<col>:\d+)?:\s*(?:(?:fatal\s+)?error:\s*|(?P<code>[A-Z]{1,4}\d{2,5})\s+)?(?P<msg>.+)$").unwrap()
});
/// eslint (stylish): a file on its own line, then `  12:5  error  msg  rule`.
static ESLINT_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<file>(?:/|\.{0,2}/?)[A-Za-z0-9_@./+-]+\.[A-Za-z][A-Za-z0-9]{0,5})\s*$")
        .unwrap()
});
static ESLINT_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+\d+:\d+\s+error\s+(?P<msg>.+?)(?:\s{2,}(?P<code>[@\w/-]+))?\s*$").unwrap()
});
/// pytest's collection error: `ERROR tests/test_a.py - ImportError: …`
/// (round 3: an error in that file, not "failed").
static PYTEST_COLLECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ERROR (?P<file>\S+\.py)(?: - (?P<msg>.+))?$").unwrap());
/// Failing tests: cargo, go, pytest, jest/vitest. A duration after the
/// name isn't part of it, with parentheses (jest's `✕ adds (12 ms)`) or
/// without (vitest's `× adds 12ms`): it differs every run.
static TEST_FAILS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^test (?P<name>\S+) \.\.\. FAILED$",
        r"^\s*--- FAIL: (?P<name>\S+)",
        r"^(?:FAILED|ERROR) (?P<name>\S+::\S+)",
        r"^\s*(?:✕|×|✗)\s+(?P<name>.+?)(?:\s+\(?\d+(?:\.\d+)?\s*m?s\)?)?$",
        r"^\s*FAIL\s+(?P<name>\S+\s+>\s+.+?)(?:\s+\d+\s*m?s)?$",
        r"^\s*●\s+(?P<name>.+›.+)$",
    ]
    .iter()
    .map(|r| Regex::new(r).unwrap())
    .collect()
});
/// Jest's line for a test file's result, `FAIL __tests__/App.test.tsx` or
/// `PASS …` (a slow one's duration after it).
static JEST_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?P<result>FAIL|PASS)\s+(?P<file>\S+\.[A-Za-z0-9]+)(?:\s+\([\d.]+\s*m?s\))?\s*$",
    )
    .unwrap()
});
/// Jest's heading for a test file that couldn't run.
static JEST_SUITE_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*●\s+Test suite failed to run\s*$").unwrap());
/// Jest's heading for a failing test, in a failed test file's block.
static JEST_TEST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*●\s+(?P<name>.+)$").unwrap());
/// Jest's headings in a test file's block that aren't tests.
const JEST_NOT_TESTS: &[&str] = &["Console", "Cannot log after tests are done"];
/// cargo's `Running unittests src/lib.rs (target/debug/deps/alpha-1a2b)`:
/// the crate the tests after it are in (P3.1: two crates' `tests::it`).
static CARGO_RUNNING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*Running .*\((?:[^)]*[/\\])?deps[/\\](?P<krate>[A-Za-z0-9_]+)-[0-9a-f]+(?:\.exe)?\)\s*$").unwrap()
});
/// A message that's only an exception's class (`AssertionError`): a
/// traceback's last line, not a failure of its own (P3.1).
static BARE_EXCEPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w.]*(?:Error|Exception|Failure)\s*$").unwrap());

/// Line and column numbers inside a message, which move when code above
/// them changes: `a.rs:12:5`, `(12,5)`, `line 12`.
static MESSAGE_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+(?::\d+)?\b|\(\d+,\s*\d+\)|\bline \d+\b").unwrap());

fn key_message(msg: &str) -> String {
    let m = MESSAGE_LOCATION.replace_all(msg, "");
    let m = m.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::chat_db::store::char_prefix(&m, MAX_MESSAGE_CHARS).to_string()
}

fn key_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// A key for each failure in a failed check's output: for an error, the
/// check, file, error code and message without line numbers; for a failing
/// test, its name. Output with neither is the check alone. Each key once,
/// sorted; at most MAX_KEYS (the rest are counted in one key).
pub fn failure_keys(kind: &str, output: &str, timed_out: bool) -> Vec<String> {
    let mut keys = cap_keys(kind, all_failure_keys(kind, output, timed_out));
    // Round 3: a check that timed out says nothing about the tests it
    // didn't reach, whatever it printed before the kill: the round can't
    // be compared (repair.rs `blind`), so the key is always there.
    if timed_out {
        keys.push(format!("{kind}|timed out"));
    }
    keys
}

/// At most MAX_KEYS of `keys` (sorted): the rest are counted in one key.
pub fn cap_keys(kind: &str, mut keys: Vec<String>) -> Vec<String> {
    if keys.len() > MAX_KEYS {
        let more = keys.len() - (MAX_KEYS - 1);
        keys.truncate(MAX_KEYS - 1);
        keys.push(format!("{kind}|and {more} more"));
    }
    keys
}

/// Every failure's key in a check's output ([`failure_keys`] without the
/// cap), sorted: each line is read by the first of [`RULES`] that knows it.
/// Output with no key is the check alone, unless it timed out.
fn all_failure_keys(kind: &str, output: &str, timed_out: bool) -> Vec<String> {
    let clean = clean_output(output);
    let lines: Vec<&str> = clean.lines().collect();
    let mut reading = Reading {
        kind,
        lines: &lines,
        keys: Vec::new(),
        seen: std::collections::HashSet::new(),
        generic: Vec::new(),
        tests: false,
        eslint_file: None,
        krate: None,
        jest_file: None,
    };
    for i in 0..lines.len() {
        for rule in RULES {
            if rule(&mut reading, i) {
                break;
            }
        }
    }
    let Reading {
        mut keys,
        mut seen,
        generic,
        tests,
        ..
    } = reading;
    // P3.1: next to failing tests, the generic lines are the same failures
    // again.
    if !tests {
        for k in generic {
            if seen.insert(k.clone()) {
                keys.push(k);
            }
        }
    }
    if keys.is_empty() && !timed_out {
        keys.push(format!("{kind}|failed"));
    }
    keys.sort();
    keys
}

/// What reading a check's output found so far: the keys, and what earlier
/// lines said that the next ones need (the file an ESLint block is about,
/// the crate cargo's tests are in, the test file a Jest block is about).
struct Reading<'a> {
    kind: &'a str,
    lines: &'a [&'a str],
    keys: Vec<String>,
    seen: std::collections::HashSet<String>,
    /// Keys from the generic `path:line: msg` shape: tracebacks, assertion
    /// lines. Next to failing tests they're the same failures again (P3.1).
    generic: Vec<String>,
    /// A failing test was found.
    tests: bool,
    eslint_file: Option<String>,
    krate: Option<String>,
    jest_file: Option<String>,
}

impl Reading<'_> {
    fn push(&mut self, key: String) {
        if self.seen.insert(key.clone()) {
            self.keys.push(key);
        }
    }

    fn test(&mut self, name: &str) {
        let key = format!("{}|test|{name}", self.kind);
        self.push(key);
        self.tests = true;
    }
}

/// One way tools print a failure: whether line `i` of the output is one it
/// knows, noting what it says (`true` ends that line's reading). The rules
/// are tried in [`RULES`]' order. To teach the checks another tool's
/// output, write its rule and list it.
type Rule = fn(&mut Reading, usize) -> bool;

const RULES: &[Rule] = &[
    cargo_running,
    tsc_error,
    rust_error,
    pytest_collection_error,
    jest_test_file,
    jest_suite_failed,
    failing_test,
    jest_top_level_test,
    eslint_file,
    eslint_error,
    blank_line,
    generic_location,
];

/// cargo's `Running …` line: the crate the next tests are in.
fn cargo_running(r: &mut Reading, i: usize) -> bool {
    let Some(c) = CARGO_RUNNING.captures(r.lines[i]) else {
        return false;
    };
    r.krate = Some(c["krate"].to_string());
    true
}

fn tsc_error(r: &mut Reading, i: usize) -> bool {
    let Some(c) = TSC.captures(r.lines[i]) else {
        return false;
    };
    let key = format!(
        "{}|{}|{}|{}",
        r.kind,
        key_path(&c["file"]),
        &c["code"],
        key_message(&c["msg"])
    );
    r.push(key);
    true
}

/// rustc's `error[E0308]: …`, its file on a `-->` line just below.
fn rust_error(r: &mut Reading, i: usize) -> bool {
    let Some(c) = RUST_ERROR.captures(r.lines[i]) else {
        return false;
    };
    if RUST_SUMMARY.is_match(&c["msg"]) {
        return true;
    }
    let file = r.lines[i + 1..r.lines.len().min(i + 6)]
        .iter()
        .find_map(|l| RUST_ARROW.captures(l).map(|a| key_path(&a["file"])))
        .unwrap_or_default();
    let code = c.name("code").map_or("", |m| m.as_str());
    let key = format!("{}|{file}|{code}|{}", r.kind, key_message(&c["msg"]));
    r.push(key);
    true
}

fn pytest_collection_error(r: &mut Reading, i: usize) -> bool {
    let Some(c) = PYTEST_COLLECT.captures(r.lines[i]) else {
        return false;
    };
    let msg = c.name("msg").map_or("", |m| m.as_str());
    let key = format!("{}|{}||{}", r.kind, key_path(&c["file"]), key_message(msg));
    r.push(key);
    true
}

/// Jest's `FAIL __tests__/App.test.tsx` (and `PASS …`): the test file the
/// lines below are about, while it failed.
fn jest_test_file(r: &mut Reading, i: usize) -> bool {
    let Some(c) = JEST_FILE.captures(r.lines[i]) else {
        return false;
    };
    r.jest_file = (&c["result"] == "FAIL").then(|| key_path(&c["file"]));
    true
}

/// Jest's `● Test suite failed to run`: a test file none of whose tests
/// ran (a module it can't load, as an unmocked native module in a React
/// Native app; a syntax error). Its file and the first line of why.
fn jest_suite_failed(r: &mut Reading, i: usize) -> bool {
    if !JEST_SUITE_FAILED.is_match(r.lines[i]) {
        return false;
    }
    let why = r.lines[i + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let file = r.jest_file.clone().unwrap_or_default();
    let key = format!(
        "{}|{file}||Test suite failed to run: {}",
        r.kind,
        key_message(why)
    );
    r.push(key);
    true
}

/// A failing test's line: cargo, go, pytest, jest, vitest (`TEST_FAILS`).
fn failing_test(r: &mut Reading, i: usize) -> bool {
    let line = r.lines[i];
    let Some(c) = TEST_FAILS.iter().find_map(|re| re.captures(line)) else {
        return false;
    };
    let name = c["name"].trim();
    // In a cargo workspace, the crate too: `alpha::tests::it`.
    let name = match &r.krate {
        Some(k) if line.starts_with("test ") => format!("{k}::{name}"),
        _ => name.to_string(),
    };
    r.test(&name);
    true
}

/// Jest's heading of a failing test outside any `describe` (`● adds`, as
/// React Native's template test is), in a failed test file's block: the
/// ones inside are `● Suite › adds` (`TEST_FAILS`).
fn jest_top_level_test(r: &mut Reading, i: usize) -> bool {
    if r.jest_file.is_none() {
        return false;
    }
    let Some(c) = JEST_TEST.captures(r.lines[i]) else {
        return false;
    };
    let name = c["name"].trim();
    if JEST_NOT_TESTS.iter().any(|n| name.starts_with(n)) {
        return false;
    }
    r.test(name);
    true
}

/// ESLint's stylish format: a file on its own line, its errors below it.
fn eslint_file(r: &mut Reading, i: usize) -> bool {
    let Some(c) = ESLINT_FILE.captures(r.lines[i]) else {
        return false;
    };
    r.eslint_file = Some(key_path(&c["file"]));
    true
}

fn eslint_error(r: &mut Reading, i: usize) -> bool {
    let Some(file) = r.eslint_file.clone() else {
        return false;
    };
    let Some(c) = ESLINT_ROW.captures(r.lines[i]) else {
        return false;
    };
    let code = c.name("code").map_or("", |m| m.as_str());
    let key = format!("{}|{file}|{code}|{}", r.kind, key_message(&c["msg"]));
    r.push(key);
    true
}

/// A blank line ends an ESLint file's block.
fn blank_line(r: &mut Reading, i: usize) -> bool {
    if !r.lines[i].trim().is_empty() {
        return false;
    }
    r.eslint_file = None;
    true
}

/// The generic `path:line: msg` shape (gcc, clang, go vet, ruff, and
/// tracebacks and assertion lines, which only count without failing tests).
fn generic_location(r: &mut Reading, i: usize) -> bool {
    let line = r.lines[i];
    let Some(c) = GENERIC.captures(line) else {
        return false;
    };
    let file = key_path(&c["file"]);
    // Stack frames (indented) and library paths aren't the errors;
    // a line that starts with its path is (go vet, gcc).
    let indented = line.starts_with(char::is_whitespace);
    if file.contains("node_modules/")
        || indented && !FAILURE_LINE.is_match(line) && c.name("code").is_none()
    {
        return true;
    }
    if BARE_EXCEPTION.is_match(c["msg"].trim()) {
        return true;
    }
    // A location alone, `src/a.ts:11:5`, read as the line and the message
    // `5`: a code frame's heading, not an error of its own.
    if c["msg"].trim().chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    // Round 3: a compiler's note, warning or help line points at
    // code that's fine (`src/b.h:3:5: note: declared here`).
    let msg_lower = c["msg"].trim().to_ascii_lowercase();
    if ["note:", "warning:", "help:"]
        .iter()
        .any(|p| msg_lower.starts_with(p))
    {
        return true;
    }
    let code = c.name("code").map_or("", |m| m.as_str());
    let key = format!("{}|{file}|{code}|{}", r.kind, key_message(&c["msg"]));
    // Round 2, P3.1: a compiler's line (`./a.go:3:2: undefined: foo`:
    // its path first, with a column) is an error of its own, even
    // next to failing tests; tracebacks and assertion lines aren't.
    if c.name("col").is_some() && !indented {
        r.push(key);
    } else {
        r.generic.push(key);
    }
    true
}

/// How many tests a run ran, from its summary lines (cargo, vitest, jest,
/// pytest) or, for `go test -v`, its result lines; None when it doesn't
/// say.
pub fn tests_ran(output: &str) -> Option<u32> {
    static CARGO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed").unwrap());
    static JS_TOTAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*Tests:?\s+(.*?)(?:\((\d+)\)|(\d+) total)").unwrap());
    static JS_PARTS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+) (?:passed|failed)").unwrap());
    static PYTEST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^=+ (.*\b(?:passed|failed)\b.*) in [\d.]+s").unwrap());
    let clean = clean_output(output);
    let cargo: Vec<u32> = CARGO
        .captures_iter(&clean)
        .map(|c| c[1].parse::<u32>().unwrap_or(0) + c[2].parse::<u32>().unwrap_or(0))
        .collect();
    if !cargo.is_empty() {
        return Some(cargo.iter().sum());
    }
    if let Some(c) = JS_TOTAL.captures_iter(&clean).last() {
        if let Some(total) = c.get(2).or(c.get(3)) {
            return total.as_str().parse().ok();
        }
        let sum: u32 = JS_PARTS
            .captures_iter(&c[1])
            .filter_map(|p| p[1].parse::<u32>().ok())
            .sum();
        return (sum > 0).then_some(sum);
    }
    if let Some(c) = PYTEST.captures_iter(&clean).last() {
        let sum: u32 = JS_PARTS
            .captures_iter(&c[1])
            .filter_map(|p| p[1].parse::<u32>().ok())
            .sum();
        return Some(sum);
    }
    go_tests_ran(&clean)
}

/// V08: `go test -v` prints no total, but a line per test and subtest it
/// ran (`--- PASS: TestAdd (0.00s)`, `    --- FAIL: TestX/case (0.00s)`):
/// those count, skipped ones not. Only verbose output says what ran: without
/// `-v`, go prints just the failures, which aren't the count.
fn go_tests_ran(clean: &str) -> Option<u32> {
    static RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^=== RUN\s+\S").unwrap());
    static RESULT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*--- (?:PASS|FAIL): \S+ \(").unwrap());
    if !RUN.is_match(clean) {
        return None;
    }
    Some(RESULT.find_iter(clean).count() as u32)
}

/*
 * ---------------------------------------------------------------------------
 * Failures before the change (before.rs)
 * ---------------------------------------------------------------------------
 */

/// What failed in a check's output, the paths in it taken relative to
/// where the check ran, so that a run in a copy of the project elsewhere
/// compares: each failure's key ([`failure_keys`], not capped), and its
/// failure lines, for output whose only key is the check's own (nothing
/// told its failures apart).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Failed {
    pub keys: std::collections::BTreeSet<String>,
    pub lines: std::collections::BTreeSet<String>,
}

/// The failures in a failed check's `output`, run in one of `roots`.
pub fn failed_in(kind: &str, output: &str, roots: &[String]) -> Failed {
    let text = without_roots(output, roots);
    Failed {
        keys: all_failure_keys(kind, &text, false).into_iter().collect(),
        lines: failure_lines(&text),
    }
}

/// `now`'s failure keys split into those `before` doesn't have, the
/// change's, and those it has (both sorted). The check's key alone (no
/// failure told apart) is `before`'s when `before` failed that way too,
/// with every failure line of `now`.
pub fn split_by_before(kind: &str, now: &Failed, before: &Failed) -> (Vec<String>, Vec<String>) {
    let alone = format!("{kind}|failed");
    let (known, own): (Vec<String>, Vec<String>) = now.keys.iter().cloned().partition(|k| {
        before.keys.contains(k) && (*k != alone || now.lines.is_subset(&before.lines))
    });
    (own, known)
}

/// `text` with each of `roots` (folders, absolute) taken off the paths in
/// it: `/home/me/app/src/a.ts` is `src/a.ts` in a check run in
/// `/home/me/app`. The longest first, so one inside another goes whole.
pub fn without_roots(text: &str, roots: &[String]) -> String {
    let mut roots: Vec<String> = roots
        .iter()
        .map(|r| r.replace('\\', "/").trim_end_matches('/').to_string())
        .filter(|r| r.len() > 1)
        .collect();
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    roots.iter().fold(text.to_string(), |text, root| {
        text.replace(&format!("{root}/"), "")
    })
}

/// A duration in a line (`(12 ms)`, `2.5 s`, `Done in 1.79s`): it differs
/// every run.
static DURATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(?\b\d+(?:\.\d+)?\s?m?s\b\)?").unwrap());

/// The lines that say something failed, as they compare between runs:
/// without durations, spaces collapsed.
fn failure_lines(text: &str) -> std::collections::BTreeSet<String> {
    clean_output(text)
        .lines()
        .filter(|l| FAILURE_LINE.is_match(l) && !ALL_CLEAR.is_match(l))
        .map(|l| {
            DURATION
                .replace_all(l, "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/*
 * ---------------------------------------------------------------------------
 * Environment failures (V03, D12)
 * ---------------------------------------------------------------------------
 */

/// A shell that didn't find a program: `sh: 1: vitest: not found`,
/// `/bin/bash: line 1: jest: command not found`, `zsh: command not found:
/// tsc`, and cmd's `'vitest' is not recognized as an internal or external
/// command`.
static SHELL_NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:/[\w./-]*/)?(?:sh|bash|dash|zsh|ksh|ash)(?:\.exe)?:\s*(?:\d+:\s*|line \d+:\s*)?)?(?:command not found:\s*\S+|\S+: command not found|\S+: not found)\s*$|is not recognized as an internal or external command",
    )
    .unwrap()
});
/// A bare `X: not found` is a shell's only from one: with exit code 127,
/// or a shell's name first. A test's own `user: not found` isn't.
static SHELL_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:/[\w./-]*/)?(?:sh|bash|dash|zsh|ksh|ash)(?:\.exe)?:|command not found|is not recognized").unwrap()
});
/// make's line for a recipe whose program wasn't found.
static MAKE_127: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S*make(?:\[\d+\])?: \*\*\* .*Error 127\s*$").unwrap());
/// Node and TypeScript: a module or a package that isn't installed.
/// `Cannot find module './util'` is the code's: only a package's name
/// (`vitest`, `@scope/pkg`, `pkg/sub`) is the environment's.
static NODE_MISSING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Cannot find (?:module|package) ['"](?P<name>[^'"]+)['"]"#).unwrap()
});
/// Python: `ModuleNotFoundError: No module named 'requests'`, and
/// `python3: No module named pytest` (`python -m pytest` without it).
static PYTHON_MISSING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"No module named ['"]?[A-Za-z_][\w.]*['"]?"#).unwrap());
/// cargo run where there's no crate.
static NO_CARGO_TOML: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"could not find `Cargo\.toml`").unwrap());

/// Longest line kept to say what failed.
const ENVIRONMENT_LINE_CHARS: usize = 200;

/// Whether a module a failure names is a package (`vitest`, `@scope/x`,
/// `lodash/fp`), not a path (`./util`, `/abs`, `C:\x`) or Node's own
/// (`node:fs`).
fn is_package_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !name.starts_with('.')
        && !name.starts_with('/')
        && !name.starts_with('\\')
        && !name.starts_with("node:")
        && name.as_bytes().get(1) != Some(&b':')
}

/// Whether a failed check's output (with its exit code) says its
/// environment failed, not the code: a program that isn't installed (exit
/// code 127, "command not found"), a package that isn't (Node's "Cannot
/// find module" of a package, Python's "No module named"), no
/// `Cargo.toml` where it ran. The line that says so, or for exit code 127
/// with none, what that code means. None: the code's failure, as far as
/// the output tells.
pub fn environment_failure(exit_code: Option<i32>, output: &str) -> Option<String> {
    let clean = clean_output(output);
    let line_of = |line: &str| {
        crate::chat_db::store::char_prefix(line.trim(), ENVIRONMENT_LINE_CHARS).to_string()
    };
    for line in clean.lines() {
        let trimmed = line.trim();
        if SHELL_NOT_FOUND.is_match(trimmed)
            && (exit_code == Some(127) || SHELL_PREFIX.is_match(trimmed))
        {
            return Some(line_of(trimmed));
        }
        if MAKE_127.is_match(trimmed) || NO_CARGO_TOML.is_match(trimmed) {
            return Some(line_of(trimmed));
        }
        if let Some(c) = NODE_MISSING.captures(trimmed) {
            if is_package_name(&c["name"]) {
                return Some(line_of(trimmed));
            }
        }
        if PYTHON_MISSING.is_match(trimmed) {
            return Some(line_of(trimmed));
        }
    }
    (exit_code == Some(127)).then(|| "exit code 127: a program it runs wasn't found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_to_start_and_end() {
        let text: String = (1..=500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_output(&text, 2, 3, 100_000);
        assert_eq!(
            out,
            "line 1\nline 2\n[… 495 lines omitted …]\nline 498\nline 499\nline 500"
        );
        let out = cap_output(&text, 200, 200, 50);
        assert!(
            out.starts_with("[… output shortened to its last") && out.ends_with("line 500"),
            "{out}"
        );
    }

    #[test]
    fn keeps_error_blocks_and_summary() {
        let mut lines: Vec<String> = (0..300)
            .map(|i| format!("test passing_{i} ... ok"))
            .collect();
        lines[100] = "error[E0308]: mismatched types".into();
        lines[101] = "  --> src/main.rs:3:5".into();
        lines.push("test result: FAILED. 299 passed; 1 failed".into());
        let out = focus_failures(&lines.join("\n"), 8_000);
        assert!(out.contains("error[E0308]") && out.contains("src/main.rs:3:5"));
        assert!(out.contains("test result: FAILED"));
        assert!(!out.contains("passing_50 "));
        assert!(out.contains("lines …]"));
    }

    #[test]
    fn finds_error_locations() {
        let text = "src/a.ts(12,5): error TS1\n  --> src/main.rs:3:1\nFile \"app/x.py\", line 9\nsee https://x.io/a.js:1\n./src/a.ts:4:2";
        assert_eq!(
            error_locations(text),
            vec!["src/a.ts", "src/main.rs", "app/x.py"]
        );
        let (changed, other) = split_by_changed(
            &["src/main.rs".into(), "lib/b.rs".into()],
            &["src-tauri/src/main.rs".into()],
        );
        assert_eq!(changed, vec!["src/main.rs"]);
        assert_eq!(other, vec!["lib/b.rs"]);
    }

    #[test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\x1b[31merror\x1b[0m"), "error");
    }

    #[test]
    fn output_edge_cases() {
        assert_eq!(cap_output("", 2, 2, 100), "");
        // One huge line over the limit: its end is kept, never nothing.
        let long = format!("{}end", "x".repeat(10_000));
        let out = cap_output(&long, 2, 2, 100);
        assert!(
            out.starts_with("[… output shortened to the last 100 characters of its last line …]\n"),
            "{out}"
        );
        assert!(out.ends_with("xxend") && out.chars().count() < 200, "{out}");
        // Short lines before a huge last line: still its end.
        assert!(cap_output(&format!("a\nb\n{long}"), 2, 2, 100).ends_with("xxend"));
        // Multi-byte characters are cut whole.
        assert!(cap_output(&"é".repeat(500), 2, 2, 100).ends_with(&"é".repeat(100)));
        // Progress bars that redraw with "\r" become lines.
        assert_eq!(
            clean_output("10%\r50%\r100%\r\ndone"),
            "10%\n50%\n100%\ndone"
        );
        // Short failing output is kept whole, CRLF normalized, colors gone.
        assert_eq!(
            focus_failures("\x1b[31merror\x1b[0m: x\r\nnext\r\n", 8_000),
            "error: x\nnext"
        );
        // Long output with no error words: start and end.
        let plain: String = (0..300).map(|i| format!("line {i}\n")).collect();
        assert!(focus_failures(&plain, 8_000).contains("lines omitted"));
        // "0 failed" is a summary, not an error.
        let mut lines: Vec<String> = (0..200).map(|i| format!("ok {i}")).collect();
        lines.insert(10, "test result: ok. 3 passed; 0 failed".into());
        assert!(!focus_failures(&lines.join("\n"), 8_000).contains("[… 9 lines …]"));
        // Libraries and versions aren't source the agent wrote.
        assert!(error_locations(
            "node_modules/a/b.js:3\n  at ./node_modules/x/y.ts:1:2\nv 1.2.3:4"
        )
        .is_empty());
        assert_eq!(
            error_locations("src/a.ts:1:1\nsrc/a.ts:9:9"),
            ["src/a.ts"],
            "each once"
        );
    }

    /// Round 2, P3.1: a build error in one Go package next to a failing
    /// test in another is kept; once it compiles, its own failing tests
    /// aren't a regression of the round before.
    #[test]
    fn go_build_error_next_to_another_packages_test_failure_is_kept() {
        let before = "# example.com/a\n./a.go:3:2: undefined: foo\nFAIL\texample.com/a [build failed]\n\
            --- FAIL: TestB (0.00s)\n    b_test.go:9: got 1, want 2\nFAIL\nFAIL\texample.com/b\t0.002s\n";
        let keys = failure_keys("test", before, false);
        assert!(
            keys.contains(&"test|a.go||undefined: foo".to_string()),
            "{keys:?}"
        );
        assert!(keys.contains(&"test|test|TestB".to_string()), "{keys:?}");
        let after = "--- FAIL: TestA (0.00s)\n    a_test.go:4: wrong\nFAIL\nFAIL\texample.com/a\t0.001s\nok\texample.com/b\n";
        let round = |out: &str| super::super::repair::RoundFailures {
            keys: failure_keys("test", out, false).into_iter().collect(),
            ran: ["test".to_string()].into(),
            tests_ran: None,
            own_tests: None,
        };
        let j = super::super::repair::judge(&[round(before)], &round(after));
        assert_ne!(
            j.verdict,
            super::super::repair::Verdict::Regression,
            "{j:?}"
        );
    }

    /// Round 2 (§4.2): a pytest long traceback is one failing test, not
    /// its frames or its assertion line.
    #[test]
    fn pytest_long_traceback_frames_arent_keys() {
        let out = "_____ test_add _____\n\ndef test_add():\n>       assert add(2, 2) == 4\n\
            src/math.py:10: in add\n    return a - b\n\
            tests/test_math.py:5: in test_add\n    assert add(2, 2) == 4\n\
            E   AssertionError: expected 4\n\
            tests/test_math.py:5: AssertionError: expected 4\n\
            FAILED tests/test_math.py::test_add - AssertionError: expected 4\n";
        assert_eq!(
            failure_keys("test", out, false),
            ["test|test|tests/test_math.py::test_add"]
        );
    }

    /// Round 3: a compiler's note, warning and help lines next to an error
    /// or a failing test aren't failures (gcc, mypy).
    #[test]
    fn notes_and_warnings_arent_keys() {
        let out = "src/a.c:12:5: error: expected ';'\nsrc/b.h:3:5: note: declared here\n\
                   src/c.c:1:1: warning: unused variable 'x' [-Wunused-variable]\n--- FAIL: TestA (0.00s)\n";
        assert_eq!(
            failure_keys("test", out, false),
            ["test|src/a.c||expected ';'", "test|test|TestA"]
        );
        let mypy = "src/a.py:12: error: Argument 1 has incompatible type\nsrc/b.py:3: note: \"foo\" defined here\n";
        assert_eq!(
            failure_keys("typecheck", mypy, false),
            ["typecheck|src/a.py||Argument 1 has incompatible type"]
        );
    }

    /// Round 3: a timed-out check keeps its `timed out` key next to what
    /// it printed before the kill, so the round stays incomparable.
    #[test]
    fn a_timeout_with_partial_output_is_still_blind() {
        let keys = failure_keys("test", "--- FAIL: TestA (0.00s)\n", true);
        assert_eq!(keys, ["test|test|TestA", "test|timed out"]);
        let round = |keys: &[&str]| super::super::repair::RoundFailures {
            keys: keys.iter().map(|k| k.to_string()).collect(),
            ran: ["test".to_string()].into(),
            tests_ran: None,
            own_tests: None,
        };
        let j = super::super::repair::judge(
            &[round(&["test|test|TestA", "test|test|TestB"])],
            &round(&["test|test|TestA", "test|timed out"]),
        );
        assert!(j.fixed.is_empty(), "{j:?}");
        assert_eq!(j.verdict, super::super::repair::Verdict::First);
    }

    /// Round 2 (§4.2): a pytest collection error is an error key; the
    /// tests failing once it's fixed aren't "new" next to it.
    #[test]
    fn pytest_collection_error_is_an_error_key() {
        let before = failure_keys(
            "test",
            "ERROR tests/test_a.py - ImportError: no module named x\n",
            false,
        );
        assert_eq!(
            before,
            ["test|tests/test_a.py||ImportError: no module named x"]
        );
        let after = failure_keys(
            "test",
            "FAILED tests/test_a.py::test_one - assert 1 == 2\n",
            false,
        );
        let round = |keys: Vec<String>| super::super::repair::RoundFailures {
            keys: keys.into_iter().collect(),
            ran: ["test".to_string()].into(),
            tests_ran: None,
            own_tests: None,
        };
        let j = super::super::repair::judge(&[round(before)], &round(after));
        assert_ne!(
            j.verdict,
            super::super::repair::Verdict::Regression,
            "{j:?}"
        );
    }

    /// Round 2 (§4.2): a crate's name is added only to cargo's own test
    /// lines (after its Running line), on any platform's path.
    #[test]
    fn crate_prefix_only_on_cargo_test_lines() {
        let go = "     Running unittests src/lib.rs (target/debug/deps/alpha-1a2b)\n--- FAIL: TestX (0.00s)\n";
        assert_eq!(failure_keys("test", go, false), ["test|test|TestX"]);
        let it = "     Running tests/it.rs (target/debug/deps/it-0a1b)\ntest adds ... FAILED\n";
        assert_eq!(failure_keys("test", it, false), ["test|test|it::adds"]);
        let win = "     Running unittests src\\lib.rs (target\\debug\\deps\\alpha-1a2b.exe)\ntest t ... FAILED\n";
        assert_eq!(failure_keys("test", win, false), ["test|test|alpha::t"]);
    }

    /// Round 2 (§4.2): with more errors than keys kept, one fewer is
    /// progress and one more isn't a regression of passing code.
    #[test]
    fn eslint_sixty_errors_round_pair() {
        use super::super::repair::{judge, RoundFailures, Verdict};
        let lint = |n: usize| {
            let mut out = String::from("/p/src/a.ts\n");
            for i in 0..n {
                out.push_str(&format!(
                    "  {}:1  error  rule {i} broken  rule-{i}\n",
                    i + 1
                ));
            }
            RoundFailures {
                keys: failure_keys("lint", &out, false).into_iter().collect(),
                ran: ["lint".to_string()].into(),
                tests_ran: None,
                own_tests: None,
            }
        };
        let j = judge(&[lint(60)], &lint(59));
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
        assert!(j.broke.is_empty());
        assert_ne!(judge(&[lint(60)], &lint(61)).verdict, Verdict::Regression);
    }

    /// Round 2 (§4.2): cargo's compile error and a failing test from the
    /// same run are both kept.
    #[test]
    fn cargo_compile_error_and_test_failure_keep_both() {
        let out = "error[E0425]: cannot find value `x` in this scope\n --> src/lib.rs:3:5\n\
            test tests::adds ... FAILED\n";
        let keys = failure_keys("test", out, false);
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(
            keys.iter().any(|k| k.starts_with("test|src/lib.rs|E0425|")),
            "{keys:?}"
        );
        assert!(keys.contains(&"test|test|tests::adds".to_string()));
    }

    /// Round 2, P3.1: cargo's closing summaries aren't errors of their own.
    #[test]
    fn cargo_summaries_arent_keys() {
        let out = "test tests::it ... FAILED\n\nerror: doctest failed, to rerun pass `--doc`\n\
            error: process didn't exit successfully: `target/debug/deps/a-1a2b` (exit status: 101)\n\
            error: 1 target failed:\n    `-p a --lib`\n";
        assert_eq!(failure_keys("test", out, false), ["test|test|tests::it"]);
    }

    #[test]
    fn failure_keys_survive_moved_lines() {
        let before = "src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\nFound 1 error.";
        let after = "src/a.ts(40,9): error TS2322: Type 'string' is not assignable to type 'number'.\nFound 1 error.";
        assert_eq!(
            failure_keys("typecheck", before, false),
            failure_keys("typecheck", after, false)
        );
        assert_eq!(
            failure_keys("typecheck", before, false),
            ["typecheck|src/a.ts|TS2322|Type 'string' is not assignable to type 'number'."]
        );
        let pretty = "src/b.ts:3:1 - error TS2304: Cannot find name 'x'.";
        assert_eq!(
            failure_keys("typecheck", pretty, false),
            ["typecheck|src/b.ts|TS2304|Cannot find name 'x'."]
        );
    }

    #[test]
    fn failure_keys_for_rust_and_other_tools() {
        let cargo = "error[E0308]: mismatched types\n  --> src/main.rs:3:5\n   |\n3  |     x\n\nerror: could not compile `app` due to 1 previous error";
        assert_eq!(
            failure_keys("build", cargo, false),
            ["build|src/main.rs|E0308|mismatched types"]
        );
        let moved = cargo.replace("main.rs:3:5", "main.rs:30:1");
        assert_eq!(
            failure_keys("build", &moved, false),
            failure_keys("build", cargo, false)
        );

        let eslint = "/p/src/a.ts\n  3:7  error  'x' is assigned a value but never used  no-unused-vars\n  9:1  warning  Unexpected console  no-console\n\n✖ 1 problem";
        assert_eq!(
            failure_keys("lint", eslint, false),
            ["lint|/p/src/a.ts|no-unused-vars|'x' is assigned a value but never used"]
        );
        let ruff = "app/x.py:1:8: F401 [*] `os` imported but unused\nFound 1 error.";
        assert_eq!(
            failure_keys("lint", ruff, false),
            ["lint|app/x.py|F401|[*] `os` imported but unused"]
        );
        let go = "./main.go:7:2: undefined: foo";
        assert_eq!(
            failure_keys("build", go, false),
            ["build|main.go||undefined: foo"]
        );
        // A stack frame isn't an error of its own.
        assert_eq!(
            failure_keys("test", "    at src/a.ts:3:4\nError: boom", false),
            ["test|failed"]
        );
    }

    #[test]
    fn failing_tests_are_keyed_by_name() {
        let cargo = "running 3 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nfailures:\n\n---- a::bad stdout ----\nthread panicked at src/a.rs:9:5\n\ntest result: FAILED. 2 passed; 1 failed";
        let keys = failure_keys("test", cargo, false);
        assert!(keys.contains(&"test|test|a::bad".to_string()), "{keys:?}");
        assert_eq!(tests_ran(cargo), Some(3));

        let pytest = "FAILED tests/test_x.py::test_add - assert 1 == 2\n===== 1 failed, 4 passed in 0.12s =====";
        assert_eq!(
            failure_keys("test", pytest, false),
            ["test|test|tests/test_x.py::test_add"]
        );
        assert_eq!(tests_ran(pytest), Some(5));

        let vitest = " FAIL  src/a.test.ts > math > adds 3ms\n × math > adds\n Test Files  1 failed (1)\n      Tests  1 failed | 6 passed (7)";
        let keys = failure_keys("test", vitest, false);
        assert!(
            keys.contains(&"test|test|math > adds".to_string()),
            "{keys:?}"
        );
        assert_eq!(tests_ran(vitest), Some(7));
        assert_eq!(
            tests_ran("Tests:       1 failed, 2 passed, 3 total"),
            Some(3)
        );
        assert_eq!(tests_ran("nothing to see"), None);
    }

    /// Jest, as a React Native app's tests print it: a test file that
    /// couldn't run (an unmocked native module), a failing test outside any
    /// `describe`, one inside; console output and passing files aren't
    /// failures.
    #[test]
    fn jest_files_that_cant_run_and_top_level_tests_are_keyed() {
        let jest = "FAIL __tests__/Fake.test.tsx\n  ● Test suite failed to run\n\n    Jest encountered an unexpected token\n\n    Details:\n\n    /home/me/app/node_modules/react-native-fake/index.js:1\n    SyntaxError: Cannot use import statement outside a module\n\n      at Runtime.createScriptFromCode (node_modules/jest-runtime/build/index.js:1505:14)\n\nFAIL __tests__/Math.test.ts (5.21 s)\n  ● adds\n\n    expect(received).toBe(expected) // Object.is equality\n\n      at Object.toBe (__tests__/Math.test.ts:1:36)\n\n  ● Console\n\n    console.log\n      hi\n\n  ● math › subtracts\n\n    expect(received).toBe(expected)\n\nPASS __tests__/App.test.tsx\n  ● Console\n\nTest Suites: 2 failed, 1 passed, 3 total\nTests:       2 failed, 1 passed, 3 total";
        assert_eq!(
            failure_keys("test", jest, false),
            [
                "test|__tests__/Fake.test.tsx||Test suite failed to run: Jest encountered an unexpected token",
                "test|test|adds",
                "test|test|math › subtracts",
            ]
        );
        assert_eq!(tests_ran(jest), Some(3));
        // A single test file prints its tests with their durations, which
        // differ every run: not part of the name.
        let verbose = "FAIL __tests__/App.test.tsx\n  ✕ renders correctly (370 ms)\n  ✓ adds (2 ms)\n\n  ● renders correctly\n";
        let again = verbose.replace("370 ms", "1204 ms");
        assert_eq!(
            failure_keys("test", verbose, false),
            ["test|test|renders correctly"]
        );
        assert_eq!(
            failure_keys("test", verbose, false),
            failure_keys("test", &again, false)
        );
    }

    /// Before the change and after it, in different folders: the same
    /// failures compare equal, paths relative to where each ran; a new one
    /// is the change's.
    #[test]
    fn failures_compare_with_the_code_before_the_change() {
        let before = failed_in(
            "lint",
            "/tmp/red-1/src/a.ts\n  2:7  error  'x' is never used  no-unused-vars\n",
            &["/tmp/red-1".into(), "/home/me/app".into()],
        );
        let now = failed_in(
            "lint",
            "/tmp/chat/src/a.ts\n  3:7  error  'x' is never used  no-unused-vars\n\n/tmp/chat/src/b.ts\n  1:1  error  'y' is never used  no-unused-vars\n",
            &["/tmp/chat".into(), "/home/me/app".into()],
        );
        let (own, known) = split_by_before("lint", &now, &before);
        assert_eq!(own, ["lint|src/b.ts|no-unused-vars|'y' is never used"]);
        assert_eq!(known, ["lint|src/a.ts|no-unused-vars|'x' is never used"]);
        // The longest root first: one inside another goes whole.
        assert_eq!(
            without_roots(
                "/a/b/c/x.ts /a/b/y.ts /a/bc/z.ts",
                &["/a/b".into(), "/a/b/c/".into()]
            ),
            "x.ts y.ts /a/bc/z.ts"
        );
        assert_eq!(
            without_roots("/x.ts", &["/".into(), String::new()]),
            "/x.ts"
        );
    }

    /// ESLint's React Compiler rules print a code frame under their error,
    /// headed by its location alone: no error of its own.
    #[test]
    fn a_location_alone_is_no_error() {
        let eslint = "/p/src/hooks/a.web.ts\n  11:5  error  Error: Calling setState synchronously within an effect\n\nEffects are intended to synchronize state.\n\n/p/src/hooks/a.web.ts:11:5\n   9 |\n> 11 |     setHasHydrated(true);\n\n✖ 1 problem (1 error, 0 warnings)";
        assert_eq!(
            failure_keys("lint", eslint, false),
            ["lint|/p/src/hooks/a.web.ts||Error: Calling setState synchronously within an effect"]
        );
        // With a message, it's one (gcc, go vet).
        assert_eq!(
            failure_keys("lint", "./a.go:3:2: undefined: foo", false),
            ["lint|a.go||undefined: foo"]
        );
    }

    /// V08: `go test -v` counts its tests and subtests that ran; a skipped
    /// one doesn't count, and output without `-v` says no count.
    #[test]
    fn go_test_verbose_output_says_how_many_ran() {
        let verbose = "=== RUN   TestAdd\n--- PASS: TestAdd (0.00s)\n=== RUN   TestTable\n=== RUN   TestTable/zero\n\
            === RUN   TestTable/one\n    table_test.go:12: got 2, want 1\n--- FAIL: TestTable (0.00s)\n\
            \x20   --- PASS: TestTable/zero (0.00s)\n    --- FAIL: TestTable/one (0.00s)\n\
            === RUN   TestSlow\n    slow_test.go:5: skipping in short mode\n--- SKIP: TestSlow (0.00s)\n\
            FAIL\nFAIL\texample.com/m\t0.004s\n=== RUN   TestOther\n--- PASS: TestOther (0.00s)\nPASS\nok  \texample.com/m/other\t0.002s\n";
        assert_eq!(tests_ran(verbose), Some(5));
        // A package with no tests.
        assert_eq!(
            tests_ran("=== RUN   TestNothing\n--- SKIP: TestNothing (0.00s)\nok  \tm\t0.001s"),
            Some(0)
        );
        // Without -v: the failures alone, no count.
        let quiet = "--- FAIL: TestTable (0.00s)\n    --- FAIL: TestTable/one (0.00s)\nFAIL\nFAIL\tm\t0.004s";
        assert_eq!(tests_ran(quiet), None);
        assert_eq!(tests_ran("ok  \texample.com/m\t0.002s"), None);
        // Garbage, and a line that only looks like one.
        assert_eq!(tests_ran("=== RUNNING nothing\n--- PASS: \n"), None);
        assert_eq!(tests_ran("\u{0}\u{1} --- PASS"), None);
    }

    #[test]
    fn a_failure_with_nothing_recognizable_is_the_check_alone() {
        assert_eq!(
            failure_keys("build", "Segmentation fault", false),
            ["build|failed"]
        );
        assert_eq!(failure_keys("test", "", true), ["test|timed out"]);
        let many: String = (0..80).map(|i| format!("test t{i} ... FAILED\n")).collect();
        let keys = failure_keys("test", &many, false);
        assert_eq!(keys.len(), MAX_KEYS);
        assert!(keys.last().unwrap().ends_with("and 31 more"));
    }

    /// V03 (D12): what a failed check says when its environment failed,
    /// from each tool's own words.
    #[test]
    fn an_environment_failure_is_told_from_the_codes() {
        let cases: &[(Option<i32>, &str, &str)] = &[
            (Some(127), "> t@1 test\n> vitest\n\nsh: 1: vitest: not found\n", "sh: 1: vitest: not found"),
            (Some(127), "/bin/bash: line 1: jest: command not found", "/bin/bash: line 1: jest: command not found"),
            (Some(1), "zsh: command not found: tsc", "zsh: command not found: tsc"),
            (Some(1), "bash: pytest: command not found", "bash: pytest: command not found"),
            (Some(1), "'vitest' is not recognized as an internal or external command,", "'vitest' is not recognized as an internal or external command,"),
            (Some(2), "make: vitest: No such file or directory\nmake: *** [Makefile:2: test] Error 127", "make: *** [Makefile:2: test] Error 127"),
            (Some(1), "Error: Cannot find module 'vitest/config'\nRequire stack:\n- /p/vite.config.js", "Error: Cannot find module 'vitest/config'"),
            (Some(1), "Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@acme/ui' imported from /p/src/a.js", "Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@acme/ui' imported from /p/src/a.js"),
            (Some(2), "src/a.ts(1,20): error TS2307: Cannot find module 'react' or its corresponding type declarations.", "src/a.ts(1,20): error TS2307: Cannot find module 'react' or its corresponding type declarations."),
            (Some(2), "E   ModuleNotFoundError: No module named 'requests'", "E   ModuleNotFoundError: No module named 'requests'"),
            (Some(1), "/usr/bin/python3: No module named pytest", "/usr/bin/python3: No module named pytest"),
            (Some(101), "error: could not find `Cargo.toml` in `/p` or any parent directory", "error: could not find `Cargo.toml` in `/p` or any parent directory"),
        ];
        for (code, output, line) in cases {
            assert_eq!(
                environment_failure(*code, output).as_deref(),
                Some(*line),
                "{output}"
            );
        }
        // Color codes and CRLF don't hide it.
        assert_eq!(
            environment_failure(Some(127), "\x1b[31msh: 1: vitest: not found\x1b[0m\r\n")
                .as_deref(),
            Some("sh: 1: vitest: not found")
        );
        // Exit code 127 with nothing to say what wasn't found.
        assert_eq!(
            environment_failure(Some(127), "").as_deref(),
            Some("exit code 127: a program it runs wasn't found")
        );
    }

    /// V03: the code's own failures aren't the environment's: a relative
    /// import, Node's own modules, a test that prints "not found", and
    /// output that says nothing of the kind.
    #[test]
    fn the_codes_failures_arent_the_environments() {
        let cases: &[(Option<i32>, &str)] = &[
            (
                Some(1),
                "Error: Cannot find module './util'\nRequire stack:",
            ),
            (Some(1), "Error: Cannot find module '../lib/x.js'"),
            (Some(1), "Error: Cannot find module '/abs/x'"),
            (Some(1), "Error: Cannot find module 'node:fs'"),
            (
                Some(1),
                "expected 'user: not found' to equal 'ok'\nuser: not found",
            ),
            (
                Some(1),
                "FAIL src/a.test.ts > finds users\nAssertionError: expected 1 to be 2",
            ),
            (
                Some(2),
                "src/a.ts(3,5): error TS2322: Type 'string' is not assignable to type 'number'.",
            ),
            (Some(1), ""),
            (None, "killed"),
            (
                Some(1),
                "\u{0}\u{1}\u{2} garbage \u{fffd}\u{fffd} Cannot find",
            ),
            (Some(1), "Cannot find module"),
        ];
        for (code, output) in cases {
            assert_eq!(environment_failure(*code, output), None, "{output:?}");
        }
    }
}

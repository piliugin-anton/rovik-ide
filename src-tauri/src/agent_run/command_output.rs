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
/// Failing tests: cargo, go, pytest, jest/vitest.
static TEST_FAILS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^test (?P<name>\S+) \.\.\. FAILED$",
        r"^\s*--- FAIL: (?P<name>\S+)",
        r"^(?:FAILED|ERROR) (?P<name>\S+::\S+)",
        r"^\s*(?:✕|×|✗)\s+(?P<name>.+?)(?:\s+\d+\s*m?s)?$",
        r"^\s*FAIL\s+(?P<name>\S+\s+>\s+.+?)(?:\s+\d+\s*m?s)?$",
        r"^\s*●\s+(?P<name>.+›.+)$",
    ]
    .iter()
    .map(|r| Regex::new(r).unwrap())
    .collect()
});
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
    let clean = clean_output(output);
    let lines: Vec<&str> = clean.lines().collect();
    let mut keys: Vec<String> = Vec::new();
    let push = |k: String, keys: &mut Vec<String>| {
        if !keys.contains(&k) {
            keys.push(k);
        }
    };
    let mut eslint_file: Option<String> = None;
    // Keys from the generic `path:line: msg` shape: tracebacks, assertion
    // lines. Next to failing tests they're the same failures again (P3.1).
    let mut generic: Vec<String> = Vec::new();
    let mut tests = false;
    let mut krate: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = CARGO_RUNNING.captures(line) {
            krate = Some(c["krate"].to_string());
            continue;
        }
        if let Some(c) = TSC.captures(line) {
            push(
                format!(
                    "{kind}|{}|{}|{}",
                    key_path(&c["file"]),
                    &c["code"],
                    key_message(&c["msg"])
                ),
                &mut keys,
            );
            continue;
        }
        if let Some(c) = RUST_ERROR.captures(line) {
            if RUST_SUMMARY.is_match(&c["msg"]) {
                continue;
            }
            let file = lines[i + 1..lines.len().min(i + 6)]
                .iter()
                .find_map(|l| RUST_ARROW.captures(l).map(|a| key_path(&a["file"])))
                .unwrap_or_default();
            let code = c.name("code").map_or("", |m| m.as_str());
            push(
                format!("{kind}|{file}|{code}|{}", key_message(&c["msg"])),
                &mut keys,
            );
            continue;
        }
        if let Some(c) = TEST_FAILS.iter().find_map(|r| r.captures(line)) {
            let name = c["name"].trim();
            // In a cargo workspace, the crate too: `alpha::tests::it`.
            let name = match &krate {
                Some(k) if line.starts_with("test ") => format!("{k}::{name}"),
                _ => name.to_string(),
            };
            push(format!("{kind}|test|{name}"), &mut keys);
            tests = true;
            continue;
        }
        if let Some(c) = ESLINT_FILE.captures(line) {
            eslint_file = Some(key_path(&c["file"]));
            continue;
        }
        if let (Some(file), Some(c)) = (&eslint_file, ESLINT_ROW.captures(line)) {
            let code = c.name("code").map_or("", |m| m.as_str());
            push(
                format!("{kind}|{file}|{code}|{}", key_message(&c["msg"])),
                &mut keys,
            );
            continue;
        }
        if line.trim().is_empty() {
            eslint_file = None;
        }
        if let Some(c) = GENERIC.captures(line) {
            let file = key_path(&c["file"]);
            // Stack frames (indented) and library paths aren't the errors;
            // a line that starts with its path is (go vet, gcc).
            let indented = line.starts_with(char::is_whitespace);
            if file.contains("node_modules/")
                || indented && !FAILURE_LINE.is_match(line) && c.name("code").is_none()
            {
                continue;
            }
            if BARE_EXCEPTION.is_match(c["msg"].trim()) {
                continue;
            }
            let code = c.name("code").map_or("", |m| m.as_str());
            let key = format!("{kind}|{file}|{code}|{}", key_message(&c["msg"]));
            // Round 2, P3.1: a compiler's line (`./a.go:3:2: undefined: foo`:
            // its path first, with a column) is an error of its own, even
            // next to failing tests; tracebacks and assertion lines aren't.
            if c.name("col").is_some() && !indented {
                push(key, &mut keys);
            } else if !generic.contains(&key) {
                generic.push(key);
            }
        }
    }
    if !tests {
        for k in generic {
            push(k, &mut keys);
        }
    }
    if keys.is_empty() {
        keys.push(format!(
            "{kind}|{}",
            if timed_out { "timed out" } else { "failed" }
        ));
    }
    keys.sort();
    if keys.len() > MAX_KEYS {
        let more = keys.len() - (MAX_KEYS - 1);
        keys.truncate(MAX_KEYS - 1);
        keys.push(format!("{kind}|and {more} more"));
    }
    keys
}

/// How many tests a run ran, from its summary lines (cargo, vitest, jest,
/// pytest); None when it doesn't say.
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
    None
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
        };
        let j = super::super::repair::judge(&[round(before)], &round(after));
        assert_ne!(
            j.verdict,
            super::super::repair::Verdict::Regression,
            "{j:?}"
        );
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
}

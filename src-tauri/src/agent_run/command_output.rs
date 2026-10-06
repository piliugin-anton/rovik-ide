//! Shapes command output for the model: what it needs to act on, in as few
//! tokens as possible. Used for run_command results and for the
//! verification gate's failure reports.
//!
//!   - `cap_output`: long output keeps its start and end (errors and
//!     summaries come last), whole lines only.
//!   - `focus_failures`: a failing build or test run keeps its error blocks
//!     and final summary, not thousands of lines of passing tests.
//!   - `error_locations`: the files the errors point at, so a report can say
//!     whether they're files the agent changed.
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

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The start and the end of long output, whole lines. Errors and summaries
/// are usually at the end; the start shows what ran.
pub fn cap_output(text: &str, head_lines: usize, tail_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let out = if lines.len() > head_lines + tail_lines {
        let omitted = format!("[… {} lines omitted …]", lines.len() - head_lines - tail_lines);
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
    kept.reverse();
    format!("[… output shortened to its last {} lines …]\n{}", kept.len(), kept.join("\n"))
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
static ALL_CLEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b0 (failed|failures|errors?)\b|\bno (errors|failures)\b").unwrap());

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
    let clean = strip_ansi(text).replace("\r\n", "\n").replace('\r', "\n");
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
        let Some(start) = gap_start.take() else { return };
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
static NOT_SOURCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(https?|node|file):|^\d+(\.\d+)+$|/node_modules/|\.min\.js$").unwrap());

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
pub fn split_by_changed(locations: &[String], changed_files: &[String]) -> (Vec<String>, Vec<String>) {
    let norm = |p: &str| {
        let p = p.replace('\\', "/");
        let p = p.strip_prefix("./").unwrap_or(&p);
        p.trim_start_matches('/').to_string()
    };
    let changed: Vec<String> = changed_files.iter().map(|c| norm(c)).collect();
    let is_changed = |loc: &str| {
        let l = norm(loc);
        changed.iter().any(|c| *c == l || c.ends_with(&format!("/{l}")) || l.ends_with(&format!("/{c}")))
    };
    locations.iter().cloned().partition(|loc| is_changed(loc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_to_start_and_end() {
        let text: String = (1..=500).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let out = cap_output(&text, 2, 3, 100_000);
        assert_eq!(out, "line 1\nline 2\n[… 495 lines omitted …]\nline 498\nline 499\nline 500");
        let out = cap_output(&text, 200, 200, 50);
        assert!(out.starts_with("[… output shortened to its last") && out.ends_with("line 500"), "{out}");
    }

    #[test]
    fn keeps_error_blocks_and_summary() {
        let mut lines: Vec<String> = (0..300).map(|i| format!("test passing_{i} ... ok")).collect();
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
        assert_eq!(error_locations(text), vec!["src/a.ts", "src/main.rs", "app/x.py"]);
        let (changed, other) =
            split_by_changed(&["src/main.rs".into(), "lib/b.rs".into()], &["src-tauri/src/main.rs".into()]);
        assert_eq!(changed, vec!["src/main.rs"]);
        assert_eq!(other, vec!["lib/b.rs"]);
    }

    #[test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\x1b[31merror\x1b[0m"), "error");
    }
}

//! Making the verification gate's fix rounds converge (docs/design/
//! subagents-plan.md, section 3: R01, R02, R05). They help any run, with or
//! without subagents:
//!
//!   - R01, progress: each round's failures are keys (command_output.rs
//!     `failure_keys`), the same when only line numbers moved. A round made
//!     progress when it fixed some and broke none; it made none when the
//!     same failures came back (or the ones of two rounds ago), and the
//!     agent is told so instead of being sent the same report again.
//!   - R02, rollback: before each fix round, a checkpoint of the files the
//!     run changed. A round that makes a check fail that didn't fail before
//!     (a regression) is rolled back to it, and the agent is told what its
//!     attempt broke. A file someone else changed meanwhile is never
//!     overwritten: it's left as it is and listed (K15).
//!   - R05, test integrity: the test files and the check setup are compared
//!     with what they were when the run first touched them. Removed tests,
//!     new skip or `only` markers, fewer assertions, fewer tests run, and
//!     changes to what the checks run are flagged: the agent is asked to
//!     undo them or say why, and a run that keeps them isn't reported as
//!     verified (it waits for the user).
//!
//! What can't be seen: in a chat that isn't isolated, files a command
//! changed (not the write tool) have no checkpoint or baseline here, so
//! they're neither rolled back nor compared.

use super::run::Run;
use super::subagent::{Original, Source};
use super::tools;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

/// A checkpoint isn't taken of more than this (rollback then isn't offered).
const MAX_CHECKPOINT_BYTES: usize = 64 << 20;

/// Tests: a smaller checkpoint limit for runs in one folder (other tests
/// running meanwhile keep theirs).
#[cfg(test)]
pub static CHECKPOINT_LIMIT_IN: std::sync::Mutex<Option<(String, usize)>> =
    std::sync::Mutex::new(None);

fn checkpoint_limit(_root: &str) -> usize {
    #[cfg(test)]
    if let Some((dir, limit)) = CHECKPOINT_LIMIT_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        if _root.starts_with(dir.as_str()) {
            return *limit;
        }
    }
    MAX_CHECKPOINT_BYTES
}
/// The rolled-back attempt's diff kept in the record, at most.
const MAX_ROLLBACK_DIFF_CHARS: usize = 6_000;

/*
 * ---------------------------------------------------------------------------
 * R01: progress between rounds
 * ---------------------------------------------------------------------------
 */

/// One round's failures, as the next round compares with them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoundFailures {
    pub keys: BTreeSet<String>,
    /// The checks (kinds) that ran: their keys can be compared.
    pub ran: BTreeSet<String>,
    /// Tests the test check ran, when it said.
    pub tests_ran: Option<u32>,
    /// Round 2, P3.7: tests in the files the run itself wrote, then (None:
    /// not known, a round restored from before a restart).
    pub own_tests: Option<u32>,
}

/// Round 2, P3.7: fewer tests ran than in an earlier round, not counting
/// the tests in the run's own test files (it may remove tests it added,
/// and what it removes from a file is flagged by the file). A test file
/// deleted by a command, or tests that stopped being found, show here.
pub fn fewer_tests_flag(history: &[RoundFailures], now: &RoundFailures) -> Option<String> {
    // Round 3: a drop in the total beyond what removing tests from the
    // run's own files explains. Its own count is read from the files, so
    // tests it adds to a file the check doesn't run can't be "fewer".
    let counted = |r: &RoundFailures| Some((r.tests_ran?, r.own_tests?));
    let (before, own_before) = history.iter().filter_map(counted).max()?;
    let (after, own_after) = counted(now)?;
    let dropped = before.saturating_sub(after);
    let removed_own = own_before.saturating_sub(own_after);
    (dropped > removed_own).then(|| {
        format!("fewer tests ran than before, beyond any the run removed from its own test files ({before} → {after})")
    })
}

/// The check a key belongs to (`failure_keys` puts it first).
fn kind_of(key: &str) -> &str {
    key.split('|').next().unwrap_or("")
}

/// A failing test's key: `{kind}|test|{name}`.
fn is_test_key(key: &str) -> bool {
    key.split('|').nth(1) == Some("test")
}

/// A key that names no failure: the check alone (`test|failed`,
/// `test|timed out`) or the count past the shown keys (`test|and 6 more`).
fn is_opaque(key: &str) -> bool {
    key.split('|').count() == 2
}

/// Whether a failure that's new this round is known to have passed in the
/// round before (`prev`): its check passed then, or it's a test and the
/// tests ran then with nothing hiding it (no compile errors, no opaque key);
/// or it's an error in a check that had only failing tests (the code built).
/// A new error in a check that had errors before may only have been hidden
/// behind them: compilers report some errors only once earlier ones are
/// fixed, and a fixed compile error lets tests run that couldn't before.
fn broke_passing(key: &str, prev: &RoundFailures) -> bool {
    let kind = kind_of(key);
    let before: Vec<&String> = prev.keys.iter().filter(|p| kind_of(p) == kind).collect();
    if before.is_empty() {
        return true;
    }
    if is_opaque(key) || before.iter().any(|p| is_opaque(p)) {
        return false;
    }
    let tests_before = before.iter().any(|p| is_test_key(p));
    // Errors that can stop tests running. A traceback's `path:line: msg`
    // lines next to failing tests aren't keys at all (failure_keys, P3.1),
    // so a check with failing tests has test keys only.
    let errors_before = before.iter().any(|p| !is_test_key(p));
    if is_test_key(key) {
        tests_before && !errors_before
    } else {
        !errors_before
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Nothing to compare with (the first round, or no check ran in both).
    First,
    /// Fewer failures, none new.
    Progress,
    /// The same failures as the round before, or as two rounds before.
    NoProgress,
    /// A failure in what passed before: a check that passed, or a test
    /// that ran and didn't fail (`Judgement::broke`).
    Regression,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Judgement {
    pub verdict: Verdict,
    /// Failing now, not before.
    pub new: Vec<String>,
    /// The new failures in what is known to have passed before: they make
    /// the round a regression (and roll it back). The other new ones may
    /// have been hidden behind the earlier failures (a compile error before
    /// the tests ran, errors a compiler reports only once earlier ones are
    /// fixed, output with no keys to compare, the "and N more" key).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub broke: Vec<String>,
    /// Failing before, not now.
    pub fixed: Vec<String>,
    /// Back to the failures of two rounds ago.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub back_and_forth: bool,
}

/// How this round compares with the earlier ones (`history`, oldest first).
/// Only checks that ran in both rounds are compared: tests skipped because
/// the type check failed aren't "fixed", and their failures once the type
/// check passes aren't "new".
pub fn judge(history: &[RoundFailures], now: &RoundFailures) -> Judgement {
    let Some(prev) = history.last() else {
        return Judgement {
            verdict: Verdict::First,
            new: Vec::new(),
            broke: Vec::new(),
            fixed: Vec::new(),
            back_and_forth: false,
        };
    };
    // P3.3: a check that timed out (or said nothing to key) in either round
    // can't be compared: nothing of it counts as fixed or broken.
    // Round 2: only a timeout; a check whose output had nothing to key
    // (`test|failed`) still ran and failed.
    // Round 3: a check that couldn't run at all (its process didn't start)
    // says nothing either.
    let blind = |r: &RoundFailures, kind: &str| {
        r.keys.iter().any(|k| {
            kind_of(k) == kind && (k.ends_with("|timed out") || k.ends_with("|could not run"))
        })
    };
    let both: BTreeSet<&String> = prev
        .ran
        .intersection(&now.ran)
        .filter(|kind| !blind(prev, kind) && !blind(now, kind))
        .collect();
    let comparable = |k: &&String| both.iter().any(|b| b.as_str() == kind_of(k));
    let new: Vec<String> = now
        .keys
        .iter()
        .filter(comparable)
        .filter(|k| !prev.keys.contains(*k))
        .cloned()
        .collect();
    let fixed: Vec<String> = prev
        .keys
        .iter()
        .filter(comparable)
        .filter(|k| !now.keys.contains(*k))
        .cloned()
        .collect();
    let broke: Vec<String> = new
        .iter()
        .filter(|k| broke_passing(k, prev))
        .cloned()
        .collect();
    let back_and_forth =
        history.len() >= 2 && history[history.len() - 2].keys == now.keys && prev.keys != now.keys;
    let verdict = if both.is_empty() {
        Verdict::First
    } else if !broke.is_empty() {
        Verdict::Regression
    } else if back_and_forth || fixed.is_empty() {
        Verdict::NoProgress
    } else {
        Verdict::Progress
    };
    Judgement {
        verdict,
        new,
        broke,
        fixed,
        back_and_forth,
    }
}

/// Whether a path a tool printed is a file the round wrote. An absolute
/// path under a known root is that root's file; one under no known root
/// matches by its tail (the only reading left). A relative path is what a
/// tool run from the project prints, or from a subfolder (cargo in
/// src-tauri/ prints `src/x.rs` for `src-tauri/src/x.rs`): never the other
/// way round, so `crates/x/src/lib.rs` isn't the written `src/lib.rs`.
fn printed_is_written(printed: &str, written: &str, roots: &[&str]) -> bool {
    let p = printed.replace('\\', "/");
    let p = p.strip_prefix("./").unwrap_or(&p);
    let Some(w) = super::paths::norm_rel(written) else {
        return false;
    };
    let absolute = p.starts_with('/') || p.as_bytes().get(1) == Some(&b':');
    if absolute {
        for root in roots.iter().filter(|r| !r.is_empty()) {
            let root = root.replace('\\', "/");
            let root = root.trim_end_matches('/');
            if let Some(rel) = p.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
                return super::paths::same(rel, &w);
            }
        }
        return p.ends_with(&format!("/{w}"));
    }
    super::paths::same(p, &w) || w.ends_with(&format!("/{p}"))
}

impl Judgement {
    /// P3.2: a new error in a file the round itself wrote (`written`) is a
    /// slip in the fix (an unused import), reported with the failures; the
    /// fix stays. A regression is a test that fails now, or an error in a
    /// file the round didn't touch. Tools print paths their own way
    /// (absolute, from a subfolder): compared by their tail (round 2).
    /// `roots` (round 3): the folders tools print absolute paths under
    /// (the chat's view, the project), so `/p/packages/ui/src/a.ts` is
    /// `packages/ui/src/a.ts`, not the written `src/a.ts`.
    pub fn keeping_slips_in(mut self, written: &[String], roots: &[&str]) -> Judgement {
        self.broke.retain(|k| {
            is_test_key(k)
                || k.split('|')
                    .nth(1)
                    .is_none_or(|file| !written.iter().any(|w| printed_is_written(file, w, roots)))
        });
        if self.verdict == Verdict::Regression && self.broke.is_empty() {
            self.verdict = if self.fixed.is_empty() || self.back_and_forth {
                Verdict::NoProgress
            } else {
                Verdict::Progress
            };
        }
        self
    }

    /// The same round with the failures the integrity question was about
    /// (`flags`, by file) not counted as broken: after the agent was asked
    /// to put back a weakened test, that test failing again is what was
    /// asked for, not a regression (R05). Others still are (P3.7).
    pub fn without_regression_in(mut self, flags: &[String]) -> Judgement {
        self.broke.retain(|k| !named_in(k, flags));
        if self.broke.is_empty() {
            self = self.without_regression();
        }
        self
    }

    /// The same round with nothing counted as broken.
    pub fn without_regression(mut self) -> Judgement {
        if self.verdict == Verdict::Regression {
            self.verdict = if self.fixed.is_empty() || self.back_and_forth {
                Verdict::NoProgress
            } else {
                Verdict::Progress
            };
        }
        self.broke.clear();
        self
    }
}

/// Whether a failure key is about a file an integrity flag names
/// (`path: …`): its file, or a test whose name has the file's stem
/// (`posts > edits` for `posts.test.ts`, `tests/test_x.py::t`).
fn named_in(key: &str, flags: &[String]) -> bool {
    let detail = key
        .split_once('|')
        .map_or(key, |(_, rest)| rest)
        .to_lowercase();
    flags.iter().any(|f| {
        let path = f.split(": ").next().unwrap_or(f).to_lowercase();
        let name = path.rsplit('/').next().unwrap_or(&path);
        let stem = name.split('.').next().unwrap_or(name);
        detail.contains(&path) || (stem.len() >= 3 && detail.contains(stem))
    })
}

/// Keys as a list for the agent, without their check prefix.
pub fn key_list(keys: &[String], max: usize) -> String {
    let shown: Vec<String> = keys
        .iter()
        .take(max)
        .map(|k| {
            format!(
                "- {}",
                k.split_once('|').map_or(k.as_str(), |(_, rest)| rest)
            )
        })
        .collect();
    let more = keys.len().saturating_sub(max);
    if more > 0 {
        format!("{}\n- and {more} more", shown.join("\n"))
    } else {
        shown.join("\n")
    }
}

/// What the agent is told about this round's progress, in its failure
/// report (nothing for the first round).
pub fn progress_note(j: &Judgement, remaining: usize) -> Option<String> {
    let note = progress_verdict_note(j, remaining)?;
    // New failures that aren't regressions: the earlier ones hid them.
    if j.new.is_empty() || j.verdict == Verdict::Regression {
        return Some(note);
    }
    Some(format!(
        "{note} {} new failure{} showed up that the earlier ones may have hidden (a later stage of the build or the tests now runs):\n{}",
        j.new.len(),
        if j.new.len() == 1 { "" } else { "s" },
        key_list(&j.new, 8)
    ))
}

fn progress_verdict_note(j: &Judgement, remaining: usize) -> Option<String> {
    match j.verdict {
        Verdict::First | Verdict::Regression => None,
        Verdict::Progress => Some(format!(
            "Progress: your last fix resolved {} failure{}; {remaining} remain{}.",
            j.fixed.len(),
            if j.fixed.len() == 1 { "" } else { "s" },
            if remaining == 1 { "s" } else { "" },
        )),
        Verdict::NoProgress if j.back_and_forth => Some(
            "No progress: these are the same failures as two rounds ago, so your last two fixes undid each other. \
             Step back: find the cause both attempts missed, and don't repeat either fix."
                .into(),
        ),
        Verdict::NoProgress => Some(
            "No progress: your last fix left exactly the same failures. Don't repeat it. Read the failing code and \
             the error again, check your assumption about the cause, and try a different fix."
                .into(),
        ),
    }
}

/*
 * ---------------------------------------------------------------------------
 * R05: test integrity
 * ---------------------------------------------------------------------------
 */

/// A test file by the usual conventions (`*.test.ts`, `__tests__/`,
/// `tests/`, `test_*.py`, `*_test.go`, Rust `tests.rs` modules…).
pub fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    let in_dir = |d: &str| p.starts_with(&format!("{d}/")) || p.contains(&format!("/{d}/"));
    in_dir("__tests__")
        || in_dir("tests")
        || in_dir("test")
        || in_dir("spec")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.starts_with("test_") && name.ends_with(".py")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
}

/// A file that decides what the checks run: scripts, test-runner, lint and
/// type-check configuration.
pub fn is_check_setup(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    const NAMES: &[&str] = &[
        "package.json",
        "cargo.toml",
        "pyproject.toml",
        "setup.cfg",
        "tox.ini",
        "pytest.ini",
        // P3.8: pytest's hooks and fixtures can skip or deselect anything.
        "conftest.py",
        "ruff.toml",
        ".ruff.toml",
        "mypy.ini",
        "clippy.toml",
        "biome.json",
    ];
    const PREFIXES: &[&str] = &[
        "tsconfig",
        "jsconfig",
        "vitest.config",
        "vite.config",
        "jest.config",
        "playwright.config",
        "karma.conf",
        "eslint.config",
        ".eslintrc",
        ".mocharc",
    ];
    NAMES.contains(&name)
        || PREFIXES.iter().any(|pre| name.starts_with(pre))
        || p.ends_with(".cargo/config.toml")
}

/// The part of a manifest that decides what the checks run, so adding a
/// dependency isn't flagged: package.json's scripts, and the test and lint
/// sections of Cargo.toml, pyproject.toml and setup.cfg. Other files whole.
fn check_relevant(path: &str, text: &str) -> String {
    let name = path.replace('\\', "/").to_lowercase();
    let name = name.rsplit('/').next().unwrap_or("").to_string();
    let sections: &[&str] = match name.as_str() {
        "package.json" => {
            return package_scripts(text).map_or_else(String::new, |s| s.to_string());
        }
        "cargo.toml" => &[
            "[lints",
            "[workspace.lints",
            "[[test]]",
            "[[bench]]",
            "[lib]",
        ],
        "pyproject.toml" => &["[tool.pytest", "[tool.ruff", "[tool.mypy", "[tool.coverage"],
        "setup.cfg" => &["[tool:pytest]", "[flake8]", "[mypy", "[coverage"],
        "conftest.py" => return conftest_relevant(text),
        _ => return text.to_string(),
    };
    let mut keep = false;
    let mut package = false;
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            keep = sections.iter().any(|s| t.starts_with(s));
            package = name == "cargo.toml" && t == "[package]";
        }
        // P3.8: `autotests = false` (and the like) drops the tests folder.
        let auto = package && t.starts_with("auto");
        if (keep || auto) && !t.is_empty() {
            out.push_str(t);
            out.push('\n');
        }
    }
    out
}

/// What a file's tests look like, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TestCounts {
    pub tests: usize,
    pub skips: usize,
    pub only: usize,
    pub assertions: usize,
}

struct Patterns {
    tests: Regex,
    skips: Regex,
    only: Option<Regex>,
    assertions: Regex,
}

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

static JS: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    tests: re(r"\b(?:it|test)(?:\.(?:each|concurrent|only|skip|todo|failing)\b[^(]*)?\s*\("),
    // P3.8: a body that returns (or `if (false)`) first skips it too; a
    // bare `return`, not `return request(app)…` (round 2).
    skips: re(
        r"\b(?:it|test|describe)\.(?:skip|todo)\b|\bx(?:it|test|describe)\s*\(|\b(?:it|test)\s*\([^,]*,\s*(?:async\s*)?(?:\(\s*\)|function\s*\w*\s*\(\s*\))\s*(?:=>\s*)?\{\s*(?:if\s*\(\s*false\s*\)|return[ \t]*(?:;|\}|\r?\n))",
    ),
    only: Some(re(
        r"\b(?:it|test|describe)\.only\b|\bf(?:it|describe)\s*\(",
    )),
    assertions: re(r"\bexpect\s*\(|\bassert(?:\.\w+)?\s*\("),
});
static RUST: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    tests: re(r"#\[(?:tokio::)?test\b"),
    // P3.8: a test module behind a feature, and a test that returns first.
    skips: re(
        r"#\[ignore\b|#\[cfg\([^\]]*\bfeature\s*=[^\]]*\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w*test|#\[(?:tokio::)?test\]\s*(?:async\s+)?fn\s+\w+\s*\(\s*\)\s*\{\s*(?:return\b|if\s+false\b)",
    ),
    only: None,
    assertions: re(r"\b(?:assert|assert_eq|assert_ne|debug_assert)!\s*\("),
});
static PYTHON: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    tests: re(r"(?m)^\s*(?:async\s+)?def\s+test_"),
    // P3.8: a module-wide `pytestmark`, and a test that returns first.
    skips: re(
        r"@pytest\.mark\.(?:skip|skipif|xfail)\b|@unittest\.skip|\bpytest\.skip\s*\(|\bpytestmark\s*=.*\bpytest\.mark\.(?:skip|skipif|xfail)\b|(?m)^\s*(?:async\s+)?def\s+test_\w*\([^)]*\)\s*(?:->\s*\w+\s*)?:\s*\n\s*(?:return\b|if\s+False\s*:)",
    ),
    only: None,
    assertions: re(r"(?m)^\s*assert\b|\bself\.assert\w+\s*\(|\bpytest\.raises\s*\("),
});
static GO: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    tests: re(r"(?m)^func\s+Test\w*\s*\("),
    skips: re(
        r"\bt\.Skip(?:f|Now)?\s*\(|(?m)^func\s+Test\w*\s*\([^)]*\)\s*\{\s*(?:return\b|if\s+false\b)",
    ),
    only: None,
    assertions: re(
        r"\bt\.(?:Error|Errorf|Fatal|Fatalf|Fail|FailNow)\s*\(|\b(?:assert|require)\.\w+\s*\(",
    ),
});

fn patterns_for(path: &str) -> Option<&'static Patterns> {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Some(&JS),
        "rs" => Some(&RUST),
        "py" => Some(&PYTHON),
        "go" => Some(&GO),
        _ => None,
    }
}

pub fn count_tests(path: &str, content: &str) -> Option<TestCounts> {
    let p = patterns_for(path)?;
    Some(TestCounts {
        tests: p.tests.find_iter(content).count(),
        skips: p.skips.find_iter(content).count(),
        only: p.only.as_ref().map_or(0, |o| o.find_iter(content).count()),
        assertions: p.assertions.find_iter(content).count(),
    })
}

/// Round 2, P3.8: a file in a test folder that's code, in a language the
/// IDE can't read the tests of (not a snapshot, fixture or readme).
fn is_test_source(path: &str) -> bool {
    const EXTS: &[&str] = &[
        "rb", "java", "kt", "kts", "scala", "groovy", "cs", "fs", "vb", "php", "swift", "m", "mm",
        "c", "cc", "cpp", "cxx", "h", "hpp", "ex", "exs", "erl", "hs", "ml", "clj", "cljs", "dart",
        "lua", "pl", "pm", "t", "r", "jl", "zig", "nim", "cr", "d", "elm", "sh", "bash", "bats",
        "ps1",
    ];
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| EXTS.contains(&ext.to_lowercase().as_str()))
}

/// conftest.py's lines that decide what runs (round 2, P3.8): what it
/// ignores, deselects or skips, and its collection hooks. A fixture isn't.
fn conftest_relevant(text: &str) -> String {
    static DECIDES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"collect_ignore|pytest_collection_modifyitems|pytest_ignore_collect|pytest_runtest_setup|skip|deselect|addopts")
            .unwrap()
    });
    let mut out = String::new();
    let mut in_hook = false;
    for line in text.lines() {
        let indented = line.starts_with(char::is_whitespace);
        if !indented && !line.trim().is_empty() {
            in_hook = line.trim_start().starts_with("def pytest_");
        }
        if DECIDES.is_match(line) || (in_hook && indented) {
            out.push_str(line.trim());
            out.push('\n');
        }
    }
    out
}

/// The scripts of a package.json (what `npm test` and the like run).
fn package_scripts(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("scripts")
        .cloned()
}

/// What weakens the checks in one file's change, from what it was when the
/// run first touched it (`before`, None: it didn't exist) to now (`after`,
/// None: deleted). Test files are counted whole; other source files only by
/// their tests and skip markers (inline test modules), since their
/// assertions may be the program's own.
pub fn integrity_flags(path: &str, before: Option<&str>, after: Option<&str>) -> Vec<String> {
    let mut flags = Vec::new();
    if is_check_setup(path) {
        let changed = match (before, after) {
            (Some(b), Some(a)) => check_relevant(path, b) != check_relevant(path, a),
            (Some(b), None) => !check_relevant(path, b).is_empty(),
            _ => false,
        };
        if changed {
            let what = match path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .to_lowercase()
                .as_str()
            {
                "package.json" => " (its scripts)",
                "cargo.toml" | "pyproject.toml" | "setup.cfg" => " (its test or lint settings)",
                _ => "",
            };
            flags.push(format!(
                "{path}: changed, and it decides what the checks run{what}"
            ));
        }
        return flags;
    }
    let test_file = is_test_path(path);
    // P3.8: a test file the IDE can't read the tests of: any change to it
    // is the user's to look at (closed, not open).
    if test_file
        && patterns_for(path).is_none()
        && is_test_source(path)
        && before.is_some()
        && before != after
    {
        flags.push(format!(
            "{path}: a test file the IDE can't check was {}",
            if after.is_none() {
                "deleted"
            } else {
                "changed"
            }
        ));
        return flags;
    }
    let count = |text: Option<&str>| text.and_then(|t| count_tests(path, t));
    let (was, now) = (count(before), count(after));
    if let (Some(was), None) = (was, after) {
        if test_file || was.tests > 0 {
            flags.push(format!(
                "{path}: deleted, with {} test{}",
                was.tests,
                if was.tests == 1 { "" } else { "s" }
            ));
        }
        return flags;
    }
    let now = match now {
        Some(n) => n,
        None => return flags,
    };
    // A new file's tests take nothing away, but an `only` in it keeps the
    // others from running.
    let was = was.unwrap_or_default();
    if now.only > was.only {
        flags.push(format!(
            "{path}: added `only` ({} → {}), so other tests don't run",
            was.only, now.only
        ));
    }
    if before.is_none() {
        return flags;
    }
    if now.tests < was.tests {
        flags.push(format!(
            "{path}: {} test{} removed ({} → {})",
            was.tests - now.tests,
            if was.tests - now.tests == 1 { "" } else { "s" },
            was.tests,
            now.tests
        ));
    }
    if now.skips > was.skips {
        flags.push(format!(
            "{path}: skip markers added ({} → {})",
            was.skips, now.skips
        ));
    }
    if test_file && now.assertions < was.assertions {
        flags.push(format!(
            "{path}: fewer assertions ({} → {})",
            was.assertions, now.assertions
        ));
    }
    flags
}

/// The note asking the agent about flagged changes.
pub fn integrity_note(flags: &[String], checks_pass: bool) -> String {
    let list = flags
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let head = if checks_pass {
        "[Automatic check by the IDE, not written by the user]\nThe checks pass, but these changes weaken the existing tests or change what the checks run:"
    } else {
        "Also, these changes weaken the existing tests or change what the checks run:"
    };
    format!(
        "{head}\n{list}\n\nA test that existed before this task may change only when the request or the plan needs it. \
         Undo these changes and fix the code instead. If the request does need one, keep it and say why in your \
         final reply, citing the request; the user reviews every flagged change before your work counts as verified."
    )
}

/*
 * ---------------------------------------------------------------------------
 * R02: checkpoints and rollback
 * ---------------------------------------------------------------------------
 */

/// Files as they were before a fix round: content, or None when the file
/// didn't exist.
#[derive(Debug, Default)]
pub struct Checkpoint {
    pub files: BTreeMap<String, Option<Vec<u8>>>,
    /// The run's changed files then.
    pub changed_files: Vec<String>,
    /// Over the size limit: no rollback for this round.
    pub too_big: bool,
}

/// What a rollback did, for the record and the agent.
#[derive(Debug, Default, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rollback {
    /// Put back as they were before the round.
    pub restored: Vec<String>,
    /// Changed by someone else meanwhile: left as they are.
    pub skipped: Vec<String>,
    /// The failures the attempt brought in.
    pub broke: Vec<String>,
    /// The rolled-back attempt, as a diff.
    pub diff: String,
    /// Not isolated: commands that may have changed files in the round
    /// (P3.5). Their changes aren't known, so they weren't undone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

/// A path relative to the root, with forward slashes, whatever spelling the
/// tool call used.
pub fn rel_key(root: &str, path: &str) -> String {
    let abs = tools::to_absolute_path(path, root).replace('\\', "/");
    let root = root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    match abs.strip_prefix(root) {
        // `root` + `/…`, not a sibling folder whose name starts the same.
        Some(r) if r.is_empty() || r.starts_with('/') => {
            super::paths::norm_rel(r.trim_start_matches('/')).unwrap_or_else(|| abs.clone())
        }
        _ => abs,
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Run {
    /// Whether the run works in its chat's own copy (an overlay), where
    /// nobody else writes.
    pub(super) fn isolated(&self) -> bool {
        self.root.is_some() && self.root != self.project_root
    }

    /// The files the chat's overlay changed (project-relative), when it has one.
    pub(super) fn overlay_paths(&self) -> Vec<String> {
        if !self.isolated() {
            return Vec::new();
        }
        crate::overlay::chats::changes(&self.dialog_id)
            .iter()
            .map(|c| c.path().replace('\\', "/"))
            .collect()
    }

    /// A file is about to be written by the write tool: what it was is kept
    /// as its baseline (the first time in the run, R05) and in the open
    /// checkpoint (the first time in the round, R02).
    pub(super) fn note_before_write(&mut self, path: &str) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let key = rel_key(&root, path);
        // E03: the lead changes a test the test writer wrote.
        if self.sub().is_none()
            && self.test_writer_files.contains(&key)
            && !self.tests_changed.contains(&key)
        {
            self.tests_changed.push(key.clone());
        }
        let needs_baseline = !self.baseline.contains_key(&key);
        let needs_checkpoint = self
            .checkpoint
            .as_ref()
            .is_some_and(|c| !c.too_big && !c.files.contains_key(&key));
        if !needs_baseline && !needs_checkpoint {
            return;
        }
        let abs = tools::to_absolute_path(path, &root);
        let Ok(before) = read(Path::new(&abs)) else {
            return;
        };
        if !self.baseline.contains_key(&key) {
            // In an isolated chat, the project's copy is what the file was
            // before the chat (an earlier run may have changed the view).
            // A subagent runs within one round of its lead's.
            let original = match (&self.project_root, self.isolated()) {
                (Some(project), true) => Original::new(
                    read(&Path::new(project).join(&key)).ok().flatten(),
                    Source::ProjectCopy,
                ),
                _ if self.sub().is_some() => {
                    Original::new(before.clone(), Source::ViewAtRoundStart)
                }
                _ => Original::new(before.clone(), Source::ViewAtRunStart),
            };
            self.baseline.insert(key.clone(), original);
        }
        if let Some(cp) = self.checkpoint.as_mut().filter(|c| !c.too_big) {
            cp.files.entry(key).or_insert(before);
        }
    }

    /// A checkpoint of the files the run changed, before a fix round.
    pub(super) fn take_checkpoint(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let mut cp = Checkpoint {
            changed_files: self.changed_files.clone(),
            ..Default::default()
        };
        // A new round: its commands are counted afresh (P3.5).
        self.round_commands.clear();
        let mut paths: BTreeSet<String> = self
            .changed_files
            .iter()
            .map(|p| rel_key(&root, p))
            .collect();
        paths.extend(self.overlay_paths());
        let mut size = 0usize;
        for key in paths {
            // Unreadable (a folder, permissions): left out.
            if let Ok(content) = read(Path::new(&tools::to_absolute_path(&key, &root))) {
                size += content.as_ref().map_or(0, Vec::len);
                cp.files.insert(key, content);
            }
            if size > checkpoint_limit(&root) {
                cp.too_big = true;
                cp.files.clear();
                break;
            }
        }
        self.checkpoint = Some(cp);
    }

    /// Puts the files back as they were at the checkpoint. A file changed
    /// by someone other than this run since (direct mode: the user, another
    /// chat) is left as it is and listed (K15). None: no checkpoint to go
    /// back to.
    pub(super) async fn roll_back(&mut self, broke: Vec<String>) -> Option<Rollback> {
        let root = self.root.clone()?;
        let cp = self.checkpoint.take().filter(|c| !c.too_big)?;
        let isolated = self.isolated();
        // Changed in the round but not in the checkpoint: in an isolated
        // chat, a command's change; before the round it was the project's.
        let mut targets: BTreeMap<String, Option<Vec<u8>>> = cp.files.clone();
        for key in self.overlay_paths() {
            if let std::collections::btree_map::Entry::Vacant(slot) = targets.entry(key) {
                let original = self
                    .project_root
                    .as_ref()
                    .and_then(|p| read(&Path::new(p).join(slot.key())).ok().flatten());
                slot.insert(original);
            }
        }
        let seen = self.seen.clone();
        let mut result = Rollback {
            broke,
            commands: if isolated {
                Vec::new()
            } else {
                std::mem::take(&mut self.round_commands)
            },
            ..Default::default()
        };
        let mut diffs: Vec<String> = Vec::new();
        let mut written: Vec<(String, Option<String>, String, bool)> = Vec::new();
        for (key, target) in &targets {
            let abs = tools::to_absolute_path(key, &root);
            let path = Path::new(&abs);
            let Ok(current) = read(path) else {
                result.skipped.push(key.clone());
                continue;
            };
            if current == *target {
                continue;
            }
            // Not isolated: only what this run itself last wrote or read is
            // its to undo.
            if !isolated {
                let ours = match &current {
                    Some(bytes) => seen.get(path) == Some(crate::file_guard::hash(bytes)),
                    // Gone since: the run's doing when it last saw the file
                    // as the checkpoint has it (P3.4).
                    None => target
                        .as_ref()
                        .is_some_and(|t| seen.get(path) == Some(crate::file_guard::hash(t))),
                };
                if !ours {
                    result.skipped.push(key.clone());
                    continue;
                }
            }
            let text = |b: &Option<Vec<u8>>| {
                b.as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default()
            };
            let (was, now) = (text(target), text(&current));
            let hunks = super::diff::diff_lines(
                &super::diff::split_lines(&was),
                &super::diff::split_lines(&now),
            );
            if !hunks.is_empty() {
                diffs.push(format!("--- {key}\n{}", super::diff::compact_diff(&hunks)));
            }
            let done = {
                let _lock = crate::file_guard::lock(path);
                match target {
                    Some(bytes) => path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::write(path, bytes)),
                    None => std::fs::remove_file(path),
                }
            };
            match done {
                Ok(()) => {
                    match target {
                        Some(bytes) => {
                            seen.record(path, bytes);
                            written.push((
                                abs.clone(),
                                current.as_ref().map(|_| now.clone()),
                                was,
                                false,
                            ));
                        }
                        // P3.4: gone again: the editor is told too.
                        None => written.push((abs.clone(), Some(now.clone()), String::new(), true)),
                    }
                    crate::outline::invalidate_path(path);
                    crate::repo_map::invalidate_path(Path::new(&root), path);
                    result.restored.push(key.clone());
                }
                Err(_) => result.skipped.push(key.clone()),
            }
        }
        if isolated {
            let id = self.dialog_id.clone();
            let _ = crate::rt::spawn_blocking(move || {
                if let Some(o) = crate::overlay::chats::get(&id) {
                    o.fs.flush();
                }
            })
            .await;
        }
        for (abs, before, content, deleted) in written {
            self.core.emit(super::events::AgentEvent::FileWritten {
                abs_path: abs,
                before,
                content,
                reviewed: true,
                deleted,
            });
        }
        // The run's list of changed files is what it was then, plus what
        // couldn't be put back.
        let mut changed = cp.changed_files.clone();
        for key in &result.skipped {
            if !changed.iter().any(|c| rel_key(&root, c) == *key) {
                changed.push(key.clone());
            }
        }
        self.changed_files = changed;
        self.map_writes_since_refresh += 1;
        let mut diff = diffs.join("\n");
        if diff.chars().count() > MAX_ROLLBACK_DIFF_CHARS {
            diff = format!(
                "{}\n[… diff shortened …]",
                crate::chat_db::store::char_prefix(&diff, MAX_ROLLBACK_DIFF_CHARS)
            );
        }
        result.diff = diff;
        Some(result)
    }

    /// The files the run changed, relative to the root: those it wrote, and
    /// in an isolated chat all its overlay's changes.
    pub(super) fn touched_keys(&self) -> BTreeSet<String> {
        let Some(root) = self.root.clone() else {
            return BTreeSet::new();
        };
        let mut paths: BTreeSet<String> = self.baseline.keys().cloned().collect();
        paths.extend(self.overlay_paths());
        paths.extend(self.changed_files.iter().map(|p| rel_key(&root, p)));
        paths
    }

    /// What a file was before the run changed it: Some(None) when it didn't
    /// exist; None when that isn't known (a command's change outside an
    /// isolated chat).
    pub(super) fn original_of(&self, key: &str) -> Option<Option<String>> {
        self.original_bytes_of(key).map(|o| o.text())
    }

    /// `original_of` as bytes, with where they're from.
    pub(super) fn original_bytes_of(&self, key: &str) -> Option<Original> {
        if let Some(b) = self.baseline.get(key) {
            return Some(b.clone());
        }
        // An overlay change the write tool didn't make: the project's copy
        // is what it was.
        match (&self.project_root, self.isolated()) {
            (Some(project), true) => Some(Original::new(
                std::fs::read(Path::new(project).join(key)).ok(),
                Source::ProjectCopy,
            )),
            _ => None,
        }
    }

    /// R05: what weakens the checks among the files the run changed, from
    /// their baselines.
    /// Round 2, P3.7: the tests in the test files the run changed, as
    /// they are now.
    pub(super) fn own_test_count(&self) -> u32 {
        let Some(root) = self.root.clone() else {
            return 0;
        };
        self.touched_keys()
            .iter()
            .filter(|k| super::project::is_test_path(k))
            .filter_map(|k| {
                let text = std::fs::read(tools::to_absolute_path(k, &root)).ok()?;
                count_tests(k, &String::from_utf8_lossy(&text))
            })
            .map(|c| c.tests as u32)
            .sum()
    }

    pub(super) fn integrity_flags(&self) -> Vec<String> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut flags = Vec::new();
        for key in self.touched_keys() {
            let Some(before) = self.original_of(&key) else {
                continue;
            };
            let after = std::fs::read(tools::to_absolute_path(&key, &root))
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned());
            flags.extend(integrity_flags(&key, before.as_deref(), after.as_deref()));
        }
        flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(keys: &[&str], ran: &[&str]) -> RoundFailures {
        RoundFailures {
            keys: keys.iter().map(|k| k.to_string()).collect(),
            ran: ran.iter().map(|k| k.to_string()).collect(),
            tests_ran: None,
            own_tests: None,
        }
    }

    /// P3.1: a test that passed and now fails is a regression, in each
    /// runner's real output: tracebacks and summaries aren't errors that
    /// could have hidden it.
    #[test]
    fn regressions_are_found_in_pytest_go_and_cargo_workspaces() {
        use super::super::command_output::failure_keys;
        let of = |out: &str| {
            let keys = failure_keys("test", out, false);
            RoundFailures {
                keys: keys.into_iter().collect(),
                ran: ["test".to_string()].into_iter().collect(),
                tests_ran: None,
                own_tests: None,
            }
        };
        let cases = [
            (
                "pytest",
                "____ test_a ____\n\n    def test_a():\n>       assert 1 == 2\nE       AssertionError\n\ntests/test_x.py:5: AssertionError\nFAILED tests/test_x.py::test_a - AssertionError\n",
                "____ test_b ____\n\n    def test_b():\n>       assert f() == 3\nE       AssertionError\n\ntests/test_x.py:9: AssertionError\nFAILED tests/test_x.py::test_b - AssertionError\n",
            ),
            (
                "go",
                "--- FAIL: TestA (0.00s)\n    a_test.go:10: want 1, got 2\nFAIL\nFAIL\texample.com/p\t0.01s\n",
                "--- FAIL: TestB (0.00s)\n    a_test.go:20: want 3, got 4\nFAIL\nFAIL\texample.com/p\t0.01s\n",
            ),
            (
                "cargo workspace",
                "     Running unittests src/lib.rs (target/debug/deps/alpha-1a2b3c)\ntest tests::it ... FAILED\n\nerror: test failed, to rerun pass `-p alpha --lib`\nerror: 2 targets failed:\n    `-p alpha --lib`\n",
                "     Running unittests src/lib.rs (target/debug/deps/alpha-1a2b3c)\ntest tests::it ... ok\n     Running unittests src/lib.rs (target/debug/deps/beta-4d5e6f)\ntest tests::it ... FAILED\n\nerror: test failed, to rerun pass `-p beta --lib`\n",
            ),
        ];
        for (runner, before, after) in cases {
            let j = judge(&[of(before)], &of(after));
            assert_eq!(j.verdict, Verdict::Regression, "{runner}: {j:?}");
        }
        // Two crates' tests of one name are two tests.
        let keys = failure_keys("test", cases[2].2, false);
        assert_eq!(keys, ["test|test|beta::tests::it"]);
    }

    /// P3.2: the fix passes the test and leaves an unused import in the
    /// file it wrote: progress, with the lint failure reported, no rollback.
    #[test]
    fn a_slip_in_the_fixed_file_isnt_a_regression() {
        let ran = ["test", "lint"];
        let r1 = round(&["test|test|t::one"], &ran);
        let r2 = round(
            &["lint|src/a.ts|no-unused-vars|'x' is defined but never used."],
            &ran,
        );
        assert_eq!(
            judge(std::slice::from_ref(&r1), &r2).verdict,
            Verdict::Regression
        );
        let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["./src/a.ts".into()], &[]);
        assert_eq!(j.verdict, Verdict::Progress);
        assert!(j.broke.is_empty() && j.new.len() == 1);
        // In a file it didn't write: still a regression.
        let j = judge(&[r1], &r2).keeping_slips_in(&["src/b.ts".into()], &[]);
        assert_eq!(j.verdict, Verdict::Regression);
    }

    /// Round 2, P3.3: output with nothing to key (mocha, `make test`, a
    /// segfault) still says the tests ran and failed: after they passed,
    /// that's a regression. Only a timeout is blind.
    #[test]
    fn an_unparsed_failure_after_green_is_still_a_regression() {
        let r1 = round(&["lint|src/a.ts|no-unused-vars|x"], &["lint", "test"]);
        let r2 = round(&["test|failed"], &["lint", "test"]);
        let j = judge(std::slice::from_ref(&r1), &r2);
        assert_eq!(j.verdict, Verdict::Regression, "{j:?}");
        assert_eq!(j.broke, ["test|failed"]);
        assert_eq!(j.fixed, ["lint|src/a.ts|no-unused-vars|x"]);
    }

    /// Round 2, P3.2: linters print absolute paths, and cargo in src-tauri/
    /// prints paths from there: an error in the file the round wrote is
    /// still a slip, however it's printed.
    #[test]
    fn a_slip_in_an_absolute_or_subfolder_path_is_still_a_slip() {
        let ran = ["test", "lint", "build"];
        let r1 = round(&["test|test|t::one"], &ran);
        for (key, written) in [
            ("lint|/p/src/a.ts|rule|m", "src/a.ts"),
            ("build|src/x.rs|E0308|m", "src-tauri/src/x.rs"),
            ("lint|.\\src\\a.ts|rule|m", "src/a.ts"),
        ] {
            let r2 = round(&[key], &ran);
            let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&[written.into()], &[]);
            assert_eq!(j.verdict, Verdict::Progress, "{key}: {j:?}");
            assert!(j.broke.is_empty(), "{key}");
            // Another file of the same name elsewhere isn't the one written.
            let j =
                judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["lib/b.ts".into()], &[]);
            assert_eq!(j.verdict, Verdict::Regression, "{key}");
        }
    }

    /// Round 3: another file of the same name, in another crate or package,
    /// isn't the written one: a relative path from a subfolder is only
    /// ever shorter than the written path, and an absolute one under a
    /// known root is that root's file.
    #[test]
    fn a_same_named_file_elsewhere_isnt_a_slip() {
        let ran = ["test", "lint", "build"];
        let r1 = round(&["test|test|t::one"], &ran);
        let r2 = round(&["build|crates/x/src/lib.rs|E0308|m"], &ran);
        let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/lib.rs".into()], &[]);
        assert_eq!(j.verdict, Verdict::Regression, "{j:?}");
        assert_eq!(j.broke, ["build|crates/x/src/lib.rs|E0308|m"]);
        let r2 = round(&["lint|/p/packages/ui/src/a.ts|rule|m"], &ran);
        let j =
            judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/a.ts".into()], &["/p"]);
        assert_eq!(j.verdict, Verdict::Regression, "{j:?}");
        // The written file under the root is the slip it was.
        let r2 = round(&["lint|/p/src/a.ts|rule|m"], &ran);
        let j =
            judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/a.ts".into()], &["/p"]);
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
        // A Windows view root, printed with backslashes.
        let r2 = round(&["lint|C:\\v\\src\\a.ts|rule|m"], &ran);
        let j = judge(std::slice::from_ref(&r1), &r2)
            .keeping_slips_in(&["src/a.ts".into()], &["C:\\v"]);
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
    }

    /// Round 2 (§4.2): an error in a file an earlier round changed (not
    /// this one) is a regression; so is one in an untouched file next to a
    /// slip in a written one.
    #[test]
    fn an_error_in_a_file_changed_in_an_earlier_round_is_a_regression() {
        let ran = ["test", "lint"];
        let r1 = round(&["test|test|t::one"], &ran);
        let r2 = round(&["lint|src/old.ts|rule|m"], &ran);
        let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/new.ts".into()], &[]);
        assert_eq!(j.verdict, Verdict::Regression);
        let r2 = round(
            &["lint|src/new.ts|rule|m", "lint|src/other.ts|rule|m"],
            &ran,
        );
        let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/new.ts".into()], &[]);
        assert_eq!(j.verdict, Verdict::Regression, "{j:?}");
        assert_eq!(j.broke, ["lint|src/other.ts|rule|m"]);
    }

    /// Round 2 (§4.2): {a test, an error in the written file, an error in
    /// an untouched one} × {relative, absolute, subfolder paths}.
    #[test]
    fn slip_matrix() {
        let ran = ["test", "lint"];
        let r1 = round(&["test|test|t::one"], &ran);
        for (written, printed) in [
            ("src/a.ts", "src/a.ts"),
            ("src/a.ts", "/home/me/p/src/a.ts"),
            ("app/src/a.ts", "src/a.ts"),
        ] {
            let other = printed.replace("a.ts", "b.ts");
            for (key, broken) in [
                ("test|test|t::two".to_string(), true),
                (format!("lint|{printed}|rule|m"), false),
                (format!("lint|{other}|rule|m"), true),
            ] {
                let r2 = round(&[key.as_str()], &ran);
                let j =
                    judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&[written.into()], &[]);
                assert_eq!(j.broke.contains(&key), broken, "{written} / {key}: {j:?}");
            }
        }
    }

    /// Round 2 (§4.2): a slip in a file the round wrote doesn't excuse an
    /// error in one it didn't touch: still a regression, for that one.
    #[test]
    fn a_slip_with_an_untouched_file_too_is_a_regression() {
        let ran = ["test", "lint"];
        let r1 = round(&["test|test|t::one"], &ran);
        let r2 = round(&["lint|src/a.ts|rule|m", "lint|src/b.ts|rule|m"], &ran);
        let j = judge(std::slice::from_ref(&r1), &r2).keeping_slips_in(&["src/a.ts".into()], &[]);
        assert_eq!(j.verdict, Verdict::Regression, "{j:?}");
        assert_eq!(j.broke, ["lint|src/b.ts|rule|m"]);
    }

    /// Round 2 (§4.2): a timeout in either round makes that check blind,
    /// the others still compared.
    #[test]
    fn timeout_on_prev_only_now_only_both() {
        let ran = ["test", "lint"];
        let lint_fixed = |prev: &[&str], now: &[&str]| {
            let j = judge(&[round(prev, &ran)], &round(now, &ran));
            assert!(
                j.new
                    .iter()
                    .chain(&j.fixed)
                    .all(|k| !k.starts_with("test|")),
                "{j:?}"
            );
            j
        };
        let j = lint_fixed(&["test|timed out", "lint|a|x"], &["test|test|t::one"]);
        assert_eq!(j.fixed, ["lint|a|x"]);
        let j = lint_fixed(&["test|test|t::one", "lint|a|x"], &["test|timed out"]);
        assert_eq!(j.fixed, ["lint|a|x"]);
        let j = lint_fixed(&["test|timed out"], &["test|timed out", "lint|b|y"]);
        assert_eq!(j.verdict, Verdict::Regression, "lint still compared: {j:?}");
    }

    /// Round 2 (§4.2): "and N more" isn't a timeout: past the shown keys,
    /// one failure fewer is progress.
    #[test]
    fn and_n_more_isnt_blind() {
        let keys = |n: usize| -> Vec<String> {
            let mut k: Vec<String> = (0..49).map(|i| format!("test|test|t::{i:02}")).collect();
            k.push(format!("test|and {n} more"));
            k
        };
        let r = |ks: &[String]| RoundFailures {
            keys: ks.iter().cloned().collect(),
            ran: ["test".to_string()].into(),
            tests_ran: None,
            own_tests: None,
        };
        let j = judge(&[r(&keys(2))], &r(&keys(1)));
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
    }

    /// Round 2 (§4.2): a flag names a test by its file's path or its stem
    /// (3 letters or more), nothing else.
    #[test]
    fn named_in_matches_file_and_stem_only() {
        let flags = ["tests/add.test.ts: skip markers added (0 → 1)".to_string()];
        assert!(named_in("test|test|tests/add.test.ts > adds", &flags));
        assert!(named_in("test|test|add > adds", &flags));
        assert!(!named_in("test|test|sub > subtracts", &flags));
        let short = ["t/ab.rs: 1 test removed (2 → 1)".to_string()];
        assert!(
            !named_in("test|test|tab::x", &short),
            "a 2-letter stem names nothing"
        );
    }

    /// Round 2 (§4.2): forgiving the flagged test leaves another broken
    /// one a regression; forgiving all of them falls back to progress.
    #[test]
    fn without_regression_in_falls_back_when_others_broke() {
        let ran = ["test"];
        let r1 = round(&["test|test|other::x"], &ran);
        let flags = ["tests/add.test.ts: skip markers added (0 → 1)".to_string()];
        let both = round(&["test|test|add::adds", "test|test|sub::subs"], &ran);
        let j = judge(std::slice::from_ref(&r1), &both).without_regression_in(&flags);
        assert_eq!(j.verdict, Verdict::Regression);
        assert_eq!(j.broke, ["test|test|sub::subs"]);
        let only = round(&["test|test|add::adds"], &ran);
        let j = judge(&[r1], &only).without_regression_in(&flags);
        assert_ne!(j.verdict, Verdict::Regression);
    }

    /// Round 2, P3.7: fewer tests than the most an earlier round ran, the
    /// run's own test files aside; rounds of unknown own tests don't count.
    #[test]
    fn fewer_tests_counts_the_others_only() {
        let r = |ran: u32, own: Option<u32>| RoundFailures {
            tests_ran: Some(ran),
            own_tests: own,
            ..Default::default()
        };
        assert_eq!(
            fewer_tests_flag(&[r(5, Some(0)), r(7, Some(2))], &r(4, Some(1))),
            Some(
                "fewer tests ran than before, beyond any the run removed from its own test files (7 → 4)".into()
            )
        );
        assert_eq!(fewer_tests_flag(&[r(5, Some(0))], &r(6, Some(1))), None);
        // Round 3: tests added to a file the check doesn't run aren't fewer.
        assert_eq!(fewer_tests_flag(&[r(20, Some(10))], &r(20, Some(15))), None);
        assert!(fewer_tests_flag(&[r(20, Some(10))], &r(12, Some(10))).is_some());
        // Its own tests removed explain the drop; one more doesn't.
        assert_eq!(fewer_tests_flag(&[r(20, Some(10))], &r(15, Some(5))), None);
        assert!(fewer_tests_flag(&[r(20, Some(10))], &r(14, Some(5))).is_some());
        assert_eq!(
            fewer_tests_flag(&[r(9, None)], &r(3, Some(0))),
            None,
            "restored round"
        );
        assert_eq!(fewer_tests_flag(&[], &r(3, Some(0))), None);
    }

    /// Round 3: a check that couldn't run (its process didn't start) says
    /// nothing either: not a regression after a green round.
    #[test]
    fn a_check_that_could_not_run_is_not_a_regression() {
        let ran = ["lint", "test"];
        let r1 = round(&["lint|a|x"], &ran);
        let r2 = round(&["test|could not run"], &ran);
        let j = judge(std::slice::from_ref(&r1), &r2);
        assert_ne!(j.verdict, Verdict::Regression, "{j:?}");
        assert_eq!(j.fixed, ["lint|a|x"]);
    }

    /// P3.3: a round whose tests timed out says nothing about them: not
    /// progress, nothing resolved.
    #[test]
    fn a_timed_out_check_is_incomparable() {
        let r1 = round(&["test|test|t::one", "test|test|t::two"], &["test"]);
        let r2 = round(&["test|timed out"], &["test"]);
        let j = judge(std::slice::from_ref(&r1), &r2);
        assert_eq!(j.verdict, Verdict::First, "{j:?}");
        assert!(j.fixed.is_empty() && j.new.is_empty());
        assert!(progress_note(&j, 1).is_none());
        // And back: the tests that fail after a timeout aren't "new".
        let j = judge(&[r1.clone(), r2], &r1);
        assert_eq!(j.verdict, Verdict::First, "{j:?}");
    }

    /// P3.7: putting a weakened test back forgives that test failing, not
    /// another test breaking.
    #[test]
    fn only_the_flagged_tests_failure_is_forgiven() {
        let ran = ["test"];
        let r1 = round(&["test|test|other > x"], &ran);
        let r2 = round(&["test|test|posts > edits", "test|test|feed > lists"], &ran);
        let flags = ["src/posts.test.ts: skip markers added (0 → 1)".to_string()];
        let j = judge(std::slice::from_ref(&r1), &r2).without_regression_in(&flags);
        assert_eq!(j.verdict, Verdict::Regression);
        assert_eq!(j.broke, ["test|test|feed > lists"]);
        let r3 = round(&["test|test|posts > edits"], &ran);
        let j = judge(&[r1], &r3).without_regression_in(&flags);
        assert_ne!(j.verdict, Verdict::Regression);
    }

    /// P3.8: each common way round a test is flagged, and a test file the
    /// IDE can't count is flagged on any change.
    #[test]
    fn the_common_bypasses_are_flagged() {
        let flagged = |path: &str, before: &str, after: &str| {
            let f = integrity_flags(path, Some(before), Some(after));
            assert!(!f.is_empty(), "{path}: {after}");
            f
        };
        // pytest's conftest and ini.
        flagged(
            "tests/conftest.py",
            "import pytest\n",
            "import pytest\ncollect_ignore = ['test_a.py']\n",
        );
        flagged(
            "pytest.ini",
            "[pytest]\n",
            "[pytest]\naddopts = -k 'not slow'\n",
        );
        // Cargo: the tests folder turned off.
        let cargo = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n";
        flagged("Cargo.toml", cargo, &format!("{cargo}autotests = false\n"));
        assert!(
            integrity_flags(
                "Cargo.toml",
                Some(cargo),
                Some(&cargo.replace("0.1.0", "0.2.0"))
            )
            .is_empty(),
            "a version bump isn't"
        );
        // A module-wide pytest skip.
        let py = "def test_a():\n    assert f() == 1\n";
        flagged(
            "tests/test_a.py",
            py,
            &format!("import pytest\npytestmark = pytest.mark.skip\n{py}"),
        );
        // A test that returns first.
        flagged(
            "tests/test_a.py",
            py,
            "def test_a():\n    return\n    assert f() == 1\n",
        );
        let js = "test('adds', () => {\n  expect(add(1, 2)).toBe(3);\n});\n";
        flagged(
            "src/add.test.ts",
            js,
            &js.replace("=> {\n", "=> {\n  if (false) "),
        );
        flagged(
            "src/add.test.ts",
            js,
            &js.replace("=> {\n", "=> {\n  return;\n"),
        );
        let go =
            "func TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\t\tt.Fatal(\"x\")\n\t}\n}\n";
        flagged(
            "add_test.go",
            go,
            &go.replace("{\n\tif Add", "{\n\treturn\n\tif Add"),
        );
        // A Rust test module behind a feature, and a test that returns.
        let rs = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(add(1, 2), 3);\n    }\n}\n";
        flagged(
            "src/lib.rs",
            rs,
            &rs.replace("#[cfg(test)]", "#[cfg(feature = \"never\")]"),
        );
        flagged(
            "src/lib.rs",
            rs,
            &rs.replace("fn adds() {\n", "fn adds() {\n        return;\n"),
        );
        // A test file in a language it can't count.
        let f = flagged("spec/add_spec.rb", "it 'adds' do\nend\n", "");
        assert!(
            f[0].contains("a test file the IDE can't check was changed"),
            "{f:?}"
        );
    }

    /// Round 2, P3.8: ordinary test work isn't flagged: a snapshot, a
    /// fixture or a readme in a test folder, a JS test that returns its
    /// request, a plain fixture in conftest.py.
    #[test]
    fn legit_test_refactors_arent_flagged() {
        let none = |path: &str, before: &str, after: &str| {
            let f = integrity_flags(path, Some(before), Some(after));
            assert!(f.is_empty(), "{path}: {f:?}");
        };
        none(
            "__tests__/__snapshots__/a.snap",
            "exports[`a`] = `1`;\n",
            "exports[`a`] = `2`;\n",
        );
        none("tests/fixtures/x.json", "{\"a\": 1}\n", "{\"a\": 2}\n");
        none("tests/README.md", "# Tests\n", "# Tests\n\nRun them.\n");
        none("tests/fixtures/seed.sql", "insert 1;\n", "insert 2;\n");
        let js = "test('lists', () => {\n  expect(1).toBe(1);\n});\n";
        let added = format!(
            "{js}test('serves', () => {{\n  return request(app).get('/').expect(200);\n}});\n"
        );
        none("src/app.test.js", js, &added);
        none(
            "tests/conftest.py",
            "import pytest\n",
            "import pytest\n\n@pytest.fixture\ndef user():\n    return {'name': 'Ann'}\n",
        );
        // What decides what runs still is.
        assert!(!integrity_flags(
            "tests/conftest.py",
            Some("import pytest\n"),
            Some("import pytest\n\ndef pytest_collection_modifyitems(items):\n    items[:] = []\n"),
        )
        .is_empty());
        assert!(!integrity_flags("tests/add_test.rb", Some("x\n"), Some("y\n")).is_empty());
    }

    fn flagged_by(path: &str, before: &str, after: Option<&str>) -> Vec<String> {
        integrity_flags(path, Some(before), after)
    }

    // Round 2 (§4.2): each way round a test, on its own.
    #[test]
    fn bypass_conftest_collect_ignore() {
        assert!(!flagged_by(
            "tests/conftest.py",
            "import pytest\n",
            Some("import pytest\ncollect_ignore = ['test_a.py']\n")
        )
        .is_empty());
    }
    #[test]
    fn bypass_pytest_ini_addopts() {
        assert!(!flagged_by(
            "pytest.ini",
            "[pytest]\n",
            Some("[pytest]\naddopts = -k 'not slow'\n")
        )
        .is_empty());
    }
    #[test]
    fn bypass_cargo_autotests_and_autobins() {
        let cargo = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n";
        for line in ["autotests = false", "autobins = false"] {
            assert!(
                !flagged_by("Cargo.toml", cargo, Some(&format!("{cargo}{line}\n"))).is_empty(),
                "{line}"
            );
        }
    }
    #[test]
    fn bypass_pytestmark_list() {
        let py = "def test_a():\n    assert f() == 1\n";
        let after = format!("import pytest\npytestmark = [pytest.mark.skip]\n{py}");
        assert!(!flagged_by("tests/test_a.py", py, Some(&after)).is_empty());
    }
    #[test]
    fn bypass_python_early_return_and_if_false() {
        let py = "def test_a():\n    assert f() == 1\n";
        for body in ["    return\n", "    if False:\n"] {
            let after = py.replace("):\n", &format!("):\n{body}"));
            assert!(
                !flagged_by("tests/test_a.py", py, Some(&after)).is_empty(),
                "{body}"
            );
        }
    }
    #[test]
    fn bypass_js_if_false_and_bare_return() {
        let js = "test('adds', () => {\n  expect(add(1, 2)).toBe(3);\n});\n";
        for first in ["  if (false) ", "  return;\n"] {
            let after = js.replace("=> {\n", &format!("=> {{\n{first}"));
            assert!(
                !flagged_by("src/add.test.ts", js, Some(&after)).is_empty(),
                "{first}"
            );
        }
    }
    #[test]
    fn bypass_go_return() {
        let go =
            "func TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\t\tt.Fatal(\"x\")\n\t}\n}\n";
        let after = go.replace("{\n\tif Add", "{\n\treturn\n\tif Add");
        assert!(!flagged_by("add_test.go", go, Some(&after)).is_empty());
    }
    #[test]
    fn bypass_rust_feature_module_and_return() {
        let rs = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(add(1, 2), 3);\n    }\n}\n";
        let feature = rs.replace("#[cfg(test)]", "#[cfg(feature = \"never\")]");
        let ret = rs.replace("fn adds() {\n", "fn adds() {\n        return;\n");
        for after in [feature, ret] {
            assert!(
                !flagged_by("src/lib.rs", rs, Some(&after)).is_empty(),
                "{after}"
            );
        }
    }
    #[test]
    fn bypass_unsupported_language_changed_and_deleted() {
        let rb = "it 'adds' do\n  expect(add(1, 2)).to eq(3)\nend\n";
        let changed = flagged_by("spec/x_spec.rb", rb, Some("it 'adds' do\nend\n"));
        assert!(
            changed[0].contains("can't check was changed"),
            "{changed:?}"
        );
        let deleted = flagged_by("spec/x_spec.rb", rb, None);
        assert!(
            deleted[0].contains("can't check was deleted"),
            "{deleted:?}"
        );
    }

    /// Round 2 (§4.2): ordinary changes aren't flagged; known gaps pinned.
    #[test]
    fn integrity_edges_pinned() {
        // A dependency added to Cargo.toml.
        let cargo = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\n";
        assert!(flagged_by(
            "Cargo.toml",
            cargo,
            Some(&format!("{cargo}serde = \"1\"\n"))
        )
        .is_empty());
        // A test module behind a feature that was there before: unchanged.
        let rs = "#[cfg(feature = \"slow\")]\nmod tests {\n    #[test]\n    fn adds() { assert!(true); }\n}\n";
        assert!(flagged_by(
            "src/lib.rs",
            rs,
            Some(&rs.replace("assert!(true)", "assert!(1 == 1)"))
        )
        .is_empty());
        // A new test file in a language the IDE can't read: nothing to compare.
        assert!(integrity_flags("spec/new_spec.rb", None, Some("it 'x' do\nend\n")).is_empty());
        // Gap: swapping an assertion for one that always holds keeps the
        // counts the same, so it isn't seen.
        let js = "test('adds', () => {\n  expect(add(1, 2)).toBe(3);\n});\n";
        assert!(flagged_by(
            "src/add.test.ts",
            js,
            Some(&js.replace("toBe(3)", "toBeDefined()"))
        )
        .is_empty());
        // Gap: a docstring before the early return hides it from the rule.
        let py = "def test_a():\n    assert f() == 1\n";
        let doc = py.replace("):\n", "):\n    \"\"\"Doc.\"\"\"\n    return\n");
        assert!(flagged_by("tests/test_a.py", py, Some(&doc)).is_empty());
    }

    #[test]
    fn progress_no_progress_and_regression_are_told_apart() {
        let r1 = round(
            &["typecheck|a.ts|TS1|x", "typecheck|b.ts|TS2|y"],
            &["typecheck", "lint"],
        );
        assert_eq!(judge(&[], &r1).verdict, Verdict::First);

        let fewer = round(&["typecheck|a.ts|TS1|x"], &["typecheck", "lint"]);
        let j = judge(std::slice::from_ref(&r1), &fewer);
        assert_eq!(j.verdict, Verdict::Progress);
        assert_eq!(j.fixed, ["typecheck|b.ts|TS2|y"]);
        assert!(progress_note(&j, 1)
            .unwrap()
            .contains("resolved 1 failure; 1 remains"));

        let same = judge(std::slice::from_ref(&r1), &r1);
        assert_eq!(same.verdict, Verdict::NoProgress);
        assert!(progress_note(&same, 2).unwrap().starts_with("No progress"));

        let broke = round(
            &["typecheck|a.ts|TS1|x", "lint|c.ts|rule|z"],
            &["typecheck", "lint"],
        );
        let j = judge(std::slice::from_ref(&r1), &broke);
        assert_eq!(j.verdict, Verdict::Regression);
        assert_eq!(j.new, ["lint|c.ts|rule|z"]);
        assert_eq!(progress_note(&j, 2), None);
    }

    #[test]
    fn checks_that_didnt_run_before_are_not_compared() {
        // Round 1: the type check failed, so tests were skipped.
        let r1 = round(&["typecheck|a.ts|TS1|x"], &["typecheck"]);
        // Round 2: the type check passes; tests run and fail.
        let r2 = round(&["test|test|adds"], &["typecheck", "test"]);
        let j = judge(&[r1], &r2);
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
        assert!(j.new.is_empty());
    }

    #[test]
    fn failures_the_earlier_ones_hid_are_not_a_regression() {
        let ran = ["test"];
        // A compile error, fixed: the tests run, and one fails.
        let built = round(&["test|src/lib.rs|E0425|cannot find value `x`"], &ran);
        let tested = round(&["test|test|t::adds"], &ran);
        let j = judge(std::slice::from_ref(&built), &tested);
        assert_eq!(j.verdict, Verdict::Progress, "{j:?}");
        assert_eq!(j.new, ["test|test|t::adds"]);
        assert!(j.broke.is_empty());
        assert!(progress_note(&j, 1).unwrap().contains("may have hidden"));

        // Rust reports some errors only once earlier ones are fixed.
        let types = round(&["test|src/a.rs|E0308|mismatched types"], &ran);
        let borrow = round(&["test|src/a.rs|E0502|cannot borrow"], &ran);
        assert!(judge(std::slice::from_ref(&types), &borrow)
            .broke
            .is_empty());

        // Output with no keys to compare, and the count past the shown keys.
        let opaque = round(&["test|failed"], &ran);
        assert!(judge(std::slice::from_ref(&opaque), &tested)
            .broke
            .is_empty());
        let many = round(&["test|a.ts|TS1|x", "test|and 11 more"], &ran);
        let fewer = round(&["test|b.ts|TS1|y", "test|and 6 more"], &ran);
        let j = judge(std::slice::from_ref(&many), &fewer);
        assert_eq!(j.new.len(), 2);
        assert!(j.broke.is_empty(), "{j:?}");
    }

    #[test]
    fn what_passed_before_and_fails_now_is_a_regression() {
        let ran = ["test", "lint"];
        // The tests ran and only `a` failed: `b` failing now broke.
        let before = round(&["test|test|t::a"], &ran);
        let now = round(&["test|test|t::b"], &ran);
        assert_eq!(
            judge(std::slice::from_ref(&before), &now).broke,
            ["test|test|t::b"]
        );
        // The code built before (only tests failed): a compile error now broke.
        let now = round(&["test|src/a.rs|E0425|x"], &ran);
        let j = judge(std::slice::from_ref(&before), &now);
        assert_eq!(j.verdict, Verdict::Regression);
        // A check that passed: anything in it broke.
        let now = round(&["test|test|t::a", "lint|c.ts|rule|z"], &ran);
        assert_eq!(
            judge(std::slice::from_ref(&before), &now).broke,
            ["lint|c.ts|rule|z"]
        );
        // Asked to put back a weakened test: not a regression.
        let j = judge(
            std::slice::from_ref(&before),
            &round(&["test|test|t::b"], &ran),
        )
        .without_regression();
        assert_eq!(j.verdict, Verdict::Progress);
        assert!(j.broke.is_empty());
    }

    #[test]
    fn going_back_to_two_rounds_ago_is_no_progress() {
        let a = round(&["test|test|a", "test|test|b"], &["test"]);
        let b = round(&["test|test|a"], &["test"]);
        let j = judge(&[a.clone(), b], &a);
        // It's also a regression against the last round (b came back).
        assert_eq!(j.verdict, Verdict::Regression);
        assert!(j.back_and_forth);
        let c = round(&["test|test|c", "test|test|d"], &["test"]);
        let d = round(&["test|test|c"], &["test"]);
        let fixed_then_same = judge(&[c.clone(), d.clone()], &d);
        assert_eq!(fixed_then_same.verdict, Verdict::NoProgress);
        assert!(!fixed_then_same.back_and_forth);
    }

    #[test]
    fn test_paths_and_check_setup() {
        for p in [
            "src/a.test.ts",
            "src/__tests__/b.ts",
            "tests/it.rs",
            "app/test_x.py",
            "pkg/x_test.go",
            "src-tauri/src/chat_db/tests.rs",
            "src/agent_run/subagent_e2e_tests.rs",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in ["src/a.ts", "src/testing.rs", "latest/x.py"] {
            assert!(!is_test_path(p), "{p}");
        }
        for p in [
            "package.json",
            "src-tauri/Cargo.toml",
            "tsconfig.node.json",
            "vitest.config.ts",
            ".eslintrc.cjs",
        ] {
            assert!(is_check_setup(p), "{p}");
        }
        assert!(!is_check_setup("src/config.ts"));
    }

    #[test]
    fn weakened_tests_are_flagged() {
        let before =
            "test('a', () => { expect(1).toBe(1); });\ntest('b', () => { expect(2).toBe(2); });\n";
        let skipped = "test.skip('a', () => { expect(1).toBe(1); });\ntest('b', () => { expect(2).toBe(2); });\n";
        let flags = integrity_flags("src/a.test.ts", Some(before), Some(skipped));
        assert_eq!(flags, ["src/a.test.ts: skip markers added (0 → 1)"]);

        let removed = "test('b', () => { expect(2).toBe(2); });\n";
        let flags = integrity_flags("src/a.test.ts", Some(before), Some(removed));
        assert!(
            flags.contains(&"src/a.test.ts: 1 test removed (2 → 1)".to_string()),
            "{flags:?}"
        );
        assert!(flags.iter().any(|f| f.contains("fewer assertions (2 → 1)")));

        let only = before.replace("test('a'", "test.only('a'");
        assert!(
            integrity_flags("src/a.test.ts", Some(before), Some(&only))[0].contains("added `only`")
        );
        assert_eq!(
            integrity_flags("src/a.test.ts", Some(before), None),
            ["src/a.test.ts: deleted, with 2 tests"]
        );
        // New tests, more assertions, and unrelated edits are fine.
        let more = format!("{before}test('c', () => {{ expect(3).toBe(3); }});\n");
        assert!(integrity_flags("src/a.test.ts", Some(before), Some(&more)).is_empty());
        assert!(integrity_flags("src/new.test.ts", None, Some(before)).is_empty());

        // Rust: an inline test module in a source file.
        let rs =
            "fn f() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert!(true); }\n}\n";
        let ignored = rs.replace("#[test]", "#[test]\n    #[ignore]");
        assert_eq!(
            integrity_flags("src/lib.rs", Some(rs), Some(&ignored)),
            ["src/lib.rs: skip markers added (0 → 1)"]
        );
        // A source file's own assertions aren't counted.
        assert!(integrity_flags(
            "src/lib.rs",
            Some("fn f() { assert!(x); }"),
            Some("fn f() {}")
        )
        .is_empty());

        // Python and Go.
        let py = "def test_a():\n    assert f() == 1\n";
        let py_skip = "import pytest\n@pytest.mark.skip\ndef test_a():\n    assert f() == 1\n";
        assert_eq!(
            integrity_flags("tests/test_a.py", Some(py), Some(py_skip)).len(),
            1
        );
        let go = "func TestA(t *testing.T) {\n\tif f() != 1 { t.Fatal(\"x\") }\n}\n";
        let go_skip =
            "func TestA(t *testing.T) {\n\tt.Skip()\n\tif f() != 1 { t.Fatal(\"x\") }\n}\n";
        assert_eq!(
            integrity_flags("a_test.go", Some(go), Some(go_skip)).len(),
            1
        );
    }

    #[test]
    fn manifests_are_flagged_for_their_check_settings_only() {
        let cargo = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n\n[lints.rust]\nunsafe_code = \"forbid\"\n";
        let dep = cargo.replace("serde = \"1\"", "serde = \"1\"\nregex = \"1\"");
        assert!(integrity_flags("Cargo.toml", Some(cargo), Some(&dep)).is_empty());
        let lints = cargo.replace("forbid", "allow");
        assert_eq!(
            integrity_flags("Cargo.toml", Some(cargo), Some(&lints)),
            ["Cargo.toml: changed, and it decides what the checks run (its test or lint settings)"]
        );
        let py = "[project]\nname = \"x\"\n[tool.pytest.ini_options]\naddopts = \"-q\"\n";
        let py_dep = py.replace("[project]\n", "[project]\ndependencies = [\"x\"]\n");
        assert!(integrity_flags("pyproject.toml", Some(py), Some(&py_dep)).is_empty());
        let py_ignore = py.replace("-q", "-q --ignore=tests");
        assert_eq!(
            integrity_flags("pyproject.toml", Some(py), Some(&py_ignore)).len(),
            1
        );
    }

    #[test]
    fn check_setup_changes_are_flagged() {
        let pkg = r#"{ "name": "x", "version": "1.0.0", "scripts": { "test": "vitest run" } }"#;
        let bumped = r#"{ "name": "x", "version": "1.0.1", "scripts": { "test": "vitest run" } }"#;
        let neutered = r#"{ "name": "x", "version": "1.0.0", "scripts": { "test": "true" } }"#;
        assert!(integrity_flags("package.json", Some(pkg), Some(bumped)).is_empty());
        assert_eq!(
            integrity_flags("package.json", Some(pkg), Some(neutered)),
            ["package.json: changed, and it decides what the checks run (its scripts)"]
        );
        assert_eq!(
            integrity_flags("tsconfig.json", Some("{}"), Some(r#"{"x":1}"#)).len(),
            1
        );
        assert!(
            integrity_flags("tsconfig.json", None, Some("{}")).is_empty(),
            "a new config isn't a change"
        );
    }

    #[test]
    fn keys_are_relative_to_the_root() {
        assert_eq!(rel_key("/p", "src/a.ts"), "src/a.ts");
        assert_eq!(rel_key("/p/", "./src/a.ts"), "src/a.ts");
        assert_eq!(rel_key("/p", "/p/src/a.ts"), "src/a.ts");
        // Every spelling is one key; a sibling folder isn't inside.
        assert_eq!(rel_key("/p", "src/./a.ts"), "src/a.ts");
        assert_eq!(rel_key("/p", "src//a.ts"), "src/a.ts");
        assert_eq!(rel_key("/p", "/px/a.ts"), "/px/a.ts");
    }

    /// Round 2 (§4.2): the reference for "fewer tests" is the most an
    /// earlier round ran, not the first or the last round's count; the
    /// run's own tests, first written in round 2, aside: [5, 7, 6] → 7.
    #[test]
    fn reference_is_the_max_before_first_test_write() {
        let r = |ran: u32, own: u32| RoundFailures {
            tests_ran: Some(ran),
            own_tests: Some(own),
            ..Default::default()
        };
        // The most: 9 (its own 2 among them); 8 with the same own 2 is fewer.
        let history = [r(5, 0), r(9, 2), r(8, 2)];
        assert_eq!(
            fewer_tests_flag(&history, &r(8, 2)),
            Some(
                "fewer tests ran than before, beyond any the run removed from its own test files (9 → 8)".into()
            )
        );
        assert_eq!(
            fewer_tests_flag(&history, &r(9, 2)),
            None,
            "as many as the most"
        );
    }
}

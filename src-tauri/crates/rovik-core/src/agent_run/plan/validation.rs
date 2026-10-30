//! A plan's validation commands (V02): the checks its "## Validation"
//! section names, which become the run's checks when it's implemented.

use super::steps::{is_fence, section};
use regex::Regex;
use std::sync::LazyLock;

/// Programs a validation command starts with: package managers, build
/// tools, test runners, interpreters (`node check.js`).
const CHECK_PROGRAMS: &[&str] = &[
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "node",
    "tsc",
    "eslint",
    "prettier",
    "vitest",
    "jest",
    "mocha",
    "playwright",
    "cargo",
    "go",
    "python",
    "python3",
    "pytest",
    "tox",
    "nox",
    "uv",
    "poetry",
    "mypy",
    "pyright",
    "ruff",
    "flake8",
    "pylint",
    "make",
    "just",
    "cmake",
    "ctest",
    "gradle",
    "mvn",
    "dotnet",
    "swift",
    "bundle",
    "rake",
    "rspec",
    "php",
    "composer",
    "phpunit",
    "mix",
    "sbt",
    "stack",
    "cabal",
    "zig",
    "bash",
    "sh",
];

/// Programs that check on their own (`pytest`), wherever they come.
const CHECKERS: &[&str] = &[
    "pytest", "mypy", "pyright", "ruff", "flake8", "pylint", "tox", "nox", "tsc", "eslint",
    "phpunit", "rspec", "ctest", "jest", "mocha", "vitest", "clippy",
];

/// Words that make a command a check (`npm run test:unit`, `cargo build`).
const CHECK_WORDS: &[&str] = &[
    "test",
    "tests",
    "check",
    "lint",
    "build",
    "typecheck",
    "clippy",
    "vet",
    "verify",
];

/// Words of commands that don't end on their own: a check that ran one
/// would wait for its timeout.
const RUNS_ON: &[&str] = &["dev", "start", "serve", "watch", "runserver", "preview"];

/// The commands of a plan's "## Validation" section, in order, each once:
/// the code spans (`` `cargo test` ``) and the lines of code blocks that
/// read as a check, not a file (`tests/test_x.py`), a function, or a step
/// to do by hand. They become an Autopilot task's checks when the project
/// has none (V02). Nothing when the plan has no such section.
pub fn validation_commands(plan: &str) -> Vec<String> {
    let Some(section) = section(plan, "validation") else {
        return Vec::new();
    };
    let mut spans: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in section.lines() {
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            spans.push(line.trim().to_string());
            continue;
        }
        spans.extend(CODE_SPAN.captures_iter(line).map(|c| c[1].to_string()));
    }
    let mut out: Vec<String> = Vec::new();
    for span in spans {
        let command = span.trim().trim_start_matches("$ ").trim().to_string();
        if is_validation_command(&command) && !out.contains(&command) {
            out.push(command);
        }
    }
    out
}

static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").unwrap());

/// Whether a span reads as a check to run (see `validation_commands`).
fn is_validation_command(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some(first) = words.first() else {
        return false;
    };
    let program = first.rsplit('/').next().unwrap_or(first).to_lowercase();
    let script = first.starts_with("./");
    if !CHECK_PROGRAMS.contains(&program.as_str()) && !script {
        return false;
    }
    // The pieces of each word: `test:unit`, `check.js`, `--watch`.
    let pieces: Vec<String> = words
        .iter()
        .flat_map(|w| w.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
        .collect();
    if pieces.iter().any(|p| RUNS_ON.contains(&p.as_str())) {
        return false;
    }
    // `cargo run`, `go run`: the program itself, not a check of it.
    if matches!(program.as_str(), "cargo" | "go") && words.get(1) == Some(&"run") {
        return false;
    }
    pieces
        .iter()
        .any(|p| CHECKERS.contains(&p.as_str()) || CHECK_WORDS.contains(&p.as_str()))
}

/// A validation command's kind, for the order the gate runs checks in
/// (type check, lint, tests, build).
pub fn validation_kind(command: &str) -> &'static str {
    let words: Vec<String> = command
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    if has("tsc") || has("typecheck") || has("type-check") || has("mypy") || has("pyright") {
        "typecheck"
    } else if has("lint")
        || has("clippy")
        || has("eslint")
        || has("ruff")
        || has("flake8")
        || has("pylint")
        || has("vet")
        || has("prettier")
    {
        "lint"
    } else if has("build") && !has("test") {
        "build"
    } else {
        "test"
    }
}

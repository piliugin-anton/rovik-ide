//! Plan mode and the agent's own bookkeeping tools:
//!
//!   - Plan mode investigates without changing anything and ends with a
//!     plan, saved with `submit_plan` as Markdown in `.rovik/plans/`, where it
//!     can be read, edited and implemented. A revised plan in the same chat
//!     updates the same file.
//!   - `ask_user` (Plan mode): questions only the user can answer. The run
//!     waits for the answer in the chat; it's the tool's result.
//!   - `todo` (Agent mode): the agent's checklist, shown read-only above the
//!     input. Each call writes the whole list, so the latest call in a run
//!     is the current state; the tool calls are the record.

use crate::chat_db::store::MessageRow;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::LazyLock;

pub const PLANS_DIR: &str = ".rovik/plans";

/// Tools Plan mode doesn't offer (or allow, if a model calls them anyway).
pub const PLAN_BLOCKED_TOOLS: &[&str] = &["write", "bash"];

/// Questions one run may ask; past that, the agent decides.
pub const MAX_QUESTIONS_PER_RUN: u32 = 3;

/// Added to the system prompt in Plan mode.
pub const PLAN_MODE_INSTRUCTIONS: &str = r###"## Plan mode

The user chose Plan mode: plan the task, don't carry it out. You can read files, list folders, search the code and use the web; you can't change files or run commands. The plan is a detailed, file-level spec the user reviews (and may edit) before it's built, so precision matters more than brevity: a precise, well-scoped plan builds cleanly.

1. Investigate first: read the relevant code, find the files that will change, the functions and types involved, and the existing patterns and conventions the change should follow.
2. Ask only what the code can't tell you and what really changes the design, the behavior or the scope (two existing approaches to choose between, an unclear requirement, a trade-off only the user can make). Use the ask_user tool, early, with all your questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything you can decide from the repository: decide it and say so in the plan. Never ask a question in plain text and stop; use ask_user.
3. Save the plan with the submit_plan tool, as Markdown. It's saved as a file the user may edit before implementing it, so make it stand on its own:
   - a "# " title;
   - a one-paragraph summary of the approach;
   - "## Steps": a numbered list, in order, one reviewable change per step. Each step has:
     - a short title;
     - the files it touches, as repo-relative paths in backticks (e.g. `src/db/users.rs`). Name only paths you have seen in the repository; mark files the step creates as "(new)";
     - what to add, change or remove in them, by name: the functions, methods, types, components, fields or settings, and how they behave. A one-line signature (e.g. `fn load_users(db: &Db) -> Result<Vec<User>>`) or a config line is welcome; leave out function bodies and longer code;
     - anything that calls or depends on the change and must be updated with it.
     Use as many steps as the task needs (usually 3 to 12);
   - "## Decisions": what the user answered, and what you decided yourself (with why);
   - "## Validation": how to check the result: the commands to run (build, typecheck, tests, lint), the tests to add or update, and what to try by hand;
   - "## Risks": risks or open points, if any.
   submit_plan checks the plan and rejects one without a title, numbered steps that each name a file and what changes there, or a validation section; fix what it reports and submit again.
   The plan goes in submit_plan, not in your reply: after submitting, reply with a few sentences at most. Never write the plan out in a reply, and never write it more than once.
4. If a plan already exists (it's shown below), the user's new message is about that plan. When they add context or ask for changes, revise the existing plan: keep what still holds, work the new information into the steps, decisions, validation and risks it affects, and submit the complete revised plan. Don't replace the plan with a summary of what the user said. If they only ask a question, answer it without submitting.
Keep it concrete enough that it can be implemented step by step without re-investigating. The user implements it by switching to Agent mode."###;

/*
 * ---------------------------------------------------------------------------
 * Plans
 * ---------------------------------------------------------------------------
 */

static STEPS_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b.*$").unwrap());
static VALIDATION_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^##\s+.*\b(verif\w*|validat\w*|test\w*|check\w*|acceptance)\b.*$").unwrap()
});
static NEXT_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^##\s").unwrap());
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s+\S").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#\s+\S").unwrap());
static CODE_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());
/// A path with a folder (`src/a.rs`, `src/components/`) or a file name
/// with an extension (`Cargo.toml`, `.env`).
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s`'"(\[*])(\.?[\w@~-][\w.@~-]*(?:/[\w.@~\[\]-]+)+/?|\.?[\w-]*[A-Za-z_][\w-]*\.[A-Za-z][\w]{0,9})"#).unwrap()
});
/// File names without an extension that are still clearly files.
static BARE_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(Makefile|Dockerfile|Justfile|Procfile|Gemfile|Rakefile|Containerfile)\b")
        .unwrap()
});
/// Words that say what a step does to its files.
static CHANGE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(add\w*|chang\w*|creat\w*|remov\w*|delet\w*|renam\w*|mov\w*|updat\w*|replac\w*|extend\w*|edit\w*|modif\w*|introduc\w*|implement\w*|refactor\w*|split\w*|extract\w*|merg\w*|wire\w*|register\w*|expos\w*|export\w*|import\w*|call\w*|return\w*|handl\w*|pass\w*|store\w*|emit\w*|drop\w*|switch\w*|convert\w*|define\w*|declare\w*|new|fix\w*|adjust\w*|insert\w*|append\w*|keep\w*|use|uses|using|set|sets|write\w*|read\w*|check\w*|validat\w*|test\w*|migrat\w*|document\w*|configur\w*|enabl\w*|disabl\w*|wrap\w*|guard\w*|inline\w*|generat\w*|render\w*|show\w*|hide\w*|load\w*|sav\w*|pars\w*|map\w*|send\w*|fetch\w*|accept\w*|reject\w*|limit\w*|cach\w*)\b",
    )
    .unwrap()
});

/// Whether a line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// The lines of `text` with their byte offsets and whether each is inside
/// a fenced code block (fence lines count as inside).
fn lines_with_fences(text: &str) -> Vec<(usize, &str, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut fence = false;
    for line in text.split_inclusive('\n') {
        let opens = is_fence(line);
        out.push((at, line, fence || opens));
        if opens {
            fence = !fence;
        }
        at += line.len();
    }
    out
}

/// The text of the `## ...` section that starts at `heading_end`: up to
/// the next `## ` heading outside a code block.
fn section_after(text: &str, heading_end: usize) -> &str {
    let after = &text[heading_end..];
    let end = lines_with_fences(after)
        .into_iter()
        .find(|(_, line, fenced)| !fenced && NEXT_HEADING.is_match(line))
        .map_or(after.len(), |(at, _, _)| at);
    &after[..end]
}

/// Whether a step names a file or folder of the repository.
fn names_a_file(step: &str) -> bool {
    if BARE_FILE.is_match(step) {
        return true;
    }
    PATH.captures_iter(step).any(|c| {
        let token = c[1].trim_end_matches('.');
        if token.contains('/') {
            // Not a URL, a fraction or "and/or".
            return !token.contains("://")
                && token.split('/').filter(|s| !s.is_empty()).count() >= 2
                && token.chars().any(|ch| ch.is_ascii_alphabetic())
                && !matches!(
                    token.to_ascii_lowercase().as_str(),
                    "and/or" | "either/or" | "i/o" | "n/a"
                );
        }
        // "e.g", "i.e", "vs.x": abbreviations, not file names.
        let (name, ext) = token.rsplit_once('.').unwrap_or((token, ""));
        !(name.trim_start_matches('.').len() <= 2 && ext.len() <= 2) && token.len() >= 3
    })
}

/// The numbered items of a steps section, each with its sub-lines: the
/// least indented numbered lines outside code blocks (a numbered list
/// nested in a step is part of that step).
fn split_steps(section: &str) -> Vec<&str> {
    let numbered: Vec<(usize, usize)> = lines_with_fences(section)
        .into_iter()
        .filter(|(_, line, fenced)| !fenced && NUMBERED.is_match(line) && !line.trim().is_empty())
        .map(|(at, line, _)| (at, line.len() - line.trim_start().len()))
        .collect();
    let indent = numbered.iter().map(|(_, i)| *i).min().unwrap_or(0);
    let starts: Vec<usize> = numbered
        .into_iter()
        .filter(|(_, i)| *i == indent)
        .map(|(at, _)| at)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| &section[start..starts.get(i + 1).copied().unwrap_or(section.len())])
        .collect()
}

/// "2", "2 and 4", "2, 4 and 5".
fn list_numbers(numbers: &[usize]) -> String {
    let words: Vec<String> = numbers.iter().map(usize::to_string).collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => words.join(""),
    }
}

/// What's wrong with a plan, if anything (empty: it's fine). Kept to the
/// minimum a plan needs to be built from (a title, steps that each name a
/// file and what changes there, a way to validate it), so a model doesn't
/// invent detail just to get past the check.
pub fn plan_problems(markdown: &str) -> Vec<String> {
    let text = markdown.trim();
    if text.is_empty() {
        return vec!["The plan is empty.".into()];
    }
    let mut problems = Vec::new();
    let first = text.split('\n').find(|l| !l.trim().is_empty());
    if !first.is_some_and(|l| TITLE.is_match(l)) {
        problems.push("Start with a \"# \" title line.".into());
    }
    let steps_heading = STEPS_HEADING.find(text);
    match steps_heading {
        None => {
            problems.push("Add a \"## Steps\" section with the steps as a numbered list.".into())
        }
        Some(heading) => {
            // Code fences can hold numbered lines and paths that aren't steps.
            let section = CODE_BLOCK.replace_all(section_after(text, heading.end()), "");
            let steps = split_steps(&section);
            if steps.len() < 2 {
                problems.push(
                    "List the steps under \"## Steps\" as a numbered list (1., 2., ...).".into(),
                );
            } else {
                let no_file: Vec<usize> = (1..=steps.len())
                    .filter(|&n| !names_a_file(steps[n - 1]))
                    .collect();
                if !no_file.is_empty() {
                    problems.push(format!(
                        "Step{} {} name{} no file: give the repo-relative path of each file a step changes, in backticks \
                         (e.g. `src/db/users.rs`), and mark files it creates as \"(new)\". Name only paths you've seen in the \
                         repository.",
                        if no_file.len() > 1 { "s" } else { "" },
                        list_numbers(&no_file),
                        if no_file.len() > 1 { "" } else { "s" },
                    ));
                }
                let vague: Vec<usize> = (1..=steps.len())
                    .filter(|&n| {
                        let step = steps[n - 1];
                        step.split_whitespace().count() < 6 || !CHANGE_WORD.is_match(step)
                    })
                    .collect();
                if !vague.is_empty() {
                    problems.push(format!(
                        "Step{} {} {} too vague: say what to add, change or remove in the files, by name (the functions, \
                         types, components or settings, and how they behave); a one-line signature is fine.",
                        if vague.len() > 1 { "s" } else { "" },
                        list_numbers(&vague),
                        if vague.len() > 1 { "are" } else { "is" },
                    ));
                }
            }
        }
    }
    let validation = VALIDATION_HEADING
        .find_iter(text)
        .find(|h| !steps_heading.is_some_and(|s| s.start() == h.start()))
        .map(|h| section_after(text, h.end()).trim());
    match validation {
        Some(section) if !section.is_empty() => {}
        _ => problems.push(
            "Add a \"## Validation\" section: the commands to run (build, typecheck, tests, lint), the tests to add or \
             update, and what to check by hand."
                .into(),
        ),
    }
    // Code belongs in the implementation, not the plan. Signatures and
    // config lines (up to 3 lines) are welcome and don't count.
    let (mut code_lines, mut longest) = (0usize, 0usize);
    for block in CODE_BLOCK.find_iter(text) {
        let lines = block.as_str().split('\n').count().saturating_sub(2);
        if lines <= 3 {
            continue;
        }
        code_lines += lines;
        longest = longest.max(lines);
    }
    if longest > 15 || code_lines > 40 {
        problems.push(
            "Leave the code out: say what changes and where, in words; short snippets (a signature, a config line) are fine."
                .into(),
        );
    }
    if text.chars().count() > 40_000 {
        problems.push("Shorten it: the plan is over 40,000 characters.".into());
    }
    problems
}

/// "2026-10-04-add-a-users-table" from the request.
fn file_stem(request: &str, date: &str) -> String {
    let lower = request.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .take(7)
        .map(str::to_string)
        .collect();
    let slug: String = words.join("-").chars().take(60).collect();
    format!("{date}-{}", if slug.is_empty() { "plan" } else { &slug })
}

/// `root` and `rel` (written with `/`) as one path in the OS's own form: the
/// file tree and editor tabs name files that way, so `C:\p/.rovik/plans`
/// would open as a second tab of the same file on Windows.
fn join(root: &str, rel: &str) -> String {
    let mut path = std::path::PathBuf::from(root);
    path.extend(rel.split('/'));
    path.to_string_lossy().into_owned()
}

/// Saves a plan and returns its path. `existing`: the chat's earlier plan
/// file, which a revision replaces; otherwise a new name that's free.
pub fn save_plan_file(
    root: &str,
    request: &str,
    plan: &str,
    existing: Option<&str>,
    date: &str,
) -> Result<String, String> {
    let path = match existing {
        Some(p) => p.to_string(),
        None => {
            let stem = file_stem(request, date);
            let mut path = join(root, &format!("{PLANS_DIR}/{stem}.md"));
            let mut n = 2;
            while Path::new(&path).exists() && n < 100 {
                path = join(root, &format!("{PLANS_DIR}/{stem}-{n}.md"));
                n += 1;
            }
            path
        }
    };
    let body = plan.trim();
    // A title, if the plan doesn't start with one.
    let titled = if body.starts_with("# ") || body.starts_with("#\t") {
        body.to_string()
    } else {
        let first: String = request
            .split('\n')
            .next()
            .unwrap_or("")
            .chars()
            .take(100)
            .collect();
        format!("# Plan: {first}\n\n{body}")
    };
    crate::filesystem::write_app_file(&path, &format!("{titled}\n"))?;
    Ok(path)
}

/// The plan's path for display: relative to the project.
pub fn relative_plan_path(path: &str, root: Option<&str>) -> String {
    let Some(root) = root else {
        return path.to_string();
    };
    let r = root.trim_end_matches(['/', '\\']);
    match path.strip_prefix(r) {
        Some(rest) if !rest.is_empty() => rest[1..].to_string(),
        _ => path.to_string(),
    }
}

/// Plan mode: the current plan (as the file is now), for the model to revise.
pub fn current_plan_context(plan_file: Option<&str>, root: Option<&str>) -> String {
    let Some(file) = plan_file else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(file) else {
        return String::new();
    };
    if text.trim().is_empty() {
        return String::new();
    }
    [
        "## The current plan".to_string(),
        format!(
            "Saved in {}; the user may have edited it. This is the version to revise.",
            relative_plan_path(file, root)
        ),
        String::new(),
        "<current_plan>".into(),
        text.trim().to_string(),
        "</current_plan>".into(),
    ]
    .join("\n")
}

static LOOKS_LIKE_STEPS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b").unwrap());
static NUMBERED_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s").unwrap());

/// A reply that reads like a plan (written out instead of submitted).
pub fn looks_like_plan(text: &str) -> bool {
    LOOKS_LIKE_STEPS.is_match(text) && NUMBERED_LINE.is_match(text)
}

/*
 * ---------------------------------------------------------------------------
 * A plan's structure (D01) and the checks against the project (B07)
 * ---------------------------------------------------------------------------
 */

/// A file a plan step names.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFile {
    /// Repo-relative, as written (a folder ends with `/`).
    pub path: String,
    /// Marked "(new)": the step creates it.
    pub new: bool,
    /// Marked "(read)" or "(read-only)": context the step uses but doesn't
    /// change (F03: what a contract step settles).
    pub read_only: bool,
}

/// One numbered step of a plan's "## Steps" section.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// As numbered in the plan.
    pub number: usize,
    pub title: String,
    /// The files it names, each once, in order.
    pub files: Vec<PlanFile>,
    /// The whole step, its first line included.
    pub body: String,
    /// `Group: <name>`: steps that may be built side by side (F03).
    pub group: Option<String>,
}

static STEP_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)[.)]\s+(.*)$").unwrap());
static BACKTICKED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").unwrap());
static NEW_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:new|new file|created?)\)").unwrap());
static READ_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:read|read-only|readonly|reads|context)\)").unwrap());
static GROUP_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[\s>*-]*\**group\**\s*:\s*\**\s*([^*\n]+?)\s*\**\s*$").unwrap()
});

/// Extensions a backticked name must have to count as a file, so that
/// `state.config` or `self.items` isn't taken for one.
const FILE_EXTENSIONS: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "json",
    "jsonc",
    "toml",
    "yaml",
    "yml",
    "md",
    "mdx",
    "txt",
    "css",
    "scss",
    "sass",
    "less",
    "html",
    "htm",
    "vue",
    "svelte",
    "py",
    "pyi",
    "go",
    "mod",
    "sum",
    "java",
    "kt",
    "kts",
    "gradle",
    "swift",
    "m",
    "mm",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "csproj",
    "sln",
    "rb",
    "php",
    "lua",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "bat",
    "cmd",
    "sql",
    "graphql",
    "gql",
    "proto",
    "xml",
    "ini",
    "cfg",
    "conf",
    "env",
    "lock",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "mli",
    "fs",
    "fsx",
    "scala",
    "clj",
    "cljs",
    "elm",
    "zig",
    "nim",
    "r",
    "jl",
    "tf",
    "hcl",
    "nix",
    "svg",
    "png",
    "jpg",
    "ico",
    "wasm",
    "vert",
    "frag",
    "wgsl",
    "glsl",
    "astro",
    "prisma",
    "pl",
    "pm",
    "tex",
    "rst",
    "adoc",
    "csv",
    "tsv",
    "makefile",
    "dockerfile",
];

/// The repo-relative path a backticked token names, if it's one.
fn as_path(token: &str) -> Option<String> {
    let t = token.trim().trim_start_matches("./");
    if t.is_empty()
        || t.contains(char::is_whitespace)
        || t.contains("://")
        || t.contains([
            '(', ')', '<', '>', '{', '}', '*', '?', '$', '=', ',', ';', '"', '\'', '|',
        ])
        || t.starts_with('/')
        || t.starts_with('-')
        || t.starts_with('@')
        || (t.len() > 1 && t.as_bytes()[1] == b':')
    {
        return None;
    }
    // `path:line` points into a file.
    let t = t.split(':').next().unwrap_or(t);
    let name = t.trim_end_matches('/').rsplit('/').next().unwrap_or(t);
    let folder = t.ends_with('/') || t.trim_end_matches('/').contains('/');
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    let named_file = BARE_FILE.is_match(name) && name.len() == BARE_FILE.find(name)?.len();
    let known = ext.as_deref().is_some_and(|e| FILE_EXTENSIONS.contains(&e))
        || named_file
        || (name.starts_with('.') && name.len() > 2 && !name[1..].contains('.'));
    // `a::b` paths, `x.y` member access: not files.
    if t.contains("::") {
        return None;
    }
    (known || (folder && t.ends_with('/')) || (folder && ext.is_none() && t.contains('/')))
        .then(|| t.to_string())
}

fn step_title(first_line: &str) -> String {
    let t = first_line.trim().trim_matches('*').trim();
    let t = t.split(" — ").next().unwrap_or(t);
    t.trim_end_matches(':').trim_matches('*').trim().to_string()
}

/// D01: the plan's steps, in order (empty when it has no steps section).
pub fn steps(markdown: &str) -> Vec<PlanStep> {
    let text = markdown.trim();
    let Some(heading) = STEPS_HEADING.find(text) else {
        return Vec::new();
    };
    // Code blocks stay in a step's body (an exact signature, a schema).
    let section = section_after(text, heading.end());
    split_steps(section)
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            // NUMBERED's `^\s*` can start at the blank line before a step.
            let raw = raw.trim_start();
            let first = raw.lines().next().unwrap_or("");
            let (number, rest) = STEP_NUMBER
                .captures(first)
                .map(|c| (c[1].parse().unwrap_or(i + 1), c[2].to_string()))
                .unwrap_or((i + 1, first.to_string()));
            let mut files: Vec<PlanFile> = Vec::new();
            // Paths in code blocks are code, not the step's files.
            for (_, line, _) in lines_with_fences(raw)
                .into_iter()
                .filter(|(_, _, fenced)| !fenced)
            {
                let new = NEW_MARK.is_match(line);
                let read_only = READ_MARK.is_match(line);
                for c in BACKTICKED.captures_iter(line) {
                    if let Some(path) = as_path(&c[1]) {
                        match files.iter_mut().find(|f| f.path == path) {
                            Some(f) => {
                                f.new |= new;
                                // Changed anywhere in the step: not read-only.
                                f.read_only &= read_only;
                            }
                            None => files.push(PlanFile {
                                path,
                                new,
                                read_only,
                            }),
                        }
                    }
                }
            }
            PlanStep {
                number,
                title: step_title(&rest),
                files,
                body: raw.trim_end().to_string(),
                group: GROUP_LINE
                    .captures(raw)
                    .map(|c| c[1].trim().trim_matches('`').to_string())
                    .filter(|g| !g.is_empty()),
            }
        })
        .collect()
}

#[allow(dead_code)] // D03's worker brief reads "## Decisions" with it.
/// D01: the text of the plan's `## <name>` section (a heading that starts
/// with the name, case aside), without its heading.
pub fn section(markdown: &str, name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let mut at = 0;
    while let Some(h) = NEXT_HEADING.find(&markdown[at..]) {
        let start = at + h.start();
        let line_end = markdown[start..]
            .find('\n')
            .map_or(markdown.len(), |e| start + e);
        let title = markdown[start + 2..line_end].trim().to_lowercase();
        if title.starts_with(&lower) {
            return Some(section_after(markdown, line_end).trim().to_string());
        }
        at = line_end;
    }
    None
}

/// B07: what's wrong with a plan against the project, beyond its form:
/// files that aren't there, a file used before the step that creates it,
/// and steps of one group that share a file.
pub fn project_problems(markdown: &str, root: &Path) -> Vec<String> {
    let steps = steps(markdown);
    let mut problems = Vec::new();
    // Created by which step.
    let created: Vec<(&str, usize)> = steps
        .iter()
        .flat_map(|s| {
            s.files
                .iter()
                .filter(|f| f.new)
                .map(move |f| (f.path.as_str(), s.number))
        })
        .collect();
    let exists = |path: &str| root.join(path.trim_end_matches('/')).exists();
    let mut missing: Vec<String> = Vec::new();
    for step in &steps {
        for file in step.files.iter().filter(|f| !f.new) {
            let creator = created
                .iter()
                .find(|(p, _)| p.trim_end_matches('/') == file.path.trim_end_matches('/'))
                .map(|(_, n)| *n);
            match creator {
                Some(n) if n > step.number && !exists(&file.path) => problems.push(format!(
                    "Step {} uses `{}`, which step {n} creates: put the steps in an order where a file is created before it's used.",
                    step.number, file.path
                )),
                Some(_) => {}
                None if !exists(&file.path) && !missing.contains(&file.path) => {
                    missing.push(file.path.clone());
                    problems.push(format!(
                        "Step {} names `{}`, which isn't in the project: fix the path, or mark it \"(new)\" if the step creates it.",
                        step.number, file.path
                    ));
                }
                None => {}
            }
        }
    }
    // A group's steps run side by side: they can't share a file.
    let mut groups: Vec<&str> = steps.iter().filter_map(|s| s.group.as_deref()).collect();
    groups.dedup();
    let mut seen_groups: Vec<&str> = Vec::new();
    for group in groups {
        if seen_groups.contains(&group) {
            continue;
        }
        seen_groups.push(group);
        let members: Vec<&PlanStep> = steps
            .iter()
            .filter(|s| s.group.as_deref() == Some(group))
            .collect();
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                // Files both change: read-only ones are the contract's.
                let shared: Vec<&str> = a
                    .files
                    .iter()
                    .filter(|f| !f.read_only)
                    .filter(|f| b.files.iter().any(|g| g.path == f.path && !g.read_only))
                    .map(|f| f.path.as_str())
                    .collect();
                if !shared.is_empty() {
                    problems.push(format!(
                        "Steps {} and {} are both in group \"{group}\" but both name {}: steps in a group are built side by side, \
                         so they can't share a file. Move the shared change into a step before the group, or take one of the steps out of it.",
                        a.number,
                        b.number,
                        shared.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
        // F03: a file the group's steps use as shared context is settled
        // first, by a contract step before the group that changes it.
        let first = members.iter().map(|s| s.number).min().unwrap_or(0);
        let mut context: Vec<&str> = Vec::new();
        for f in members
            .iter()
            .flat_map(|s| s.files.iter().filter(|f| f.read_only))
        {
            let users = members
                .iter()
                .filter(|s| s.files.iter().any(|g| g.path == f.path))
                .count();
            if users >= 2 && !context.contains(&f.path.as_str()) {
                context.push(&f.path);
            }
        }
        for path in context {
            let settled = steps.iter().any(|s| {
                s.number < first
                    && s.group.as_deref() != Some(group)
                    && s.files.iter().any(|f| f.path == path && !f.read_only)
            });
            if !settled {
                problems.push(format!(
                    "The steps of group \"{group}\" all build on `{path}`, but no step before the group settles it: add a contract step \
                     before the group that adds the shared types, signatures or names there (with stub bodies), so the group's steps \
                     work against the same interface."
                ));
            }
        }
    }
    problems
}

/// B07b: the note the agent gets when the plan it's asked to implement
/// has problems (the user may have edited it after it was checked).
pub fn plan_check_note(file: &str, problems: &[String]) -> String {
    format!(
        "[Automatic check by the IDE, not written by the user]\nThe plan file {file} was checked as it is now (it may have been edited since it was written), and the checker found:\n- {}\n\nCheck each against the code before you start. Where the plan is wrong, follow the code and the user's intent, and say what you changed from the plan when you finish.",
        problems.join("\n- ")
    )
}

/// `plan_problems` and, with a project, `project_problems`.
pub fn plan_problems_in(markdown: &str, root: Option<&Path>) -> Vec<String> {
    let mut problems = plan_problems(markdown);
    if let Some(root) = root {
        problems.extend(project_problems(markdown, root));
    }
    problems
}

/*
 * ---------------------------------------------------------------------------
 * Questions (ask_user)
 * ---------------------------------------------------------------------------
 */

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recommended: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub options: Vec<QuestionOption>,
    /// "Other" with the user's own words.
    pub allow_custom: bool,
}

/// One question's answer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum QuestionAnswer {
    Option {
        label: String,
    },
    Custom {
        text: String,
    },
    /// "You decide".
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum QuestionsOutcome {
    Answers {
        answers: Vec<QuestionAnswer>,
    },
    /// Typed in the composer instead of answering in the card.
    Reply {
        reply: String,
    },
    Stopped {
        stopped: bool,
    },
}

const MAX_QUESTIONS: usize = 3;
const MAX_OPTIONS: usize = 6;

fn trimmed(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The questions from the tool's arguments, tolerating the shapes models
/// produce: { questions: [...] }, or one question at the top level; options
/// as objects or plain strings.
pub fn parse_questions(args: &Map<String, Value>) -> Vec<Question> {
    let top = Value::Object(args.clone());
    let raw: Vec<&Value> = match args.get("questions").and_then(|q| q.as_array()) {
        Some(list) => list.iter().collect(),
        None => vec![&top],
    };
    let mut out = Vec::new();
    for q in raw.into_iter().take(MAX_QUESTIONS) {
        let Some(r) = q.as_object() else { continue };
        let Some(question) = trimmed(r.get("question")) else {
            continue;
        };
        let options: Vec<QuestionOption> = r
            .get("options")
            .and_then(|o| o.as_array())
            .into_iter()
            .flatten()
            .filter_map(|o| match o {
                Value::String(s) => {
                    let label = s.trim();
                    (!label.is_empty()).then(|| QuestionOption {
                        label: label.into(),
                        description: None,
                        recommended: false,
                    })
                }
                Value::Object(opt) => trimmed(opt.get("label")).map(|label| QuestionOption {
                    label,
                    description: opt
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(str::to_string),
                    recommended: opt.get("recommended") == Some(&Value::Bool(true)),
                }),
                _ => None,
            })
            .take(MAX_OPTIONS)
            .collect();
        // Without options the only way to answer is in words.
        let allow_custom = r.get("allow_custom") != Some(&Value::Bool(false)) || options.is_empty();
        out.push(Question {
            question,
            reason: trimmed(r.get("reason")),
            options,
            allow_custom,
        });
    }
    out
}

/// What the model reads as the tool's result.
pub fn describe_outcome(questions: &[Question], outcome: &QuestionsOutcome) -> String {
    match outcome {
        QuestionsOutcome::Stopped { .. } => "Error: the user stopped the task instead of answering.".into(),
        QuestionsOutcome::Reply { reply } => format!(
            "The user replied in their own words instead of picking options:\n\n{reply}\n\n\
             Treat this as their answer to your questions. For anything it leaves open, decide yourself and say so in the plan."
        ),
        QuestionsOutcome::Answers { answers } => {
            let mut lines = vec!["The user answered:".to_string()];
            for (i, q) in questions.iter().enumerate() {
                let answer = match answers.get(i).unwrap_or(&QuestionAnswer::Agent) {
                    QuestionAnswer::Option { label } => label.clone(),
                    QuestionAnswer::Custom { text } => format!("\"{text}\" (in their own words)"),
                    QuestionAnswer::Agent => {
                        "left it to you: decide, and say what you chose and why in the plan".to_string()
                    }
                };
                lines.push(format!("{}. {}\n   → {answer}", i + 1, q.question));
            }
            lines.push(String::new());
            lines.push("Treat these answers as requirements. Continue planning.".into());
            lines.join("\n")
        }
    }
}

/// How each was answered, for the agent log.
pub fn outcome_kinds(outcome: &QuestionsOutcome) -> Vec<String> {
    match outcome {
        QuestionsOutcome::Answers { answers } => answers
            .iter()
            .map(|a| match a {
                QuestionAnswer::Option { .. } => "option",
                QuestionAnswer::Custom { .. } => "custom",
                QuestionAnswer::Agent => "agent",
            })
            .map(str::to_string)
            .collect(),
        QuestionsOutcome::Reply { .. } => vec!["reply".into()],
        QuestionsOutcome::Stopped { .. } => vec!["stopped".into()],
    }
}

/*
 * ---------------------------------------------------------------------------
 * The checklist (todo)
 * ---------------------------------------------------------------------------
 */

const TODO_STATUSES: &[&str] = &["pending", "in_progress", "done", "skipped", "changed"];
pub const MAX_TODO_ITEMS: usize = 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub title: String,
    pub status: String,
}

pub fn parse_todo(args: &Map<String, Value>) -> Vec<TodoItem> {
    args.get("items")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
        .take(MAX_TODO_ITEMS)
        .filter_map(|r| match r {
            Value::String(s) => {
                let title = s.trim();
                (!title.is_empty()).then(|| TodoItem {
                    title: title.into(),
                    status: "pending".into(),
                })
            }
            Value::Object(o) => trimmed(o.get("title")).map(|title| {
                let status = o
                    .get("status")
                    .and_then(|s| s.as_str())
                    .filter(|s| TODO_STATUSES.contains(s))
                    .unwrap_or("pending");
                TodoItem {
                    title,
                    status: status.into(),
                }
            }),
            _ => None,
        })
        .collect()
}

/// Finished: done, or skipped/changed with the reason given.
pub fn is_settled(item: &TodoItem) -> bool {
    matches!(item.status.as_str(), "done" | "skipped" | "changed")
}

/// A checklist the model can't be acting on: more than one step claimed as
/// current. None when the list is usable (zero in progress is fine: between
/// steps, or all settled). An error string keeps the model from drifting into
/// parallel work without adding new concepts to the prompt.
pub fn todo_problems(items: &[TodoItem]) -> Option<String> {
    let active: Vec<&TodoItem> = items.iter().filter(|i| i.status == "in_progress").collect();
    if active.len() > 1 {
        let names = active
            .iter()
            .map(|i| format!("\"{}\"", i.title))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(format!(
            "Error: only one step can be in_progress at a time (you marked {names}). \
             Keep the step you're on in_progress and the rest pending, then call todo again."
        ));
    }
    None
}

/// What the model reads after an update.
pub fn describe_todo(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Error: the checklist is empty. Pass items: [{ title, status }].".into();
    }
    if let Some(problem) = todo_problems(items) {
        return problem;
    }
    let done = items.iter().filter(|i| is_settled(i)).count();
    let current = items.iter().find(|i| i.status == "in_progress");
    format!(
        "Checklist updated: {done} of {} done{}.",
        items.len(),
        current
            .map(|c| format!("; now: {}", c.title))
            .unwrap_or_default()
    )
}

/// Whether a stored message is one the IDE wrote (check failures, nudges).
pub fn is_harness(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| meta.get("harness"))
        .is_some_and(|h| !h.is_null())
}

/// Whether the user dismissed a checklist here (`meta.checklistClosed`):
/// on the message holding its `todo` call (the panel's dismiss ✕), or, as
/// older chats have it, on the user message that followed it.
pub fn is_checklist_dismissed(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| {
            meta.get("checklistClosed")
                .or_else(|| meta.get("checklist_closed"))
        })
        .is_some_and(|v| v == &Value::Bool(true))
}

/// The latest `todo` call in one message, if it parses to a non-empty list.
fn todo_in_message(m: &MessageRow) -> Option<Vec<TodoItem>> {
    // A step worker's result: the checklist with its step marked done (D04).
    if let Some(items) = m
        .meta
        .as_ref()
        .and_then(|meta| meta.get("checklistUpdate"))
        .and_then(|v| serde_json::from_value::<Vec<TodoItem>>(v.clone()).ok())
        .filter(|items| !items.is_empty())
    {
        return Some(items);
    }
    let call = m
        .tool_calls
        .iter()
        .flatten()
        .rev()
        .find(|c| c.name == "todo")?;
    if let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) {
        let items = parse_todo(&args);
        if !items.is_empty() {
            return Some(items);
        }
    }
    None
}

/// The checklist the chat is currently working from.
///
/// - `carried == false`: the latest `todo` call in the current request
///   (since the last real user message).
/// - `carried == true`: the current request made no `todo` call, so the
///   latest open checklist from the previous request is carried forward. It
///   counts as closed (and is not carried) when every step is settled or the
///   user dismissed it.
///
/// A dismissed checklist (`meta.checklistClosed` on the message holding the
/// `todo` call) is closed wherever it is: nothing is current until a newer
/// `todo` call.
///
/// Looking back past exactly one user message (to the latest `todo` in the
/// previous request only) keeps a checklist alive across a short follow-up
/// like "continue" that never makes a new `todo` call. A second user
/// message stops the search, so an old checklist doesn't linger across
/// unrelated work. A newer `todo` call replaces it, and the carry note
/// tells the model to close the old one when the new message is a different
/// task.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentChecklist {
    pub items: Vec<TodoItem>,
    pub carried: bool,
    pub from_message_id: Option<String>,
}

pub fn current_checklist(messages: &[MessageRow]) -> Option<CurrentChecklist> {
    let last_user = messages
        .iter()
        .rposition(|m| m.role == "user" && !is_harness(m));
    let Some(last) = last_user else {
        // No user message yet: whatever the latest todo call says.
        for m in messages.iter().rev() {
            if let Some(items) = todo_in_message(m) {
                if is_checklist_dismissed(m) {
                    return None;
                }
                return Some(CurrentChecklist {
                    items,
                    carried: false,
                    from_message_id: Some(m.id.clone()),
                });
            }
        }
        return None;
    };
    // The current request: everything after the last real user message.
    for m in messages[last + 1..].iter().rev() {
        if let Some(items) = todo_in_message(m) {
            if is_checklist_dismissed(m) {
                return None;
            }
            return Some(CurrentChecklist {
                items,
                carried: false,
                from_message_id: Some(m.id.clone()),
            });
        }
    }
    // No checklist in the current request: a dismissal closes it.
    if is_checklist_dismissed(&messages[last]) {
        return None;
    }
    // The previous request only: past the last user message, up to the one
    // before it. A second user message stops the search, so an old
    // checklist doesn't linger across unrelated work.
    let prev_user = messages[..last]
        .iter()
        .rposition(|m| m.role == "user" && !is_harness(m));
    let from = prev_user.map(|p| p + 1).unwrap_or(0);
    for m in messages[from..last].iter().rev() {
        if let Some(items) = todo_in_message(m) {
            if items.iter().all(is_settled) || is_checklist_dismissed(m) {
                return None;
            }
            return Some(CurrentChecklist {
                items,
                carried: true,
                from_message_id: Some(m.id.clone()),
            });
        }
    }
    None
}

/// Past these, a task without a checklist has clearly outgrown the "3 or
/// more steps" rule (the checklist gate, Agent mode): a write to a 3rd
/// distinct file, a command after 2 changed files, or this many tool calls.
pub const GATE_FILES: usize = 3;
pub const GATE_FILES_BEFORE_COMMAND: usize = 2;
pub const GATE_TOOL_CALLS: u32 = 12;

/// Why the gate asks for a checklist before this call, if it does. Only
/// work counts (writes and commands); `changed_files` are the distinct
/// project files written so far in the run. Below the thresholds a small
/// task finishes without a checklist, as the rule allows.
pub fn checklist_gate_reason(
    tool: &str,
    path: Option<&str>,
    changed_files: &[String],
    tool_calls: u32,
) -> Option<String> {
    let norm = |p: &str| p.trim().trim_start_matches("./").to_string();
    match tool {
        "write" => {
            let new_file = path
                .map(norm)
                .is_some_and(|p| !changed_files.iter().any(|f| norm(f) == p));
            let files = changed_files.len() + usize::from(new_file);
            if files >= GATE_FILES {
                return Some(format!("{files} files changed"));
            }
        }
        "bash" => {
            if changed_files.len() >= GATE_FILES_BEFORE_COMMAND {
                return Some(format!(
                    "{} files changed, now a command",
                    changed_files.len()
                ));
            }
        }
        _ => return None,
    }
    (tool_calls >= GATE_TOOL_CALLS).then(|| format!("{tool_calls} tool calls made"))
}

/// The gate's refusal: what to put in the checklist, finished work included.
pub fn checklist_gate_error(reason: &str, tool: &str) -> String {
    format!(
        "Error: this task has grown past a few steps ({reason}) and has no checklist. Create it now with the \
         todo tool: the steps you've already finished as done, the current one in_progress, the rest pending. \
         Then repeat this {tool} call."
    )
}

/// Steps still open, for carry notes and nudges.
pub fn open_steps(items: &[TodoItem]) -> Vec<&TodoItem> {
    items.iter().filter(|i| !is_settled(i)).collect()
}

/// The app's own message carrying an unfinished checklist into a new
/// request: lists what is left and says what to do whether the message
/// continues that work or starts a new task. An app message (harness), so
/// it never starts a new request itself.
pub fn carry_note(items: &[TodoItem]) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = format!(
        "[Automatic note from the IDE, not written by the user]\nUnfinished checklist from earlier work ({done} of {} done):",
        items.len()
    );
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note.push_str(
        "\nIf this message continues that work, mark the first unfinished step in_progress with todo and go on. \
         If it is a new task, either mark the old steps skipped (with the reason) or changed, or create a new checklist with todo.",
    );
    note
}

/// The app's own message resuming an unfinished checklist on Continue:
/// the remaining steps, with the first unfinished step to pick up. An app
/// message (harness), so it never starts a new request itself. No user
/// message is created, so nothing gets lost.
pub fn continue_note(items: &[TodoItem]) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = format!(
        "[Automatic note from the IDE, not written by the user]\nContinuing the unfinished checklist ({done} of {} done) - pick up the first unfinished step with todo and go on:",
        items.len()
    );
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note
}

/// The checklist as repeated on every CLM request (right after the task
/// card): the app shows it, so the model keeps working from it without
/// copying it into the card. Trimming mode keeps using `focus_note`.
pub fn checklist_note(items: &[TodoItem], carried: bool) -> String {
    let done = items.iter().filter(|i| is_settled(i)).count();
    let mut note = if carried {
        format!(
            "[your checklist, carried from earlier work ({done} of {} done) - the panel shows it, so don't copy it into the task card:]",
            items.len()
        )
    } else {
        format!(
            "[your checklist ({done} of {} done) - the panel shows it, so don't copy it into the task card:]",
            items.len()
        )
    };
    for item in items {
        note.push_str(&format!("\n- [{}] {}", item.status, item.title));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lists_belong_to_their_step_and_code_blocks_stay() {
        let plan = "# API\n\nTwo parts.\n\n## Steps\n\n\
            1. **API**\n   1. Add `src/api/users.rs` (new).\n   2. Register it in `src/api/mod.rs`.\n\
            2. **Pages**\n   - `src/pages.rs`: render it, with exactly this signature:\n\n     ```rust\n     \
            ## not a heading\n     pub fn page(id: u32) -> Html { todo!(`src/other.rs`) }\n     ```\n\
            3. **Docs**\n   - `README.md`: say so.\n\n## Validation\n- `cargo test`\n";
        let s = steps(plan);
        let numbers: Vec<usize> = s.iter().map(|x| x.number).collect();
        assert_eq!(numbers, [1, 2, 3], "{s:#?}");
        assert_eq!(s[0].title, "API");
        let files = |i: usize| {
            s[i].files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(files(0), ["src/api/users.rs", "src/api/mod.rs"]);
        // The code stays in the body; its paths aren't the step's files.
        assert!(s[1].body.contains("pub fn page(id: u32)"), "{}", s[1].body);
        assert_eq!(files(1), ["src/pages.rs"]);
        // A `## ` line in a code block doesn't end the steps.
        assert_eq!(files(2), ["README.md"]);
    }
    use serde_json::json;

    const GOOD_PLAN: &str = "# Add users\n\nSummary.\n\n## Steps\n\
        1. Users table\n   - `migrations/002_users.sql` (new): create the `users` table with `id`, `email`.\n\
        2. API\n   - `src/api/users.rs`: add `fn list_users(db: &Db) -> Vec<User>` and register it in `src/api/mod.rs`.\n\n\
        ## Decisions\n- none\n\n## Validation\n- `cargo test`\n- open /users by hand";

    /// A plan as Plan mode writes them: bold titles, sub-bullets, a code
    /// span that isn't a file, a member access, a `path:line`, a folder.
    const REAL_PLAN: &str = "# Add a users page\n\nServe users from the API and list them.\n\n\
        ## Steps\n\n\
        1. **Users table**\n   - `migrations/002_users.sql` (new): create `users` with `id`, `email`.\n   - Register it in `migrations/README.md`.\n\n\
        2. **List users in the API** — the backend part\n   - `src/api/users.rs` (new): add `fn list_users(db: &Db) -> Result<Vec<User>>`; read `state.config` for the page size.\n   - `src/api/mod.rs:12`: register the route.\n   - Group: backend\n\n\
        3. **Page**\n   - `src/pages/Users.tsx` (new) under `src/pages/`: render the list; call `api.listUsers()`.\n   - Group: backend\n\n\
        ## Decisions\n- Paginate at 50 (the existing default).\n- Keep `users` in SQLite.\n\n\
        ## Validation\n- `cargo test`\n- open /users by hand\n";

    #[test]
    fn steps_parse_into_number_title_files_and_group() {
        let s = steps(REAL_PLAN);
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].number, s[0].title.as_str()), (1, "Users table"));
        assert_eq!(s[1].title, "List users in the API");
        assert_eq!(s[2].title, "Page");
        let files = |i: usize| {
            s[i].files
                .iter()
                .map(|f| (f.path.as_str(), f.new))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            files(0),
            [
                ("migrations/002_users.sql", true),
                ("migrations/README.md", false)
            ]
        );
        // Not files: the signature, `state.config`, `users`; a path:line is its file.
        assert_eq!(
            files(1),
            [("src/api/users.rs", true), ("src/api/mod.rs", false)]
        );
        assert_eq!(
            files(2),
            [("src/pages/Users.tsx", true), ("src/pages/", true)]
        );
        assert_eq!(s[1].group.as_deref(), Some("backend"));
        assert_eq!(s[0].group, None);
        assert!(s[1].body.contains("register the route"));
        assert!(
            !s[0].body.contains("List users"),
            "a step ends where the next begins"
        );

        assert_eq!(
            section(REAL_PLAN, "Decisions").as_deref(),
            Some("- Paginate at 50 (the existing default).\n- Keep `users` in SQLite.")
        );
        assert_eq!(
            section(REAL_PLAN, "validation").as_deref(),
            Some("- `cargo test`\n- open /users by hand")
        );
        assert_eq!(section(REAL_PLAN, "Risks"), None);
        // The checker's own fixture.
        let g = steps(GOOD_PLAN);
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[1].files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["src/api/users.rs", "src/api/mod.rs"]
        );
        assert!(steps("# No steps\n\nJust text.").is_empty());
    }

    #[test]
    fn what_counts_as_a_file() {
        for yes in [
            "src/a.rs",
            "Cargo.toml",
            "./src/b.ts",
            "src/components/",
            "Dockerfile",
            ".env",
            "src/a.rs:10-20",
            "docs/design",
        ] {
            assert!(as_path(yes).is_some(), "{yes}");
        }
        for no in [
            "fn a(x: u8)",
            "state.config",
            "self.items",
            "users",
            "std::fs::read",
            "https://x.io/a.js",
            "/etc/hosts",
            "--flag",
            "a.b.c()",
            "*.ts",
            "@context/TASK.md",
            "C:/x.rs",
            "e.g",
        ] {
            assert!(as_path(no).is_none(), "{no}");
        }
    }

    struct Tree(std::path::PathBuf);
    impl Tree {
        fn new(files: &[&str]) -> Tree {
            let dir = std::env::temp_dir().join(format!("plan-tree-{}", uuid::Uuid::new_v4()));
            for f in files {
                let p = dir.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, "x").unwrap();
            }
            std::fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn plans_are_checked_against_the_project() {
        let tree = Tree::new(&["migrations/README.md", "src/api/mod.rs"]);
        // Every file exists or is new: only the group shares a file? No:
        // steps 2 and 3 name different files.
        assert_eq!(project_problems(REAL_PLAN, &tree.0), Vec::<String>::new());

        // A missing path, reported once.
        let missing = REAL_PLAN.replace("`src/api/mod.rs:12`", "`src/api/routes.rs`");
        let p = project_problems(&missing, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 2 names `src/api/routes.rs`, which isn't in the project"),
            "{p:?}"
        );

        // Used before the step that creates it.
        let early = REAL_PLAN.replace(
            "Register it in `migrations/README.md`.",
            "Use the helper in `src/api/users.rs`.",
        );
        let p = project_problems(&early, &tree.0);
        assert_eq!(p, ["Step 1 uses `src/api/users.rs`, which step 2 creates: put the steps in an order where a file is created before it's used."]);

        // Two steps of a group naming one file.
        let shared = REAL_PLAN.replace("call `api.listUsers()`", "register it in `src/api/mod.rs`");
        let p = project_problems(&shared, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with(
                "Steps 2 and 3 are both in group \"backend\" but both name `src/api/mod.rs`"
            ),
            "{p:?}"
        );
        // F03: shared context needs a contract step before the group.
        let contract = "# C\n\n## Steps\n\n\
            1. **Contract**\n   - `src/api/mod.rs`: declare `fn list_users()` and `fn add_user()` with stub bodies.\n\
            2. **List**\n   - `src/api/users.rs` (new): implement `list_users`.\n   - `src/api/mod.rs` (read): the signature.\n   - Group: api\n\
            3. **Add**\n   - `src/api/add.rs` (new): implement `add_user`.\n   - `src/api/mod.rs` (read-only): the signature.\n   - Group: api\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(project_problems(contract, &tree.0), Vec::<String>::new());
        let s = steps(contract);
        assert!(s[1]
            .files
            .iter()
            .any(|f| f.path == "src/api/mod.rs" && f.read_only));
        assert!(!s[0].files[0].read_only);
        let no_contract = contract.replace(
            "`src/api/mod.rs`: declare",
            "`migrations/README.md`: declare",
        );
        let p = project_problems(&no_contract, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].contains("all build on `src/api/mod.rs`, but no step before the group settles it"),
            "{p:?}"
        );
        // Out of a group, sharing is fine.
        let ungrouped = shared.replace("   - Group: backend\n", "");
        assert!(project_problems(&ungrouped, &tree.0).is_empty());

        // With a project, plan_problems_in adds these to the form checks.
        assert_eq!(plan_problems_in(&missing, Some(&tree.0)).len(), 1);
        assert!(plan_problems_in(&missing, None).is_empty());
    }

    #[test]
    fn plan_checks() {
        assert!(
            plan_problems(GOOD_PLAN).is_empty(),
            "{:?}",
            plan_problems(GOOD_PLAN)
        );
        let bad = "Some plan\n## Steps\n1. Only one";
        let problems = plan_problems(bad);
        assert_eq!(problems.len(), 3, "title, steps, validation: {problems:?}");
        assert_eq!(plan_problems("  "), vec!["The plan is empty."]);
        let code = format!("{GOOD_PLAN}\n```\n{}```", "x\n".repeat(20));
        assert_eq!(plan_problems(&code).len(), 1);
        assert!(plan_problems(&code)[0].starts_with("Leave the code out"));
        // One-line signatures don't count as code.
        let sigs = format!(
            "{GOOD_PLAN}\n{}",
            "```rust\nfn a() -> u8;\n```\n".repeat(30)
        );
        assert!(
            plan_problems(&sigs).is_empty(),
            "{:?}",
            plan_problems(&sigs)
        );
        assert!(looks_like_plan("## Steps\n1. do it"));
        assert!(!looks_like_plan("Done. 1. thing"));
    }

    #[test]
    fn steps_need_files_and_changes() {
        // Titles alone aren't a plan.
        let terse = "# T\n## Steps\n1. Table\n2. API\n## Validation\n- run the tests";
        let problems = plan_problems(terse);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[0].starts_with("Steps 1 and 2 name no file"),
            "{problems:?}"
        );
        assert!(
            problems[1].starts_with("Steps 1 and 2 are too vague"),
            "{problems:?}"
        );
        // Only the step without a file is reported.
        let one = "# T\n## Steps\n1. Add the `load` function to `src/a.rs` for reading users.\n\
                   2. Update the docs so they explain the new loading, e.g. in the readme.\n## Testing\n- cargo test";
        assert_eq!(plan_problems(one), vec![plan_problems(one)[0].clone()]);
        assert!(plan_problems(one)[0].starts_with("Step 2 names no file"));
        // Bare names, folders and new files count; URLs and "and/or" don't.
        assert!(names_a_file("change the base image in Dockerfile"));
        assert!(names_a_file("move it under src/components/"));
        assert!(names_a_file("set the flag in Cargo.toml (new section)"));
        assert!(names_a_file("**package.json**: add a script"));
        assert!(!names_a_file(
            "see https://example.com and/or ask, i.e. later"
        ));
        assert!(!names_a_file("bump to 1.2 and update the API."));
    }

    #[test]
    fn plans_need_validation() {
        let no_check = GOOD_PLAN.replace("## Validation", "## Notes");
        let problems = plan_problems(&no_check);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("Add a \"## Validation\""));
        // An empty section doesn't count; other names do.
        assert_eq!(
            plan_problems(&GOOD_PLAN.replace("- `cargo test`\n- open /users by hand", "")).len(),
            1
        );
        assert!(plan_problems(&GOOD_PLAN.replace("## Validation", "## How to verify")).is_empty());
        assert!(
            plan_problems(&GOOD_PLAN.replace("## Validation", "## Testing and validation"))
                .is_empty()
        );
    }

    #[test]
    fn stems_and_relative_paths() {
        assert_eq!(
            file_stem("Add a users table, please!", "2026-10-04"),
            "2026-10-04-add-a-users-table-please"
        );
        assert_eq!(file_stem("???", "2026-10-04"), "2026-10-04-plan");
        assert_eq!(
            relative_plan_path("/p/.rovik/plans/a.md", Some("/p/")),
            ".rovik/plans/a.md"
        );
        assert_eq!(relative_plan_path("/q/a.md", Some("/p")), "/q/a.md");
    }

    #[test]
    fn questions_in_the_shapes_models_send() {
        let args = json!({ "questions": [
            { "question": " Which DB? ", "options": ["Postgres", { "label": "SQLite", "recommended": true }], "allow_custom": false },
            { "question": "" },
            { "question": "Name?" }
        ]});
        let qs = parse_questions(args.as_object().unwrap());
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].question, "Which DB?");
        assert!(!qs[0].allow_custom && qs[0].options[1].recommended);
        assert!(qs[1].allow_custom, "no options: answered in words");
        let single = json!({ "question": "Top level?", "options": [] });
        assert_eq!(parse_questions(single.as_object().unwrap()).len(), 1);

        let outcome: QuestionsOutcome =
            serde_json::from_value(json!({ "answers": [{ "kind": "option", "label": "SQLite" }] }))
                .unwrap();
        let text = describe_outcome(&qs, &outcome);
        assert!(
            text.contains("1. Which DB?\n   → SQLite")
                && text.contains("2. Name?\n   → left it to you")
        );
        let stopped: QuestionsOutcome = serde_json::from_value(json!({ "stopped": true })).unwrap();
        assert!(describe_outcome(&qs, &stopped).starts_with("Error:"));
        assert_eq!(
            serde_json::to_value(&stopped).unwrap(),
            json!({ "stopped": true })
        );
    }

    #[test]
    fn todo_lists() {
        let args = json!({ "items": ["a", { "title": "b", "status": "in_progress" }, { "title": "c", "status": "bogus" }] });
        let items = parse_todo(args.as_object().unwrap());
        assert_eq!(items[2].status, "pending");
        assert_eq!(
            describe_todo(&items),
            "Checklist updated: 0 of 3 done; now: b."
        );
    }

    #[test]
    fn plan_and_question_edge_cases() {
        // A steps heading with more words, steps with ")".
        let parens = GOOD_PLAN
            .replace("## Steps", "## Implementation steps")
            .replace("1. ", "1) ")
            .replace("2. ", "2) ");
        assert!(
            plan_problems(&parens).is_empty(),
            "{:?}",
            plan_problems(&parens)
        );
        // Steps belong under the steps heading, not a later section.
        let misplaced = GOOD_PLAN.replace("## Steps\n", "## Steps\nnone\n## Other\n");
        assert_eq!(plan_problems(&misplaced).len(), 1);
        assert!(
            plan_problems(&format!("{GOOD_PLAN}\n{}", "x".repeat(40_001)))
                .iter()
                .any(|p| p.contains("over 40,000"))
        );
        // Leading blank lines before the title are fine.
        assert!(plan_problems(&format!("\n\n{GOOD_PLAN}")).is_empty());

        // At most 3 questions, 6 options; junk skipped.
        let many = json!({ "questions": [
            { "question": "1", "options": ["a","b","c","d","e","f","g"] },
            { "question": "2", "options": [1, null, { "label": " " }, { "label": "ok" }] },
            "not an object",
            { "question": "3" },
            { "question": "4" }
        ]});
        let qs = parse_questions(many.as_object().unwrap());
        assert_eq!(qs.len(), 2, "the string and the 4th are out");
        assert_eq!(qs[0].options.len(), 6);
        assert_eq!(
            qs[1]
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["ok"]
        );
        assert!(parse_questions(json!({}).as_object().unwrap()).is_empty());
        // Fewer answers than questions: the rest are left to the agent.
        let qs = parse_questions(
            json!({ "questions": [{ "question": "A?" }, { "question": "B?" }] })
                .as_object()
                .unwrap(),
        );
        let outcome = QuestionsOutcome::Answers {
            answers: vec![QuestionAnswer::Custom {
                text: "mine".into(),
            }],
        };
        let text = describe_outcome(&qs, &outcome);
        assert!(
            text.contains("\"mine\" (in their own words)")
                && text.contains("B?\n   → left it to you")
        );
        assert_eq!(outcome_kinds(&outcome), ["custom"]);
    }

    #[test]
    fn checklist_edge_cases() {
        assert_eq!(
            describe_todo(&[]),
            "Error: the checklist is empty. Pass items: [{ title, status }]."
        );
        let many: Vec<Value> = (0..40).map(|i| json!(format!("step {i}"))).collect();
        assert_eq!(
            parse_todo(json!({ "items": many }).as_object().unwrap()).len(),
            MAX_TODO_ITEMS
        );
        let all_settled = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "skipped" }, { "title": "c", "status": "changed" }] })
                .as_object()
                .unwrap(),
        );
        assert!(all_settled.iter().all(is_settled));
        assert_eq!(
            describe_todo(&all_settled),
            "Checklist updated: 3 of 3 done."
        );
        // Two steps in_progress: rejected, naming both, so the model narrows to one.
        let two_active = parse_todo(
            json!({ "items": [{ "title": "a", "status": "in_progress" }, { "title": "b", "status": "in_progress" }, { "title": "c", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        let err = describe_todo(&two_active);
        assert!(err.starts_with("Error: only one step"), "{err}");
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "{err}");
        // Zero in_progress with open steps stays fine: between steps, not an error.
        let between = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        assert_eq!(describe_todo(&between), "Checklist updated: 1 of 2 done.");
    }

    #[test]
    fn checklist_note_repeats_the_list_without_copying_it() {
        let items = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] })
                .as_object()
                .unwrap(),
        );
        let note = checklist_note(&items, false);
        assert!(note.contains("1 of 2 done"), "{note}");
        assert!(
            note.contains("- [done] a") && note.contains("- [pending] b"),
            "{note}"
        );
        assert!(note.contains("don't copy it into the task card"), "{note}");
        assert!(
            checklist_note(&items, true).contains("carried from earlier work"),
            "{note}"
        );
    }

    fn row(id: &str, role: &str, meta: Option<Value>) -> MessageRow {
        MessageRow {
            id: id.into(),
            dialog_id: "d".into(),
            seq: 0,
            role: role.into(),
            content: id.into(),
            body_external: false,
            body_bytes: 0,
            tool_calls: None,
            tool_call_id: None,
            meta,
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 0,
        }
    }

    fn todo_row(id: &str, items: Value) -> MessageRow {
        let mut m = row(id, "assistant", None);
        m.tool_calls = Some(vec![crate::chat_db::store::ToolCall {
            id: format!("{id}-call"),
            name: "todo".into(),
            arguments: items.to_string(),
            extra_content: None,
        }]);
        m
    }

    /// An unfinished checklist is carried across one user message.
    #[test]
    fn unfinished_checklist_is_carried_across_one_user_message() {
        let messages = vec![
            row("u1", "user", None),
            todo_row(
                "a1",
                json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] }),
            ),
            row("u2", "user", None),
        ];
        let carried = current_checklist(&messages).expect("carried");
        assert!(carried.carried, "{carried:?}");
        assert_eq!(carried.from_message_id.as_deref(), Some("a1"));
        assert_eq!(carried.items.len(), 2);
        assert_eq!(open_steps(&carried.items).len(), 1);
    }

    /// A fully settled checklist is not carried: the agent is wrapping up.
    #[test]
    fn settled_checklist_is_not_carried() {
        let messages = vec![
            row("u1", "user", None),
            todo_row(
                "a1",
                json!({ "items": [{ "title": "a", "status": "done" }] }),
            ),
            row("u2", "user", None),
        ];
        assert_eq!(current_checklist(&messages), None);
    }

    /// A newer `todo` call in the current request wins over the carried one.
    #[test]
    fn newer_todo_call_wins() {
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
            todo_row("a2", json!({ "items": ["new step"] })),
        ];
        let current = current_checklist(&messages).expect("current");
        assert!(!current.carried, "{current:?}");
        assert_eq!(current.from_message_id.as_deref(), Some("a2"));
        assert_eq!(current.items[0].title, "new step");
    }

    /// A second user message stops the search: an old checklist doesn't
    /// linger across unrelated work.
    #[test]
    fn second_user_message_stops_the_search() {
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
            row("u3", "user", None),
        ];
        assert_eq!(current_checklist(&messages), None);
    }

    /// The gate follows the "3 or more steps" rule: a 3rd distinct file, a
    /// command after 2 changed files, or 12 tool calls; never reads.
    #[test]
    fn checklist_gate_thresholds() {
        let files = |n: usize| (0..n).map(|i| format!("f{i}.rs")).collect::<Vec<_>>();
        // Writes: the 3rd distinct file trips it; rewriting a known one doesn't.
        assert_eq!(
            checklist_gate_reason("write", Some("new.rs"), &files(1), 2),
            None
        );
        assert_eq!(
            checklist_gate_reason("write", Some("new.rs"), &files(2), 3).as_deref(),
            Some("3 files changed")
        );
        assert_eq!(
            checklist_gate_reason("write", Some("./f1.rs"), &files(2), 3),
            None
        );
        // Commands: after 2 changed files, not after 1 (edit, then test).
        assert_eq!(checklist_gate_reason("bash", None, &files(1), 2), None);
        assert!(checklist_gate_reason("bash", None, &files(2), 3).is_some());
        // Many calls without a checklist trip it for work, not for reads.
        assert_eq!(
            checklist_gate_reason("bash", None, &[], GATE_TOOL_CALLS).as_deref(),
            Some("12 tool calls made")
        );
        assert_eq!(
            checklist_gate_reason("read", Some("a.rs"), &files(5), 40),
            None
        );
        let error = checklist_gate_error("3 files changed", "write");
        assert!(
            error.starts_with("Error:") && error.contains("already finished as done"),
            "{error}"
        );
    }

    /// Dismissed on the message holding the `todo` call (the panel's ✕):
    /// closed in its own request, and never carried.
    #[test]
    fn dismissed_on_its_own_message_closes_the_checklist() {
        let mut held = todo_row("a1", json!({ "items": ["step"] }));
        held.meta = Some(json!({ "checklistClosed": true }));
        // In the current request.
        let current = vec![row("u1", "user", None), held.clone()];
        assert_eq!(current_checklist(&current), None);
        // Not carried into the next one.
        let next = vec![row("u1", "user", None), held, row("u2", "user", None)];
        assert_eq!(current_checklist(&next), None);
        // An older, undismissed one isn't resurrected by the dismissal.
        let older = vec![
            row("u1", "user", None),
            todo_row("a0", json!({ "items": ["old"] })),
            {
                let mut m = todo_row("a1", json!({ "items": ["new"] }));
                m.meta = Some(json!({ "checklistClosed": true }));
                m
            },
        ];
        assert_eq!(current_checklist(&older), None);
    }

    /// The panel's dismiss (meta.checklistClosed) closes the checklist.
    #[test]
    fn checklist_closed_is_respected() {
        let dismissed = |key: &str| {
            vec![
                row("u1", "user", None),
                todo_row("a1", json!({ "items": ["old step"] })),
                row("u2", "user", Some(json!({ key: true }))),
            ]
        };
        assert_eq!(current_checklist(&dismissed("checklistClosed")), None);
        assert_eq!(current_checklist(&dismissed("checklist_closed")), None);
        // Without the flag the same history carries.
        let messages = vec![
            row("u1", "user", None),
            todo_row("a1", json!({ "items": ["old step"] })),
            row("u2", "user", None),
        ];
        assert!(current_checklist(&messages).is_some());
    }
}

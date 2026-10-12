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

Not every message asks for a plan. When the user asks a question or wants something explained (why something happens, how a part of the code works, what causes a bug, what an error means), investigate and answer in your reply, naming the files and functions involved, and don't call submit_plan: there's nothing to build yet. Plan when the user asks for a change (add, fix, remove or rework something) or for a plan.

1. Investigate first: read the relevant code, find the files that will change, the functions and types involved, and the existing patterns and conventions the change should follow.
2. Ask only what the code can't tell you and what really changes the design, the behavior or the scope (two existing approaches to choose between, an unclear requirement, a trade-off only the user can make). Use the ask_user tool, early, with all your questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything you can decide from the repository: decide it and say so in the plan. Never ask a question in plain text and stop; use ask_user.
3. Save the plan with the submit_plan tool, as Markdown. It's saved as a file the user may edit before implementing it, so make it stand on its own:
   - a "# " title;
   - a one-paragraph summary of the approach;
   - "## Steps": a numbered list, in order, one reviewable change per step. Each step has:
     - a short title;
     - the files it touches, each as its full path from the project root in backticks (`src/db/users.rs`), never the file name alone (`users.rs`) or a path from a folder inside the project, even when the name is unique. Name only paths you have seen in the repository; mark files the step creates as "(new)". Only files and folders count as the step's files: CSS classes and selectors (`.btn-primary`), identifiers, labels and commands in backticks don't name one, so a step that changes styles names the stylesheet (`src/styles/app.css`) and then the classes in it;
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
/// `## Steps` itself, taken before any other heading with "step" in it
/// (P4.4: a "## Next steps" note before it isn't the steps).
static STEPS_HEADING_EXACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+(?:the\s+)?steps\s*:?\s*$").unwrap());
/// Then one that starts with "Steps" (`## Steps (ordered)`, `## 3. Steps`),
/// still before one that only mentions them.
static STEPS_HEADING_LEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^##[ \t]+(?:\d+[.)][ \t]*)?(?:the[ \t]+)?steps\b[^\n]*$").unwrap()
});

/// The steps section's heading (P4.4).
fn steps_heading(text: &str) -> Option<regex::Match<'_>> {
    STEPS_HEADING_EXACT
        .find(text)
        .or_else(|| STEPS_HEADING_LEADING.find(text))
        .or_else(|| STEPS_HEADING.find(text))
}
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
    // A segment may be a Next.js route group, `(auth)` (round 3).
    Regex::new(r#"(?:^|[\s`'"(\[*])(\.?[\w@~-][\w.@~-]*(?:/(?:\([\w.-]+\)|[\w.@~\[\]-]+))+/?|\.?[\w-]*[A-Za-z_][\w-]*(?:\.[\w-]+)*\.[A-Za-z][\w]{0,9})"#).unwrap()
});
/// Root-level `.js` names with a capital that are files, not libraries.
const ROOT_JS_FILES: &[&str] = &["app", "index", "gruntfile", "gulpfile", "main", "server"];
/// Objects whose members read like root files (`process.env`, `req.json`).
const MEMBER_RECEIVERS: &[&str] = &[
    "process",
    "import.meta",
    "req",
    "res",
    "request",
    "response",
    "resp",
    "ctx",
    "context",
    "state",
    "this",
    "self",
    "window",
    "document",
    "console",
    "err",
    "error",
    "event",
    "result",
    "message",
    "msg",
    "body",
    "payload",
    "props",
];
/// File names without an extension that are still clearly files.
static BARE_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(Makefile|Dockerfile|Justfile|Procfile|Gemfile|Rakefile|Containerfile)\b")
        .unwrap()
});
/// Words that say what a step does to its files.
static CHANGE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(add\w*|chang\w*|creat\w*|bump\w*|upgrad\w*|downgrad\w*|install\w*|pin|pins|pinned|rewrit\w*|rework\w*|revert\w*|restor\w*|simplif\w*|deprecat\w*|reorder\w*|run|runs|rais\w*|lower\w*|increas\w*|decreas\w*|apply|applies|requir\w*|allow\w*|skip\w*|ignor\w*|comput\w*|sort\w*|filter\w*|build\w*|compil\w*|format\w*|remov\w*|delet\w*|renam\w*|mov\w*|updat\w*|replac\w*|extend\w*|edit\w*|modif\w*|introduc\w*|implement\w*|refactor\w*|split\w*|extract\w*|merg\w*|wire\w*|register\w*|expos\w*|export\w*|import\w*|call\w*|return\w*|handl\w*|pass\w*|store\w*|emit\w*|drop\w*|switch\w*|convert\w*|define\w*|declare\w*|new|fix\w*|adjust\w*|insert\w*|append\w*|keep\w*|use|uses|using|set|sets|write\w*|read\w*|check\w*|validat\w*|test\w*|migrat\w*|document\w*|configur\w*|enabl\w*|disabl\w*|wrap\w*|guard\w*|inline\w*|generat\w*|render\w*|show\w*|hide\w*|load\w*|sav\w*|pars\w*|map\w*|send\w*|fetch\w*|accept\w*|reject\w*|limit\w*|cach\w*)\b",
    )
    .unwrap()
});

/// Whether a line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    // A span closed on its own line (```ts x = 1```) opens nothing (round 3).
    ["```", "~~~"]
        .iter()
        .any(|m| t.starts_with(m) && !t[m.len()..].contains(m))
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

/// Whether a step names a file or folder of the repository: the same
/// reading as its scope's (P4.3).
fn names_a_file(step: &str) -> bool {
    !step_files(step).is_empty()
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

/// P4.4: each step's number, as written when the plan's numbers go up one
/// by one, else its place (`1. 1. 1.`, as some editors number lists), and
/// the number the plan shows when that's another.
fn step_numbers(raws: &[&str]) -> Vec<(usize, Option<usize>)> {
    let written: Vec<Option<usize>> = raws
        .iter()
        .map(|raw| {
            STEP_NUMBER
                .captures(raw.trim_start().lines().next().unwrap_or(""))
                .and_then(|c| c[1].parse().ok())
        })
        .collect();
    let increasing = written.iter().all(Option::is_some) && written.windows(2).all(|w| w[0] < w[1]);
    written
        .into_iter()
        .enumerate()
        .map(|(i, w)| {
            let number = if increasing {
                w.unwrap_or(i + 1)
            } else {
                i + 1
            };
            (number, w.filter(|w| *w != number))
        })
        .collect()
}

/// "4", or "4 (numbered 3. in the plan)": how messages name a step.
fn step_name(number: usize, written: Option<usize>) -> String {
    match written {
        Some(w) => format!("{number} (numbered {w}. in the plan)"),
        None => number.to_string(),
    }
}

/// "2", "2 and 4", "2, 4 and 5".
fn list_steps(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.join(""),
    }
}

/// What's wrong with a plan, if anything (empty: it's fine). Kept to the
/// minimum a plan needs to be built from (a title, steps that each name a
/// file and what changes there, a way to validate it), so a model doesn't
/// invent detail just to get past the check.
pub fn plan_problems(markdown: &str) -> Vec<String> {
    // P4.4: one kind of line ending.
    let markdown = markdown.replace("\r\n", "\n");
    let text = markdown.trim();
    if text.is_empty() {
        return vec!["The plan is empty.".into()];
    }
    let mut problems = Vec::new();
    let first = text.split('\n').find(|l| !l.trim().is_empty());
    if !first.is_some_and(|l| TITLE.is_match(l)) {
        problems.push("Start with a \"# \" title line.".into());
    }
    let steps_heading = steps_heading(text);
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
                // P4.4: named as `steps` numbers them (`2. 4. 7.` keeps its numbers).
                let names: Vec<String> = step_numbers(&steps)
                    .into_iter()
                    .map(|(n, w)| step_name(n, w))
                    .collect();
                let no_file: Vec<&str> = (0..steps.len())
                    .filter(|&i| !names_a_file(steps[i]))
                    .map(|i| names[i].as_str())
                    .collect();
                if !no_file.is_empty() {
                    problems.push(format!(
                        "Step{} {} name{} no file: give the full path from the project root of each file a step changes, in \
                         backticks (e.g. `src/db/users.rs`, not `users.rs`), and mark files it creates as \"(new)\". Name only \
                         paths you've seen in the repository; CSS classes, selectors and other names in backticks aren't files.",
                        if no_file.len() > 1 { "s" } else { "" },
                        list_steps(&no_file),
                        if no_file.len() > 1 { "" } else { "s" },
                    ));
                }
                let vague: Vec<&str> = (0..steps.len())
                    .filter(|&i| {
                        let step = steps[i];
                        step.split_whitespace().count() < 6 || !CHANGE_WORD.is_match(step)
                    })
                    .map(|i| names[i].as_str())
                    .collect();
                if !vague.is_empty() {
                    problems.push(format!(
                        "Step{} {} {} too vague: say what to add, change or remove in the files, by name (the functions, \
                         types, components or settings, and how they behave); a one-line signature is fine.",
                        if vague.len() > 1 { "s" } else { "" },
                        list_steps(&vague),
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
    // A code block left open runs to the end: the steps after it would be
    // its step's body, silently.
    if text.lines().filter(|l| is_fence(l)).count() % 2 == 1 {
        problems.push(
            "Close the code block that's left open (```): everything after it reads as code, steps included.".into(),
        );
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
    // Round 2: one kind of line ending in the file, as the plan is read.
    let plan = plan.replace("\r\n", "\n");
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
        // P4.9: only at a separator (`/a/bc` isn't under `/a/b`).
        Some(rest) => match rest.strip_prefix(['/', '\\']) {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => path.to_string(),
        },
        None => path.to_string(),
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
static TITLE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#\s+\S").unwrap());
/// A section a plan has besides its steps.
static PLAN_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^##\s+.*\b(decisions?|validat\w*|verif\w*|risks?)\b").unwrap()
});

/// A reply that reads like a plan (written out instead of submitted): a
/// steps section with numbered steps, and a title or another of a plan's
/// sections. An answer with "## Steps to reproduce" and a numbered list
/// is an explanation, not a plan to submit.
pub fn looks_like_plan(text: &str) -> bool {
    LOOKS_LIKE_STEPS.is_match(text)
        && NUMBERED_LINE.is_match(text)
        && (TITLE_LINE.is_match(text) || PLAN_SECTION.is_match(text))
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
    /// As numbered in the plan, or its place when the plan's numbers don't
    /// go up one by one (P4.4).
    pub number: usize,
    /// The number the plan shows, when it isn't `number` (`1. 2. 2. 3.`:
    /// the last is step 4, written "3.").
    pub written: Option<usize>,
    pub title: String,
    /// The files it names, each once, in order.
    pub files: Vec<PlanFile>,
    /// The whole step, its first line included.
    pub body: String,
    /// `Group: <name>`: steps that may be built side by side (F03).
    pub group: Option<String>,
}

impl PlanStep {
    /// "4", or "4 (numbered 3. in the plan)": how messages name it.
    pub fn named(&self) -> String {
        step_name(self.number, self.written)
    }
}

static STEP_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)[.)]\s+(.*)$").unwrap());
static BACKTICKED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").unwrap());
static NEW_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:new|new file|created?)\)").unwrap());
static READ_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:read|read-only|readonly|reads|context)\)").unwrap());
/// A `Group:` line's name, in lower case: what's in its backticks, else
/// its text; a note or a period after it isn't the name (`` `api`. ``,
/// `api (runs alongside step 2)`).
fn group_name(raw: &str) -> String {
    let t = raw.trim();
    let name = match t.strip_prefix('`').and_then(|r| r.split_once('`')) {
        Some((inside, _)) => inside,
        None => t
            .split(" (")
            .next()
            .unwrap_or(t)
            .trim_end_matches(['.', ',', ';']),
    };
    name.trim().to_lowercase()
}

static GROUP_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[\s>*-]*\**group\**\s*:\s*\**\s*([^*\n]+?)\s*\**\s*$").unwrap()
});

/// Extensions a backticked name must have to count as a file, so that
/// `state.config` or `self.items` isn't taken for one.
pub(super) const FILE_EXTENSIONS: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "mts",
    "cts",
    "json",
    "jsonc",
    "json5",
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
    "work",
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

/// Dotfile stems (`.env`, `.gitignore`, `.env.local`'s `.env`) that name
/// a file at the project root, where a dot-leading name is otherwise a CSS
/// class or selector (`.btn-primary`, `.nav-item.active`) or a bare
/// extension (`.ts`).
const DOTFILES: &[&str] = &[
    "env",
    "envrc",
    "gitignore",
    "gitattributes",
    "gitmodules",
    "gitkeep",
    "gitconfig",
    "git-blame-ignore-revs",
    "mailmap",
    "dockerignore",
    "gcloudignore",
    "helmignore",
    "npmignore",
    "npmrc",
    "nvmrc",
    "yarnrc",
    "eslintrc",
    "eslintignore",
    "prettierrc",
    "prettierignore",
    "stylelintrc",
    "babelrc",
    "browserslistrc",
    "editorconfig",
    "huskyrc",
    "lintstagedrc",
    "mocharc",
    "nycrc",
    "swcrc",
    "tool-versions",
    "python-version",
    "ruby-version",
    "node-version",
    "java-version",
    "terraform-version",
    "flake8",
    "pylintrc",
    "coveragerc",
    "pre-commit-config",
    "clang-format",
    "clang-tidy",
    "rustfmt",
    "cargo",
    "htaccess",
    "htpasswd",
    "bashrc",
    "zshrc",
    "vimrc",
    "github",
    "gitlab",
    "vscode",
    "idea",
    "devcontainer",
    "husky",
    "storybook",
    "circleci",
    "well-known",
    "rovik",
];

/// Whether `.{stem}` (the name up to its second dot) is a dotfile or a
/// hidden folder of the project, not a CSS class or a bare extension.
fn known_dotfile(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    DOTFILES.contains(&lower.as_str())
        || ["rc", "ignore", "config", "version", "versions"]
            .iter()
            .any(|suffix| lower.len() > suffix.len() && lower.ends_with(suffix))
}

/// The repo-relative path a backticked token names, if it's one.
fn as_path(token: &str) -> Option<String> {
    let token = token.replace('\\', "/");
    let t = token.trim().trim_start_matches("./");
    // Parentheses: only as a route group's segment, `app/(auth)/page.tsx`
    // (round 3); `fn a(x)` and `foo()` aren't paths.
    let route_groups = t.contains('/')
        && t.split('/').all(|seg| {
            !seg.contains(['(', ')'])
                || (seg.len() > 2
                    && seg.starts_with('(')
                    && seg.ends_with(')')
                    && seg[1..seg.len() - 1]
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
        });
    if t.is_empty()
        || t.contains(char::is_whitespace)
        || t.contains("://")
        || t.contains([
            '<', '>', '{', '}', '*', '?', '$', '=', ',', ';', '"', '\'', '|',
        ])
        || (t.contains(['(', ')']) && !route_groups)
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
    // With a letter: `docs/v1.2` and HTTP/1.1 are versions.
    let short_ext = ext.as_deref().is_some_and(|e| {
        (1..=10).contains(&e.len())
            && e.chars().all(|c| c.is_ascii_alphanumeric())
            && e.chars().any(|c| c.is_ascii_alphabetic())
    });
    // At the root, a dot-leading name is a file when it's a known dotfile
    // (`.env`, `.gitignore`, `.env.local`, `.tool-versions`) or has a file
    // extension after it (`.foo.json`); a CSS class or selector
    // (`.btn-primary`, `.card`, `.nav-item.active`) or a bare extension
    // (`.ts`) isn't one. (Plans used to read backticked classes as missing
    // files, and asked for them to be fixed or marked new.)
    if !t.contains('/') && name.starts_with('.') {
        let first = name[1..].split('.').next().unwrap_or("");
        let with_extension =
            name[1..].contains('.') && ext.as_deref().is_some_and(|e| FILE_EXTENSIONS.contains(&e));
        if !(known_dotfile(first) || with_extension) {
            return None;
        }
    }
    // `Node.js` and its kind name a project, not a file; so does a
    // capitalised `Solid.js` at the root (a root `App.js`, `Gruntfile.js`
    // is a file), and `React/Vue.js` (every part capitalised) (round 3).
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if ext.as_deref() == Some("js") {
        let lower = stem.to_ascii_lowercase();
        let capitalised = |s: &str| s.starts_with(|c: char| c.is_ascii_uppercase());
        let library = super::triage::JS_NAMES.contains(&lower.as_str())
            || (capitalised(stem) && !ROOT_JS_FILES.contains(&lower.as_str()));
        if !t.contains('/') && library {
            return None;
        }
        if t.contains('/') && t.split('/').all(capitalised) && library {
            return None;
        }
    }
    // Member access on the usual objects (`process.env`, `res.json`,
    // `state.lock`) isn't a root file (round 3).
    if !t.contains('/') && MEMBER_RECEIVERS.contains(&stem.to_ascii_lowercase().as_str()) {
        return None;
    }
    // P4.3: in a folder, any short extension (`grammar.pest`, `x.snap`); at
    // the root one when the stem says it's a file name (`.env.local`,
    // `Dockerfile.dev`, `docker-compose.override`), not `self.items`.
    let in_folder_file = t.contains('/') && short_ext;
    let root_file = short_ext
        && (stem.starts_with('.')
            || stem.contains('-')
            || BARE_FILE.find(stem).is_some_and(|m| m.len() == stem.len()));
    let known = ext.as_deref().is_some_and(|e| FILE_EXTENSIONS.contains(&e))
        || in_folder_file
        || root_file
        || named_file
        || (name.starts_with('.') && name.len() > 2 && !name[1..].contains('.'));
    // `a::b` paths, `x.y` member access: not files.
    if t.contains("::") {
        return None;
    }
    if !(known || (folder && t.ends_with('/')) || (folder && ext.is_none() && t.contains('/'))) {
        return None;
    }
    // One spelling (`paths`); a folder keeps its `/`; `..` isn't the project's.
    let p = super::paths::norm_rel(t).filter(|p| !p.is_empty())?;
    Some(if t.ends_with('/') { format!("{p}/") } else { p })
}

fn step_title(first_line: &str) -> String {
    let t = first_line.trim().trim_matches('*').trim();
    let t = t.split(" — ").next().unwrap_or(t);
    t.trim_end_matches(':').trim_matches('*').trim().to_string()
}

/// The paths a line names, each with where it ends: backticked, or (P4.3)
/// written plainly, the same way the form check reads them.
fn line_paths(line: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut ticked: Vec<(usize, usize)> = Vec::new();
    for c in BACKTICKED.captures_iter(line) {
        let m = c.get(0).expect("a match");
        ticked.push((m.start(), m.end()));
        if let Some(p) = as_path(&c[1]) {
            out.push((p, m.end()));
        }
    }
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    for c in PATH.captures_iter(line) {
        let m = c.get(1).expect("a group");
        tokens.push((m.start(), m.end()));
        if ticked
            .iter()
            .any(|(a, b)| m.start() >= *a && m.start() < *b)
        {
            continue;
        }
        let token = m.as_str().trim_end_matches('.');
        if !plain_path(token) || !plain_file_shaped(token) {
            continue;
        }
        // A call, `res.json(users)`, isn't a file (round 3); a path's mark
        // may follow it without a space (`src/a.rs(new)`).
        if line[m.end()..].starts_with('(') && !token.contains('/') {
            continue;
        }
        if let Some(p) = as_path(token) {
            out.push((p, m.start() + token.len()));
        }
    }
    // Bare file names: Dockerfile, Makefile.
    for m in BARE_FILE.find_iter(line) {
        // Part of a longer name (`Dockerfile.dev`): that name's.
        if ticked
            .iter()
            .chain(&tokens)
            .any(|(a, b)| m.start() >= *a && m.start() < *b)
        {
            continue;
        }
        if let Some(p) = as_path(m.as_str()) {
            if !out.iter().any(|(q, _)| *q == p) {
                out.push((p, m.end()));
            }
        }
    }
    out.sort_by_key(|(_, end)| *end);
    out
}

/// A plainly written token that's a path: with a `/`, not a URL, a
/// fraction or "and/or"; else a file name, not "e.g." or "vs.x".
fn plain_path(token: &str) -> bool {
    if token.contains('/') {
        // `github.com/foo/bar/README.md`: a web address (round 3).
        let domain = token
            .split('/')
            .next()
            .and_then(|host| host.rsplit_once('.'))
            .is_some_and(|(_, tld)| {
                matches!(
                    tld.to_ascii_lowercase().as_str(),
                    "com" | "org" | "io" | "dev" | "net" | "app" | "co" | "ai" | "me" | "sh"
                )
            });
        return !token.contains("://")
            && !domain
            && token.split('/').filter(|s| !s.is_empty()).count() >= 2
            && token.chars().any(|ch| ch.is_ascii_alphabetic())
            && !matches!(
                token.to_ascii_lowercase().as_str(),
                "and/or" | "either/or" | "i/o" | "n/a"
            );
    }
    let (name, ext) = token.rsplit_once('.').unwrap_or((token, ""));
    !(name.trim_start_matches('.').len() <= 2 && ext.len() <= 2) && token.len() >= 3
}

/// P4.3: written plainly, a path counts only with an extension or a
/// closing `/` (`src/a.rs`, `src/api/`): "add/remove", "A/B" and
/// "client/server" are prose.
fn plain_file_shaped(token: &str) -> bool {
    let name = token.rsplit('/').next().unwrap_or(token);
    token.ends_with('/')
        || name
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !ext.is_empty() && (!stem.is_empty() || name.len() > 2))
}

/// The files a step names (its raw text): one reading for the form check
/// and the steps' scopes (P4.3). Paths in code blocks are code.
fn step_files(raw: &str) -> Vec<PlanFile> {
    let mut files: Vec<PlanFile> = Vec::new();
    for (_, line, _) in lines_with_fences(raw)
        .into_iter()
        .filter(|(_, _, fenced)| !fenced)
    {
        // P4.2: a mark is its path's when it follows it; anywhere on the
        // line only when the line names one path.
        let named = line_paths(line);
        let one = named.len() == 1;
        for (path, end) in named {
            // Emphasis isn't a gap: `a.rs` **(new)**, **`a.rs`** (new).
            let next =
                line[end..].trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '_');
            let adjacent = |mark: &Regex| {
                next.starts_with('(') && mark.find(next).is_some_and(|m| m.start() == 0)
            };
            let new = adjacent(&NEW_MARK) || (one && NEW_MARK.is_match(line));
            let read_only = adjacent(&READ_MARK) || (one && READ_MARK.is_match(line));
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
    files
}

/// D01: the plan's steps, in order (empty when it has no steps section).
pub fn steps(markdown: &str) -> Vec<PlanStep> {
    // P4.4: one kind of line ending.
    let markdown = markdown.replace("\r\n", "\n");
    let text = markdown.trim();
    let Some(heading) = steps_heading(text) else {
        return Vec::new();
    };
    // Code blocks stay in a step's body (an exact signature, a schema).
    let section = section_after(text, heading.end());
    let raws: Vec<&str> = split_steps(section)
        .into_iter()
        // NUMBERED's `^\s*` can start at the blank line before a step.
        .map(str::trim_start)
        .collect();
    let numbers = step_numbers(&raws);
    raws.into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let first = raw.lines().next().unwrap_or("");
            let rest = STEP_NUMBER
                .captures(first)
                .map_or_else(|| first.to_string(), |c| c[2].to_string());
            let (number, written) = numbers[i];
            let files = step_files(raw);
            PlanStep {
                number,
                written,
                title: step_title(&rest),
                files,
                body: raw.trim_end().to_string(),
                // P4.4: outside code blocks; one name, any case; "none" isn't.
                group: lines_with_fences(raw)
                    .into_iter()
                    .filter(|(_, _, fenced)| !fenced)
                    .find_map(|(_, line, _)| GROUP_LINE.captures(line))
                    .map(|c| group_name(&c[1]))
                    .filter(|g| !g.is_empty() && !matches!(g.as_str(), "none" | "-" | "n/a")),
            }
        })
        .collect()
}

#[allow(dead_code)] // D03's worker brief reads "## Decisions" with it.
/// D01: the text of the plan's `## <name>` section (a heading that starts
/// with the name, case aside), without its heading.
pub fn section(markdown: &str, name: &str) -> Option<String> {
    let markdown = markdown.replace("\r\n", "\n");
    let markdown = markdown.as_str();
    let lower = name.to_lowercase();
    let mut at = 0;
    while let Some(h) = NEXT_HEADING.find(&markdown[at..]) {
        let start = at + h.start();
        let line_end = markdown[start..]
            .find('\n')
            .map_or(markdown.len(), |e| start + e);
        let title = markdown[start + 2..line_end].trim().to_lowercase();
        // `## 5. Decisions` is the Decisions section, as `## 3. Steps` is the steps.
        let title = title
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(['.', ')'])
            .filter(|_| title.starts_with(|c: char| c.is_ascii_digit()))
            .map_or(title.as_str(), str::trim_start);
        if title.starts_with(&lower) {
            return Some(section_after(markdown, line_end).trim().to_string());
        }
        at = line_end;
    }
    None
}

/// Where a relative path's `..` parts take it, as written (no links
/// followed); None when out of the project.
fn without_dots(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(if path.ends_with('/') && !parts.is_empty() {
        format!("{}/", parts.join("/"))
    } else {
        parts.join("/")
    })
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
    // A folder a new file of the plan is in comes with it (P4.2).
    let exists = |path: &str| {
        root.join(path.trim_end_matches('/')).exists()
            || (path.ends_with('/')
                && created
                    .iter()
                    .any(|(p, _)| super::paths::under(path, p) && !super::paths::same(path, p)))
    };
    let mut missing: Vec<String> = Vec::new();
    // A file named by its file name alone (`users.rs`) that isn't at the
    // root: where it is in the project, so the step can name it in full.
    let bare = |path: &str| !path.trim_end_matches('/').contains('/');
    let bare_missing: Vec<&str> = steps
        .iter()
        .flat_map(|s| s.files.iter())
        .filter(|f| !f.new && bare(&f.path) && !exists(&f.path))
        .map(|f| f.path.as_str())
        .collect();
    let located = files_named(root, &bare_missing);
    // P4.9: a path out of the project (`../x`) is never a step's file
    // (`as_path` drops it, so nothing probes the disk for it): say so.
    for step in &steps {
        for token in step.body.split('`').skip(1).step_by(2) {
            let t = token.trim().replace('\\', "/");
            let escapes = t.starts_with("../") || t.contains("/../") || t == "..";
            if escapes && super::paths::norm_rel(&t).is_none() && !missing.contains(&t) {
                missing.push(t.clone());
                // `src/../x.rs` is `x.rs`: inside, but not read as a file.
                match without_dots(&t) {
                    None => problems.push(format!(
                        "Step {} names `{t}`, which is outside the project: use a path inside it.",
                        step.named()
                    )),
                    Some(inside) if !inside.is_empty() => problems.push(format!(
                        "Step {} names `{t}`: write it as `{inside}`, without `..`.",
                        step.named()
                    )),
                    // The root itself is no step's file (as `./`).
                    Some(_) => {}
                }
            }
        }
        for file in step.files.iter().filter(|f| !f.new) {
            let creator = created
                .iter()
                .find(|(p, _)| p.trim_end_matches('/') == file.path.trim_end_matches('/'))
                .map(|(_, n)| *n);
            match creator {
                Some(n) if n > step.number && !exists(&file.path) => problems.push(format!(
                    "Step {} uses `{}`, which step {n} creates: put the steps in an order where a file is created before it's used.",
                    step.named(), file.path
                )),
                Some(_) => {}
                None if !exists(&file.path) && !missing.contains(&file.path) => {
                    missing.push(file.path.clone());
                    let found = located
                        .iter()
                        .find(|(name, _)| *name == file.path)
                        .map(|(_, paths)| paths)
                        .filter(|paths| !paths.is_empty());
                    match found {
                        Some(paths) => problems.push(format!(
                            "Step {} names `{}` by its file name alone: write its full path from the project root, {}.",
                            step.named(),
                            file.path,
                            match paths.as_slice() {
                                [one] => format!("`{one}`"),
                                many => format!(
                                    "one of {}",
                                    many.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
                                ),
                            }
                        )),
                        None => problems.push(format!(
                            "Step {} names `{}`, which isn't in the project: fix the path (in full from the project root), \
                             or mark it \"(new)\" if the step creates it.",
                            step.named(),
                            file.path
                        )),
                    }
                }
                None => {}
            }
        }
        // A new file named by its file name alone goes at the project
        // root; a source file there is almost always a path cut short.
        for file in step.files.iter().filter(|f| f.new && bare(&f.path)) {
            let ext = file
                .path
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .unwrap_or_default();
            if SOURCE_EXTENSIONS.contains(&ext.as_str())
                && !root_has_extension(root, &ext)
                && !missing.contains(&file.path)
            {
                missing.push(file.path.clone());
                problems.push(format!(
                    "Step {} creates `{}` at the project root (a file name alone is a root file), where the project keeps no \
                     `.{ext}` file: write its full path from the project root, with the folder it goes in.",
                    step.named(),
                    file.path
                ));
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
                // P4.1: what one changes, the other may not name at all
                // (read-only too: it would read a moving target), and a
                // folder covers what's in it. (New files in one folder are
                // kept apart by the workers' scopes: see group.rs.)
                let mut shared: Vec<String> = Vec::new();
                for (x, y) in [(a, b), (b, a)] {
                    for f in x.files.iter().filter(|f| !f.read_only) {
                        for g in &y.files {
                            if files_overlap(&f.path, &g.path) {
                                let p = format!("`{}`", f.path.trim_end_matches('/'));
                                if !shared.contains(&p) {
                                    shared.push(p);
                                }
                            }
                        }
                    }
                }
                if !shared.is_empty() {
                    problems.push(format!(
                        "Steps {} and {} are both in group \"{group}\" but both name {}: steps in a group are built side by side, \
                         so they can't share a file. Move the shared change into a step before the group, or take one of the steps out of it.",
                        a.number,
                        b.number,
                        shared.join(", ")
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

/// Extensions of source files, which live in a folder, not at the root.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "swift", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "css", "scss", "sass", "less", "vue", "svelte", "ex",
    "exs", "dart", "scala", "lua", "sql", "astro", "elm", "zig", "hs",
];

/// Whether a file with this extension is at the project root.
fn root_has_extension(root: &Path, ext: &str) -> bool {
    std::fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.path()
                    .extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(ext))
        })
    })
}

/// How many paths `files_named` gives for one name, and how many files
/// it looks at.
const NAMED_PATHS: usize = 4;
const NAMED_WALK_LIMIT: usize = 50_000;

/// Where the project keeps files with these names: each name with its
/// repo-relative paths (up to NAMED_PATHS, sorted), as git sees the
/// project (ignored files, `.git`, `node_modules` and `target` left out).
fn files_named(root: &Path, names: &[&str]) -> Vec<(String, Vec<String>)> {
    let mut names: Vec<&str> = names.to_vec();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(String, Vec<String>)> =
        names.iter().map(|n| (n.to_string(), Vec::new())).collect();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| {
            let name = e.file_name();
            name != ".git" && name != "node_modules" && name != "target"
        })
        .build();
    for entry in walker.flatten().take(NAMED_WALK_LIMIT) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy();
        let Some((_, paths)) = found.iter_mut().find(|(n, _)| *n == file_name) else {
            continue;
        };
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.contains('/') || paths.contains(&rel) {
                continue;
            }
            paths.push(rel);
        }
    }
    for (_, paths) in &mut found {
        paths.sort();
        paths.truncate(NAMED_PATHS);
    }
    found
}

/// P4.1: two plan paths that touch the same files: the same path in any
/// spelling, or one a folder (`src/api/`) the other is in.
fn files_overlap(a: &str, b: &str) -> bool {
    use super::paths::{same, under};
    let folder = |p: &str| p.trim_end().ends_with('/');
    same(a, b) || (folder(a) && under(a, b)) || (folder(b) && under(b, a))
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

/// A title's words alone: lower case, letters and digits, one space
/// between; so two spellings of the same step or finding compare equal.
pub fn simple_words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn trimmed(v: Option<&Value>) -> Option<String> {
    super::permissions::trimmed_str(v).map(str::to_string)
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
    let norm = |p: &str| super::paths::key(p).unwrap_or_else(|| p.trim().to_string());
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

    /// Round 3: member access on the usual objects, and a call, aren't
    /// root files, written plainly or in backticks; root files still are.
    #[test]
    fn member_access_is_not_a_root_file() {
        let step = "1. **API**\n   - `src/api.ts`: parse with `req.json()`, read `process.env` and \
                    `import.meta.env`; reply with res.json(users) and lock state.lock() first.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["src/api.ts"]);
        for yes in ["package.json", ".env", "go.mod", "main.rs", "index.ts"] {
            assert!(as_path(yes).is_some(), "{yes}");
        }
        for no in ["process.env", "res.json", "import.meta.env", "ctx.state"] {
            assert_eq!(as_path(no), None, "{no}");
        }
    }

    /// Round 3: a Next.js route group segment, `app/(auth)/page.tsx`, is a
    /// path; parentheses anywhere else still aren't.
    #[test]
    fn route_group_segments_are_paths() {
        assert_eq!(
            as_path("app/(auth)/login/page.tsx").as_deref(),
            Some("app/(auth)/login/page.tsx")
        );
        let step = "1. **L**\n   - `app/(auth)/login/page.tsx` (new): the form, next to app/(auth)/layout.tsx.";
        let files = step_files(step);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["app/(auth)/login/page.tsx", "app/(auth)/layout.tsx"]);
        assert!(files[0].new);
        for no in ["fn a(x: u8)", "foo()", "a.b.c()", "src/(a b)/x.ts", "src/()/x.ts"] {
            assert_eq!(as_path(no), None, "{no}");
        }
        let plan = "# L\n\nLogin.\n\n## Steps\n\n1. **Login page**\n   - Add `app/(auth)/login/page.tsx` (new): the form.\n\
                    2. **Layout**\n   - Change `app/(auth)/layout.tsx` to wrap it in the card.\n\n## Validation\n- `npm test`\n";
        assert_eq!(plan_problems(plan), Vec::<String>::new());
    }

    /// Round 3: a capitalised `X.js` at the root is a library's name, and
    /// so is `React/Vue.js`; `App.js`, `Gruntfile.js`, `src/solid.js` are
    /// files; a web address with a path isn't one.
    #[test]
    fn library_names_and_domains_are_prose() {
        for no in ["Solid.js", "Discord.js", "Day.js", "PDF.js", "React/Vue.js", "Node.js"] {
            assert_eq!(as_path(no), None, "{no}");
        }
        for yes in ["App.js", "Gruntfile.js", "src/solid.js", "server.js", "src/Day.js"] {
            assert!(as_path(yes).is_some(), "{yes}");
        }
        let step = "1. **A**\n   - Build the React/Vue.js adapter in `src/a.ts`, per github.com/foo/bar/README.md, with Solid.js.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["src/a.ts"]);
    }

    /// Round 3: a code span opened and closed on one line isn't an open
    /// block, and the steps after it are read.
    #[test]
    fn one_line_fence_is_not_open() {
        let plan = GOOD_PLAN.replace(
            "2. API\n",
            "2. API\n   - ```ts export const x = 1``` is enough in `src/x.ts`.\n",
        );
        assert_eq!(plan_problems(&plan), Vec::<String>::new());
        assert_eq!(steps(&plan).len(), 2);
        let open = GOOD_PLAN.replace("2. API\n", "2. API\n   ```ts\n   const x = 1;\n");
        assert!(plan_problems(&open)
            .iter()
            .any(|p| p.contains("Close the code block")));
    }

    /// Round 3: a precise one-line step with a verb the list lacked (bump,
    /// upgrade, revert) isn't vague.
    #[test]
    fn bump_and_upgrade_steps_are_not_vague() {
        let plan = GOOD_PLAN.replace(
            "## Decisions",
            "3. Bump `tokio` to 1.40 in `migrations/002_users.sql`.\n\
             4. Revert the `--release` flag in `src/api/mod.rs`.\n\n## Decisions",
        );
        assert_eq!(plan_problems(&plan), Vec::<String>::new());
    }

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
            // P4.2: the mark is the path's it follows.
            [("src/pages/Users.tsx", true), ("src/pages/", false)]
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
            ".env.local",
            ".gitignore",
            ".eslintrc.json",
            ".tool-versions",
            ".rovik",
            "src/a.rs:10-20",
            "docs/design",
        ] {
            assert!(as_path(yes).is_some(), "{yes}");
        }
        for no in [
            "fn a(x: u8)",
            ".btn-primary",
            ".card",
            ".nav-item.active",
            ".modal:hover",
            ".ts",
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

    /// P4.4: CRLF, `1. 1. 1.` numbering, `Group: none`, a "Next steps"
    /// note before the steps.
    #[test]
    fn steps_parse_robustly() {
        let plan =
            "# P\r\n\r\n## Next steps (later)\r\n\r\n1. Nothing here.\r\n\r\n## Steps\r\n\r\n\
            1. **One**\r\n   - `src/a.rs`: add a.\r\n   - Group: Pages\r\n\
            1. **Two**\r\n   - `src/b.rs`: add b.\r\n   - Group: none\r\n\
            1. **Three**\r\n   - `src/c.rs`: add c.\r\n   - Group: pages\r\n\r\n\
            ## Validation\r\n- `cargo test`\r\n";
        let s = steps(plan);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(s[0].title, "One");
        assert_eq!(s[0].files[0].path, "src/a.rs");
        assert_eq!(s[0].group.as_deref(), Some("pages"));
        assert_eq!(s[1].group, None);
        assert_eq!(s[2].group.as_deref(), Some("pages"));
        assert!(!s[0].body.contains('\r'));
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
    }

    /// Round 2, P4.4: `1. 2. 2. 3.` is steps 1 to 4, and the last two say
    /// what the plan numbers them, in messages too.
    #[test]
    fn duplicate_number_in_the_middle() {
        let plan = "# P\n\n## Steps\n\n1. **A**\n   - `src/a.rs`: add a.\n\
            2. **B**\n   - `src/b.rs`: add b.\n2. **C**\n   - `src/c.rs`: add c.\n\
            3. **D**\n   - `src/gone.rs`: add d.\n\n## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(
            s.iter().map(|s| s.written).collect::<Vec<_>>(),
            [None, None, Some(2), Some(3)]
        );
        let tree = Tree::new(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let p = project_problems(plan, &tree.0);
        assert!(
            p[0].starts_with("Step 4 (numbered 3. in the plan) names `src/gone.rs`"),
            "{p:?}"
        );
        // Numbers that go up one by one are as written.
        assert!(steps(REAL_PLAN).iter().all(|s| s.written.is_none()));
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
        // P4.2: `a.rs` (new): register in `routs.rs` leaves routs.rs to the
        // existence check.
        let marks = "# M\n\n## Steps\n\n1. **A**\n   - `src/a.rs` (new): register it in `src/routs.rs`.\n\n## Validation\n- `cargo test`\n";
        let p = project_problems(marks, &tree.0);
        assert!(
            p.iter()
                .any(|p| p.starts_with("Step 1 names `src/routs.rs`, which isn't in the project")),
            "{p:?}"
        );
        // P4.1: a folder and a file in it; a file one changes and another
        // reads; each refused with its steps.
        let group = |a: &str, b: &str| {
            format!(
                "# G\n\n## Steps\n\n1. **Contract**\n   - `src/api/mod.rs`: stubs.\n\
                 2. **A**\n   - {a}\n   - Group: g\n3. **B**\n   - {b}\n   - Group: g\n\n## Validation\n- `cargo test`\n"
            )
        };
        let p = project_problems(
            &group(
                "`src/api/`: the handlers.",
                "`src/api/mod.rs`: register them.",
            ),
            &tree.0,
        );
        assert!(
            p.iter()
                .any(|p| p
                    .starts_with("Steps 2 and 3 are both in group \"g\" but both name `src/api`")),
            "{p:?}"
        );
        let p = project_problems(
            &group(
                "`src/api/mod.rs`: change the signature.",
                "`src/api/mod.rs` (read): uses it.",
            ),
            &tree.0,
        );
        assert!(
            p.iter().any(|p| p.starts_with(
                "Steps 2 and 3 are both in group \"g\" but both name `src/api/mod.rs`"
            )),
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

    /// Round 2, P4.3: word pairs and project names in prose aren't step
    /// files, so they don't stop the plan being saved.
    #[test]
    fn prose_word_pairs_are_not_files() {
        let plan = "# UI\n\nTidy the toolbar.\n\n## Steps\n\n\
            1. **Buttons**\n   - Change the add/remove buttons and an A/B test, bump Node.js in `src/ui.rs`.\n\
            2. **Client**\n   - Keep the client/server split; update `src/ui.rs` docs.\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        let files: Vec<&str> = s[0].files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["src/ui.rs"], "{s:#?}");
        assert_eq!(s[1].files.len(), 1, "{s:#?}");
        let tree = Tree::new(&["src/ui.rs"]);
        assert_eq!(plan_problems_in(plan, Some(&tree.0)), Vec::<String>::new());
        // Written plainly, a path still counts with an extension or a `/`.
        let plain = plan.replace("`src/ui.rs`", "src/ui.rs");
        assert_eq!(steps(&plain)[0].files[0].path, "src/ui.rs");
        assert_eq!(
            steps("## Steps\n1. change src/api/ here")[0].files[0].path,
            "src/api/"
        );
    }

    /// Round 2, P4.3: root files with extensions the list doesn't know.
    #[test]
    fn root_level_unknown_extensions_still_count() {
        for name in [
            ".env.local",
            "vite.config.mts",
            "Dockerfile.dev",
            "go.work",
            "renovate.json5",
        ] {
            for written in [format!("`{name}`"), name.to_string()] {
                let step = format!("1. **Config**\n   - Change {written} to set the port.");
                assert!(names_a_file(&step), "{written}");
                let files: Vec<String> = step_files(&step).into_iter().map(|f| f.path).collect();
                assert_eq!(files, [name], "{written}");
            }
        }
        // Member access stays code.
        assert!(step_files("1. Update `self.items` and `config.port`.").is_empty());
    }

    /// Round 2: a plan written with CRLF is saved with LF only.
    #[test]
    fn save_plan_file_normalises_line_endings() {
        let tree = Tree::new(&["src/a.rs"]);
        let root = tree.0.to_string_lossy().into_owned();
        let path = save_plan_file(
            &root,
            "Do it",
            "# P\r\n\r\n## Steps\r\n1. x\r\n",
            None,
            "2026-01-01",
        )
        .unwrap();
        let saved = std::fs::read_to_string(path).unwrap();
        assert_eq!(saved, "# P\n\n## Steps\n1. x\n");
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
        assert!(looks_like_plan("# P\n## Steps\n1. do it"));
        assert!(looks_like_plan(
            "Here:\n\n## Steps\n1. a\n2. b\n\n## Validation\n- `cargo test`"
        ));
        assert!(!looks_like_plan("Done. 1. thing"));
        // An explanation with numbered steps isn't a plan to submit.
        assert!(!looks_like_plan(
            "The panel flickers because of a race.\n\n## Steps to reproduce\n1. Open a file\n2. Resize\n\n## Cause\nx"
        ));
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
        // P4.3: the form check and the step's scope read paths alike: any
        // extension in a folder, backslashes, no backticks needed.
        for (step, path) in [
            (
                "1. **G**\n   - `src/grammar.pest`: add a rule.",
                "src/grammar.pest",
            ),
            (
                "1. **S**\n   - `tests/x.snap`: update the snapshot.",
                "tests/x.snap",
            ),
            ("1. **P**\n   - change src/plain.rs to log.", "src/plain.rs"),
            ("1. **W**\n   - `src\\x.rs`: add a field.", "src/x.rs"),
        ] {
            assert!(names_a_file(step), "{step}");
            let files = step_files(step);
            assert_eq!(files.first().map(|f| f.path.as_str()), Some(path), "{step}");
        }
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
        // P4.9: a sibling folder with the same start; no panic on a
        // multi-byte character after the root.
        assert_eq!(relative_plan_path("/pq/a.md", Some("/p")), "/pq/a.md");
        assert_eq!(relative_plan_path("/pé/a.md", Some("/p")), "/pé/a.md");
    }

    #[test]
    fn paths_outside_the_project_are_named() {
        let tree = Tree::new(&["src/a.rs"]);
        let plan = "# P\n\nx\n\n## Steps\n\n1. **One**\n   - `../secrets/key.rs`: read it.\n   - `src/a.rs`: use it.\n\n## Validation\n- `cargo test`\n";
        let p = project_problems(plan, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].contains("`../secrets/key.rs`, which is outside the project"),
            "{p:?}"
        );
    }

    /// CSS classes and selectors in backticks aren't a step's files: a
    /// step that names only them names no file, and one that names the
    /// stylesheet too is checked against the project without them.
    #[test]
    fn css_classes_are_not_files() {
        let tree = Tree::new(&["src/styles/app.css", "src/App.tsx"]);
        let classes = "# Tidy the sidebar\n\nx\n\n## Steps\n\n\
            1. **Narrow it**\n   - `.sidebar`: set the width to 240px; `.sidebar__item`: add padding.\n\
            2. **Hover**\n   - `src/styles/app.css`: add a `.sidebar__item:hover` rule and change `.nav-item.active`.\n\n\
            ## Validation\n- `npm run build`\n";
        let p = plan_problems(classes);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 1 names no file")
                && p[0].contains("CSS classes, selectors and other names in backticks aren't files"),
            "{p:?}"
        );
        let with_file =
            classes.replace("`.sidebar`: set", "`src/styles/app.css`: in `.sidebar`, set");
        assert_eq!(
            plan_problems_in(&with_file, Some(&tree.0)),
            Vec::<String>::new()
        );
        let s = steps(&with_file);
        assert_eq!(s[0].files.len(), 1, "{:?}", s[0].files);
        assert_eq!(s[1].files.len(), 1, "{:?}", s[1].files);
        // Dotfiles still count, and are checked.
        let dotfile = classes.replace("`.sidebar`: set", "`.env.local`: set");
        let p = project_problems(&dotfile, &tree.0);
        assert!(
            p.iter()
                .any(|p| p.starts_with("Step 1 names `.env.local`, which isn't in the project")),
            "{p:?}"
        );
    }

    /// A file named by its file name alone is pointed to its full path;
    /// a new source file without a folder is sent back for one.
    #[test]
    fn bare_file_names_get_their_full_path() {
        let tree = Tree::new(&[
            "src/db/users.rs",
            "src/api/users.rs",
            "src/db/mod.rs",
            "Cargo.toml",
            "index.html",
        ]);
        let plan = |a: &str, b: &str| {
            format!(
                "# P\n\nx\n\n## Steps\n\n1. **One**\n   - {a}: add `fn load()`.\n2. **Two**\n   - {b}: add `fn save()`.\n\n## Validation\n- `cargo test`\n"
            )
        };
        let p = project_problems(&plan("`mod.rs`", "`Cargo.toml`"), &tree.0);
        assert_eq!(
            p,
            ["Step 1 names `mod.rs` by its file name alone: write its full path from the project root, `src/db/mod.rs`."]
        );
        let p = project_problems(&plan("`users.rs`", "`src/db/mod.rs`"), &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].ends_with("one of `src/api/users.rs`, `src/db/users.rs`."),
            "{p:?}"
        );
        // Reported once, with the full-path hint when nothing matches.
        let p = project_problems(&plan("`other.rs`", "`other.rs`"), &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with(
                "Step 1 names `other.rs`, which isn't in the project: fix the path (in full"
            ),
            "{p:?}"
        );
        // New: a source file at the root needs its folder; a root file is fine.
        let p = project_problems(&plan("`handlers.rs` (new)", "`src/db/mod.rs`"), &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 1 creates `handlers.rs` at the project root")
                && p[0].contains("no `.rs` file"),
            "{p:?}"
        );
        assert!(
            project_problems(&plan("`CHANGELOG.md` (new)", "`Dockerfile` (new)"), &tree.0)
                .is_empty()
        );
        assert!(project_problems(&plan("`src/db/new.rs` (new)", "`index.html`"), &tree.0).is_empty());
        // The form check alone doesn't know the project: no hint without a root.
        assert!(plan_problems_in(&plan("`mod.rs`", "`Cargo.toml`"), None).is_empty());
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

    /// Round 2 (§4.2), P4.2: on a line naming two paths, "(read)" is the mark
    /// of the path it follows; one after neither marks neither; a file the
    /// step changes on another line isn't read-only.
    #[test]
    fn read_mark_adjacent_only() {
        let marked = |step: &str| -> Vec<String> {
            step_files(step)
                .into_iter()
                .map(|f| {
                    if f.read_only {
                        format!("{} (read)", f.path)
                    } else {
                        f.path
                    }
                })
                .collect()
        };
        assert_eq!(
            marked("1. **A**\n   - `src/a.rs`: use the type in `src/types.rs` (read)."),
            ["src/a.rs", "src/types.rs (read)"]
        );
        assert_eq!(
            marked("1. **A**\n   - `src/types.rs` (read) for `src/a.rs`: the signature."),
            ["src/types.rs (read)", "src/a.rs"]
        );
        assert_eq!(
            marked("1. **A**\n   - `src/a.rs` and `src/b.rs`, with the types as context (read)."),
            ["src/a.rs", "src/b.rs"]
        );
        assert_eq!(
            marked("1. **A**\n   - `src/types.rs` (read): the signature.\n   - `src/types.rs`: add a field."),
            ["src/types.rs"]
        );
    }

    /// Round 2 (§4.2), P4.2: a mark right after its path with no space
    /// (`` `a.rs`(new) ``) is that path's, backticked or plain.
    #[test]
    fn mark_with_no_space() {
        for (line, path) in [
            ("`a.rs`(new): register it in `src/mod.rs`.", "a.rs"),
            ("src/a.rs(new): register it in `src/mod.rs`.", "src/a.rs"),
        ] {
            let files = step_files(&format!("1. **A**\n   - {line}"));
            let got: Vec<(&str, bool)> = files.iter().map(|f| (f.path.as_str(), f.new)).collect();
            assert_eq!(got, [(path, true), ("src/mod.rs", false)], "{line}");
        }
        let files = step_files("1. **A**\n   - `src/types.rs`(read) for `src/a.rs`.");
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files[0].read_only && !files[1].read_only, "{files:?}");
    }

    /// Round 2 (§4.2), P4.2: on a line that names one path, a mark anywhere
    /// on it is that path's; a mark on a line naming no path marks nothing.
    #[test]
    fn one_path_mark_anywhere() {
        for (step, new, read_only) in [
            (
                "1. **A**\n   - Create (new file) `src/a.rs` with the helper.",
                true,
                false,
            ),
            (
                "1. **A**\n   - `src/a.rs`: the helper module (new).",
                true,
                false,
            ),
            (
                "1. **A**\n   - Context (read-only): the signature in `src/a.rs`.",
                false,
                true,
            ),
            (
                "1. **A** (new)\n   - `src/a.rs`: the helper.\n   - It's new (new).",
                false,
                false,
            ),
        ] {
            let files = step_files(step);
            assert_eq!(files.len(), 1, "{step}: {files:?}");
            assert_eq!(files[0].path, "src/a.rs", "{step}");
            assert_eq!(
                (files[0].new, files[0].read_only),
                (new, read_only),
                "{step}"
            );
        }
    }

    /// Round 2 (§4.2), P4.2: a folder that only a new file of the plan is in
    /// counts as there, from another step and a level up; a folder whose
    /// name only starts the same doesn't.
    #[test]
    fn new_file_makes_its_folder_exist() {
        let tree = Tree::new(&["src/lib.rs"]);
        let plan = "# F\n\n## Steps\n\n\
            1. **Module**\n   - `src/feature/sub/mod.rs` (new): add the module.\n\
            2. **Wire**\n   - `src/feature/`: register the module.\n   - `src/lib.rs`: declare it.\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(project_problems(plan, &tree.0), Vec::<String>::new());
        let p = project_problems(&plan.replace("`src/feature/`", "`src/feat/`"), &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 2 names `src/feat/`, which isn't in the project"),
            "{p:?}"
        );
    }

    /// Round 2 (§4.2), P4.3: library names written like files (Node.js,
    /// `Vue.js`, Three.js) aren't step files, in prose or backticks; a `.js`
    /// file in a folder or with another stem still is.
    #[test]
    fn node_js_in_prose_is_not_a_file() {
        let step = "1. **Deps**\n   - Bump Node.js, `Vue.js` and Three.js; load them in `src/app.js` and server.js.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["src/app.js", "server.js"]);
        assert!(!names_a_file("1. **Deps**\n   - Bump Node.js."));
        assert_eq!(as_path("Node.js"), None);
    }

    /// Round 2 (§4.2): an absolute Windows path (drive, UNC), backticked or
    /// plain, isn't the project's; a relative one with `\` is folded.
    #[test]
    fn windows_absolute_rejected() {
        for no in [
            "C:\\proj\\src\\a.rs",
            "c:/proj/a.rs",
            "D:a.rs",
            "\\\\server\\share\\a.rs",
        ] {
            assert_eq!(as_path(no), None, "{no}");
        }
        assert_eq!(as_path(".\\src\\a.rs").as_deref(), Some("src/a.rs"));
        let step = "1. **Win**\n   - Change `C:\\proj\\src\\a.rs` and C:/proj/src/b.rs to log.";
        assert!(step_files(step).is_empty(), "{:?}", step_files(step));
        assert!(!names_a_file(step));
    }

    /// Round 2 (§4.2): the path form (`as_path`) refuses a name with a space
    /// and keeps accented and CJK names, backticked or plain.
    #[test]
    fn spaces_and_unicode() {
        assert_eq!(as_path("src/my file.rs"), None);
        assert_eq!(
            as_path("src/données/é.rs").as_deref(),
            Some("src/données/é.rs")
        );
        assert_eq!(as_path("src/深/a.rs").as_deref(), Some("src/深/a.rs"));
        let step =
            "1. **I18n**\n   - `src/données/é.rs`: add the strings; load them in src/深/a.rs.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["src/données/é.rs", "src/深/a.rs"]);
        assert!(step_files("1. **S**\n   - `src/my file.rs`: add a field.").is_empty());
    }

    /// Round 2 (§4.2): a folder three deep keeps its closing `/` in every
    /// spelling, so it's read as a folder, and is checked as one.
    #[test]
    fn three_deep_folder_keeps_slash() {
        for t in ["src/a/b/c/", "./src//a/b/c/", "src\\a\\b\\c\\"] {
            assert_eq!(as_path(t).as_deref(), Some("src/a/b/c/"), "{t}");
        }
        assert_eq!(as_path("src/a/b/c").as_deref(), Some("src/a/b/c"));
        let step = "1. **C**\n   - Move the helpers under src/a/b/c/ and `src/a/b/d/`.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["src/a/b/c/", "src/a/b/d/"]);
        let tree = Tree::new(&["src/a/b/c/x.rs", "src/a/b/d/y.rs"]);
        assert_eq!(
            project_problems(&format!("## Steps\n{step}"), &tree.0),
            Vec::<String>::new()
        );
    }

    /// Round 2 (§4.2), pinned: a path with `\` written plainly isn't read
    /// (only backticked ones are folded to `/`), so a step naming only that
    /// names no file and the form check asks for a backticked path.
    #[test]
    fn unbackticked_backslash() {
        let plain = "1. **W**\n   - Change src\\api\\users.rs to log the id.";
        assert!(step_files(plain).is_empty(), "{:?}", step_files(plain));
        assert!(!names_a_file(plain));
        let ticked = plain.replace("src\\api\\users.rs", "`src\\api\\users.rs`");
        assert_eq!(step_files(&ticked)[0].path, "src/api/users.rs");
        let plan = format!(
            "# W\n\n## Steps\n\n{plain}\n2. **X**\n   - `src/x.rs`: add the log helper.\n\n## Validation\n- `cargo test`\n"
        );
        let p = plan_problems(&plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 1 names no file"), "{p:?}");
    }

    /// Round 2 (§4.2): a step that names no file is still a step (number,
    /// title and body kept, no files); the form check names it, the project
    /// check adds nothing, and its worker's scope is empty until a grant.
    #[test]
    fn step_with_zero_files() {
        use crate::agent_run::worker::WriteScope;
        let plan = "# Z\n\nx\n\n## Steps\n\n\
            1. **Helper**\n   - `src/a.rs`: add `fn helper()`.\n\
            2. **Run it** — run the migration by hand and check the logs for errors.\n\
            3. **Use**\n   - `src/b.rs`: call `helper()` from `main`.\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(s[1].title, "Run it");
        assert!(s[1].files.is_empty(), "{:?}", s[1].files);
        assert!(s[1].body.contains("check the logs"), "{}", s[1].body);
        let p = plan_problems(plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 2 names no file"), "{p:?}");
        let tree = Tree::new(&["src/a.rs", "src/b.rs"]);
        assert_eq!(project_problems(plan, &tree.0), Vec::<String>::new());
        let scope = WriteScope::for_step(&s[1]);
        assert_eq!(scope, WriteScope::default());
        assert_eq!(scope.describe(), "nothing yet");
        assert!(!scope.allows("src/a.rs", true) && !scope.allows("new.rs", false));
    }

    /// Round 2 (§4.2), P4.2: a mark in bold or italics right after a path is
    /// that path's, also on a line that names another path.
    #[test]
    fn bold_mark_after_path() {
        for line in [
            "`src/a.rs` **(new)**: register it in `src/mod.rs`.",
            "**`src/a.rs`** (new): register it in `src/mod.rs`.",
            "`src/a.rs` _(new)_: register it in `src/mod.rs`.",
        ] {
            let files = step_files(&format!("1. **A**\n   - {line}"));
            let got: Vec<(&str, bool)> = files.iter().map(|f| (f.path.as_str(), f.new)).collect();
            assert_eq!(got, [("src/a.rs", true), ("src/mod.rs", false)], "{line}");
        }
        let files =
            step_files("1. **A**\n   - `src/types.rs` **(read)** for `src/a.rs`: the signature.");
        let got: Vec<(&str, bool)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.read_only))
            .collect();
        assert_eq!(got, [("src/types.rs", true), ("src/a.rs", false)]);
    }

    /// Round 2 (§4.2), P4.3: an extension has a letter, so a version
    /// (`docs/v1.2`, HTTP/1.1, node-v18.2) isn't a file; `media/intro.mp4`
    /// and a versioned folder with its `/` are.
    #[test]
    fn digit_extension_is_not_a_file() {
        for no in ["docs/v1.2", "api/v2.0.1", "HTTP/1.1", "node-v18.2"] {
            assert_eq!(as_path(no), None, "{no}");
        }
        assert_eq!(
            as_path("media/intro.mp4").as_deref(),
            Some("media/intro.mp4")
        );
        assert_eq!(as_path("docs/v1.2/").as_deref(), Some("docs/v1.2/"));
        let step = "1. **Docs**\n   - Move the docs/v1.2 notes and the HTTP/1.1 caveat into `docs/guide.md`.";
        let files: Vec<String> = step_files(step).into_iter().map(|f| f.path).collect();
        assert_eq!(files, ["docs/guide.md"]);
    }

    /// Round 2, P4.4: numbers that go up with gaps (`2. 4. 7.`) are kept as
    /// written, with no separate written number, and the checks use them.
    #[test]
    fn gapped_numbers_are_kept() {
        let plan = "# P\n\n## Steps\n\n2. **B**\n   - `src/b.rs`: add b.\n\
            4. **D**\n   - `src/d.rs`: add d.\n7. **G**\n   - `src/gone.rs`: add g.\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [2, 4, 7]);
        assert!(s.iter().all(|s| s.written.is_none()), "{s:#?}");
        assert_eq!(s[2].named(), "7");
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
        let tree = Tree::new(&["src/b.rs", "src/d.rs"]);
        let p = project_problems(plan, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 7 names `src/gone.rs`"), "{p:?}");
    }

    /// P4.4: `## Steps:` is `## Steps` (any case, a space before the colon
    /// too), taken before a "## Next steps" note.
    #[test]
    fn steps_heading_colon() {
        let plan = "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps:\n\n\
            1. **One**\n   - `src/a.rs`: add a.\n2. **Two**\n   - `src/b.rs`: add b.\n\n## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
        let spaced = plan.replace("## Steps:", "## STEPS :");
        assert_eq!(steps(&spaced).len(), 2);
    }

    /// P4.4: a `Group:` line in a ``` or ~~~ code block isn't the step's
    /// group; a real one after the block still is.
    #[test]
    fn group_in_fenced_code_ignored() {
        let plan = "# P\n\n## Steps\n\n\
            1. **One**\n   - `src/a.rs`: add a, configured as:\n\n     ```yaml\n     Group: fenced\n     ```\n\
            2. **Two**\n   - `src/b.rs`: add b, configured as:\n\n     ~~~\n     group: tilde\n     ~~~\n\
            3. **Three**\n   - `src/c.rs`: add c.\n\n     ```\n     Group: fenced\n     ```\n   - Group: Real\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(s.len(), 3, "{s:#?}");
        assert_eq!(s[0].group, None);
        assert_eq!(s[1].group, None);
        assert_eq!(s[2].group.as_deref(), Some("real"));
        // The code stays in the body.
        assert!(s[1].body.contains("group: tilde"), "{}", s[1].body);
    }

    /// P4.4: numbered sub-lists indented with tabs, spaces or both belong
    /// to their step: only the least indented numbered lines are steps.
    #[test]
    fn tabs_vs_spaces_sub_lists() {
        let plan = "# P\n\n## Steps\n\n\
            1. **Tabs**\n\t1. Add `src/a.rs` (new).\n\t2. Register it in `src/lib.rs`.\n\
            2. **Spaces**\n    1. Add `src/b.rs` (new).\n    2. Register it in `src/lib.rs`.\n\
            3. **Both**\n\t1. Add `src/c.rs` (new).\n  \t2. Register it in `src/lib.rs`.\n\t\t1. Export it.\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| s.number).collect::<Vec<_>>(),
            [1, 2, 3],
            "{s:#?}"
        );
        for (step, new) in s.iter().zip(["src/a.rs", "src/b.rs", "src/c.rs"]) {
            let files: Vec<(&str, bool)> = step
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.new))
                .collect();
            assert_eq!(files, [(new, true), ("src/lib.rs", false)], "{}", step.body);
        }
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
    }

    /// P4.4: numbered sub-lists (`.` or `)`, nested twice) belong to their
    /// step: they aren't steps, don't renumber the steps and don't count as
    /// the numbers the plan shows.
    #[test]
    fn numbered_sub_lists_inside_step() {
        let plan = "# P\n\n## Steps\n\n\
            1. **Store**\n   1. Add `src/store.rs` (new) with `fn save(x: &X)`.\n   2. Register it in `src/lib.rs`.\n      1. Export `save` from the crate root.\n\
            2. **Use it**\n   1) Call `save` from `src/api.rs`.\n   2) Update `src/api_test.rs` to check it.\n   3) Document it in `README.md`.\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| (s.number, s.written)).collect::<Vec<_>>(),
            [(1, None), (2, None)],
            "{s:#?}"
        );
        assert_eq!(s[1].title, "Use it");
        let files = |i: usize| {
            s[i].files
                .iter()
                .map(|f| (f.path.as_str(), f.new))
                .collect::<Vec<_>>()
        };
        assert_eq!(files(0), [("src/store.rs", true), ("src/lib.rs", false)]);
        assert_eq!(
            files(1),
            [
                ("src/api.rs", false),
                ("src/api_test.rs", false),
                ("README.md", false)
            ]
        );
        assert!(s[0].body.contains("1. Export `save`"), "{}", s[0].body);
        assert!(!s[0].body.contains("Use it"));
        assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
    }

    /// P4.4: a group's name reads the same in backticks, in bold or both,
    /// in any case, so these steps are one group; `none` in backticks is
    /// still no group.
    #[test]
    fn group_name_with_backticks_and_bold() {
        let plan = "# P\n\n## Steps\n\n\
            1. **A**\n   - `src/a.rs`: add a.\n   - Group: `Backend`\n\
            2. **B**\n   - `src/b.rs`: add b.\n   - **Group:** backend\n\
            3. **C**\n   - `src/c.rs`: add c.\n   - **Group**: **backend**\n\
            4. **D**\n   - `src/d.rs`: add d.\n   - Group: **`backend`**\n\
            5. **E**\n   - `src/e.rs`: add e.\n   - **Group: `BACKEND`**\n\
            6. **F**\n   - `src/f.rs`: add f.\n   - Group: `none`\n\n\
            ## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(
            s.iter().map(|s| s.group.as_deref()).collect::<Vec<_>>(),
            [
                Some("backend"),
                Some("backend"),
                Some("backend"),
                Some("backend"),
                Some("backend"),
                None
            ],
            "{s:#?}"
        );
        // One group: two of its steps can't share a file.
        let shared = plan.replace("`src/b.rs`: add b.", "`src/a.rs`: add b.");
        let tree = Tree::new(&["src/a.rs", "src/c.rs", "src/d.rs", "src/e.rs", "src/f.rs"]);
        let p = project_problems(&shared, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with(
                "Steps 1 and 2 are both in group \"backend\" but both name `src/a.rs`"
            ),
            "{p:?}"
        );
    }

    /// Round 2, P4.4: `## Steps (ordered)` is the steps heading, alone and
    /// after a "## Next steps" note (which would otherwise be read first).
    #[test]
    fn steps_heading_with_suffix() {
        let plan = "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps (ordered)\n\n\
            1. **One**\n   - `src/a.rs`: add a.\n\
            2. **Two**\n   - `src/b.rs`: add b.\n\n## Validation\n- `cargo test`\n";
        let alone = plan.replace("## Next steps (later)\n\n1. Nothing here.\n\n", "");
        for plan in [alone.as_str(), plan] {
            let s = steps(plan);
            assert_eq!(
                s.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
                ["One", "Two"],
                "{plan}"
            );
            assert_eq!(s[1].files[0].path, "src/b.rs");
            assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
        }
    }

    /// Round 2, P4.4: a numbered `## 3. Steps` is the steps heading, alone
    /// and after a "## 2. Next steps" note; its number isn't a step's.
    #[test]
    fn steps_heading_numbered() {
        let plan = "# P\n\n## 1. Summary\n\nTwo files.\n\n## 2. Next steps (later)\n\n1. Nothing here.\n\n\
            ## 3. Steps\n\n1. **One**\n   - `src/a.rs`: add a.\n2. **Two**\n   - `src/b.rs`: add b.\n\n\
            ## 4. Validation\n- `cargo test`\n";
        let alone = plan.replace("## 2. Next steps (later)\n\n1. Nothing here.\n\n", "");
        for plan in [alone.as_str(), plan] {
            let s = steps(plan);
            assert_eq!(
                s.iter().map(|s| s.number).collect::<Vec<_>>(),
                [1, 2],
                "{plan}"
            );
            assert_eq!(s[0].title, "One");
            assert_eq!(s[1].files[0].path, "src/b.rs");
            assert!(plan_problems(plan).is_empty(), "{:?}", plan_problems(plan));
        }
    }

    /// Round 2: a numbered heading (`## 5. Decisions`) is its name's
    /// section, so a step worker's brief keeps the plan's decisions.
    #[test]
    fn section_finds_numbered_headings() {
        let plan = "# P\n\n## 1. Summary\n\nTwo files.\n\n## 3. Steps\n\n\
            1. **One**\n   - `src/a.rs`: add a.\n2. **Two**\n   - `src/b.rs`: add b.\n\n\
            ## 4) Validation\n- `cargo test`\n\n## 5. Decisions\n\n- Keep the old API.\n";
        assert_eq!(
            section(plan, "Decisions").as_deref(),
            Some("- Keep the old API.")
        );
        assert_eq!(
            section(plan, "validation").as_deref(),
            Some("- `cargo test`")
        );
        assert_eq!(section(plan, "Summary").as_deref(), Some("Two files."));
        assert_eq!(section("## 2024 roadmap\n\nx\n", "roadmap"), None);
        assert_eq!(
            section("## 2024 roadmap\n\nx\n", "2024").as_deref(),
            Some("x")
        );
    }

    /// Round 2, P4.4: the form check names a step as `steps` numbers it:
    /// in `2. 4. 7.` the step without a file is step 4, not "step 2"; in
    /// `1. 2. 2. 3.` the last is step 4, numbered 3. in the plan.
    #[test]
    fn gapped_numbers_in_form_check_messages() {
        let plan = "# P\n\n## Steps\n\n2. **B**\n   - `src/b.rs`: add b.\n\
            4. **D**\n   - Add the d helper next to b, with its own tests.\n\
            7. **G**\n   - `src/g.rs`: add g.\n\n## Validation\n- `cargo test`\n";
        let p = plan_problems(plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 4 names no file"), "{p:?}");
        let plan = "# P\n\n## Steps\n\n1. **A**\n   - `src/a.rs`: add a.\n\
            2. **B**\n   - `src/b.rs`: add b.\n2. **C**\n   - `src/c.rs`: add c.\n\
            3. **D**\n   - `src/d.rs`.\n\n## Validation\n- `cargo test`\n";
        let p = plan_problems(plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 4 (numbered 3. in the plan) is too vague"),
            "{p:?}"
        );
    }

    /// Round 2, P4.4: a code block that's never closed runs to the end of
    /// the plan (no guess at where it was meant to end), so the steps after
    /// it are its step's body; the form check asks for it to be closed.
    #[test]
    fn unclosed_fence_swallows_rest() {
        let plan = "# P\n\n## Steps\n\n\
            1. **One**\n   - `src/a.rs`: add `a` to the module.\n\
            2. **Two**\n   - `src/b.rs`: add `b` with this signature:\n\n     ```rust\n     pub fn b() -> u8\n\
            3. **Three**\n   - `src/c.rs`: call `b` from `c`.\n\n## Validation\n- `cargo test`\n";
        let s = steps(plan);
        assert_eq!(s.len(), 2, "{s:#?}");
        assert_eq!(
            s[1].files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["src/b.rs"]
        );
        assert!(
            s[1].body.contains("3. **Three**") && s[1].body.contains("## Validation"),
            "{}",
            s[1].body
        );
        let p = plan_problems(plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Close the code block"), "{p:?}");
        let closed = plan.replace("pub fn b() -> u8\n", "pub fn b() -> u8\n     ```\n");
        assert_eq!(steps(&closed).len(), 3);
        assert!(
            plan_problems(&closed).is_empty(),
            "{:?}",
            plan_problems(&closed)
        );
    }

    /// Round 2, P4.9: a Windows root, with or without its last separator,
    /// and a `/`-written root a path was joined onto; the shown path keeps
    /// the OS's separators. A sibling with the same start isn't under it.
    #[test]
    fn relative_plan_path_windows_root() {
        let file = r"C:\p\.rovik\plans\a.md";
        assert_eq!(
            relative_plan_path(file, Some(r"C:\p")),
            r".rovik\plans\a.md"
        );
        assert_eq!(
            relative_plan_path(file, Some(r"C:\p\")),
            r".rovik\plans\a.md"
        );
        assert_eq!(
            relative_plan_path(r"C:/p\.rovik\plans\a.md", Some("C:/p")),
            r".rovik\plans\a.md"
        );
        assert_eq!(
            relative_plan_path(r"C:\pq\a.md", Some(r"C:\p")),
            r"C:\pq\a.md"
        );
        assert_eq!(relative_plan_path(r"C:\a.md", Some(r"C:\")), "a.md");
    }

    /// Round 2, P4.9: the root itself is shown whole, never as an empty
    /// path; without a root the path is shown as it is.
    #[test]
    fn relative_plan_path_root_equals_path() {
        assert_eq!(relative_plan_path("/p", Some("/p")), "/p");
        assert_eq!(relative_plan_path("/p", Some("/p/")), "/p");
        assert_eq!(relative_plan_path("/p/", Some("/p")), "/p/");
        assert_eq!(relative_plan_path("/", Some("/")), "/");
        assert_eq!(relative_plan_path(r"C:\p", Some(r"C:\p\")), r"C:\p");
        assert_eq!(relative_plan_path("/p/a.md", None), "/p/a.md");
    }

    /// Round 2, P4.9 (documented): `src/..` is the project root, not a
    /// path out of it. Like `./` it's no step's file and isn't reported as
    /// outside the project; a step that names nothing else names no file.
    #[test]
    fn trailing_dotdot_is_dropped_silently() {
        assert!(as_path("src/..").is_none());
        let tree = Tree::new(&["src/a.rs"]);
        let plan = "# P\n\nx\n\n## Steps\n\n\
            1. **Root**\n   - `src/..`: update the list of what is in it.\n\
            2. **Use**\n   - `src/a.rs`: use the list in the loader.\n\n\
            ## Validation\n- `cargo test`\n";
        assert!(
            steps(plan)[0].files.is_empty(),
            "{:?}",
            steps(plan)[0].files
        );
        let p = project_problems(plan, &tree.0);
        assert!(p.is_empty(), "{p:?}");
        let p = plan_problems(plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 1 names no file"), "{p:?}");
    }

    /// Round 2, P4.9: a `..` that stays in the project (`src/../x.rs`) isn't
    /// "outside the project": the check says how to write it; one that
    /// leaves it is still outside, and `src/../` (the root) is no file.
    #[test]
    fn dotdot_inside_the_project_is_named_as_it_resolves() {
        let tree = Tree::new(&["src/a.rs", "x.rs"]);
        let plan = "# P\n\nx\n\n## Steps\n\n\
            1. **One**\n   - `src/../x.rs`: use it.\n   - `src/api/../lib/`: the helpers.\n\
               - `src/../../k.rs`: read it.\n   - `src/../`: format it.\n   - `src/a.rs`: use it.\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(
            project_problems(plan, &tree.0),
            [
                "Step 1 names `src/../x.rs`: write it as `x.rs`, without `..`.",
                "Step 1 names `src/api/../lib/`: write it as `src/lib/`, without `..`.",
                "Step 1 names `src/../../k.rs`, which is outside the project: use a path inside it.",
            ]
        );
        assert_eq!(without_dots("a/b/../../c"), Some("c".into()));
        assert_eq!(without_dots("a/../.."), None);
        assert_eq!(without_dots("./a/./b/"), Some("a/b/".into()));
    }

    /// Round 2, P4.4: a period or a note after a group's name isn't part of
    /// it, so these steps are the group the lead asks for ("backend").
    #[test]
    fn group_name_followed_by_a_note_or_period() {
        let plan = "# P\n\n## Steps\n\n\
            1. **A**\n   - `src/a.rs`: add a.\n   - Group: `backend`.\n\
            2. **B**\n   - `src/b.rs`: add b.\n   - Group: `Backend` (runs alongside step 1)\n\
            3. **C**\n   - `src/c.rs`: add c.\n   - Group: backend.\n\
            4. **D**\n   - `src/d.rs`: add d.\n   - **Group:** backend (with step 1)\n\
            5. **E**\n   - `src/e.rs`: add e.\n   - Group: `none`.\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(
            steps(plan)
                .iter()
                .map(|s| s.group.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("backend"),
                Some("backend"),
                Some("backend"),
                Some("backend"),
                None
            ]
        );
    }
}

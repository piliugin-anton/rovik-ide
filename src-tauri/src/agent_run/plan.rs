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

static STEPS_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b.*$").unwrap());
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
static BARE_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(Makefile|Dockerfile|Justfile|Procfile|Gemfile|Rakefile|Containerfile)\b").unwrap());
/// Words that say what a step does to its files.
static CHANGE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(add\w*|chang\w*|creat\w*|remov\w*|delet\w*|renam\w*|mov\w*|updat\w*|replac\w*|extend\w*|edit\w*|modif\w*|introduc\w*|implement\w*|refactor\w*|split\w*|extract\w*|merg\w*|wire\w*|register\w*|expos\w*|export\w*|import\w*|call\w*|return\w*|handl\w*|pass\w*|store\w*|emit\w*|drop\w*|switch\w*|convert\w*|define\w*|declare\w*|new|fix\w*|adjust\w*|insert\w*|append\w*|keep\w*|use|uses|using|set|sets|write\w*|read\w*|check\w*|validat\w*|test\w*|migrat\w*|document\w*|configur\w*|enabl\w*|disabl\w*|wrap\w*|guard\w*|inline\w*|generat\w*|render\w*|show\w*|hide\w*|load\w*|sav\w*|pars\w*|map\w*|send\w*|fetch\w*|accept\w*|reject\w*|limit\w*|cach\w*)\b",
    )
    .unwrap()
});

/// The text of the `## ...` section that starts at `heading_end`.
fn section_after(text: &str, heading_end: usize) -> &str {
    let after = &text[heading_end..];
    match NEXT_HEADING.find(after) {
        Some(next) => &after[..next.start()],
        None => after,
    }
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
                && !matches!(token.to_ascii_lowercase().as_str(), "and/or" | "either/or" | "i/o" | "n/a");
        }
        // "e.g", "i.e", "vs.x": abbreviations, not file names.
        let (name, ext) = token.rsplit_once('.').unwrap_or((token, ""));
        !(name.trim_start_matches('.').len() <= 2 && ext.len() <= 2) && token.len() >= 3
    })
}

/// The numbered items of a steps section, each with its sub-lines.
fn split_steps(section: &str) -> Vec<&str> {
    let starts: Vec<usize> = NUMBERED.find_iter(section).map(|m| m.start()).collect();
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
        None => problems.push("Add a \"## Steps\" section with the steps as a numbered list.".into()),
        Some(heading) => {
            // Code fences can hold numbered lines and paths that aren't steps.
            let section = CODE_BLOCK.replace_all(section_after(text, heading.end()), "");
            let steps = split_steps(&section);
            if steps.len() < 2 {
                problems.push("List the steps under \"## Steps\" as a numbered list (1., 2., ...).".into());
            } else {
                let no_file: Vec<usize> =
                    (1..=steps.len()).filter(|&n| !names_a_file(steps[n - 1])).collect();
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
pub fn save_plan_file(root: &str, request: &str, plan: &str, existing: Option<&str>, date: &str) -> Result<String, String> {
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
        let first: String = request.split('\n').next().unwrap_or("").chars().take(100).collect();
        format!("# Plan: {first}\n\n{body}")
    };
    crate::filesystem::write_file_content(path.clone(), format!("{titled}\n"))?;
    Ok(path)
}

/// The plan's path for display: relative to the project.
pub fn relative_plan_path(path: &str, root: Option<&str>) -> String {
    let Some(root) = root else { return path.to_string() };
    let r = root.trim_end_matches(['/', '\\']);
    match path.strip_prefix(r) {
        Some(rest) if !rest.is_empty() => rest[1..].to_string(),
        _ => path.to_string(),
    }
}

/// Plan mode: the current plan (as the file is now), for the model to revise.
pub fn current_plan_context(plan_file: Option<&str>, root: Option<&str>) -> String {
    let Some(file) = plan_file else { return String::new() };
    let Ok(text) = std::fs::read_to_string(file) else { return String::new() };
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

static LOOKS_LIKE_STEPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b").unwrap());
static NUMBERED_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s").unwrap());

/// A reply that reads like a plan (written out instead of submitted).
pub fn looks_like_plan(text: &str) -> bool {
    LOOKS_LIKE_STEPS.is_match(text) && NUMBERED_LINE.is_match(text)
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
    Option { label: String },
    Custom { text: String },
    /// "You decide".
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum QuestionsOutcome {
    Answers { answers: Vec<QuestionAnswer> },
    /// Typed in the composer instead of answering in the card.
    Reply { reply: String },
    Stopped { stopped: bool },
}

const MAX_QUESTIONS: usize = 3;
const MAX_OPTIONS: usize = 6;

fn trimmed(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
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
        let Some(question) = trimmed(r.get("question")) else { continue };
        let options: Vec<QuestionOption> = r
            .get("options")
            .and_then(|o| o.as_array())
            .into_iter()
            .flatten()
            .filter_map(|o| match o {
                Value::String(s) => {
                    let label = s.trim();
                    (!label.is_empty()).then(|| QuestionOption { label: label.into(), description: None, recommended: false })
                }
                Value::Object(opt) => trimmed(opt.get("label")).map(|label| QuestionOption {
                    label,
                    description: opt.get("description").and_then(|d| d.as_str()).map(str::to_string),
                    recommended: opt.get("recommended") == Some(&Value::Bool(true)),
                }),
                _ => None,
            })
            .take(MAX_OPTIONS)
            .collect();
        // Without options the only way to answer is in words.
        let allow_custom = r.get("allow_custom") != Some(&Value::Bool(false)) || options.is_empty();
        out.push(Question { question, reason: trimmed(r.get("reason")), options, allow_custom });
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
                (!title.is_empty()).then(|| TodoItem { title: title.into(), status: "pending".into() })
            }
            Value::Object(o) => trimmed(o.get("title")).map(|title| {
                let status = o
                    .get("status")
                    .and_then(|s| s.as_str())
                    .filter(|s| TODO_STATUSES.contains(s))
                    .unwrap_or("pending");
                TodoItem { title, status: status.into() }
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
        let names = active.iter().map(|i| format!("\"{}\"", i.title)).collect::<Vec<_>>().join(", ");
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
        current.map(|c| format!("; now: {}", c.title)).unwrap_or_default()
    )
}

/// Whether a stored message is one the IDE wrote (check failures, nudges).
pub fn is_harness(m: &MessageRow) -> bool {
    m.meta.as_ref().and_then(|meta| meta.get("harness")).is_some_and(|h| !h.is_null())
}

/// Whether the user dismissed the open checklist with this message
/// (`meta.checklistClosed`, set by the checklist panel's dismiss ✕).
pub fn is_checklist_dismissed(m: &MessageRow) -> bool {
    m.meta
        .as_ref()
        .and_then(|meta| meta.get("checklistClosed").or_else(|| meta.get("checklist_closed")))
        .is_some_and(|v| v == &Value::Bool(true))
}

/// The latest `todo` call in one message, if it parses to a non-empty list.
fn todo_in_message(m: &MessageRow) -> Option<Vec<TodoItem>> {
    let call = m.tool_calls.iter().flatten().rev().find(|c| c.name == "todo")?;
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
///   user dismissed it with `meta.checklistClosed` on the new message.
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
    let last_user = messages.iter().rposition(|m| m.role == "user" && !is_harness(m));
    let Some(last) = last_user else {
        // No user message yet: whatever the latest todo call says.
        for m in messages.iter().rev() {
            if let Some(items) = todo_in_message(m) {
                return Some(CurrentChecklist { items, carried: false, from_message_id: Some(m.id.clone()) });
            }
        }
        return None;
    };
    // The current request: everything after the last real user message.
    for m in messages[last + 1..].iter().rev() {
        if let Some(items) = todo_in_message(m) {
            return Some(CurrentChecklist { items, carried: false, from_message_id: Some(m.id.clone()) });
        }
    }
    // No checklist in the current request: a dismissal closes it.
    if is_checklist_dismissed(&messages[last]) {
        return None;
    }
    // The previous request only: past the last user message, up to the one
    // before it. A second user message stops the search, so an old
    // checklist doesn't linger across unrelated work.
    let prev_user = messages[..last].iter().rposition(|m| m.role == "user" && !is_harness(m));
    let from = prev_user.map(|p| p + 1).unwrap_or(0);
    for m in messages[from..last].iter().rev() {
        if let Some(items) = todo_in_message(m) {
            if items.iter().all(is_settled) {
                return None;
            }
            return Some(CurrentChecklist { items, carried: true, from_message_id: Some(m.id.clone()) });
        }
    }
    None
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
    use serde_json::json;

    const GOOD_PLAN: &str = "# Add users\n\nSummary.\n\n## Steps\n\
        1. Users table\n   - `migrations/002_users.sql` (new): create the `users` table with `id`, `email`.\n\
        2. API\n   - `src/api/users.rs`: add `fn list_users(db: &Db) -> Vec<User>` and register it in `src/api/mod.rs`.\n\n\
        ## Decisions\n- none\n\n## Validation\n- `cargo test`\n- open /users by hand";

    #[test]
    fn plan_checks() {
        assert!(plan_problems(GOOD_PLAN).is_empty(), "{:?}", plan_problems(GOOD_PLAN));
        let bad = "Some plan\n## Steps\n1. Only one";
        let problems = plan_problems(bad);
        assert_eq!(problems.len(), 3, "title, steps, validation: {problems:?}");
        assert_eq!(plan_problems("  "), vec!["The plan is empty."]);
        let code = format!("{GOOD_PLAN}\n```\n{}```", "x\n".repeat(20));
        assert_eq!(plan_problems(&code).len(), 1);
        assert!(plan_problems(&code)[0].starts_with("Leave the code out"));
        // One-line signatures don't count as code.
        let sigs = format!("{GOOD_PLAN}\n{}", "```rust\nfn a() -> u8;\n```\n".repeat(30));
        assert!(plan_problems(&sigs).is_empty(), "{:?}", plan_problems(&sigs));
        assert!(looks_like_plan("## Steps\n1. do it"));
        assert!(!looks_like_plan("Done. 1. thing"));
    }

    #[test]
    fn steps_need_files_and_changes() {
        // Titles alone aren't a plan.
        let terse = "# T\n## Steps\n1. Table\n2. API\n## Validation\n- run the tests";
        let problems = plan_problems(terse);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("Steps 1 and 2 name no file"), "{problems:?}");
        assert!(problems[1].starts_with("Steps 1 and 2 are too vague"), "{problems:?}");
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
        assert!(!names_a_file("see https://example.com and/or ask, i.e. later"));
        assert!(!names_a_file("bump to 1.2 and update the API."));
    }

    #[test]
    fn plans_need_validation() {
        let no_check = GOOD_PLAN.replace("## Validation", "## Notes");
        let problems = plan_problems(&no_check);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("Add a \"## Validation\""));
        // An empty section doesn't count; other names do.
        assert_eq!(plan_problems(&GOOD_PLAN.replace("- `cargo test`\n- open /users by hand", "")).len(), 1);
        assert!(plan_problems(&GOOD_PLAN.replace("## Validation", "## How to verify")).is_empty());
        assert!(plan_problems(&GOOD_PLAN.replace("## Validation", "## Testing and validation")).is_empty());
    }

    #[test]
    fn stems_and_relative_paths() {
        assert_eq!(file_stem("Add a users table, please!", "2026-10-04"), "2026-10-04-add-a-users-table-please");
        assert_eq!(file_stem("???", "2026-10-04"), "2026-10-04-plan");
        assert_eq!(relative_plan_path("/p/.rovik/plans/a.md", Some("/p/")), ".rovik/plans/a.md");
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
            serde_json::from_value(json!({ "answers": [{ "kind": "option", "label": "SQLite" }] })).unwrap();
        let text = describe_outcome(&qs, &outcome);
        assert!(text.contains("1. Which DB?\n   → SQLite") && text.contains("2. Name?\n   → left it to you"));
        let stopped: QuestionsOutcome = serde_json::from_value(json!({ "stopped": true })).unwrap();
        assert!(describe_outcome(&qs, &stopped).starts_with("Error:"));
        assert_eq!(serde_json::to_value(&stopped).unwrap(), json!({ "stopped": true }));
    }

    #[test]
    fn todo_lists() {
        let args = json!({ "items": ["a", { "title": "b", "status": "in_progress" }, { "title": "c", "status": "bogus" }] });
        let items = parse_todo(args.as_object().unwrap());
        assert_eq!(items[2].status, "pending");
        assert_eq!(describe_todo(&items), "Checklist updated: 0 of 3 done; now: b.");
    }

    #[test]
    fn plan_and_question_edge_cases() {
        // A steps heading with more words, steps with ")".
        let parens = GOOD_PLAN.replace("## Steps", "## Implementation steps").replace("1. ", "1) ").replace("2. ", "2) ");
        assert!(plan_problems(&parens).is_empty(), "{:?}", plan_problems(&parens));
        // Steps belong under the steps heading, not a later section.
        let misplaced = GOOD_PLAN.replace("## Steps\n", "## Steps\nnone\n## Other\n");
        assert_eq!(plan_problems(&misplaced).len(), 1);
        assert!(plan_problems(&format!("{GOOD_PLAN}\n{}", "x".repeat(40_001)))
            .iter()
            .any(|p| p.contains("over 40,000")));
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
        assert_eq!(qs[1].options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["ok"]);
        assert!(parse_questions(json!({}).as_object().unwrap()).is_empty());
        // Fewer answers than questions: the rest are left to the agent.
        let qs = parse_questions(json!({ "questions": [{ "question": "A?" }, { "question": "B?" }] }).as_object().unwrap());
        let outcome = QuestionsOutcome::Answers { answers: vec![QuestionAnswer::Custom { text: "mine".into() }] };
        let text = describe_outcome(&qs, &outcome);
        assert!(text.contains("\"mine\" (in their own words)") && text.contains("B?\n   → left it to you"));
        assert_eq!(outcome_kinds(&outcome), ["custom"]);
    }

    #[test]
    fn checklist_edge_cases() {
        assert_eq!(describe_todo(&[]), "Error: the checklist is empty. Pass items: [{ title, status }].");
        let many: Vec<Value> = (0..40).map(|i| json!(format!("step {i}"))).collect();
        assert_eq!(parse_todo(json!({ "items": many }).as_object().unwrap()).len(), MAX_TODO_ITEMS);
        let all_settled = parse_todo(
            json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "skipped" }, { "title": "c", "status": "changed" }] })
                .as_object()
                .unwrap(),
        );
        assert!(all_settled.iter().all(is_settled));
        assert_eq!(describe_todo(&all_settled), "Checklist updated: 3 of 3 done.");
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
        assert!(note.contains("- [done] a") && note.contains("- [pending] b"), "{note}");
        assert!(note.contains("don't copy it into the task card"), "{note}");
        assert!(checklist_note(&items, true).contains("carried from earlier work"), "{note}");
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
            todo_row("a1", json!({ "items": [{ "title": "a", "status": "done" }, { "title": "b", "status": "pending" }] })),
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
            todo_row("a1", json!({ "items": [{ "title": "a", "status": "done" }] })),
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

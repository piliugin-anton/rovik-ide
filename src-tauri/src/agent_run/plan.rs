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
pub const PLAN_BLOCKED_TOOLS: &[&str] = &["write_file", "edit_file", "run_command"];

/// Questions one run may ask; past that, the agent decides.
pub const MAX_QUESTIONS_PER_RUN: u32 = 3;

/// Added to the system prompt in Plan mode.
pub const PLAN_MODE_INSTRUCTIONS: &str = r###"## Plan mode

The user chose Plan mode: plan the task, don't carry it out. You can read files, list folders, search the code and use the web; you can't change files or run commands.

1. Investigate first: read the relevant code, find the existing patterns and conventions the change should follow.
2. Ask only what the code can't tell you and what really changes the design, the behavior or the scope (two existing approaches to choose between, an unclear requirement, a trade-off only the user can make). Use the ask_user tool, early, with all your questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything you can decide from the repository: decide it and say so in the plan. Never ask a question in plain text and stop; use ask_user.
3. Save the plan with the submit_plan tool, as Markdown. It's saved as a file the user may edit before implementing it, so make it stand on its own:
   - a "# " title;
   - a one-paragraph summary of the approach;
   - "## Steps": a numbered list, in order; each step a short title, then what changes and in which files (no code). Roughly 3 to 12 steps, each the size of something worth reviewing on its own;
   - "## Decisions": what the user answered, and what you decided yourself (with why);
   - how to verify the result (checks, tests, what to try by hand);
   - risks or open points, if any.
   The plan goes in submit_plan, not in your reply: after submitting, reply with a few sentences at most. Never write the plan out in a reply, and never write it more than once.
4. If a plan already exists (it's shown below), the user's new message is about that plan. When they add context or ask for changes, revise the existing plan: keep what still holds, work the new information into the steps, decisions and risks it affects, and submit the complete revised plan. Don't replace the plan with a summary of what the user said. If they only ask a question, answer it without submitting.
Keep it concrete enough that it can be implemented step by step without re-investigating. The user implements it by switching to Agent mode."###;

/*
 * ---------------------------------------------------------------------------
 * Plans
 * ---------------------------------------------------------------------------
 */

static STEPS_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b.*$").unwrap());
static NEXT_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^##\s").unwrap());
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s+\S").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#\s+\S").unwrap());
static CODE_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());

/// What's wrong with a plan, if anything (empty: it's fine).
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
    match STEPS_HEADING.find(text) {
        None => problems.push("Add a \"## Steps\" section with the steps as a numbered list.".into()),
        Some(steps) => {
            let after = &text[steps.end()..];
            let section = match NEXT_HEADING.find(after) {
                Some(next) => &after[..next.start()],
                None => after,
            };
            if NUMBERED.find_iter(section).count() < 2 {
                problems.push("List the steps under \"## Steps\" as a numbered list (1., 2., ...).".into());
            }
        }
    }
    // Code belongs in the implementation, not the plan.
    let (mut code_lines, mut longest) = (0usize, 0usize);
    for block in CODE_BLOCK.find_iter(text) {
        let lines = block.as_str().split('\n').count().saturating_sub(2);
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

#[derive(Debug, Clone, PartialEq)]
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

/// What the model reads after an update.
pub fn describe_todo(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Error: the checklist is empty. Pass items: [{ title, status }].".into();
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

/// The checklist of the current request: the latest todo call since the
/// user's last message (the IDE's own messages don't start a request).
pub fn latest_checklist(messages: &[MessageRow]) -> Option<Vec<TodoItem>> {
    for m in messages.iter().rev() {
        if m.role == "user" && !is_harness(m) {
            return None;
        }
        let call = m.tool_calls.iter().flatten().rev().find(|c| c.name == "todo");
        if let Some(call) = call {
            if let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) {
                let items = parse_todo(&args);
                if !items.is_empty() {
                    return Some(items);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_checks() {
        let good = "# Add users\n\nSummary.\n\n## Steps\n1. Table\n2. API\n\n## Decisions\n- none";
        assert!(plan_problems(good).is_empty(), "{:?}", plan_problems(good));
        let bad = "Some plan\n## Steps\n1. Only one";
        let problems = plan_problems(bad);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(plan_problems("  "), vec!["The plan is empty."]);
        let code = format!("# T\n## Steps\n1. a\n2. b\n```\n{}```", "x\n".repeat(20));
        assert!(plan_problems(&code)[0].starts_with("Leave the code out"));
        assert!(looks_like_plan("## Steps\n1. do it"));
        assert!(!looks_like_plan("Done. 1. thing"));
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
        assert!(plan_problems("# T\n## Implementation steps\n1) a\n2) b").is_empty());
        // Steps belong under the steps heading, not a later section.
        assert_eq!(plan_problems("# T\n## Steps\nnone\n## Other\n1. a\n2. b").len(), 1);
        assert!(plan_problems(&format!("# T\n## Steps\n1. a\n2. b\n{}", "x".repeat(40_001)))
            .iter()
            .any(|p| p.contains("over 40,000")));
        // Leading blank lines before the title are fine.
        assert!(plan_problems("\n\n# T\n## Steps\n1. a\n2. b").is_empty());

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
    }
}

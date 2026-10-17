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
use serde::Serialize;
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

The user chose Plan mode: investigate and write a plan; don't carry it out. You can read files, list folders, search the code and use the web; you can't change files or run commands.

A question (why something happens, how the code works, what an error means) gets an answer in your reply, naming the files and functions involved, and no plan. A request for a change, or for a plan, gets a plan.

1. Investigate first: the files that will change, the functions and types involved, the patterns and conventions to follow.
2. Ask only what the code can't tell you and what really changes the design, behavior or scope: with the ask_user tool, early, all questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything the repository decides: decide it and say so in the plan. Never ask in plain text and stop.
3. Save the plan with the submit_plan tool, as Markdown that stands on its own (the user may edit it before implementing it):
   - a "# " title and a one-paragraph summary of the approach;
   - "## Steps": a numbered list (`1.`, `2.`; not headings), in order, one reviewable change per step (usually 3 to 12). Each step has:
     - a short title;
     - a line "Files:" with every file it touches, in backticks, comma-separated, each by its full path from the project root (`src/db/users.rs`: never `users.rs`, nor `../src/db/users.rs` from the plan's folder); mark files it creates "(new)" and files it only reads "(read)". Name only paths you've seen in the repository. This line alone names the step's files; elsewhere, mention classes, identifiers or commands freely;
     - what to add, change or remove in them, by name (functions, types, components, settings) and how it behaves; a one-line signature or config line is fine, function bodies aren't;
     - the callers and dependents that must change with it.
     For example:
     2. **List users in the API**
        - Files: `src/api/users.rs` (new), `src/api/mod.rs`, `src/db/users.rs` (read)
        - Add `fn list_users(db: &Db) -> Result<Vec<User>>` to the new file, paging by `Config::page_size`; register the route in `mod.rs`.
   - "## Decisions": the user's answers and what you decided yourself, with why;
   - "## Validation": the commands to run (build, typecheck, tests, lint), the tests to add or update, what to try by hand;
   - "## Risks": open points, if any. Nothing else: no "## Findings" (the reviewer writes those).
   Before you submit, check the plan against the code the way a reviewer will, with your tools:
   - each function, type, setting or file a step changes: its callers and dependents (callers, search_code) change in a step too;
   - each name the plan uses exists under that name (search_code, outline), or a step creates it;
   - no step uses what a later step creates;
   - it does the whole request, and nothing beyond it;
   - each new behavior has a test to add or update under "## Validation".
   submit_plan checks the plan, and a reviewer may check it against the code; either sends it back with what to fix. Fix it and submit the complete plan again; when a reviewer held it back, answer each of its findings in `responses`. The plan goes in submit_plan, never in your reply; after submitting, reply in a few sentences.
4. When a plan exists (shown below), the user's message is about it: revise the plan with the new information, keeping what still holds, and submit it complete; don't replace it with a summary of what they said. A question about it is answered without submitting."###;

/// Plan mode's lines that Autopilot's plan stage says otherwise: the user
/// isn't there, so nothing is asked (D5) and the plan isn't theirs to edit
/// before it's built.
const PLAN_MODE_CHOSEN: &str =
    "The user chose Plan mode: investigate and write a plan; don't carry it out.";
const AUTOPILOT_PLAN_STAGE: &str = "This is the plan stage of an Autopilot task: investigate and write a plan; don't carry it out. Once it's reviewed, another run implements it, with nobody there.";
const PLAN_MODE_QUESTIONS: &str = "2. Ask only what the code can't tell you and what really changes the design, behavior or scope: with the ask_user tool, early, all questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything the repository decides: decide it and say so in the plan. Never ask in plain text and stop.";
const AUTOPILOT_QUESTIONS: &str = "2. Nobody answers questions in Autopilot: decide what the code and the request can't tell you, and write each assumption under **Assumptions** in \"## Criteria\", with why. Never ask, with a tool or in plain text.";
/// The plan's first section, after which Autopilot's plan says what done
/// means (S01, D8).
const PLAN_SUMMARY_LINE: &str = "   - a \"# \" title and a one-paragraph summary of the approach;";
const AUTOPILOT_CRITERIA: &str = "   - \"## Criteria\": what done means, as numbered statements (`1.`, `2.`), each one a test or a reading of the result can check, together covering the whole request and nothing beyond it; then **Out of scope**: what the task leaves alone, and **Assumptions**: what you decided that the request doesn't say, with why (either may say \"None.\"). The tests are written from them before the code, and the result is judged by them;";

/// What Plan mode tells the model: `PLAN_MODE_INSTRUCTIONS`, or in
/// Autopilot's plan stage the same with its own lines.
pub fn plan_mode_instructions(autopilot: bool) -> String {
    if !autopilot {
        return PLAN_MODE_INSTRUCTIONS.to_string();
    }
    PLAN_MODE_INSTRUCTIONS
        .replace(PLAN_MODE_CHOSEN, AUTOPILOT_PLAN_STAGE)
        .replace(PLAN_MODE_QUESTIONS, AUTOPILOT_QUESTIONS)
        .replace(
            PLAN_SUMMARY_LINE,
            &format!("{PLAN_SUMMARY_LINE}\n{AUTOPILOT_CRITERIA}"),
        )
}

/// The plan's form as Plan mode describes it (its sections and each
/// step's lines), in Autopilot with its criteria: what Thorough's planners
/// are asked to write (T01), so their plans pass the same checks.
pub fn plan_form() -> String {
    let all = plan_mode_instructions(true);
    let start = all.find(PLAN_SUMMARY_LINE).unwrap_or(0);
    let end = all[start..]
        .find("   Before you submit")
        .map_or(all.len(), |e| start + e);
    all[start..end]
        .lines()
        .map(|l| l.strip_prefix("   ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/*
 * ---------------------------------------------------------------------------
 * The criteria (S01, D8)
 * ---------------------------------------------------------------------------
 */

/// An Autopilot plan's "## Criteria": what done means, as numbered,
/// testable statements, then what the task leaves alone and what it
/// assumed. The test writer writes from them and the acceptance check
/// judges by them.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Criteria {
    /// The numbered statements, in order, each on one line.
    pub items: Vec<String>,
    /// What the task leaves alone; empty when it says "None".
    pub out_of_scope: Vec<String>,
    /// What the planner decided that the request doesn't say.
    pub assumptions: Vec<String>,
    /// Whether the section has its **Out of scope** part (even "None.").
    pub has_out_of_scope: bool,
    /// Whether it has its **Assumptions** part (even "None.").
    pub has_assumptions: bool,
}

/// A part's heading inside the criteria: `**Out of scope**`, `**Out of
/// scope:** the CLI`, `### Assumptions`, `- Assumptions: none`.
static CRITERIA_PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:[-*+]\s+)?(?:#{3,6}\s*)?(?:\*\*|__)?\s*(out[ -]of[ -]scope|non[- ]goals|assumptions?)\s*:?\s*(?:\*\*|__)?\s*:?\s*(.*)$",
    )
    .unwrap()
});
/// A numbered line: `1. Text`, `2) Text`.
static NUMBERED_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\d+[.)]\s+(\S.*)$").unwrap());
/// A bullet: `- Text`, `* Text`.
static BULLET_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(\S.*)$").unwrap());

/// Whether a part's line says it has nothing: "None.", "n/a", "—".
fn says_none(text: &str) -> bool {
    let t = text
        .trim()
        .trim_matches(|c: char| c == '*' || c == '_' || c == '.' || c == '-' || c == '—');
    matches!(
        t.to_lowercase().as_str(),
        "" | "none" | "n/a" | "na" | "nothing"
    )
}

/// The plan's criteria section: the first `## ` heading with "criteria" in
/// it (`## Criteria`, `## Acceptance criteria`), without its heading.
pub fn criteria_section(markdown: &str) -> Option<String> {
    let markdown = markdown.replace("\r\n", "\n");
    for (at, line, fenced) in lines_with_fences(&markdown) {
        if fenced || !NEXT_HEADING.is_match(line) {
            continue;
        }
        if line.to_lowercase().contains("criteri") {
            return Some(section_after(&markdown, at + line.len()).trim().to_string());
        }
    }
    None
}

/// S01: an Autopilot plan's criteria, read from its "## Criteria"; None
/// when it has none. Strict: only numbered lines are criteria (a line
/// under one continues it), and the two parts are found by their names.
pub fn criteria(markdown: &str) -> Option<Criteria> {
    let section = criteria_section(markdown)?;
    #[derive(PartialEq)]
    enum Part {
        Items,
        OutOfScope,
        Assumptions,
    }
    let mut part = Part::Items;
    let mut out = Criteria::default();
    // The list a line continues: the latest item of the part it's in.
    let mut open = false;
    for (_, line, fenced) in lines_with_fences(&section) {
        let line = line.trim_end_matches('\n');
        if fenced {
            continue;
        }
        if line.trim().is_empty() {
            open = false;
            continue;
        }
        if let Some(c) = CRITERIA_PART.captures(line) {
            let name = c[1].to_lowercase();
            part = if name.starts_with("assum") {
                out.has_assumptions = true;
                Part::Assumptions
            } else {
                out.has_out_of_scope = true;
                Part::OutOfScope
            };
            let rest = c[2].trim();
            open = false;
            if !says_none(rest) {
                let list = match part {
                    Part::Assumptions => &mut out.assumptions,
                    _ => &mut out.out_of_scope,
                };
                list.push(rest.to_string());
                open = true;
            }
            continue;
        }
        let list = match part {
            Part::Items => &mut out.items,
            Part::OutOfScope => &mut out.out_of_scope,
            Part::Assumptions => &mut out.assumptions,
        };
        let item = NUMBERED_ITEM
            .captures(line)
            .or_else(|| {
                (part != Part::Items)
                    .then(|| BULLET_ITEM.captures(line))
                    .flatten()
            })
            .map(|c| c[1].trim().to_string());
        match item {
            Some(text) if part != Part::Items && says_none(&text) => open = false,
            Some(text) => {
                list.push(text);
                open = true;
            }
            // A line under an item continues it.
            None if open => {
                if let Some(last) = list.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
            }
            // A part's text that isn't a list: one item, unless it says none.
            None if part != Part::Items && !says_none(line) => {
                list.push(line.trim().to_string());
                open = true;
            }
            None => {}
        }
    }
    Some(out)
}

/// T03: `markdown` with `body` as its "## Criteria": the criteria section
/// replaced where it has one, else put before its first `## ` section
/// (after the title and the summary), else at its end.
pub fn with_criteria(markdown: &str, body: &str) -> String {
    let markdown = markdown.replace("\r\n", "\n");
    let section = format!("## Criteria\n\n{}\n\n", body.trim());
    let headings: Vec<(usize, &str)> = lines_with_fences(&markdown)
        .into_iter()
        .filter(|(_, line, fenced)| !fenced && NEXT_HEADING.is_match(line))
        .map(|(at, line, _)| (at, line))
        .collect();
    let at = headings
        .iter()
        .position(|(_, line)| line.to_lowercase().contains("criteri"));
    let out = match (at, headings.first()) {
        (Some(i), _) => {
            let end = headings.get(i + 1).map_or(markdown.len(), |(at, _)| *at);
            format!(
                "{}{section}{}",
                &markdown[..headings[i].0],
                &markdown[end..]
            )
        }
        (None, Some((first, _))) => {
            format!("{}{section}{}", &markdown[..*first], &markdown[*first..])
        }
        (None, None) => format!("{}\n\n{section}", markdown.trim_end()),
    };
    format!("{}\n", out.trim_end())
}

/// S01: what an Autopilot plan's criteria lack (empty: they're there).
pub fn criteria_problems(markdown: &str) -> Vec<String> {
    let Some(c) = criteria(markdown) else {
        return vec![
            "Add a \"## Criteria\" section: what done means, as numbered statements (1., 2., ...), each one a test or a \
             reading of the result can check; then **Out of scope** and **Assumptions** (either may say \"None.\")."
                .into(),
        ];
    };
    let mut problems = Vec::new();
    if c.items.is_empty() {
        problems.push(
            "List the criteria under \"## Criteria\" as a numbered list (1., 2., ...): each a statement of what done \
             means that a test or a reading of the result can check."
                .into(),
        );
    }
    if !c.has_out_of_scope {
        problems.push(
            "Add **Out of scope** under \"## Criteria\": what the task leaves alone, or \"None.\"."
                .into(),
        );
    }
    if !c.has_assumptions {
        problems.push(
            "Add **Assumptions** under \"## Criteria\": what you decided that the request doesn't say, with why, or \
             \"None.\"."
                .into(),
        );
    }
    problems
}

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
/// A step written as a heading or a bold line instead of a list item:
/// `### Step 1: Title`, `### 1. Title`, `**Step 1: Title**` (some models
/// write steps so).
static HEADING_STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*(?:#{2,6}\s+|\*\*)\s*(?:step\s*)?(\d+)[.):]?\s*\S").unwrap()
});
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#\s+\S").unwrap());
static CODE_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());
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

/// The numbered items of a steps section, each with its sub-lines: the
/// least indented numbered lines outside code blocks (a numbered list
/// nested in a step is part of that step).
fn split_steps(section: &str) -> Vec<&str> {
    let lines = lines_with_fences(section);
    let numbered: Vec<(usize, usize)> = lines
        .iter()
        .filter(|(_, line, fenced)| !fenced && NUMBERED.is_match(line) && !line.trim().is_empty())
        .map(|(at, line, _)| (*at, line.len() - line.trim_start().len()))
        .collect();
    let indent = numbered.iter().map(|(_, i)| *i).min().unwrap_or(0);
    let mut starts: Vec<usize> = numbered
        .into_iter()
        .filter(|(_, i)| *i == indent)
        .map(|(at, _)| at)
        .collect();
    // No list: steps written as headings or bold lines.
    if starts.is_empty() {
        starts = lines
            .iter()
            .filter(|(_, line, fenced)| !fenced && HEADING_STEP.is_match(line))
            .map(|(at, _, _)| *at)
            .collect();
    }
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
pub(super) fn list_steps(names: &[&str]) -> String {
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
                let checked: Vec<StepFiles> = steps.iter().map(|s| step_files(s)).collect();
                let no_file: Vec<&str> = (0..steps.len())
                    .filter(|&i| checked[i].files.is_empty() && checked[i].bad.is_empty())
                    .map(|i| names[i].as_str())
                    .collect();
                if !no_file.is_empty() {
                    problems.push(format!(
                        "Step{} {} name{} no file: give each step a \"Files:\" line with the full path from the project root \
                         of every file it changes, in backticks and comma-separated, new files marked \"(new)\" \
                         (e.g. Files: `src/db/users.rs`, `src/api/users.rs` (new)). Only that line names a step's files.",
                        if no_file.len() > 1 { "s" } else { "" },
                        list_steps(&no_file),
                        if no_file.len() > 1 { "" } else { "s" },
                    ));
                }
                for (i, step) in checked.iter().enumerate() {
                    for (token, why) in &step.bad {
                        let name = &names[i];
                        problems.push(match why {
                            BadPath::Absolute => format!(
                                "Step {name}'s Files line has `{token}`, an absolute path: write it from the project root."
                            ),
                            BadPath::Outside(Some(inside)) => format!(
                                "Step {name}'s Files line has `{token}`: paths start at the project root, never with `..` \
                                 (the plan is saved in .rovik/plans/, but its paths aren't from that folder). If the file is \
                                 in the project, write it as `{inside}`."
                            ),
                            BadPath::Outside(None) => format!(
                                "Step {name}'s Files line has `{token}`, which is outside the project: use a path inside it, \
                                 from the project root."
                            ),
                            BadPath::NotAPath => format!(
                                "Step {name}'s Files line has `{token}`, which isn't a path: the line holds only the step's \
                                 file paths, in backticks and comma-separated; say the rest in the step's text."
                            ),
                        });
                    }
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
    crate::file_guard::write_app_file(Path::new(&path), format!("{titled}\n").as_bytes())?;
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

/// The text that implements a plan (Agent mode): the plan in full, kept in
/// view for the whole run (the current request is never dropped from the
/// context), then how to carry it out. `file`: where it's saved, as the
/// chat shows it.
pub fn implementation_request(plan: &str, file: Option<&str>) -> String {
    [
        match file {
            Some(file) => format!(
                "Implement this plan. It's saved in {file}; the user may have edited it, and this is its current version."
            ),
            None => "Implement this plan.".to_string(),
        },
        String::new(),
        "<plan>".into(),
        plan.trim().to_string(),
        "</plan>".into(),
        String::new(),
        concat!(
            "First create your checklist with the todo tool from the plan's steps, with the first step in_progress, then work through them in order, ",
            "one step at a time, each with its own todo call: call todo alone to mark a step in_progress before starting it, ",
            "do the work, then call todo alone again to mark it done before the next step. Never save up finished steps for one update at the end. ",
            "For each step, read the files it names before changing them, and make ",
            "the changes it describes to the functions, types and settings it names. Follow the plan and its decisions. ",
            "If a step turns out wrong or unnecessary (a file or function isn\u{2019}t where the plan says, or the change ",
            "doesn\u{2019}t fit the code), mark it skipped or changed with the reason instead of silently doing something else. ",
            "When the steps are done, run the checks in the plan\u{2019}s Validation section that you can run (build, ",
            "typecheck, tests, lint), add or update the tests it asks for, and fix what fails. In your final message, ",
            "say which checks passed, which you couldn\u{2019}t run and why, what is left to check by hand, and why you changed ",
            "any files the plan doesn\u{2019}t mention.",
        )
        .into(),
    ]
    .join("\n")
}

/// "Implement this plan" as the app sends it: `meta.implementsPlan` with
/// the plan's path (`meta.planPath`). The text the model gets, the plan as
/// its file is now, goes in `meta.sentContent` before the message is
/// stored, so every turn and Retry sends it the same. A message that has
/// its text already, or isn't one, is left as it is. Err: the file can't
/// be read.
pub fn expand_implementation(
    message: &mut super::UserMessage,
    root: Option<&str>,
) -> Result<(), String> {
    let Some(meta) = message.meta.as_mut().and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let implements = meta.get("implementsPlan").and_then(Value::as_bool) == Some(true);
    let path = meta
        .get("planPath")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(path), true, false) = (path, implements, meta.contains_key("sentContent")) else {
        return Ok(());
    };
    let file = match root {
        Some(root) if !Path::new(&path).is_absolute() => join(root, &path),
        _ => path,
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("The plan in {file} can't be read: {e}"))?;
    let shown = relative_plan_path(&file, root);
    meta.insert(
        "sentContent".into(),
        Value::String(implementation_request(&text, Some(&shown))),
    );
    Ok(())
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

    /// The step's text without its number, in simple words: a step that
    /// only moved (a step inserted before it) reads the same (plan_review.rs).
    pub fn content_key(&self) -> String {
        let (first, rest) = self.body.split_once('\n').unwrap_or((&self.body, ""));
        let first = STEP_NUMBER
            .captures(first)
            .map_or_else(|| first.to_string(), |c| c[2].to_string());
        simple_words(&format!("{first}\n{rest}"))
    }
}

/// A step's number and the rest of its first line: `1. Title`,
/// `### Step 1: Title`, `**Step 1: Title**`.
static STEP_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:#{1,6}\s+)?\**(?:step\s*)?(\d+)[.):]?\**\s*(?:[—–-]\s+)?(.*)$").unwrap()
});
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

/// A step's `Files:` line: the one place a step names its files. The
/// label may be bulleted or bold (`- **Files:** ...`), singular (`File:`)
/// or `Paths:`, and the items may go on list lines indented under it
/// instead of after it.
static FILES_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[\s>*+-]*\**(?:files?|paths?|file paths?)\**\s*:\**\s*(.*?)\s*$").unwrap()
});

/// Why an item of a `Files:` line isn't a path of the project.
#[derive(Debug, Clone, PartialEq)]
pub enum BadPath {
    /// `/etc/x`, `C:\x`: not relative to the project.
    Absolute,
    /// Has `..` parts, with the path it resolves to inside the project
    /// when it does: `src/../x.rs` is `x.rs`, and `../../src/x.rs`,
    /// written from the plan's folder, is `src/x.rs`.
    Outside(Option<String>),
    /// Has spaces, a scheme, a glob or other characters no path has, or
    /// is the project root.
    NotAPath,
}

/// The project path an item of a `Files:` line names, in one spelling:
/// `\` folded to `/`, `./` dropped, a folder keeping its closing `/`, a
/// `:line` suffix dropped.
pub fn file_path(token: &str) -> Result<String, BadPath> {
    let t = token.trim().replace('\\', "/");
    let t = t.trim_start_matches("./");
    let b = t.as_bytes();
    // A drive (`C:/x`, `D:a.rs`) or a UNC path isn't the project's.
    if t.starts_with('/') || (b.len() > 1 && b[1] == b':') {
        return Err(BadPath::Absolute);
    }
    if t.contains("://") {
        return Err(BadPath::NotAPath);
    }
    let t = t.split(':').next().unwrap_or(t);
    if t.is_empty()
        || t.contains(char::is_whitespace)
        || t.contains([
            '<', '>', '{', '}', '*', '?', '$', '=', ',', ';', '"', '\'', '|', '`',
        ])
    {
        return Err(BadPath::NotAPath);
    }
    if t.split('/').any(|part| part == "..") {
        // Leading `../` parts are the plan's folder's; the rest may be
        // the path from the root.
        let rest = t.trim_start_matches("../");
        return Err(match without_dots(rest) {
            Some(inside) if inside.is_empty() => BadPath::NotAPath,
            inside => BadPath::Outside(inside),
        });
    }
    let p = super::paths::norm_rel(t)
        .filter(|p| !p.is_empty())
        .ok_or(BadPath::NotAPath)?;
    Ok(if t.ends_with('/') { format!("{p}/") } else { p })
}

fn step_title(first_line: &str) -> String {
    let t = first_line.trim().trim_matches('*').trim();
    let t = t.split(" — ").next().unwrap_or(t);
    t.trim_end_matches(':').trim_matches('*').trim().to_string()
}

/// The items of a `Files:` line: split on commas and semicolons outside
/// backticks.
fn split_items(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut start, mut ticked) = (0, false);
    for (i, c) in text.char_indices() {
        match c {
            '`' => ticked = !ticked,
            ',' | ';' if !ticked => {
                items.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&text[start..]);
    items
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// What a `Files:` line names: its files, each once, and the items that
/// aren't the project's paths (with why). An item is a backticked path
/// with its mark after it (`` `src/a.rs` (new) ``); several backticked
/// paths in one item each take the mark after them; without backticks,
/// the item up to its mark is the path.
fn parse_files(text: &str) -> (Vec<PlanFile>, Vec<(String, BadPath)>) {
    let mut files: Vec<PlanFile> = Vec::new();
    let mut bad: Vec<(String, BadPath)> = Vec::new();
    for item in split_items(text) {
        // (path, the text after it: its mark).
        let mut pieces: Vec<(&str, &str)> = Vec::new();
        let ticks: Vec<usize> = item.match_indices('`').map(|(i, _)| i).collect();
        if ticks.len() >= 2 {
            for (n, pair) in ticks.chunks(2).enumerate() {
                let [open, close] = pair else { break };
                let after_end = ticks.get(2 * (n + 1)).copied().unwrap_or(item.len());
                pieces.push((&item[open + 1..*close], &item[close + 1..after_end]));
            }
        } else {
            // The path is what comes before the mark; words there make
            // it no path.
            let plain = item.trim_matches(|c| c == '*' || c == '_').trim();
            let (token, rest) = plain.find('(').map_or((plain, ""), |at| plain.split_at(at));
            let token = token.trim();
            let token = token
                .strip_suffix('.')
                .filter(|t| !t.ends_with('.'))
                .unwrap_or(token);
            pieces.push((token, rest));
        }
        for (token, after) in pieces {
            let token = token.trim();
            if token.is_empty()
                || matches!(
                    token.to_ascii_lowercase().as_str(),
                    "none" | "-" | "n/a" | "no files" | "no file"
                )
            {
                continue;
            }
            let new = NEW_MARK.is_match(after);
            let read_only = READ_MARK.is_match(after);
            match file_path(token) {
                Ok(path) => match files.iter_mut().find(|f| f.path == path) {
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
                },
                Err(why) => {
                    let token = token.replace('\\', "/");
                    if !bad.iter().any(|(t, _)| *t == token) {
                        bad.push((token, why));
                    }
                }
            }
        }
    }
    (files, bad)
}

/// A step's files, as its `Files:` line names them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StepFiles {
    /// The step has a `Files:` line.
    pub line: bool,
    pub files: Vec<PlanFile>,
    /// Items of the line that aren't the project's paths.
    pub bad: Vec<(String, BadPath)>,
}

/// The files a step names: those on its `Files:` line (outside code
/// blocks), with the list lines indented under it. Paths anywhere else in
/// the step are prose.
pub fn step_files(raw: &str) -> StepFiles {
    let lines = lines_with_fences(raw);
    let indent = |line: &str| line.len() - line.trim_start().len();
    let Some(at) = lines
        .iter()
        .position(|(_, line, fenced)| !fenced && FILES_LINE.is_match(line))
    else {
        return StepFiles::default();
    };
    let (_, line, _) = lines[at];
    let mut text = FILES_LINE
        .captures(line)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    for (_, next, fenced) in &lines[at + 1..] {
        let t = next.trim();
        if *fenced || t.is_empty() || indent(next) <= indent(line) {
            break;
        }
        // A list under the label, or a wrapped item.
        if !t.starts_with(['-', '*', '+', '`']) {
            break;
        }
        let t = t.trim_start_matches(['-', '*', '+']).trim();
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(t);
    }
    let (files, bad) = parse_files(&text);
    StepFiles {
        line: true,
        files,
        bad,
    }
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
            let files = step_files(raw).files;
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

/*
 * ---------------------------------------------------------------------------
 * The plan's validation commands (V02)
 * ---------------------------------------------------------------------------
 */

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

/// A findings heading an agent may put in its plan (`## Findings`,
/// `## Review findings`), or the IDE's `## Open review findings` from an
/// earlier save: findings are the reviewer's, never the plan's.
static FINDINGS_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^##\s+(?:open\s+)?(?:review\s+)?findings\s*:?\s*$").unwrap());

/// The plan without its findings sections (an agent revising a reviewed
/// plan sometimes ends it with "## Findings" and "None.", or keeps the
/// open findings the IDE appended), and whether any was taken out.
pub fn without_findings(markdown: &str) -> (String, bool) {
    let crlf = markdown.contains("\r\n");
    let text = markdown.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    let mut dropped = false;
    for (_, line, fenced) in lines_with_fences(&text) {
        if !fenced && NEXT_HEADING.is_match(line) {
            skipping = FINDINGS_HEADING.is_match(line.trim_end());
            dropped |= skipping;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    if !dropped {
        return (markdown.to_string(), false);
    }
    let out = out.trim_end().to_string() + "\n";
    (if crlf { out.replace('\n', "\r\n") } else { out }, true)
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
    for step in &steps {
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

/// `plan_problems_in`, and in an Autopilot task's plan its criteria (S01).
pub fn plan_problems_for(markdown: &str, root: Option<&Path>, autopilot: bool) -> Vec<String> {
    let mut problems = plan_problems_in(markdown, root);
    if autopilot {
        problems.extend(criteria_problems(markdown));
    }
    problems
}

/*
 * ---------------------------------------------------------------------------
 * Questions (ask_user)
 * ---------------------------------------------------------------------------
 */

pub use rovik_protocol::plan::{Question, QuestionAnswer, QuestionOption, QuestionsOutcome};

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

pub use rovik_protocol::plan::{CurrentChecklist, TodoItem};

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

    /// A04: Autopilot's plan stage has its own lines for Plan mode's on
    /// asking and on the user's choice; the rest is Plan mode's. The lines
    /// it replaces are Plan mode's still, or it would replace nothing.
    #[test]
    fn autopilots_plan_stage_asks_nothing() {
        for line in [PLAN_MODE_CHOSEN, PLAN_MODE_QUESTIONS] {
            assert!(PLAN_MODE_INSTRUCTIONS.contains(line), "{line}");
        }
        assert_eq!(plan_mode_instructions(false), PLAN_MODE_INSTRUCTIONS);
        let autopilot = plan_mode_instructions(true);
        assert!(
            autopilot.contains(AUTOPILOT_QUESTIONS) && autopilot.contains(AUTOPILOT_PLAN_STAGE)
        );
        assert!(!autopilot.contains("ask_user") && !autopilot.contains("The user chose"));
        assert!(autopilot.contains("submit_plan") && autopilot.contains("## Decisions"));
        // S01: and what done means, before the steps.
        assert!(PLAN_MODE_INSTRUCTIONS.contains(PLAN_SUMMARY_LINE));
        let criteria = autopilot
            .find(AUTOPILOT_CRITERIA)
            .expect("the criteria line");
        assert!(criteria < autopilot.find("\"## Steps\"").unwrap());
        assert!(!PLAN_MODE_INSTRUCTIONS.contains("## Criteria"));
    }

    const CRITERIA_PLAN: &str = "# Add users\n\nUsers can sign up.\n\n## Criteria\n\n\
        1. `POST /users` with a new email creates a user and answers 201.\n\
        2. The same email again answers 409\n   and creates nothing.\n\
        3) `GET /users` lists them, newest first.\n\n\
        **Out of scope**: password resets.\n\n\
        **Assumptions**\n\n\
        - Emails compare case-insensitively: the request doesn't say, and `users.email` is `CITEXT`.\n\
        - Paging stays as the other lists do.\n\n\
        ## Steps\n\n1. **Users**\n   - Files: `src/users.rs`\n   - Add `create_user` to `src/users.rs`.\n";

    /// S01: a plan's criteria, its parts, a criterion over two lines, the
    /// numbered list's two spellings.
    #[test]
    fn a_plans_criteria_are_read() {
        let c = criteria(CRITERIA_PLAN).unwrap();
        assert_eq!(
            c.items,
            [
                "`POST /users` with a new email creates a user and answers 201.",
                "The same email again answers 409 and creates nothing.",
                "`GET /users` lists them, newest first.",
            ]
        );
        assert_eq!(c.out_of_scope, ["password resets."]);
        assert_eq!(c.assumptions.len(), 2);
        assert!(c.assumptions[0].starts_with("Emails compare case-insensitively"));
        assert!(c.has_out_of_scope && c.has_assumptions);
        assert!(criteria_problems(CRITERIA_PLAN).is_empty());
        // `## Acceptance criteria`, `### ` parts, "None." for a part.
        let other = "# T\n\n## Acceptance criteria\n\n1. It works.\n\n### Out of scope\n\nNone.\n\n### Assumptions\n- None\n\n## Steps\n";
        let c = criteria(other).unwrap();
        assert_eq!(c.items, ["It works."]);
        assert!(c.out_of_scope.is_empty() && c.assumptions.is_empty());
        assert!(c.has_out_of_scope && c.has_assumptions);
        assert!(criteria_problems(other).is_empty());
    }

    /// S01: what's missing, said so; garbage reads as no criteria, never as
    /// some.
    #[test]
    fn a_plans_criteria_are_replaced_or_put_in_place() {
        let body = "1. It greets.\n\n**Out of scope**: None.\n\n**Assumptions**: English.";
        // Replaced where the plan has them, the rest as it was.
        let plan = "# Greet\n\nSay hi.\n\n## Criteria\n\n1. Old.\n\n## Steps\n\n1. **Greet**\n   - Files: `a.rs`\n";
        assert_eq!(
            with_criteria(plan, body),
            format!("# Greet\n\nSay hi.\n\n## Criteria\n\n{body}\n\n## Steps\n\n1. **Greet**\n   - Files: `a.rs`\n")
        );
        // Its last section.
        assert_eq!(
            with_criteria("# Greet\n\n## Acceptance criteria\n\n1. Old.\n", body),
            format!("# Greet\n\n## Criteria\n\n{body}\n")
        );
        // None: before the first section, after the title and summary.
        assert_eq!(
            with_criteria("# Greet\n\nSay hi.\n\n## Steps\n\n1. **Greet**\n", body),
            format!("# Greet\n\nSay hi.\n\n## Criteria\n\n{body}\n\n## Steps\n\n1. **Greet**\n")
        );
        assert_eq!(
            with_criteria("# Greet", body),
            format!("# Greet\n\n## Criteria\n\n{body}\n")
        );
        // A heading in a fence is the plan's text, not a section.
        let fenced = "# Greet\n\n```\n## Criteria\n```\n\n## Steps\n\n1. **Greet**\n";
        assert!(with_criteria(fenced, body)
            .starts_with("# Greet\n\n```\n## Criteria\n```\n\n## Criteria\n\n1. It greets."));
        let replaced = with_criteria(plan, body);
        assert_eq!(criteria(&replaced).unwrap().assumptions, ["English."]);
    }

    #[test]
    fn criteria_missing_or_partial_say_what_to_add() {
        assert_eq!(criteria("# T\n\n## Steps\n\n1. x"), None);
        let none = criteria_problems("# T\n\n## Steps\n\n1. x");
        assert_eq!(none.len(), 1);
        assert!(
            none[0].starts_with("Add a \"## Criteria\" section"),
            "{none:?}"
        );
        // Bullets aren't criteria; the parts missing are named.
        let bullets = "# T\n\n## Criteria\n\n- It works.\n- It's fast.\n\n## Steps\n";
        assert!(criteria(bullets).unwrap().items.is_empty());
        let problems = criteria_problems(bullets);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].starts_with("List the criteria"));
        assert!(
            problems[1].contains("**Out of scope**") && problems[2].contains("**Assumptions**")
        );
        // A numbered list in a code block isn't one.
        let fenced = "# T\n\n## Criteria\n\n```\n1. not this\n```\n\n**Out of scope**: none\n**Assumptions**: none\n";
        assert!(criteria(fenced).unwrap().items.is_empty());
        for garbage in [
            "",
            "## Criteria",
            "## Criteria\n\n\u{0}\u{1}",
            "## criteria\n\n***\n1.",
        ] {
            let c = criteria(garbage).unwrap_or_default();
            assert!(c.items.is_empty(), "{garbage:?}: {c:?}");
        }
    }

    /// S01: Autopilot's plans need criteria; anyone else's, as before.
    #[test]
    fn only_autopilots_plans_need_criteria() {
        let plan = "# Make the test pass\n\nThe test passes once ok.txt says ok.\n\n## Steps\n\n\
            1. **Write ok**\n   - Files: `ok.txt`\n   - Replace the text of `ok.txt` with `ok`, which `check.js` checks.\n\
            2. **Check**\n   - Files: `check.js` (read)\n   - Read `check.js` to confirm it checks `ok.txt`; it keeps its code.\n\n\
            ## Validation\n- `npm test`\n";
        assert!(plan_problems_for(plan, None, false).is_empty());
        let problems = plan_problems_for(plan, None, true);
        assert_eq!(problems.len(), 1, "{problems:?}");
        let with = plan.replace(
            "## Steps",
            "## Criteria\n\n1. `npm test` passes.\n\n**Out of scope**: None.\n**Assumptions**: None.\n\n## Steps",
        );
        assert!(plan_problems_for(&with, None, true).is_empty());
    }

    /// A04: "Implement this plan" sent with the plan's path gets the text
    /// the model is sent from the file as it is; other messages, and one
    /// with its text already, are left as they are.
    #[test]
    fn an_implementation_request_is_written_from_the_plans_file() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.to_string_lossy().into_owned();
        let file = dir.0.join(".rovik/plans/p.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# P\n\nThe plan.\n").unwrap();
        let message = |meta: Value| super::super::UserMessage {
            id: "u".into(),
            content: "Implement the plan in `.rovik/plans/p.md`.".into(),
            meta: Some(meta),
            attachments: Vec::new(),
        };
        let expected = implementation_request("# P\n\nThe plan.\n", Some(".rovik/plans/p.md"));
        for path in [
            file.to_string_lossy().into_owned(),
            ".rovik/plans/p.md".into(),
        ] {
            let mut m =
                message(json!({ "mode": "agent", "implementsPlan": true, "planPath": path }));
            expand_implementation(&mut m, Some(&root)).unwrap();
            assert_eq!(m.meta.as_ref().unwrap()["sentContent"], expected, "{path}");
            assert_eq!(m.meta.as_ref().unwrap()["implementsPlan"], true);
        }
        for meta in [
            json!({ "mode": "agent" }),
            json!({ "implementsPlan": true }),
            json!({ "implementsPlan": true, "planPath": "x.md", "sentContent": "as sent" }),
        ] {
            let mut m = message(meta.clone());
            expand_implementation(&mut m, Some(&root)).unwrap();
            assert_eq!(m.meta, Some(meta));
        }
        let mut gone = message(json!({ "implementsPlan": true, "planPath": "missing.md" }));
        let err = expand_implementation(&mut gone, Some(&root)).unwrap_err();
        assert!(err.contains("can't be read"), "{err}");
    }
    use serde_json::json;

    const GOOD_PLAN: &str = "# Add users\n\nSummary.\n\n## Steps\n\
        1. Users table\n   - Files: `migrations/002_users.sql` (new)\n   - Create the `users` table with `id`, `email`.\n\
        2. API\n   - Files: `src/api/users.rs`, `src/api/mod.rs`\n   - Add `fn list_users(db: &Db) -> Vec<User>` and register it in `src/api/mod.rs`.\n\n\
        ## Decisions\n- none\n\n## Validation\n- `cargo test`\n- open /users by hand";

    /// A plan as Plan mode writes them: bold titles, a Files line with
    /// marks and a `path:line`, sub-bullets with code spans and paths that
    /// aren't the step's files, a folder, a group.
    const REAL_PLAN: &str = "# Add a users page\n\nServe users from the API and list them.\n\n\
        ## Steps\n\n\
        1. **Users table**\n   - Files: `migrations/002_users.sql` (new), `migrations/README.md`\n   - Create `users` with `id`, `email`; register it in the README.\n\n\
        2. **List users in the API** — the backend part\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs:12`\n   - Add `fn list_users(db: &Db) -> Result<Vec<User>>`; read `state.config` for the page size; register the route in `src/api/mod.rs`.\n   - Group: backend\n\n\
        3. **Page**\n   - Files: `src/pages/Users.tsx` (new), `src/pages/`\n   - Render the list; call `api.listUsers()` from `src/lib/api.ts`.\n   - Group: backend\n\n\
        ## Decisions\n- Paginate at 50 (the existing default).\n- Keep `users` in SQLite.\n\n\
        ## Validation\n- `cargo test`\n- open /users by hand\n";

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

    /// A step's files are its Files line: paths in its text (prose,
    /// nested lists, code blocks) aren't its files, and a `## ` line in a
    /// code block doesn't end the steps.
    #[test]
    fn nested_lists_belong_to_their_step_and_code_blocks_stay() {
        let plan = "# API\n\nTwo parts.\n\n## Steps\n\n\
            1. **API**\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs`\n   1. Add the users module.\n   2. Register it in `src/lib.rs`.\n\
            2. **Pages**\n   - Files: `src/pages.rs`\n   - Render it, with exactly this signature:\n\n     ```rust\n     \
            ## not a heading\n     - Files: `src/fenced.rs`\n     pub fn page(id: u32) -> Html { todo!(`src/other.rs`) }\n     ```\n\
            3. **Docs**\n   - Files: `README.md`\n   - Say so in the intro.\n\n## Validation\n- `cargo test`\n";
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
        assert_eq!(
            files(0),
            ["src/api/users.rs", "src/api/mod.rs"],
            "`src/lib.rs` in the text isn't a file"
        );
        // The code stays in the body; its Files line and paths aren't the step's.
        assert!(s[1].body.contains("pub fn page(id: u32)"), "{}", s[1].body);
        assert_eq!(files(1), ["src/pages.rs"]);
        assert_eq!(files(2), ["README.md"]);
        assert_eq!(plan_problems(plan), Vec::<String>::new());
    }

    /// The Files line in the forms a model writes it: a bulleted or bold
    /// label, items after it or listed under it, with or without
    /// backticks, commas or semicolons, a mark after each path (bold too),
    /// two paths in one item, a `path:line`, a repeated path, "none".
    #[test]
    fn files_line_forms() {
        let paths = |step: &str| -> Vec<String> {
            step_files(step)
                .files
                .into_iter()
                .map(|f| {
                    format!(
                        "{}{}{}",
                        f.path,
                        if f.new { " (new)" } else { "" },
                        if f.read_only { " (read)" } else { "" }
                    )
                })
                .collect()
        };
        for step in [
            "1. **A**\n   - Files: `src/a.rs` (new), `src/types.rs` (read)\n   - Add a.",
            "1. **A**\n   - **Files:** `src/a.rs` **(new)**; `src/types.rs` _(read-only)_\n   - Add a.",
            "1. **A**\n   - **Files**: `src/a.rs` (new file) and `src/types.rs` (context)\n   - Add a.",
            "1. **A**\n   * Files: src/a.rs (new), src/types.rs (read).\n   * Add a.",
            "1. **A**\n   - Files:\n     - `src/a.rs` (new)\n     - `src/types.rs` (read)\n   - Add a.",
            "1. **A**\n   - Files: `src/a.rs:10` (new), `src\\types.rs` (read), `./src/a.rs`\n   - Add a.",
        ] {
            assert_eq!(
                paths(step),
                ["src/a.rs (new)", "src/types.rs (read)"],
                "{step}"
            );
        }
        // A list under the label ends at the step's next bullet; only the
        // first Files line counts; changed once, a file isn't read-only.
        let step = "1. **A**\n   - Files:\n     - `src/a.rs`\n     - `src/b/`\n   - (new) `src/c.rs`: in the text, not a file of the step.\n   - Files: `src/x.rs`";
        assert_eq!(paths(step), ["src/a.rs", "src/b/"]);
        assert_eq!(
            paths("1. **A**\n   - Files: `src/t.rs` (read), `src/t.rs`"),
            ["src/t.rs"]
        );
        // "none" names nothing, nor does a Files line in a code block.
        assert!(
            step_files("1. **A**\n   - Files: none\n   - Run it by hand.")
                .files
                .is_empty()
        );
        let fenced =
            step_files("1. **A**\n   - Run:\n\n     ```\n     Files: `src/a.rs`\n     ```\n");
        assert!(!fenced.line && fenced.files.is_empty(), "{fenced:?}");
        // Folders keep their `/`; accented and CJK names are paths.
        assert_eq!(
            paths("1. **A**\n   - Files: `src/a/b/c/`, `src/données/é.rs`, `src/深/a.rs`"),
            ["src/a/b/c/", "src/données/é.rs", "src/深/a.rs"]
        );
    }

    /// Items of a Files line that aren't paths are reported with what to
    /// write instead and aren't the step's files; a step whose line has
    /// only such items isn't also told it names no file.
    #[test]
    fn files_line_problems() {
        let plan = |files: &str| {
            format!(
                "# P\n\nx\n\n## Steps\n\n1. **One**\n   - Files: {files}\n   - Add the loader to it.\n\
                 2. **Two**\n   - Files: `src/b.rs`\n   - Add the saver to it.\n\n## Validation\n- `cargo test`\n"
            )
        };
        // Written from the plan's folder: the path from the root is given.
        let p = plan_problems(&plan("`../../src/a.rs`, `../src/a.rs`"));
        assert_eq!(p.len(), 2, "{p:?}");
        for m in &p {
            assert!(
                m.starts_with("Step 1's Files line has `../")
                    && m.contains("never with `..`")
                    && m.ends_with("write it as `src/a.rs`."),
                "{m}"
            );
        }
        let p = plan_problems(&plan("`..\\..\\src\\a.rs`"));
        assert!(
            p[0].starts_with("Step 1's Files line has `../../src/a.rs`"),
            "{p:?}"
        );
        let p = plan_problems(&plan("`src/../x.rs`, `src/api/../lib/`"));
        assert!(
            p[0].ends_with("write it as `x.rs`.") && p[1].ends_with("write it as `src/lib/`."),
            "{p:?}"
        );
        assert_eq!(
            plan_problems(&plan("`src/../../k.rs`")),
            ["Step 1's Files line has `src/../../k.rs`, which is outside the project: use a path inside it, from the project root."]
        );
        for abs in [
            "/etc/hosts",
            "C:\\proj\\src\\a.rs",
            "c:/proj/a.rs",
            "D:a.rs",
            "\\\\server\\share\\a.rs",
        ] {
            let p = plan_problems(&plan(&format!("`{abs}`")));
            assert_eq!(p.len(), 1, "{abs}: {p:?}");
            assert!(p[0].contains("an absolute path"), "{abs}: {p:?}");
        }
        for junk in [
            "`src/my file.rs`",
            "`https://x.io/a.js`",
            "`*.ts`",
            "`fn a(x: u8)`",
            "`./`",
            "`src/..`",
            "the loader module",
        ] {
            let p = plan_problems(&plan(junk));
            assert_eq!(p.len(), 1, "{junk}: {p:?}");
            assert!(
                p[0].starts_with("Step 1's Files line has `")
                    && p[0].contains("which isn't a path"),
                "{junk}: {p:?}"
            );
        }
        // Each reported once; the step's good paths are still its files.
        let mixed = plan("`src/a.rs`, `../src/a.rs`, `../src/a.rs`");
        assert_eq!(plan_problems(&mixed).len(), 1);
        assert_eq!(steps(&mixed)[0].files[0].path, "src/a.rs");
    }

    #[test]
    fn what_counts_as_a_file() {
        for (token, path) in [
            ("src/a.rs", "src/a.rs"),
            ("Cargo.toml", "Cargo.toml"),
            ("./src/b.ts", "src/b.ts"),
            ("src/components/", "src/components/"),
            ("Dockerfile", "Dockerfile"),
            (".env", ".env"),
            ("src/a.rs:10-20", "src/a.rs"),
            ("docs/design", "docs/design"),
            ("app/(auth)/login/page.tsx", "app/(auth)/login/page.tsx"),
            ("src\\a\\b\\c\\", "src/a/b/c/"),
            ("./src//a/b/c/", "src/a/b/c/"),
            (".\\src\\a.rs", "src/a.rs"),
            // On a Files line an item is a path, whatever it looks like.
            (".btn-primary", ".btn-primary"),
        ] {
            assert_eq!(file_path(token).as_deref(), Ok(path), "{token}");
        }
        for (token, why) in [
            ("fn a(x: u8)", BadPath::NotAPath),
            ("https://x.io/a.js", BadPath::NotAPath),
            ("*.ts", BadPath::NotAPath),
            ("src/my file.rs", BadPath::NotAPath),
            ("./", BadPath::NotAPath),
            (".", BadPath::NotAPath),
            ("src/..", BadPath::NotAPath),
            ("", BadPath::NotAPath),
            ("/etc/hosts", BadPath::Absolute),
            ("C:/x.rs", BadPath::Absolute),
            ("D:a.rs", BadPath::Absolute),
            ("\\\\server\\share\\a.rs", BadPath::Absolute),
            ("../x.rs", BadPath::Outside(Some("x.rs".into()))),
            ("../../src/", BadPath::Outside(Some("src/".into()))),
            ("src/../x.rs", BadPath::Outside(Some("x.rs".into()))),
            ("src/../../k.rs", BadPath::Outside(None)),
            ("..", BadPath::Outside(None)),
        ] {
            assert_eq!(file_path(token), Err(why), "{token}");
        }
        assert_eq!(without_dots("a/b/../../c"), Some("c".into()));
        assert_eq!(without_dots("a/../.."), None);
        assert_eq!(without_dots("./a/./b/"), Some("a/b/".into()));
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
            "3. Bump\n   - Files: `migrations/002_users.sql`\n   - Bump `tokio` to 1.40.\n\
             4. Revert\n   - Files: `src/api/mod.rs`\n   - Revert the `--release` flag.\n\n## Decisions",
        );
        assert_eq!(plan_problems(&plan), Vec::<String>::new());
    }

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
        // A path:line is its file; the text's paths and spans aren't files.
        assert_eq!(
            files(1),
            [("src/api/users.rs", true), ("src/api/mod.rs", false)]
        );
        assert_eq!(
            files(2),
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

    /// A "## Findings" section in a submitted plan (with "None." or with
    /// findings), or the IDE's "## Open review findings" from an earlier
    /// save, is left out before the plan is checked, reviewed and saved;
    /// one inside a code block is code.
    #[test]
    fn findings_sections_are_left_out_of_a_plan() {
        let plan = format!("{GOOD_PLAN}\n\n## Findings\n\nNone.\n");
        let (stripped, dropped) = without_findings(&plan);
        assert!(dropped);
        assert_eq!(stripped, format!("{GOOD_PLAN}\n"));
        assert!(plan_problems(&stripped).is_empty());
        let with_content = format!(
            "{GOOD_PLAN}\n\n## Review findings\n\n1. [high] Step 2 — x\n - Scenario: s\n\n## Risks\n\n- none\n\n## Open review findings\n\n- **[high] Step 1 — y**: z\n"
        );
        let (stripped, dropped) = without_findings(&with_content);
        assert!(dropped);
        assert_eq!(stripped, format!("{GOOD_PLAN}\n\n## Risks\n\n- none\n"));
        // CRLF stays CRLF; nothing to drop leaves the plan as it is.
        let crlf = plan.replace('\n', "\r\n");
        let (stripped, _) = without_findings(&crlf);
        assert_eq!(stripped, format!("{GOOD_PLAN}\n").replace('\n', "\r\n"));
        assert_eq!(without_findings(GOOD_PLAN), (GOOD_PLAN.to_string(), false));
        let fenced = format!("{GOOD_PLAN}\n\n```\n## Findings\nNone.\n```\n");
        assert_eq!(without_findings(&fenced), (fenced.clone(), false));
    }

    /// Steps written as `### Step N:` or `### N.` headings, or as bold
    /// `**Step N:**` lines, with the Files line as `**Files:**` under the
    /// heading, are the steps; a list of steps keeps winning over headings
    /// inside them.
    #[test]
    fn steps_as_headings() {
        let tree = Tree::new(&["src/a.rs", "src/b.rs"]);
        for (one, two) in [
            ("### Step 1: One", "### Step 2: Two"),
            ("### 1. One", "### 2. Two"),
            ("#### Step 1 — One", "#### Step 2 — Two"),
            ("**Step 1: One**", "**Step 2: Two**"),
        ] {
            let plan = format!(
                "# P\n\nx\n\n## Steps\n\n{one}\n**Files:** `src/a.rs`\n\n- Add the loader to it.\n\n\
                 {two}\n- Files: `src/b.rs` (read), `src/c.rs` (new)\n- Add the saver, reading the loader.\n\n\
                 ## Validation\n- `cargo test`\n"
            );
            let s = steps(&plan);
            assert_eq!(
                s.iter()
                    .map(|s| (s.number, s.title.as_str()))
                    .collect::<Vec<_>>(),
                [(1, "One"), (2, "Two")],
                "{one}: {s:#?}"
            );
            assert_eq!(
                s[0].files
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>(),
                ["src/a.rs"],
                "{one}"
            );
            assert_eq!(s[1].files.len(), 2, "{one}");
            assert!(s[1].files[1].new, "{one}");
            assert_eq!(
                plan_problems_in(&plan, Some(&tree.0)),
                Vec::<String>::new(),
                "{one}"
            );
        }
        // Headings inside a listed step don't split it.
        let listed = "# P\n\n## Steps\n\n1. **One**\n   - Files: `src/a.rs`\n   ### Step 1a: detail\n   - Add the loader to it.\n\
            2. **Two**\n   - Files: `src/b.rs`\n   - Add the saver to it.\n\n## Validation\n- `cargo test`\n";
        assert_eq!(steps(listed).len(), 2);
        assert!(
            plan_problems(listed).is_empty(),
            "{:?}",
            plan_problems(listed)
        );
    }

    /// The Files label in its other spellings: `File:` for one file,
    /// `Paths:`, `File paths:`.
    #[test]
    fn files_label_variants() {
        for label in ["File", "file", "Paths", "Path", "File paths", "**File**"] {
            let step =
                format!("1. **A**\n   - {label}: `src/a.rs` (new), `src/b.rs`\n   - Add a to it.");
            let files = step_files(&step).files;
            assert_eq!(
                files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
                ["src/a.rs", "src/b.rs"],
                "{label}"
            );
            assert!(files[0].new, "{label}");
        }
        assert!(
            step_files("1. **A**\n   - Profile: `src/a.rs`\n   - Add a.")
                .files
                .is_empty()
        );
    }

    /// P4.4: CRLF, `1. 1. 1.` numbering, `Group: none`, a "Next steps"
    /// note before the steps.
    #[test]
    fn steps_parse_robustly() {
        let plan =
            "# P\r\n\r\n## Next steps (later)\r\n\r\n1. Nothing here.\r\n\r\n## Steps\r\n\r\n\
            1. **One**\r\n   - Files: `src/a.rs`\r\n   - Add a to it.\r\n   - Group: Pages\r\n\
            1. **Two**\r\n   - Files: `src/b.rs`\r\n   - Add b to it.\r\n   - Group: none\r\n\
            1. **Three**\r\n   - Files: `src/c.rs`\r\n   - Add c to it.\r\n   - Group: pages\r\n\r\n\
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
        let plan = "# P\n\n## Steps\n\n1. **A**\n   - Files: `src/a.rs`\n   - Add a to it.\n\
            2. **B**\n   - Files: `src/b.rs`\n   - Add b to it.\n2. **C**\n   - Files: `src/c.rs`\n   - Add c to it.\n\
            3. **D**\n   - Files: `src/gone.rs`\n   - Add d to it.\n\n## Validation\n- `cargo test`\n";
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
        // Every file exists or is new, and steps 2 and 3 name different files.
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
        let early = REAL_PLAN.replace("(new), `migrations/README.md`", "(new), `src/api/users.rs`");
        let p = project_problems(&early, &tree.0);
        assert_eq!(p, ["Step 1 uses `src/api/users.rs`, which step 2 creates: put the steps in an order where a file is created before it's used."]);

        // Two steps of a group naming one file.
        let shared = REAL_PLAN.replace("(new), `src/pages/`", "(new), `src/api/mod.rs`");
        let p = project_problems(&shared, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with(
                "Steps 2 and 3 are both in group \"backend\" but both name `src/api/mod.rs`"
            ),
            "{p:?}"
        );
        // A path in the step's text isn't checked: only the Files line is.
        let in_text = "# M\n\n## Steps\n\n1. **A**\n   - Files: `src/a.rs` (new)\n   - Register it in `src/routs.rs`.\n\
            2. **B**\n   - Files: `src/api/mod.rs`\n   - Add the route for it.\n\n## Validation\n- `cargo test`\n";
        assert_eq!(project_problems(in_text, &tree.0), Vec::<String>::new());
        // P4.1: a folder and a file in it; a file one changes and another
        // reads; each refused with its steps.
        let group = |a: &str, b: &str| {
            format!(
                "# G\n\n## Steps\n\n1. **Contract**\n   - Files: `src/api/mod.rs`\n   - Add the stubs there.\n\
                 2. **A**\n   - Files: {a}\n   - Add the handlers there.\n   - Group: g\n\
                 3. **B**\n   - Files: {b}\n   - Register the handlers there.\n   - Group: g\n\n## Validation\n- `cargo test`\n"
            )
        };
        let p = project_problems(&group("`src/api/`", "`src/api/mod.rs`"), &tree.0);
        assert!(
            p.iter()
                .any(|p| p
                    .starts_with("Steps 2 and 3 are both in group \"g\" but both name `src/api`")),
            "{p:?}"
        );
        let p = project_problems(
            &group("`src/api/mod.rs`", "`src/api/mod.rs` (read)"),
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
            1. **Contract**\n   - Files: `src/api/mod.rs`\n   - Declare `fn list_users()` and `fn add_user()` with stub bodies.\n\
            2. **List**\n   - Files: `src/api/users.rs` (new), `src/api/mod.rs` (read)\n   - Implement `list_users` against the signature.\n   - Group: api\n\
            3. **Add**\n   - Files: `src/api/add.rs` (new), `src/api/mod.rs` (read-only)\n   - Implement `add_user` against the signature.\n   - Group: api\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(project_problems(contract, &tree.0), Vec::<String>::new());
        let s = steps(contract);
        assert!(s[1]
            .files
            .iter()
            .any(|f| f.path == "src/api/mod.rs" && f.read_only));
        assert!(!s[0].files[0].read_only);
        let no_contract = contract.replace(
            "Files: `src/api/mod.rs`\n",
            "Files: `migrations/README.md`\n",
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

    /// CSS classes, library names, word pairs and paths in a step's text
    /// aren't its files, so they don't stop the plan being saved; without
    /// a Files line they don't stand in for the file either.
    #[test]
    fn prose_is_not_files() {
        let tree = Tree::new(&["src/styles/app.css", "src/App.tsx"]);
        let plan = "# Tidy the sidebar\n\nx\n\n## Steps\n\n\
            1. **Narrow it**\n   - Files: `src/styles/app.css`\n   - `.sidebar`: set the width to 240px; `.sidebar__item`: add padding; \
            mind Node.js, the add/remove buttons, A/B tests, src/other.css and `src/gone.rs`.\n\
            2. **Hover**\n   - Files: `src/App.tsx`\n   - Add the `.sidebar__item:hover` class and change `.nav-item.active`, per github.com/foo/bar/README.md.\n\n\
            ## Validation\n- `npm run build`\n";
        assert_eq!(plan_problems_in(plan, Some(&tree.0)), Vec::<String>::new());
        assert_eq!(steps(plan)[0].files.len(), 1);
        let none = plan.replace("   - Files: `src/styles/app.css`\n", "");
        let p = plan_problems(&none);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 1 names no file: give each step a \"Files:\" line"),
            "{p:?}"
        );
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
        // Only the step without a Files line is reported.
        let one = "# T\n## Steps\n1. Loader\n   - Files: `src/a.rs`\n   - Add the `load` function for reading users.\n\
                   2. Docs\n   - Update the docs so they explain the new loading, e.g. in the readme.\n## Testing\n- cargo test";
        assert_eq!(plan_problems(one), vec![plan_problems(one)[0].clone()]);
        assert!(plan_problems(one)[0].starts_with("Step 2 names no file"));
        // The Files line decides, backticks or not; a path in the text doesn't.
        for (step, path) in [
            (
                "1. **G**\n   - Files: `src/grammar.pest`\n   - Add a rule.",
                "src/grammar.pest",
            ),
            ("1. **P**\n   - Files: src/plain.rs\n   - Log the id.", "src/plain.rs"),
            (
                "1. **W**\n   - Files: src\\api\\users.rs\n   - Add a field.",
                "src/api/users.rs",
            ),
            (
                "1. **D**\n   - Files: Dockerfile, `Cargo.toml` (new section)\n   - Change the base image.",
                "Dockerfile",
            ),
        ] {
            let files = step_files(step).files;
            assert_eq!(files.first().map(|f| f.path.as_str()), Some(path), "{step}");
            assert!(files.iter().all(|f| !f.new), "{step}");
        }
        assert!(
            step_files("1. **X**\n   - Change the base image in Dockerfile and `src/x.rs`.")
                .files
                .is_empty()
        );
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
                "# P\n\nx\n\n## Steps\n\n1. **One**\n   - Files: {a}\n   - Add `fn load()` to it.\n\
                 2. **Two**\n   - Files: {b}\n   - Add `fn save()` to it.\n\n## Validation\n- `cargo test`\n"
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
        assert!(
            project_problems(&plan("`src/db/new.rs` (new)", "`index.html`"), &tree.0).is_empty()
        );
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

    /// "n. **title**" with a Files line, a change and an optional group.
    fn step(n: usize, title: &str, files: &str, group: Option<&str>) -> String {
        let group = group
            .map(|g| format!("   - Group: {g}\n"))
            .unwrap_or_default();
        format!("{n}. **{title}**\n   - Files: {files}\n   - Add {title} to it.\n{group}")
    }

    /// Round 2 (§4.2), P4.2: a folder that only a new file of the plan is in
    /// counts as there, from another step and a level up; a folder whose
    /// name only starts the same doesn't.
    #[test]
    fn new_file_makes_its_folder_exist() {
        let tree = Tree::new(&["src/lib.rs"]);
        let plan = "# F\n\n## Steps\n\n\
            1. **Module**\n   - Files: `src/feature/sub/mod.rs` (new)\n   - Add the module.\n\
            2. **Wire**\n   - Files: `src/feature/`, `src/lib.rs`\n   - Register and declare the module.\n\n\
            ## Validation\n- `cargo test`\n";
        assert_eq!(project_problems(plan, &tree.0), Vec::<String>::new());
        let p = project_problems(&plan.replace("`src/feature/`", "`src/feat/`"), &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Step 2 names `src/feat/`, which isn't in the project"),
            "{p:?}"
        );
    }

    /// Round 2 (§4.2): a step that names no file is still a step (number,
    /// title and body kept, no files); the form check names it, the project
    /// check adds nothing, and its worker's scope is empty until a grant.
    #[test]
    fn step_with_zero_files() {
        use crate::agent_run::worker::WriteScope;
        let plan = "# Z\n\nx\n\n## Steps\n\n\
            1. **Helper**\n   - Files: `src/a.rs`\n   - Add `fn helper()`.\n\
            2. **Run it** — run the migration by hand and check the logs for errors.\n\
            3. **Use**\n   - Files: `src/b.rs`\n   - Call `helper()` from `main`.\n\n\
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

    /// Round 2, P4.4: numbers that go up with gaps (`2. 4. 7.`) are kept as
    /// written, with no separate written number, and the checks use them.
    #[test]
    fn gapped_numbers_are_kept() {
        let plan = format!(
            "# P\n\n## Steps\n\n{}{}{}\n## Validation\n- `cargo test`\n",
            step(2, "B", "`src/b.rs`", None),
            step(4, "D", "`src/d.rs`", None),
            step(7, "G", "`src/gone.rs`", None)
        );
        let s = steps(&plan);
        assert_eq!(s.iter().map(|s| s.number).collect::<Vec<_>>(), [2, 4, 7]);
        assert!(s.iter().all(|s| s.written.is_none()), "{s:#?}");
        assert_eq!(s[2].named(), "7");
        assert!(
            plan_problems(&plan).is_empty(),
            "{:?}",
            plan_problems(&plan)
        );
        let tree = Tree::new(&["src/b.rs", "src/d.rs"]);
        let p = project_problems(&plan, &tree.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 7 names `src/gone.rs`"), "{p:?}");
    }

    /// P4.4: `## Steps:` is `## Steps` (any case, a space before the colon
    /// too), taken before a "## Next steps" note.
    #[test]
    fn steps_heading_colon() {
        let plan = format!(
            "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps:\n\n{}{}\n## Validation\n- `cargo test`\n",
            step(1, "One", "`src/a.rs`", None),
            step(2, "Two", "`src/b.rs`", None)
        );
        let s = steps(&plan);
        assert_eq!(
            s.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert!(
            plan_problems(&plan).is_empty(),
            "{:?}",
            plan_problems(&plan)
        );
        let spaced = plan.replace("## Steps:", "## STEPS :");
        assert_eq!(steps(&spaced).len(), 2);
    }

    /// P4.4: a `Group:` line in a ``` or ~~~ code block isn't the step's
    /// group; a real one after the block still is.
    #[test]
    fn group_in_fenced_code_ignored() {
        let plan = "# P\n\n## Steps\n\n\
            1. **One**\n   - Files: `src/a.rs`\n   - Add a, configured as:\n\n     ```yaml\n     Group: fenced\n     ```\n\
            2. **Two**\n   - Files: `src/b.rs`\n   - Add b, configured as:\n\n     ~~~\n     group: tilde\n     ~~~\n\
            3. **Three**\n   - Files: `src/c.rs`\n   - Add c to it.\n\n     ```\n     Group: fenced\n     ```\n   - Group: Real\n\n\
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
            1. **Tabs**\n\t- Files: `src/a.rs` (new), `src/lib.rs`\n\t1. Add it.\n\t2. Register it.\n\
            2. **Spaces**\n    - Files: `src/b.rs` (new), `src/lib.rs`\n    1. Add it.\n    2. Register it.\n\
            3. **Both**\n\t- Files: `src/c.rs` (new), `src/lib.rs`\n\t1. Add it.\n  \t2. Register it.\n\t\t1. Export it.\n\n\
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
            1. **Store**\n   - Files: `src/store.rs` (new), `src/lib.rs`\n   1. Add `fn save(x: &X)`.\n   2. Register it.\n      1. Export `save` from the crate root.\n\
            2. **Use it**\n   - Files: `src/api.rs`, `src/api_test.rs`, `README.md`\n   1) Call `save`.\n   2) Check it in the test.\n   3) Document it.\n\n\
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
        let plan = format!(
            "# P\n\n## Steps\n\n{}{}{}{}{}{}\n## Validation\n- `cargo test`\n",
            step(1, "A", "`src/a.rs`", Some("`Backend`")),
            step(2, "B", "`src/b.rs`", None)
                .replace("   - Add B", "   - **Group:** backend\n   - Add B"),
            step(3, "C", "`src/c.rs`", None)
                .replace("   - Add C", "   - **Group**: **backend**\n   - Add C"),
            step(4, "D", "`src/d.rs`", Some("**`backend`**")),
            step(5, "E", "`src/e.rs`", None)
                .replace("   - Add E", "   - **Group: `BACKEND`**\n   - Add E"),
            step(6, "F", "`src/f.rs`", Some("`none`"))
        );
        let s = steps(&plan);
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
        let shared = plan.replace("Files: `src/b.rs`", "Files: `src/a.rs`");
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
        let plan = format!(
            "# P\n\n## Next steps (later)\n\n1. Nothing here.\n\n## Steps (ordered)\n\n{}{}\n## Validation\n- `cargo test`\n",
            step(1, "One", "`src/a.rs`", None),
            step(2, "Two", "`src/b.rs`", None)
        );
        let alone = plan.replace("## Next steps (later)\n\n1. Nothing here.\n\n", "");
        for plan in [alone.as_str(), plan.as_str()] {
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
        let plan = format!(
            "# P\n\n## 1. Summary\n\nTwo files.\n\n## 2. Next steps (later)\n\n1. Nothing here.\n\n\
            ## 3. Steps\n\n{}{}\n## 4. Validation\n- `cargo test`\n",
            step(1, "One", "`src/a.rs`", None),
            step(2, "Two", "`src/b.rs`", None)
        );
        let alone = plan.replace("## 2. Next steps (later)\n\n1. Nothing here.\n\n", "");
        for plan in [alone.as_str(), plan.as_str()] {
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
        let plan = format!(
            "# P\n\n## 1. Summary\n\nTwo files.\n\n## 3. Steps\n\n{}{}\n## 4) Validation\n- `cargo test`\n\n## 5. Decisions\n\n- Keep the old API.\n",
            step(1, "One", "`src/a.rs`", None),
            step(2, "Two", "`src/b.rs`", None)
        );
        assert_eq!(
            section(&plan, "Decisions").as_deref(),
            Some("- Keep the old API.")
        );
        assert_eq!(
            section(&plan, "validation").as_deref(),
            Some("- `cargo test`")
        );
        assert_eq!(section(&plan, "Summary").as_deref(), Some("Two files."));
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
        let plan = format!(
            "# P\n\n## Steps\n\n{}4. **D**\n   - Add the d helper next to b, with its own tests.\n{}\n## Validation\n- `cargo test`\n",
            step(2, "B", "`src/b.rs`", None),
            step(7, "G", "`src/g.rs`", None)
        );
        let p = plan_problems(&plan);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("Step 4 names no file"), "{p:?}");
        let plan = format!(
            "# P\n\n## Steps\n\n{}{}{}3. **D**\n   - Files: `src/d.rs`\n\n## Validation\n- `cargo test`\n",
            step(1, "A", "`src/a.rs`", None),
            step(2, "B", "`src/b.rs`", None),
            step(2, "C", "`src/c.rs`", None)
        );
        let p = plan_problems(&plan);
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
            1. **One**\n   - Files: `src/a.rs`\n   - Add `a` to the module.\n\
            2. **Two**\n   - Files: `src/b.rs`\n   - Add `b` with this signature:\n\n     ```rust\n     pub fn b() -> u8\n\
            3. **Three**\n   - Files: `src/c.rs`\n   - Call `b` from `c`.\n\n## Validation\n- `cargo test`\n";
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

    /// Round 2, P4.4: a period or a note after a group's name isn't part of
    /// it, so these steps are the group the lead asks for ("backend").
    #[test]
    fn group_name_followed_by_a_note_or_period() {
        let plan = format!(
            "# P\n\n## Steps\n\n{}{}{}{}{}\n## Validation\n- `cargo test`\n",
            step(1, "A", "`src/a.rs`", Some("`backend`.")),
            step(
                2,
                "B",
                "`src/b.rs`",
                Some("`Backend` (runs alongside step 1)")
            ),
            step(3, "C", "`src/c.rs`", Some("backend.")),
            step(4, "D", "`src/d.rs`", None).replace(
                "   - Add D",
                "   - **Group:** backend (with step 1)\n   - Add D"
            ),
            step(5, "E", "`src/e.rs`", Some("`none`."))
        );
        assert_eq!(
            steps(&plan)
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
    /// V02: the commands of a plan's Validation section, as checks: code
    /// spans and code blocks that read as checks, not files, functions,
    /// steps by hand or commands that never end.
    #[test]
    fn a_plans_validation_commands_are_its_checks() {
        let plan = "# Add users\n\n## Steps\n\n1. **Users**\n   - Files: `src/users.rs`\n   - Run `cargo fmt` there.\n\n\
            ## Validation\n\
            - `cargo test -p users` and `cargo clippy --all-targets`\n\
            - `npx tsc --noEmit`; `npm run test:unit`\n\
            - `python -m pytest tests/test_users.py`\n\
            - Add a test for `parse_args` in `tests/test_users.py`, as `test_parse_args()`\n\
            - `node check.js`, `./scripts/ci.sh check`\n\
            - Open /users by hand; `npm run dev` and `cargo run` to look; `vitest --watch`\n\
            - `cargo test -p users` again\n\
            ```sh\n$ go vet ./...\nmake test\n```\n";
        assert_eq!(
            validation_commands(plan),
            [
                "cargo test -p users",
                "cargo clippy --all-targets",
                "npx tsc --noEmit",
                "npm run test:unit",
                "python -m pytest tests/test_users.py",
                "node check.js",
                "./scripts/ci.sh check",
                "go vet ./...",
                "make test",
            ]
        );
        // No section, an empty one, or nothing that runs: none.
        assert!(validation_commands("# Plan\n\n## Steps\n1. x\n").is_empty());
        assert!(validation_commands("# Plan\n\n## Validation\n\n## Decisions\n- x\n").is_empty());
        assert!(
            validation_commands("## Validation\n- Check it by hand.\n- `ok.txt` says ok\n")
                .is_empty()
        );
        assert!(validation_commands("## Validation\n- ``\n- `   `\n- `` ` ``\n").is_empty());
        assert_eq!(
            [
                "npx tsc --noEmit",
                "cargo clippy",
                "go vet ./...",
                "npm run build",
                "cargo test",
                "node check.js"
            ]
            .map(validation_kind),
            ["typecheck", "lint", "lint", "build", "test", "test"]
        );
    }
}

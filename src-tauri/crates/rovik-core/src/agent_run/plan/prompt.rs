//! What Plan mode tells the model: its instructions (Autopilot's plan
//! stage's lines in their place), what a call of a tool it doesn't offer
//! gets, and whether a reply wrote a plan out instead of submitting it.

use regex::Regex;
use std::sync::LazyLock;

/// What a call of a tool Plan mode doesn't offer gets (its row's modes,
/// builtin_tools.rs): why not, and what comes next. For Rovik's own calls,
/// and for a connector's own tools it asks about.
pub fn blocked_in_plan_mode(name: &str, autopilot: bool) -> String {
    // Autopilot's plan stage: the task implements its plan next.
    let next = match autopilot {
        true => "Autopilot implements it next, once it's reviewed",
        false => "the user implements it in Agent mode",
    };
    format!(
        "Error: {name} isn't available in Plan mode, which doesn't change anything. \
         Finish the plan; {next}."
    )
}

/// What a call of a tool only Plan mode offers (submit_plan) gets in Agent
/// mode, which has no plan to save.
pub fn only_in_plan_mode(name: &str) -> String {
    format!(
        "Error: {name} is only available in Plan mode. Carry out the task here; a plan asked \
         for goes in your reply."
    )
}

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
pub(super) const PLAN_MODE_CHOSEN: &str =
    "The user chose Plan mode: investigate and write a plan; don't carry it out.";

pub(super) const AUTOPILOT_PLAN_STAGE: &str = "This is the plan stage of an Autopilot task: investigate and write a plan; don't carry it out. Once it's reviewed, another run implements it, with nobody there.";

pub(super) const PLAN_MODE_QUESTIONS: &str = "2. Ask only what the code can't tell you and what really changes the design, behavior or scope: with the ask_user tool, early, all questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything the repository decides: decide it and say so in the plan. Never ask in plain text and stop.";

pub(super) const AUTOPILOT_QUESTIONS: &str = "2. Nobody answers questions in Autopilot: decide what the code and the request can't tell you, and write each assumption under **Assumptions** in \"## Criteria\", with why. Never ask, with a tool or in plain text.";

/// The plan's first section, after which Autopilot's plan says what done
/// means (S01, D8).
pub(super) const PLAN_SUMMARY_LINE: &str =
    "   - a \"# \" title and a one-paragraph summary of the approach;";

pub(super) const AUTOPILOT_CRITERIA: &str = "   - \"## Criteria\": what done means, as numbered statements (`1.`, `2.`), each one a test or a reading of the result can check, together covering the whole request and nothing beyond it; then **Out of scope**: what the task leaves alone, and **Assumptions**: what you decided that the request doesn't say, with why (either may say \"None.\"). The tests are written from them before the code, and the result is judged by them;";

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

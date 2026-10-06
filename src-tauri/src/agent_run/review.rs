//! "By AI reviewer" approval. Two ways to ask, depending on the model:
//!
//!   - Chat models get a text prompt and answer {"decision", "reason"}.
//!   - Jev (TypeSafe's System One model), with "Use typed input", gets the
//!     action as JSON `state` plus named yes/no (Noul) questions on the
//!     System One API, and answers a probability per question. Code turns
//!     those into a decision with fixed thresholds: approve when every check
//!     is clearly yes, reject when any is clearly no, and ask the user in
//!     between. Jev returns no text, so the reason shown is the checks.
//!
//! The reviewer model, then each fallback, is tried until one answers. If
//! none does, or Jev is unsure, the caller asks the user: nothing runs
//! unreviewed.
//!
//! System One reference: https://docs.typesafe.ai and OpenRouter's Jev
//! recipes for gating tool calls and permission prompts.

use super::ModelProfile;
use crate::agent::{ChatParams, ChatStreamEvent, OpenAIChatOptions, ReasoningEffort, WireChatMessage};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;
use tauri::{AppHandle, Manager, Runtime};

/// Which approval group the action belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Commands,
    Edits,
    Tools,
}

pub struct ReviewInput {
    pub group: Group,
    /// The user's latest request in this chat, for intent.
    pub user_request: String,
    /// Absolute project folder.
    pub project: String,
    pub tool_name: String,
    /// One-line summary, e.g. "Run `npm test`".
    pub summary: String,
    pub args_json: String,
    /// File edits: the change as a compact diff (empty for a new file).
    pub edit_diff: Option<String>,
    /// File edits: the path as the agent gave it.
    pub edit_path: Option<String>,
    pub is_new_file: bool,
}

#[derive(Debug, PartialEq)]
pub enum ReviewResult {
    Decided { approved: bool, reason: String, model: String },
    /// The reviewer answered but wasn't sure either way: ask the user.
    Unsure { reason: String },
    Failed { errors: Vec<String> },
}

const MAX_ARGS_CHARS: usize = 6_000;
const MAX_REQUEST_CHARS: usize = 2_000;
/// Keeps a typed request well inside Jev's 32k-token context on OpenRouter.
const MAX_DIFF_CHARS: usize = 24_000;

/*
 * ---------------------------------------------------------------------------
 * Text prompt (chat models)
 * ---------------------------------------------------------------------------
 */

const SYSTEM_PROMPT: &str = r#"You review actions an AI coding agent wants to take in a user's project, inside their IDE. Decide whether the action should run.

Approve when the action is a reasonable step toward what the user asked and its effects are contained to the project.
Reject when it is destructive beyond what the task needs (deleting or overwriting unrelated files, rewriting git history, force pushes), exfiltrates data or secrets, installs or runs untrusted code from the internet, touches files outside the project, changes system settings, or clearly doesn't serve the user's request.
When unsure about something irreversible, reject.

Reply with only a JSON object, no other text:
{"decision": "approve" | "reject", "reason": "<one short sentence>"}"#;

fn cut(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count > max {
        format!("{}\n… ({} more characters)", text.chars().take(max).collect::<String>(), count - max)
    } else {
        text.to_string()
    }
}

fn prefix(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn build_prompt(input: &ReviewInput) -> String {
    let request = prefix(input.user_request.trim(), MAX_REQUEST_CHARS);
    let request = if request.is_empty() { "(not available)".to_string() } else { request };
    // For edits the diff says more than the whole new file.
    let details = match (&input.edit_diff, input.group) {
        (Some(diff), Group::Edits) if !diff.is_empty() => format!("Diff:\n{}", cut(diff, MAX_ARGS_CHARS)),
        _ => format!("Arguments:\n{}", cut(&input.args_json, MAX_ARGS_CHARS)),
    };
    [
        format!("User's request:\n{request}"),
        String::new(),
        format!("Project: {}", input.project),
        format!("Action: {}", input.summary),
        format!("Tool: {}", input.tool_name),
        details,
        String::new(),
        "Decision as JSON:".into(),
    ]
    .join("\n")
}

static THINKING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<think(?:ing)?>.*?(?:</think(?:ing)?>|$)").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\W*(approve|approved|reject|rejected)\b").unwrap());

/// Removes reasoning blocks some local models put into the content.
pub fn strip_thinking(raw: &str) -> String {
    THINKING.replace_all(raw, "").trim().to_string()
}

/// Pulls {"decision", "reason"} out of a reply, tolerating fences and extra text.
pub fn parse_decision(raw: &str) -> Option<(bool, String)> {
    let text = strip_thinking(raw);
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end > start {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text[start..=end]) {
                let decision = match parsed.get("decision") {
                    Some(Value::String(s)) => s.to_lowercase(),
                    Some(v) if !v.is_null() => v.to_string().to_lowercase(),
                    _ => String::new(),
                };
                if decision == "approve" || decision == "reject" {
                    let reason = match parsed.get("reason") {
                        Some(Value::String(s)) => s.trim().to_string(),
                        Some(v) if !v.is_null() => v.to_string(),
                        _ => String::new(),
                    };
                    let reason = if reason.is_empty() { "No reason given.".to_string() } else { reason };
                    return Some((decision == "approve", reason));
                }
            }
        }
    }
    // Some models answer with just the word.
    WORD.captures(&text)
        .map(|c| (c[1].to_lowercase().starts_with("approve"), "No reason given.".to_string()))
}

/// Providers disagree on request options: OpenAI rejects unknown fields and,
/// for reasoning models, `max_tokens`; other models ignore the effort and
/// think until the token limit runs out. So a cheap request first, then a
/// plain one (no options).
pub fn lean_then_plain(max_tokens: u32) -> [Option<OpenAIChatOptions>; 2] {
    [
        Some(OpenAIChatOptions {
            max_tokens: Some(max_tokens),
            temperature: Some(0.0),
            reasoning_effort: Some(ReasoningEffort::None),
            ..Default::default()
        }),
        None,
    ]
}

/// A single request outside any chat: the messages are given and nothing is stored.
pub async fn complete_once<R: Runtime>(
    app: &AppHandle<R>,
    profile: &ModelProfile,
    system_prompt: &str,
    user: String,
    options: Option<OpenAIChatOptions>,
) -> Result<String, String> {
    let params = ChatParams {
        base_url: profile.base_url.clone(),
        api_key: profile.api_key.clone(),
        model: profile.model.clone(),
        messages: Some(vec![WireChatMessage::text("user", user)]),
        system_prompt: Some(system_prompt.to_string()),
        streaming: profile.stream,
        first_byte_timeout_ms: profile.first_byte_timeout_ms,
        options,
        ..Default::default()
    };
    let mut text = String::new();
    let mut failure = None;
    let mut emit = |event: ChatStreamEvent| match event {
        ChatStreamEvent::Delta { content } => text.push_str(&content),
        ChatStreamEvent::Error { error } => failure = Some(error),
        _ => {}
    };
    // Never cancelled: the sender lives as long as the request.
    let (_tx, mut rx) = tokio::sync::watch::channel(false);
    let db = app.state::<crate::chat_db::DbState>();
    let vfs = app.state::<crate::vfs::VfsState>();
    crate::agent::run_chat(&db, &vfs, params, &mut rx, &mut emit).await?;
    match failure {
        Some(e) => Err(format!("{}: {}", serde_json::to_value(e.kind).unwrap_or_default().as_str().unwrap_or("error"), e.message)),
        None => Ok(text),
    }
}

async fn review_with_text<R: Runtime>(
    app: &AppHandle<R>, profile: &ModelProfile, input: &ReviewInput, errors: &mut Vec<String>) -> Option<ReviewResult> {
    let prompt = build_prompt(input);
    for options in lean_then_plain(300) {
        match complete_once(app, profile, SYSTEM_PROMPT, prompt.clone(), options).await {
            Err(e) => errors.push(format!("{}: {e}", profile.label)),
            Ok(text) => match parse_decision(&text) {
                Some((approved, reason)) => {
                    return Some(ReviewResult::Decided { approved, reason, model: profile.label.clone() })
                }
                None => errors.push(format!("{}: unclear reply", profile.label)),
            },
        }
    }
    None
}

/*
 * ---------------------------------------------------------------------------
 * Typed input (Jev / System One)
 * ---------------------------------------------------------------------------
 */

/// Every check at or above this: approved.
pub const APPROVE_AT: f64 = 0.9;
/// Any check at or below this: rejected. In between: the user decides.
pub const REJECT_AT: f64 = 0.1;

struct Noul {
    key: &'static str,
    /// Shown in the decision's reason.
    label: &'static str,
    instructions: &'static str,
}

/*
 * One proposition per question, phrased so yes means safe to run, each
 * naming the `state` fields it depends on. The task question is skipped
 * when there's no task text (Jev would read a missing task as a mismatch).
 */
fn questions(group: Group) -> &'static [Noul] {
    match group {
        Group::Commands => &[
            Noul {
                key: "reversible",
                label: "reversible",
                instructions: "Every command in `commands` only reads or changes files inside `project` and can be undone with git or by rerunning it. It does not push, publish, deploy, delete files outside the project, change system settings, or send data to a network service.",
            },
            Noul {
                key: "serves_task",
                label: "fits the task",
                instructions: "Running `commands` is a reasonable next step toward `task`.",
            },
        ],
        Group::Edits => &[
            Noul {
                key: "serves_task",
                label: "fits the task",
                instructions: "The change in `edit.diff` to the file `edit.path` is a reasonable step toward `task`.",
            },
            Noul {
                key: "scoped",
                label: "stays in scope",
                instructions: "The change in `edit.diff` only touches code that `task` is about. It does not delete or rewrite unrelated code, tests, or configuration.",
            },
            Noul {
                key: "safe",
                label: "no secrets or exfiltration",
                instructions: "The change in `edit.diff` does not add credentials or secrets, does not weaken security checks, and does not add network calls that send data somewhere `task` does not ask for.",
            },
        ],
        Group::Tools => &[
            Noul {
                key: "serves_task",
                label: "fits the task",
                instructions: "Calling the tool `tool.name` with `tool.arguments` is a reasonable next step toward `task`.",
            },
            Noul {
                key: "safe",
                label: "no secrets or exfiltration",
                instructions: "`tool.arguments` does not target credential or secret files (such as .env files, keys, or tokens) and does not send project code, secrets, or personal data to an outside service.",
            },
        ],
    }
}

fn args_object(args_json: &str) -> Value {
    if args_json.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(args_json) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => json!({}),
        Err(_) => json!({ "raw": args_json }),
    }
}

fn typed_state(input: &ReviewInput) -> Map<String, Value> {
    let task = prefix(input.user_request.trim(), MAX_REQUEST_CHARS);
    let mut state = Map::new();
    state.insert("project".into(), json!(input.project));
    if !task.is_empty() {
        state.insert("task".into(), json!(task));
    }
    let args = args_object(&input.args_json);
    let arg = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match input.group {
        Group::Commands => {
            state.insert("commands".into(), json!([arg("command")]));
        }
        Group::Edits => {
            // A new file has no diff: its content is the change.
            let diff = if input.is_new_file {
                cut(&arg("content"), MAX_DIFF_CHARS)
            } else {
                cut(input.edit_diff.as_deref().unwrap_or(""), MAX_DIFF_CHARS)
            };
            let path = input.edit_path.clone().unwrap_or_else(|| arg("path"));
            state.insert("edit".into(), json!({ "path": path, "new_file": input.is_new_file, "diff": diff }));
        }
        Group::Tools => {
            state.insert("tool".into(), json!({ "name": input.tool_name, "arguments": args }));
        }
    }
    state
}

/// A model that only speaks System One (Jev), so a text prompt can't work.
pub fn is_jev_model(model_id: &str) -> bool {
    static JEV: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|[/~:-])jev([-_.]|$)").unwrap());
    static ROUTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)jev-router").unwrap());
    // Jev Router is a chat model that routes to other models.
    JEV.is_match(model_id) && !ROUTER.is_match(model_id)
}

/// The decision from the probabilities, by the fixed thresholds.
fn decide(label: &str, scores: &[(&Noul, f64)]) -> ReviewResult {
    let shown = |list: &[&(&Noul, f64)]| {
        list.iter().map(|(q, p)| format!("{} {}%", q.label, (p * 100.0).round())).collect::<Vec<_>>().join(", ")
    };
    let failed: Vec<&(&Noul, f64)> = scores.iter().filter(|(_, p)| *p <= REJECT_AT).collect();
    if !failed.is_empty() {
        return ReviewResult::Decided { approved: false, reason: format!("Failed: {}.", shown(&failed)), model: label.into() };
    }
    let all: Vec<&(&Noul, f64)> = scores.iter().collect();
    if scores.iter().all(|(_, p)| *p >= APPROVE_AT) {
        return ReviewResult::Decided { approved: true, reason: format!("Checks passed: {}.", shown(&all)), model: label.into() };
    }
    ReviewResult::Unsure { reason: format!("{label} wasn’t sure ({}).", shown(&all)) }
}

async fn review_with_typed(profile: &ModelProfile, input: &ReviewInput, errors: &mut Vec<String>) -> Option<ReviewResult> {
    let state = typed_state(input);
    let asked: Vec<&Noul> =
        questions(input.group).iter().filter(|q| q.key != "serves_task" || state.contains_key("task")).collect();
    let body_questions: Map<String, Value> = asked
        .iter()
        .map(|q| (q.key.to_string(), json!({ "type": "noul", "instructions": q.instructions })))
        .collect();
    let body = json!({ "input": { "state": state, "questions": body_questions } });

    let response =
        match crate::system_one::request(&profile.base_url, &profile.api_key, &profile.model, body, Some(15_000)).await {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{}: {e}", profile.label));
                return None;
            }
        };

    // Every asked question needs a probability, or the answer isn't usable.
    let mut scores = Vec::new();
    for q in asked {
        let p = response.pointer(&format!("/answers/{}/noul", q.key)).and_then(|v| v.as_f64());
        match p {
            Some(p) if (0.0..=1.0).contains(&p) => scores.push((q, p)),
            _ => {
                errors.push(format!("{}: no usable answer for \"{}\"", profile.label, q.key));
                return None;
            }
        }
    }
    Some(decide(&profile.label, &scores))
}

/// The reviewer model, then each fallback, until one answers. `typed_for`
/// says which ones are asked with typed input.
pub async fn review<R: Runtime>(
    app: &AppHandle<R>,
    models: &[ModelProfile],
    input: &ReviewInput,
    typed_for: impl Fn(usize, &ModelProfile) -> bool,
) -> ReviewResult {
    let mut errors = Vec::new();
    for (i, profile) in models.iter().enumerate() {
        let answer = if typed_for(i, profile) {
            review_with_typed(profile, input, &mut errors).await
        } else {
            review_with_text(app, profile, input, &mut errors).await
        };
        // An unsure answer is an answer: it goes to the user, not to a fallback.
        if let Some(answer) = answer {
            return answer;
        }
    }
    ReviewResult::Failed { errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_from_replies() {
        assert_eq!(
            parse_decision("<think>hmm {x}</think>```json\n{\"decision\": \"Approve\", \"reason\": \"fine\"}\n```"),
            Some((true, "fine".into()))
        );
        assert_eq!(parse_decision("{\"decision\":\"reject\"}"), Some((false, "No reason given.".into())));
        assert_eq!(parse_decision("Rejected: no"), Some((false, "No reason given.".into())));
        assert_eq!(parse_decision("maybe"), None);
    }

    #[test]
    fn jev_models_and_thresholds() {
        assert!(is_jev_model("typesafe/jev-1"));
        assert!(is_jev_model("jev"));
        assert!(!is_jev_model("typesafe/jev-router"));
        assert!(!is_jev_model("jevons"));
        let qs = questions(Group::Edits);
        let scores = [(&qs[0], 0.95), (&qs[1], 0.92), (&qs[2], 0.99)];
        assert!(matches!(decide("Jev", &scores), ReviewResult::Decided { approved: true, .. }));
        let scores = [(&qs[0], 0.95), (&qs[1], 0.05), (&qs[2], 0.5)];
        assert_eq!(
            decide("Jev", &scores),
            ReviewResult::Decided { approved: false, reason: "Failed: stays in scope 5%.".into(), model: "Jev".into() }
        );
        assert!(matches!(decide("Jev", &[(&qs[0], 0.5)]), ReviewResult::Unsure { .. }));
    }

    #[test]
    fn typed_state_for_each_group() {
        let input = ReviewInput {
            group: Group::Commands,
            user_request: "  ".into(),
            project: "/p".into(),
            tool_name: "run_command".into(),
            summary: "".into(),
            args_json: r#"{"command":"ls"}"#.into(),
            edit_diff: None,
            edit_path: None,
            is_new_file: false,
        };
        let state = typed_state(&input);
        assert_eq!(Value::Object(state.clone()), json!({ "project": "/p", "commands": ["ls"] }));
        assert!(!state.contains_key("task"), "no task: the serves_task question is skipped");
        assert!(build_prompt(&input).contains("User's request:\n(not available)"));
    }
}

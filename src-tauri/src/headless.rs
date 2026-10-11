//! The agent without a window (G00, docs/design/subagents-plan.md): opens a
//! project, starts one run with a settings profile, answers the approvals
//! and questions it asks by a policy file, and writes the outcome and the
//! run log as JSON. For measuring the agent (milestone G) from the command
//! line and in CI:
//!
//!   cargo run --features headless --bin rovik-headless -- \
//!     --project ./repo --settings settings.json [--policy policy.json] \
//!     --prompt "Fix the failing test" --out report.json [--data ./data] \
//!     [--timeout 1800] [--max-tokens 1500000]
//!
//! `--max-tokens` stops the run once its requests (its subagents' included)
//! have used that many tokens, as the provider reports them: a budget cap
//! for evaluations (scripts/eval).
//!
//! The settings file:
//!
//!   { "profile": { "id": "p::m", "label": "P/m", "model": "m",
//!                  "baseUrl": "https://…/v1", "stream": true,
//!                  "contextTokens": 128000 },
//!     "apiKeyEnv": "OPENROUTER_API_KEY",
//!     "options": { "reasoning_effort": "low" },
//!     "price": { "inputPerMillion": 3.0, "outputPerMillion": 15.0 },
//!     "mode": "agent", "contextMode": "trimming",
//!     "config": { "permissions": { … }, "approvalProfiles": [],
//!                 "subagents": { "enabled": true, "kinds": { … } } } }
//!
//! The policy file (anything it doesn't cover is allowed). Its deny rules
//! for commands are the run's denied commands: refused whenever the agent
//! runs them, approvals or not. Its other rules answer approvals, when the
//! settings ask for them. A provider that reports no usage stops a run that
//! has `--max-tokens` (outcome `no-usage`): the cap can't be kept.
//!
//!   { "default": "allow",
//!     "rules": [ { "tool": "bash", "contains": "git push", "decision": "deny" } ],
//!     "questions": "recommended" }      // or "agent" (the agent decides), "stop"
//!
//! It runs on tauri's mock runtime, as the end-to-end tests do: the real
//! database, tools and requests, no webview. The chat isn't isolated (that
//! needs the virtual-files driver).

use crate::agent::OpenAIChatOptions;
use crate::agent_run::plan::{QuestionAnswer, QuestionsOutcome};
use crate::agent_run::{
    self, events, AgentConfig, AgentState, ApprovalDecision, ModelProfile, StartKind, StartRequest,
    UserMessage,
};
use crate::chat_db::store::{self, NewDialog};
use crate::chat_db::DbState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::test::{mock_builder, mock_context, noop_assets};
use tauri::{Listener, Manager};

/// What a run is started with, from the settings file.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub profile: ModelProfile,
    /// The environment variable holding the API key, so it needn't be in
    /// the file.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub options: Option<OpenAIChatOptions>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub context_mode: Option<String>,
    #[serde(default)]
    pub config: AgentConfig,
    /// What the model costs, for runs whose provider doesn't report it.
    #[serde(default)]
    pub price: Option<Price>,
}

/// US dollars per million tokens.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    #[default]
    Allow,
    Deny,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// The tool it applies to; none: any.
    #[serde(default)]
    pub tool: Option<String>,
    /// Text the call's summary or command must contain; none: any.
    #[serde(default)]
    pub contains: Option<String>,
    pub decision: Decision,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Questions {
    /// Each question's recommended option (else its first).
    #[default]
    Recommended,
    /// "You decide", for each.
    Agent,
    /// Stop the run.
    Stop,
}

/// How the runner answers what the run asks the user.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Policy {
    pub default: Decision,
    pub rules: Vec<Rule>,
    pub questions: Questions,
}

impl Policy {
    /// The first rule that matches the request decides; else the default.
    pub fn decide(&self, tool: &str, summary: &str, command: Option<&str>) -> Decision {
        self.rules
            .iter()
            .find(|r| {
                r.tool.as_deref().is_none_or(|t| t == tool)
                    && r.contains.as_deref().is_none_or(|c| {
                        summary.contains(c) || command.is_some_and(|cmd| cmd.contains(c))
                    })
            })
            .map_or(self.default, |r| r.decision)
    }

    /// Its deny rules for commands (bash, or any tool), as commands the run
    /// refuses (`Permissions::denied_commands`): approvals are asked only
    /// when the settings say so, and an evaluation's settings don't.
    pub fn denied_commands(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| r.decision == Decision::Deny)
            .filter(|r| r.tool.as_deref().is_none_or(|t| t == "bash"))
            .filter_map(|r| r.contains.as_deref())
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn answer(&self, questions: &Value) -> QuestionsOutcome {
        let list = questions.as_array().cloned().unwrap_or_default();
        match self.questions {
            Questions::Stop => QuestionsOutcome::Stopped { stopped: true },
            Questions::Agent => QuestionsOutcome::Answers {
                answers: list.iter().map(|_| QuestionAnswer::Agent).collect(),
            },
            Questions::Recommended => QuestionsOutcome::Answers {
                answers: list
                    .iter()
                    .map(|q| {
                        let options = q["options"].as_array().cloned().unwrap_or_default();
                        let pick = options
                            .iter()
                            .find(|o| o["recommended"] == true)
                            .or(options.first())
                            .and_then(|o| o["label"].as_str());
                        match pick {
                            Some(label) => QuestionAnswer::Option {
                                label: label.to_string(),
                            },
                            None => QuestionAnswer::Agent,
                        }
                    })
                    .collect(),
            },
        }
    }
}

/// The command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub project: PathBuf,
    pub settings: PathBuf,
    pub policy: Option<PathBuf>,
    pub prompt: String,
    pub out: PathBuf,
    pub data: Option<PathBuf>,
    pub timeout: Duration,
    /// Stop the run past this many tokens (all its requests).
    pub max_tokens: Option<u64>,
}

pub const USAGE: &str =
    "usage: rovik-headless --project DIR --settings FILE --prompt TEXT --out FILE \
     [--policy FILE] [--data DIR] [--timeout SECONDS]";

pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut project = None;
    let mut settings = None;
    let mut policy = None;
    let mut prompt = None;
    let mut out = None;
    let mut data = None;
    let mut timeout = Duration::from_secs(1800);
    let mut max_tokens = None;
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--project" => project = Some(PathBuf::from(value()?)),
            "--settings" => settings = Some(PathBuf::from(value()?)),
            "--policy" => policy = Some(PathBuf::from(value()?)),
            "--prompt" => prompt = Some(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--data" => data = Some(PathBuf::from(value()?)),
            "--timeout" => {
                let secs: u64 = value()?
                    .parse()
                    .map_err(|_| format!("--timeout takes seconds\n{USAGE}"))?;
                timeout = Duration::from_secs(secs.max(1));
            }
            "--max-tokens" => {
                let n: u64 = value()?
                    .parse()
                    .map_err(|_| format!("--max-tokens takes a number\n{USAGE}"))?;
                max_tokens = Some(n.max(1));
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    fn need<T>(v: Option<T>, name: &str) -> Result<T, String> {
        v.ok_or_else(|| format!("{name} is required\n{USAGE}"))
    }
    Ok(Args {
        project: need(project, "--project")?,
        settings: need(settings, "--settings")?,
        policy,
        prompt: need(prompt, "--prompt")?,
        out: need(out, "--out")?,
        data,
        timeout,
        max_tokens,
    })
}

/// Tokens in and out, and the cost when the provider says, of one reply's
/// usage (OpenAI's names, or the Responses API's).
pub fn usage_of(u: &Value) -> (u64, u64, Option<f64>) {
    let n = |keys: &[&str]| keys.iter().find_map(|k| u.get(*k)?.as_u64()).unwrap_or(0);
    (
        n(&["prompt_tokens", "input_tokens"]),
        n(&["completion_tokens", "output_tokens"]),
        u.get("cost").and_then(|c| c.as_f64()),
    )
}

/// What a run's requests used, from its run log's reply lines.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Totals {
    pub lead_in: u64,
    pub lead_out: u64,
    pub sub_in: u64,
    pub sub_out: u64,
    /// The largest request the lead sent (its context at its fullest).
    pub peak_lead_context: u64,
    /// Reported cost, when every reply with usage reported one.
    pub reported_cost: Option<f64>,
    /// Replies with usage.
    pub replies: usize,
    /// Replies that came back whole without usage: the provider doesn't
    /// report it, so neither tokens nor cost are known.
    pub missing_usage: usize,
}

impl Totals {
    pub fn tokens(&self) -> u64 {
        self.lead_in + self.lead_out + self.sub_in + self.sub_out
    }

    /// One reply line of the run log: the lead's (`dialog_id`) or a
    /// subagent's.
    pub fn add(&mut self, line: &Value, dialog_id: &str) {
        if line["event"] != "reply" {
            return;
        }
        let Some(u) = line.get("usage").filter(|u| u.is_object()) else {
            // A request that failed or was cut short may have none.
            if line.get("error").is_none() {
                self.missing_usage += 1;
            }
            return;
        };
        let (i, o, cost) = usage_of(u);
        if line["dialogId"] == dialog_id {
            self.lead_in += i;
            self.lead_out += o;
            self.peak_lead_context = self.peak_lead_context.max(i);
        } else {
            self.sub_in += i;
            self.sub_out += o;
        }
        self.reported_cost = match (self.replies, self.reported_cost, cost) {
            (0, _, c) => c,
            (_, Some(a), Some(c)) => Some(a + c),
            _ => None,
        };
        self.replies += 1;
    }

    /// Its cost: as reported, else by the settings' price; None: unknown.
    pub fn cost(&self, price: Option<Price>) -> (Option<f64>, &'static str) {
        // Some replies' tokens aren't known: nor is the cost.
        if self.missing_usage > 0 {
            return (None, "unknown");
        }
        if let Some(c) = self.reported_cost {
            return (Some(c), "provider");
        }
        match price {
            Some(p) => (
                Some(
                    (self.lead_in + self.sub_in) as f64 * p.input_per_million / 1e6
                        + (self.lead_out + self.sub_out) as f64 * p.output_per_million / 1e6,
                ),
                "price",
            ),
            None if self.replies == 0 => (Some(0.0), "none"),
            None => (None, "unknown"),
        }
    }
}

/// Reads the run log's new lines since `offset` (the log is appended to).
fn new_log_lines(path: &Path, offset: &mut u64) -> Vec<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if f.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    // Only whole lines: a line being written stays for next time.
    let whole = text.rfind('\n').map_or(0, |i| i + 1);
    *offset += whole as u64;
    text[..whole]
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs once and writes the report; returns it too.
pub async fn run(args: &Args) -> Result<Value, String> {
    // One run at a time: the agent log's folder is process-wide.
    static ONE_RUN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE_RUN.lock().await;
    let mut settings: Settings = read_json(&args.settings)?;
    if settings.profile.api_key.is_empty() {
        if let Some(var) = &settings.api_key_env {
            settings.profile.api_key = std::env::var(var).unwrap_or_default();
        }
    }
    let policy: Policy = match &args.policy {
        Some(p) => read_json(p)?,
        None => Policy::default(),
    };
    let project = args
        .project
        .canonicalize()
        .map_err(|e| format!("{}: {e}", args.project.display()))?;
    let data = match &args.data {
        Some(d) => d.clone(),
        None => std::env::temp_dir().join(format!("rovik-headless-{}", uuid::Uuid::new_v4())),
    };
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let out_dir = args
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    // The run log goes with the report.
    let log_dir = data.join("log");
    crate::project_profile::set_agent_log_dir(Some(log_dir.clone()));

    let db = DbState::open(&data).map_err(|e| e.to_string())?;
    let app = mock_builder()
        .manage(db.clone())
        .manage(crate::vfs::VfsState::default())
        .manage(crate::browser_tools::BrowserState::default())
        .manage(AgentState::default())
        .build(mock_context(noop_assets()))
        .map_err(|e| e.to_string())?;
    let handle = app.handle().clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    handle.listen_any(events::EVENT, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            let _ = tx.send(v);
        }
    });

    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: dialog_id.clone(),
        title: crate::chat_db::store::char_prefix(&args.prompt, 60).to_string(),
        created_at: store::now_ms(),
        updated_at: store::now_ms(),
        project_id: None,
    };
    db.write(move |c| store::create_dialog(c, &dialog)).await?;

    let started = Instant::now();
    let request = StartRequest {
        dialog_id: dialog_id.clone(),
        start: StartKind::Send {
            message: UserMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content: args.prompt.clone(),
                meta: Some(
                    json!({ "mode": settings.mode.clone().unwrap_or_else(|| "agent".into()) }),
                ),
            },
        },
        profile: settings.profile.clone(),
        options: settings.options.clone(),
        mode: settings.mode.clone(),
        context_mode: settings.context_mode.clone(),
        root: Some(project.to_string_lossy().into_owned()),
        current_user_content: None,
        local_date: None,
        config: Some({
            // The policy's commands refused are refused as the run asks for
            // them, whether or not the settings have commands approved.
            let mut config = settings.config.clone();
            config
                .permissions
                .denied_commands
                .extend(policy.denied_commands());
            config
        }),
        isolate: Some(false),
    };
    agent_run::agent_start(
        handle.clone(),
        handle.state::<AgentState>(),
        handle.state::<DbState>(),
        request,
    )
    .await?;

    // Answer what it asks until it ends (or the time runs out).
    let mut approvals: Vec<Value> = Vec::new();
    let mut questions = 0usize;
    let deadline = tokio::time::Instant::now() + args.timeout;
    // The token cap: the run log's replies, read as they're written.
    let log_file = log_dir.join("agent-log.jsonl");
    let mut log_offset = std::fs::metadata(&log_file).map_or(0, |m| m.len());
    let mut children: Vec<String> = Vec::new();
    let mut watched = Totals::default();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let outcome = loop {
        let next = tokio::select! {
            e = tokio::time::timeout_at(deadline, rx.recv()) => e,
            _ = tick.tick(), if args.max_tokens.is_some() => {
                for line in new_log_lines(&log_file, &mut log_offset) {
                    let of_run = line["dialogId"] == dialog_id.as_str()
                        || children.iter().any(|c| line["dialogId"] == c.as_str());
                    if of_run {
                        watched.add(&line, &dialog_id);
                    }
                }
                // The provider doesn't report usage: the cap can't be kept.
                let no_usage = watched.missing_usage > 0;
                if no_usage || args.max_tokens.is_some_and(|max| watched.tokens() > max) {
                    agent_run::agent_stop(handle.clone(), handle.state(), dialog_id.clone());
                    let _ = tokio::time::timeout(Duration::from_secs(10), async {
                        while let Some(e) = rx.recv().await {
                            if e["type"] == "runEnded" && e["dialogId"] == dialog_id.as_str() {
                                break;
                            }
                        }
                    })
                    .await;
                    break if no_usage { "no-usage" } else { "budget" }.to_string();
                }
                continue;
            }
        };
        let event = match next {
            Ok(Some(e)) => e,
            Ok(None) => break "error".to_string(),
            Err(_) => {
                agent_run::agent_stop(handle.clone(), handle.state(), dialog_id.clone());
                // Let it end and store what it has.
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(e) = rx.recv().await {
                        if e["type"] == "runEnded" && e["dialogId"] == dialog_id.as_str() {
                            break;
                        }
                    }
                })
                .await;
                break "timeout".to_string();
            }
        };
        match event["type"].as_str() {
            Some("subagentStarted") if event["dialogId"] == dialog_id.as_str() => {
                if let Some(c) = event["childDialogId"].as_str() {
                    children.push(c.to_string());
                }
            }
            Some("approvalRequested") => {
                let r = &event["request"];
                let (chat, call) = (
                    r["dialogId"].as_str().unwrap_or("").to_string(),
                    r["toolCallId"].as_str().unwrap_or("").to_string(),
                );
                let tool = r["toolName"].as_str().unwrap_or("");
                let summary = r["summary"].as_str().unwrap_or("");
                let decision = policy.decide(tool, summary, r["command"].as_str());
                approvals.push(json!({
                    "dialogId": chat,
                    "tool": tool,
                    "summary": summary,
                    "decision": match decision { Decision::Allow => "allow", Decision::Deny => "deny" },
                }));
                agent_run::agent_resolve_approval(
                    handle.clone(),
                    handle.state(),
                    chat,
                    call,
                    match decision {
                        Decision::Allow => ApprovalDecision::Allow,
                        Decision::Deny => ApprovalDecision::Deny,
                    },
                );
            }
            Some("questionsAsked") => {
                let r = &event["request"];
                questions += 1;
                agent_run::agent_answer_questions(
                    handle.clone(),
                    handle.state(),
                    r["dialogId"].as_str().unwrap_or("").to_string(),
                    r["toolCallId"].as_str().unwrap_or("").to_string(),
                    policy.answer(&r["questions"]),
                );
            }
            Some("runEnded") if event["dialogId"] == dialog_id.as_str() => {
                break event["outcome"].as_str().unwrap_or("").to_string();
            }
            _ => {}
        }
    };
    let ms = started.elapsed().as_millis() as u64;
    // The log is written off the run's threads: give the last lines a moment.
    tokio::time::sleep(Duration::from_millis(300)).await;
    crate::project_profile::set_agent_log_dir(None);

    let id = dialog_id.clone();
    let messages = db
        .read(move |c| store::get_messages(c, &id, None, 100_000))
        .await?
        .messages;
    let report = report(
        &dialog_id,
        &outcome,
        ms,
        &settings,
        &project,
        &messages,
        approvals,
        questions,
        &log_dir.join("agent-log.jsonl"),
    );
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&args.out, text).map_err(|e| format!("{}: {e}", args.out.display()))?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn report(
    dialog_id: &str,
    outcome: &str,
    ms: u64,
    settings: &Settings,
    project: &Path,
    messages: &[store::MessageRow],
    approvals: Vec<Value>,
    questions: usize,
    log: &Path,
) -> Value {
    let meta = |key: &str| -> Vec<Value> {
        messages
            .iter()
            .filter_map(|m| m.meta.as_ref()?.get(key).cloned())
            .collect()
    };
    let tool_calls: usize = messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .map(Vec::len)
        .sum();
    let mut changed: Vec<String> = Vec::new();
    for call in messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
    {
        if call.name == "write" {
            if let Some(p) = agent_run::permissions::str_arg(&call.arguments, "path") {
                if !changed.contains(&p) {
                    changed.push(p);
                }
            }
        }
    }
    let subagents = meta("subagent");
    let tokens: u64 = subagents
        .iter()
        .chain(meta("review").iter())
        .filter_map(|s| {
            let u = s.get("usage")?;
            Some(u["inputTokens"].as_u64()? + u["outputTokens"].as_u64()?)
        })
        .sum();
    let mut totals = Totals::default();
    let final_message = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .map(|m| m.content.clone());
    // This run's lines only: other runs in the process may share the file.
    let run_log: Vec<Value> = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| {
            l["dialogId"] == dialog_id
                || l["parentDialogId"] == dialog_id
                || subagents
                    .iter()
                    .any(|s| s["childDialogId"] == l["dialogId"] && !l["dialogId"].is_null())
        })
        .collect();
    for line in &run_log {
        totals.add(line, dialog_id);
    }
    let (cost, cost_source) = totals.cost(settings.price);
    json!({
        "outcome": outcome,
        "ms": ms,
        "dialogId": dialog_id,
        "project": project,
        "model": settings.profile.model,
        "mode": settings.mode.clone().unwrap_or_else(|| "agent".into()),
        "messages": messages.len(),
        "toolCalls": tool_calls,
        "filesWritten": changed,
        "verification": meta("verification"),
        "reviews": meta("review"),
        "subagents": subagents,
        "subagentTokens": tokens,
        "usage": {
            "leadInputTokens": totals.lead_in,
            "leadOutputTokens": totals.lead_out,
            "subagentInputTokens": totals.sub_in,
            "subagentOutputTokens": totals.sub_out,
            "totalTokens": totals.tokens(),
            "peakLeadContext": totals.peak_lead_context,
            "replies": totals.replies,
            "repliesWithoutUsage": totals.missing_usage,
        },
        "costUsd": cost,
        "costSource": cost_source,
        "interventions": approvals.len() + questions,
        "approvals": approvals,
        "questions": questions,
        "finalMessage": final_message,
        "runLog": run_log,
    })
}

/// The binary's entry: parses the arguments, runs, prints where the report
/// went. The exit code: 0 when the run ended (whatever its outcome; the
/// report says), 1 when it couldn't run, 2 for bad arguments.
pub fn main_cli() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match tauri::async_runtime::block_on(run(&args)) {
        Ok(report) => {
            println!(
                "{}: {} ({} ms). Report: {}",
                report["dialogId"].as_str().unwrap_or(""),
                report["outcome"].as_str().unwrap_or(""),
                report["ms"],
                args.out.display()
            );
            0
        }
        Err(e) => {
            eprintln!("rovik-headless: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn arguments_parse() {
        let a = parse_args(&argv(&[
            "--project",
            "p",
            "--settings",
            "s.json",
            "--prompt",
            "Do it",
            "--out",
            "r.json",
            "--timeout",
            "60",
        ]))
        .unwrap();
        assert_eq!(a.project, PathBuf::from("p"));
        assert_eq!(a.prompt, "Do it");
        assert_eq!(a.timeout, Duration::from_secs(60));
        assert_eq!(a.max_tokens, None);
        assert_eq!(a.policy, None);
        assert!(parse_args(&argv(&["--project", "p"]))
            .unwrap_err()
            .starts_with("--settings is required"));
        assert!(parse_args(&argv(&["--nope"]))
            .unwrap_err()
            .starts_with("unknown argument"));
        assert!(parse_args(&argv(&["--prompt"]))
            .unwrap_err()
            .contains("needs a value"));
    }

    #[test]
    fn replies_without_usage_make_the_cost_unknown() {
        let price = Some(Price {
            input_per_million: 2.0,
            output_per_million: 10.0,
        });
        let mut t = Totals::default();
        t.add(
            &json!({ "event": "reply", "dialogId": "d", "usage": null }),
            "d",
        );
        assert_eq!(t.missing_usage, 1);
        assert_eq!(t.cost(price), (None, "unknown"), "not free");
        // A request that failed or was stopped may have none: not counted.
        let mut t = Totals::default();
        t.add(
            &json!({ "event": "reply", "dialogId": "d", "error": "cancelled" }),
            "d",
        );
        assert_eq!(t.missing_usage, 0);
        assert_eq!(t.cost(price), (Some(0.0), "price"));
    }

    #[test]
    fn the_policys_command_denials_become_denied_commands() {
        let policy: Policy = serde_json::from_value(json!({
            "default": "allow",
            "rules": [
                { "tool": "bash", "contains": "git push", "decision": "deny" },
                { "contains": " sudo ", "decision": "deny" },
                { "tool": "write", "contains": ".env", "decision": "deny" },
                { "tool": "bash", "contains": "ls", "decision": "allow" },
            ],
        }))
        .unwrap();
        assert_eq!(policy.denied_commands(), ["git push", "sudo"]);
    }

    #[test]
    fn totals_and_cost_from_the_run_log() {
        let reply = |dialog: &str, usage: Value| json!({ "event": "reply", "dialogId": dialog, "usage": usage });
        let mut t = Totals::default();
        t.add(
            &reply(
                "lead",
                json!({ "prompt_tokens": 1000, "completion_tokens": 100 }),
            ),
            "lead",
        );
        t.add(
            &reply(
                "lead",
                json!({ "prompt_tokens": 3000, "completion_tokens": 50 }),
            ),
            "lead",
        );
        t.add(
            &reply("kid", json!({ "input_tokens": 500, "output_tokens": 20 })),
            "lead",
        );
        t.add(&json!({ "event": "run", "dialogId": "lead" }), "lead");
        // A request that failed: no usage, and none expected.
        t.add(
            &json!({ "event": "reply", "dialogId": "lead", "usage": null, "error": "server" }),
            "lead",
        );
        assert_eq!(t.tokens(), 4670);
        assert_eq!(t.peak_lead_context, 3000);
        assert_eq!(t.replies, 3);
        assert_eq!(t.cost(None), (None, "unknown"));
        let price = Price {
            input_per_million: 2.0,
            output_per_million: 10.0,
        };
        let (cost, source) = t.cost(Some(price));
        assert_eq!(source, "price");
        assert!((cost.unwrap() - (4500.0 * 2.0 + 170.0 * 10.0) / 1e6).abs() < 1e-12);

        let mut reported = Totals::default();
        reported.add(
            &reply("lead", json!({ "prompt_tokens": 10, "cost": 0.25 })),
            "lead",
        );
        reported.add(
            &reply("lead", json!({ "prompt_tokens": 10, "cost": 0.5 })),
            "lead",
        );
        assert_eq!(reported.cost(Some(price)), (Some(0.75), "provider"));
        assert_eq!(Totals::default().cost(None), (Some(0.0), "none"));
    }

    #[test]
    fn the_policy_decides_by_its_first_matching_rule() {
        let policy: Policy = serde_json::from_value(json!({
            "default": "allow",
            "rules": [
                { "tool": "bash", "contains": "git push", "decision": "deny" },
                { "tool": "write", "decision": "deny" },
            ],
        }))
        .unwrap();
        assert_eq!(
            policy.decide("bash", "Run `ls`", Some("ls")),
            Decision::Allow
        );
        assert_eq!(
            policy.decide("bash", "Run `git push`", Some("git push origin")),
            Decision::Deny
        );
        assert_eq!(policy.decide("write", "Edit a.ts", None), Decision::Deny);
        assert_eq!(
            Policy::default().decide("write", "x", None),
            Decision::Allow
        );
        let qs = json!([
            { "question": "Which?", "options": [{ "label": "A" }, { "label": "B", "recommended": true }] },
            { "question": "Free?", "options": [] },
        ]);
        assert_eq!(
            policy.answer(&qs),
            QuestionsOutcome::Answers {
                answers: vec![
                    QuestionAnswer::Option { label: "B".into() },
                    QuestionAnswer::Agent
                ]
            }
        );
    }
}

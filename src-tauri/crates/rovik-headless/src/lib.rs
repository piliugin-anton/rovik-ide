//! The agent without a window (G00, docs/design/subagents-plan.md): opens a
//! project, starts one run with a settings profile, answers the approvals
//! and questions it asks by a policy file, and writes the outcome and the
//! run log as JSON. For measuring the agent (milestone G) from the command
//! line and in CI:
//!
//!   cargo run -p rovik-headless -- \
//!     --project ./repo --settings settings.json [--policy policy.json] \
//!     --prompt "Fix the failing test" --out report.json [--data ./data] \
//!     [--timeout 1800] [--max-tokens 1500000] \
//!     [--mode agent|plan|autopilot] [--effort balanced|thorough] \
//!     [--steer "TEXT@N"]...
//!
//! `--mode autopilot` (S10, docs/design/autopilot-plan.md) runs the prompt
//! as an Autopilot task, at `--effort` (Balanced unless said): the runner
//! waits for the whole task, not its first run, and the report lists its
//! stages, its outcome and its report's facts. `--mode` wins over the
//! settings file's `mode`.
//!
//! `--steer "TEXT@N"` (D18, docs/design/steering-plan.md) sends TEXT to
//! the run while it works, once the lead chat has N tool results, as the
//! message box would; it can be repeated. The report's `steers` say what
//! became of each: read, handed back, or refused.
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
//! It runs on a `Core` of its own, as the end-to-end tests do: the real
//! database, tools and requests, no webview. The chat isn't isolated (that
//! needs the virtual-files driver).

use rovik_core::agent::OpenAIChatOptions;
pub use rovik_core::agent_run::autopilot::Effort;
pub use rovik_core::agent_run::budget::{usage_of, Price, Totals};
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::plan::{QuestionAnswer, QuestionsOutcome};
use rovik_core::agent_run::{
    self, AgentConfig, ApprovalDecision, ModelProfile, StartKind, StartRequest, SteerReply,
    UserMessage,
};
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::{Core, EventSink, Paths};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

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
    /// The chat mode, over the settings file's (S10: "autopilot").
    pub mode: Option<String>,
    /// An Autopilot task's effort.
    pub effort: Option<Effort>,
    /// Messages sent to the run while it works (`--steer`), in order.
    pub steers: Vec<Steer>,
}

/// A message the runner sends the run while it works (`--steer TEXT@N`).
#[derive(Debug, Clone, PartialEq)]
pub struct Steer {
    pub text: String,
    /// Sent once the lead chat has this many tool results (0: once the
    /// run has started).
    pub after: u32,
}

impl Steer {
    /// `TEXT@N`, split at its last `@`.
    fn parse(value: &str) -> Result<Steer, String> {
        let (text, after) = value
            .rsplit_once('@')
            .ok_or_else(|| format!("--steer takes TEXT@N\n{USAGE}"))?;
        let after = after
            .trim()
            .parse()
            .map_err(|_| format!("--steer takes TEXT@N, N a number of tool results\n{USAGE}"))?;
        if text.trim().is_empty() {
            return Err(format!("--steer needs a text\n{USAGE}"));
        }
        Ok(Steer {
            text: text.to_string(),
            after,
        })
    }
}

pub const USAGE: &str =
    "usage: rovik-headless --project DIR --settings FILE --prompt TEXT --out FILE \
     [--policy FILE] [--data DIR] [--timeout SECONDS] [--max-tokens N] \
     [--mode agent|plan|autopilot] [--effort balanced|thorough] [--steer TEXT@N]...";

pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut project = None;
    let mut settings = None;
    let mut policy = None;
    let mut prompt = None;
    let mut out = None;
    let mut data = None;
    let mut timeout = Duration::from_secs(1800);
    let mut max_tokens = None;
    let mut mode = None;
    let mut effort = None;
    let mut steers = Vec::new();
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
            "--mode" => {
                let m = value()?;
                if !matches!(m.as_str(), "agent" | "plan" | "autopilot") {
                    return Err(format!("--mode is agent, plan or autopilot\n{USAGE}"));
                }
                mode = Some(m);
            }
            "--effort" => {
                effort = Some(match value()?.as_str() {
                    "balanced" => Effort::Balanced,
                    "thorough" => Effort::Thorough,
                    _ => return Err(format!("--effort is balanced or thorough\n{USAGE}")),
                });
            }
            "--steer" => steers.push(Steer::parse(&value()?)?),
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
        mode,
        effort,
        steers,
    })
}

/// A scripted message's way (`--steer`): its id once sent, and the reply.
struct Steering {
    steer: Steer,
    id: String,
    reply: Option<SteerReply>,
}

/// Sends the scripted messages due now: those whose number of tool results
/// the lead chat has, in order, each once.
async fn send_due(core: &Core, dialog_id: &str, steering: &mut [Steering], results: u32) {
    for s in steering
        .iter_mut()
        .filter(|s| s.reply.is_none() && s.steer.after <= results)
    {
        let message = UserMessage {
            id: s.id.clone(),
            content: s.steer.text.clone(),
            meta: None,
            attachments: Vec::new(),
        };
        let reply = agent_run::agent_steer(core, dialog_id.to_string(), message).await;
        s.reply = Some(reply.unwrap_or_else(|reason| SteerReply::Refused { reason }));
    }
}

/// What became of each scripted message, for the report: when it was
/// sent, the reply, and its `delivered` or `returned` line from the log.
fn steers_report(steering: &[Steering], log: &Path) -> Value {
    let lines: Vec<Value> = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["event"] == "steer")
        .collect();
    let line_of = |id: &str, phase: &str| {
        lines
            .iter()
            .find(|l| l["messageId"] == id && l["phase"] == phase)
            .cloned()
            .unwrap_or(Value::Null)
    };
    json!(steering
        .iter()
        .map(|s| json!({
            "text": s.steer.text,
            "after": s.steer.after,
            "reply": s.reply.as_ref().map(|r| match r {
                SteerReply::Queued => json!("queued"),
                SteerReply::Ended => json!("ended"),
                SteerReply::Refused { reason } => json!({ "refused": reason }),
            }),
            "delivered": line_of(&s.id, "delivered"),
            "returned": line_of(&s.id, "returned"),
        }))
        .collect::<Vec<_>>())
}

/// Reads the run log's new lines since `offset` (the log is appended to).
fn new_log_lines(path: &Path, offset: &mut u64) -> Vec<Value> {
    // P6.3: shorter than what was read: rotated (agent-log.jsonl went to
    // agent-log.1.jsonl). Its rest first, then the new file from its start.
    let len = std::fs::metadata(path).map_or(0, |m| m.len());
    let mut lines = Vec::new();
    if len < *offset {
        let rotated = path.with_file_name(
            path.file_name()
                .and_then(|n| n.to_str())
                .map_or("agent-log.1.jsonl".into(), |n| {
                    n.replace(".jsonl", ".1.jsonl")
                }),
        );
        let mut old = *offset;
        lines = whole_lines_from(&rotated, &mut old, true);
        *offset = 0;
    }
    lines.extend(whole_lines_from(path, offset, false));
    lines
}

/// The whole lines from `offset` on; `to_end`: the last one too (a file
/// no one writes any more).
fn whole_lines_from(path: &Path, offset: &mut u64, to_end: bool) -> Vec<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if f.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    // Only whole lines: a line being written stays for next time.
    let whole = if to_end {
        bytes.len()
    } else {
        bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)
    };
    *offset += whole as u64;
    String::from_utf8_lossy(&bytes[..whole])
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Tests: the run under way, its core and lead chat, to act on it as the
/// user would (pick another effort mid-request). Cleared as the run ends.
#[cfg(test)]
pub(crate) static TEST_RUN: std::sync::Mutex<Option<(Core, String)>> = std::sync::Mutex::new(None);

/// The run's events, as the JSON the frontend would get, for `run` to
/// follow.
struct Events(UnboundedSender<Value>);

impl EventSink for Events {
    fn agent(&self, event: &AgentEvent) {
        if let Ok(v) = serde_json::to_value(event) {
            let _ = self.0.send(v);
        }
    }
}

/// Runs once and writes the report; returns it too.
pub async fn run(args: &Args) -> Result<Value, String> {
    // One run at a time: tests find the run under way in TEST_RUN.
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
    // P6.4: a folder it made itself goes once the report is written (the
    // report holds the run's log lines).
    let own_data = args.data.is_none();
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
    #[cfg(test)]
    struct TestRun;
    #[cfg(test)]
    impl Drop for TestRun {
        // Cleared however the run ends (an error return included).
        fn drop(&mut self) {
            *TEST_RUN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
    #[cfg(test)]
    let _test_run = TestRun;

    let (tx, mut rx) = unbounded_channel::<Value>();
    let paths = Paths {
        log_dir: log_dir.clone(),
        ..Paths::new(data.clone(), data.clone())
    };
    let core = Core::open(paths, Arc::new(Events(tx)))?;
    let db = core.db.clone();

    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: dialog_id.clone(),
        title: store::char_prefix(&args.prompt, 60).to_string(),
        created_at: store::now_ms(),
        updated_at: store::now_ms(),
        project_id: None,
    };
    db.write(move |c| store::create_dialog(c, &dialog)).await?;

    // S10: the command line's mode over the settings file's.
    if let Some(mode) = &args.mode {
        settings.mode = Some(mode.clone());
    }
    let autopilot = settings.mode.as_deref() == Some("autopilot");
    if autopilot {
        settings.config.autopilot.effort = args.effort.unwrap_or_default();
    } else if args.effort.is_some() {
        return Err(format!("--effort is for --mode autopilot\n{USAGE}"));
    }
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
                attachments: Vec::new(),
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
        project_settings: None,
    };
    #[cfg(test)]
    {
        *TEST_RUN.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((core.clone(), dialog_id.clone()));
    }
    agent_run::agent_start(&core, request).await?;
    // The scripted messages, and the lead chat's tool results so far.
    let mut steering: Vec<Steering> = args
        .steers
        .iter()
        .map(|steer| Steering {
            steer: steer.clone(),
            id: uuid::Uuid::new_v4().to_string(),
            reply: None,
        })
        .collect();
    let mut tool_results = 0u32;
    // The run started before its first event is read: those due at 0 go now.
    send_due(&core, &dialog_id, &mut steering, tool_results).await;

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
    // S10: an Autopilot task is over when its stages are, whatever runs
    // it took (the driver may record the last ones without a run).
    let mut task_tick = tokio::time::interval(Duration::from_millis(250));
    let outcome = loop {
        let next = tokio::select! {
            e = tokio::time::timeout_at(deadline, rx.recv()) => e,
            _ = task_tick.tick(), if autopilot => {
                if let Some(outcome) = task_over(&core, &dialog_id).await {
                    break outcome;
                }
                continue;
            }
            _ = tick.tick(), if args.max_tokens.is_some() => {
                for line in new_log_lines(&log_file, &mut log_offset) {
                    if of_run(&line, &dialog_id, &children) {
                        watched.add(&line, &dialog_id);
                    }
                }
                // The provider doesn't report usage: the cap can't be kept.
                let no_usage = watched.missing_usage > 0;
                if no_usage || args.max_tokens.is_some_and(|max| watched.tokens() > max) {
                    stop_and_wait(&core, &mut rx, &dialog_id).await;
                    break if no_usage { "no-usage" } else { "budget" }.to_string();
                }
                continue;
            }
        };
        let event = match next {
            Ok(Some(e)) => e,
            Ok(None) => break "error".to_string(),
            Err(_) => {
                stop_and_wait(&core, &mut rx, &dialog_id).await;
                break "timeout".to_string();
            }
        };
        match event["type"].as_str() {
            Some("messagesAdded") if event["dialogId"] == dialog_id.as_str() => {
                let results = event["messages"]
                    .as_array()
                    .map_or(0, |m| m.iter().filter(|m| m["role"] == "tool").count());
                if results > 0 {
                    tool_results += results as u32;
                    send_due(&core, &dialog_id, &mut steering, tool_results).await;
                }
            }
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
                    &core,
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
                    &core,
                    r["dialogId"].as_str().unwrap_or("").to_string(),
                    r["toolCallId"].as_str().unwrap_or("").to_string(),
                    policy.answer(&r["questions"]),
                );
            }
            // An Autopilot task goes on after a stage's run: its end is
            // the task's (`task_over`).
            Some("runEnded") if event["dialogId"] == dialog_id.as_str() && !autopilot => {
                break event["outcome"].as_str().unwrap_or("").to_string();
            }
            _ => {}
        }
    };
    let ms = started.elapsed().as_millis() as u64;
    // P6.4: the log is written off the run's threads: wait for the run's own
    // last line (bounded: a run that never started writes none).
    let wait_until = Instant::now() + Duration::from_secs(5);
    while !has_run_line(&log_dir.join("agent-log.jsonl"), &dialog_id) && Instant::now() < wait_until
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let id = dialog_id.clone();
    let messages = db
        .read(move |c| store::get_messages(c, &id, None, 100_000))
        .await?
        .messages;
    let mut report = report(
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
    if !steering.is_empty() {
        report["steers"] = steers_report(&steering, &log_dir.join("agent-log.jsonl"));
    }
    if autopilot {
        let stages = agent_run::autopilot::task_stages(&core, &dialog_id)
            .await?
            .unwrap_or_default();
        report["effort"] = json!(settings.config.autopilot.effort);
        report["stages"] = json!(stages
            .iter()
            .map(|r| json!({
                "name": r.stage.name,
                "status": r.stage.status,
                "round": r.stage.round,
                "outcome": r.stage.outcome,
                "summary": r.stage.summary,
                "files": r.stage.files,
                "spent": r.stage.spent,
            }))
            .collect::<Vec<_>>());
        report["report"] = messages
            .iter()
            .rev()
            .find_map(|m| m.meta.as_ref()?.get("report").cloned())
            .unwrap_or(Value::Null);
    }
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&args.out, text).map_err(|e| format!("{}: {e}", args.out.display()))?;
    // Closed first: Windows won't remove an open database's folder. The
    // core holds its copy of the database (round 3), so it goes before the
    // database can be the last handle and close; in tests, so does the
    // copy of the core TEST_RUN keeps.
    drop(core);
    #[cfg(test)]
    drop(_test_run);
    // The run's tasks let go of theirs as they wind down.
    for _ in 0..40 {
        if db.handles() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if let Err(e) = db.close() {
        eprintln!("rovik-headless: {e}");
    }
    if own_data {
        let _ = std::fs::remove_dir_all(&data);
    }
    Ok(report)
}

/// S10: an Autopilot task's outcome once it's over: its stages are done
/// and nothing of the chat runs (the delivery's report included). None:
/// it goes on.
async fn task_over(core: &Core, dialog_id: &str) -> Option<String> {
    if core.agents.is_running(dialog_id) {
        return None;
    }
    if agent_run::autopilot::unfinished(core, dialog_id)
        .await
        .ok()?
    {
        return None;
    }
    let stages = agent_run::autopilot::task_stages(core, dialog_id)
        .await
        .ok()??;
    let last = stages.last()?;
    Some(last.stage.outcome.clone().unwrap_or_else(|| "done".into()))
}

/// Whether the run's `"event":"run"` line (written as it ends) is in the log.
fn has_run_line(log: &Path, dialog_id: &str) -> bool {
    std::fs::read_to_string(log).is_ok_and(|text| {
        text.lines().any(|l| {
            l.contains("\"event\":\"run\"")
                && serde_json::from_str::<Value>(l)
                    .is_ok_and(|v| v["event"] == "run" && v["dialogId"] == dialog_id)
        })
    })
}

/// Stops the run and lets it end and store what it has: its `runEnded`,
/// or ten seconds.
async fn stop_and_wait(core: &Core, rx: &mut UnboundedReceiver<Value>, dialog_id: &str) {
    agent_run::agent_stop(core, dialog_id.to_string()).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = rx.recv().await {
            if e["type"] == "runEnded" && e["dialogId"] == dialog_id {
                break;
            }
        }
    })
    .await;
}

/// G02 (autopilot-plan): what the run's `tool` lines and its lead's
/// `verify` lines say, for the report.
#[derive(Debug, Default, PartialEq)]
pub struct ToolCounts {
    /// Its tool calls, its subagents' included.
    pub calls: u64,
    /// The ones that failed by the model's doing: a Stop's, a closed app's
    /// and a budget's left out (`tools::CUT_CLASSES`).
    pub errors: u64,
    /// Those by class (`tools::error_class`).
    pub classes: std::collections::BTreeMap<String, u64>,
    /// Edits whose old text didn't pick one place in the file.
    pub edit_misses: u64,
    /// The checks' rounds the lead's runs went through.
    pub verify_rounds: u64,
}

pub fn tool_counts(run_log: &[Value], dialog_id: &str) -> ToolCounts {
    use rovik_core::agent_run::tools::{CUT_CLASSES, EDIT_MISS_CLASSES};
    let mut counts = ToolCounts::default();
    for line in run_log {
        match line["event"].as_str() {
            Some("tool") => {
                counts.calls += 1;
                if line["failed"] != true {
                    continue;
                }
                let class = line["error"].as_str().unwrap_or("other");
                if CUT_CLASSES.contains(&class) {
                    continue;
                }
                counts.errors += 1;
                *counts.classes.entry(class.to_string()).or_default() += 1;
                if EDIT_MISS_CLASSES.contains(&class) {
                    counts.edit_misses += 1;
                }
            }
            Some("verify") if line["dialogId"] == dialog_id => counts.verify_rounds += 1,
            _ => {}
        }
    }
    counts
}

/// Whether a run log line is this run's: the lead's (`dialog_id`) or one of
/// its subagents' (`children`: the ones seen starting). P6.4: a subagent's
/// line names its lead, so it counts even before its start event is seen.
fn of_run(line: &Value, dialog_id: &str, children: &[String]) -> bool {
    line["dialogId"] == dialog_id
        || line["parentDialogId"] == dialog_id
        || children.iter().any(|c| line["dialogId"] == c.as_str())
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
    let children: Vec<String> = subagents
        .iter()
        .filter_map(|s| s["childDialogId"].as_str().map(str::to_string))
        .collect();
    let run_log: Vec<Value> = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| of_run(l, dialog_id, &children))
        .collect();
    for line in &run_log {
        totals.add(line, dialog_id);
    }
    // Without the run's own line the log is short (lost or unwritable):
    // what's in it isn't all the run used, so its cost isn't known.
    let whole = run_log
        .iter()
        .any(|l| l["event"] == "run" && l["dialogId"] == dialog_id);
    let (cost, cost_source) = if whole {
        totals.cost(settings.price)
    } else {
        (None, "unknown")
    };
    let tools = tool_counts(&run_log, dialog_id);
    json!({
        "outcome": outcome,
        "ms": ms,
        "dialogId": dialog_id,
        "project": project,
        "model": settings.profile.model,
        "mode": settings.mode.clone().unwrap_or_else(|| "agent".into()),
        "messages": messages.len(),
        "toolCalls": tool_calls,
        // G02: from the run log's tool and verify lines.
        "allToolCalls": tools.calls,
        "toolErrors": tools.errors,
        "toolErrorClasses": tools.classes,
        "editMisses": tools.edit_misses,
        "verifyRounds": tools.verify_rounds,
        // An Autopilot task's, set by `run`.
        "stages": Value::Null,
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
            "leadCachedTokens": totals.lead_cached,
            "subagentCachedTokens": totals.sub_cached,
            "cacheHitPercent": totals.cache_hit_percent(),
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
    match rovik_core::rt::block_on(run(&args)) {
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
mod e2e_tests;

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
        assert_eq!((a.mode.clone(), a.effort), (None, None));
        assert!(parse_args(&argv(&["--project", "p"]))
            .unwrap_err()
            .starts_with("--settings is required"));
        assert!(parse_args(&argv(&["--nope"]))
            .unwrap_err()
            .starts_with("unknown argument"));
        assert!(parse_args(&argv(&["--prompt"]))
            .unwrap_err()
            .contains("needs a value"));
        // S10: an Autopilot task, at an effort.
        let base = [
            "--project",
            "p",
            "--settings",
            "s.json",
            "--prompt",
            "Do it",
            "--out",
            "r.json",
        ];
        let with = |more: &[&str]| parse_args(&argv(&[&base[..], more].concat()));
        let a = with(&["--mode", "autopilot", "--effort", "thorough"]).unwrap();
        assert_eq!(
            (a.mode.as_deref(), a.effort),
            (Some("autopilot"), Some(Effort::Thorough))
        );
        assert!(with(&["--mode", "yolo"])
            .unwrap_err()
            .contains("agent, plan or autopilot"));
        assert!(with(&["--effort", "max"])
            .unwrap_err()
            .contains("balanced or thorough"));
        // D18: scripted messages, split at their last `@`, in order.
        let a = with(&["--steer", "use @client@2", "--steer", "stop there@0"]).unwrap();
        assert_eq!(
            a.steers,
            [
                Steer {
                    text: "use @client".into(),
                    after: 2
                },
                Steer {
                    text: "stop there".into(),
                    after: 0
                },
            ]
        );
        assert!(with(&["--steer", "no count"])
            .unwrap_err()
            .contains("TEXT@N"));
        assert!(with(&["--steer", "@3"])
            .unwrap_err()
            .contains("needs a text"));
        assert!(with(&[]).unwrap().steers.is_empty());
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
        // P6.2: usage with no token counts in it is no usage.
        for empty in [
            json!({}),
            json!({ "total_tokens": 9 }),
            json!({ "cost": 0.1 }),
        ] {
            let mut t = Totals::default();
            t.add(
                &json!({ "event": "reply", "dialogId": "d", "usage": empty }),
                "d",
            );
            assert_eq!((t.missing_usage, t.replies), (1, 0), "{empty}");
        }
        // An asked-again request that reported usage before it was cut: counted.
        let mut t = Totals::default();
        t.add(
            &json!({ "event": "reply", "dialogId": "d", "error": "asked-again",
                     "usage": { "prompt_tokens": 700, "completion_tokens": 0 } }),
            "d",
        );
        assert_eq!((t.tokens(), t.missing_usage), (700, 0));
        // A request that failed or was stopped may have none: not counted.
        let mut t = Totals::default();
        t.add(
            &json!({ "event": "reply", "dialogId": "d", "error": "cancelled" }),
            "d",
        );
        assert_eq!(t.missing_usage, 0);
        assert_eq!(t.cost(price), (Some(0.0), "price"));
    }

    /// Round 2, P6.2: the usage shapes providers send, each pinned:
    /// (tokens, replies, missing).
    #[test]
    fn usage_shapes() {
        for (usage, want) in [
            (json!({ "prompt_tokens": 10 }), (10, 1, 0)),
            (json!({ "input_tokens": 5, "output_tokens": 7 }), (12, 1, 0)),
            (json!({ "total_tokens": 9 }), (0, 0, 1)),
            (json!({}), (0, 0, 1)),
            (
                json!({ "prompt_tokens": 10, "completion_tokens": 2, "prompt_tokens_details": { "cached_tokens": 8 } }),
                (12, 1, 0),
            ),
            // Floats from a proxy: whole numbers count, fractions don't.
            (
                json!({ "prompt_tokens": 10.0, "completion_tokens": 3.0 }),
                (13, 1, 0),
            ),
            (json!({ "prompt_tokens": 10.5 }), (0, 0, 1)),
        ] {
            let mut t = Totals::default();
            t.add(
                &json!({ "event": "reply", "dialogId": "d", "usage": usage }),
                "d",
            );
            assert_eq!((t.tokens(), t.replies, t.missing_usage), want, "{usage}");
        }
        // Absent: missing, unless the request failed.
        let mut t = Totals::default();
        t.add(&json!({ "event": "reply", "dialogId": "d" }), "d");
        assert_eq!((t.replies, t.missing_usage), (0, 1));
        let mut t = Totals::default();
        t.add(
            &json!({ "event": "reply", "dialogId": "d", "error": "x" }),
            "d",
        );
        assert_eq!((t.replies, t.missing_usage), (0, 0));
    }

    /// G02: the report's counts from the run log: every tool line is a
    /// call; a failure the run cut (Stop, a closed app, a budget) isn't the
    /// model's error; an edit miss is one whose old text didn't pick one
    /// place; the checks' rounds are the lead's.
    #[test]
    fn tool_counts_from_the_run_log() {
        let tool = |failed: bool, error: Option<&str>| json!({ "event": "tool", "dialogId": "lead", "tool": "write", "failed": failed, "error": error });
        let log = vec![
            tool(false, None),
            tool(true, Some("old_text not found")),
            tool(true, Some("old_text not unique")),
            tool(true, Some("no such file")),
            tool(true, Some("stopped")),
            tool(true, Some("not run")),
            json!({ "event": "tool", "dialogId": "kid", "parentDialogId": "lead", "tool": "bash", "failed": true, "error": "denied" }),
            json!({ "event": "verify", "dialogId": "lead", "round": 1 }),
            json!({ "event": "verify", "dialogId": "lead", "round": 2 }),
            json!({ "event": "verify", "dialogId": "kid", "round": 1 }),
            json!({ "event": "reply", "dialogId": "lead" }),
        ];
        let counts = tool_counts(&log, "lead");
        assert_eq!(
            counts,
            ToolCounts {
                calls: 7,
                errors: 4,
                classes: [
                    ("denied".to_string(), 1),
                    ("no such file".to_string(), 1),
                    ("old_text not found".to_string(), 1),
                    ("old_text not unique".to_string(), 1),
                ]
                .into_iter()
                .collect(),
                edit_misses: 2,
                verify_rounds: 2,
            }
        );
        assert_eq!(tool_counts(&[], "lead"), ToolCounts::default());
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
    fn the_log_watcher_reads_whole_lines_and_follows_a_rotation() {
        let dir = std::env::temp_dir().join(format!("headless-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("agent-log.jsonl");
        std::fs::write(&log, "{\"n\":1}\n{\"n\":2}\n{\"n\":").unwrap();
        let mut offset = 0;
        let got = new_log_lines(&log, &mut offset);
        assert_eq!(
            got,
            [json!({ "n": 1 }), json!({ "n": 2 })],
            "the partial line waits"
        );
        // It's finished, then the log rotates and a new line goes in the new file.
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"3}\n{\"n\":4}\n").unwrap();
        drop(f);
        std::fs::rename(&log, dir.join("agent-log.1.jsonl")).unwrap();
        std::fs::write(&log, "{\"n\":5}\n").unwrap();
        let got = new_log_lines(&log, &mut offset);
        assert_eq!(
            got,
            [json!({ "n": 3 }), json!({ "n": 4 }), json!({ "n": 5 })]
        );
        assert_eq!(offset, 8);
        let _ = std::fs::remove_dir_all(&dir);
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
                json!({ "prompt_tokens": 3000, "completion_tokens": 50, "prompt_tokens_details": { "cached_tokens": 2500 } }),
            ),
            "lead",
        );
        t.add(
            &reply("kid", json!({ "input_tokens": 500, "output_tokens": 20, "input_tokens_details": { "cached_tokens": 100 } })),
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
        // What the provider served from its cache, by who asked.
        assert_eq!((t.lead_cached, t.sub_cached), (2500, 100));
        assert_eq!(t.cache_hit_percent(), Some(62), "2500 of 4000");
        assert_eq!(Totals::default().cache_hit_percent(), None);
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

    /// Round 2 (section 4.2): the lead's peak context is its largest
    /// request; a subagent's larger ones, before or after, aren't the lead's.
    #[test]
    fn peak_lead_context_only_from_lead_lines() {
        let reply = |dialog: &str, usage: Value| json!({ "event": "reply", "dialogId": dialog, "usage": usage });
        let mut t = Totals::default();
        t.add(
            &reply(
                "kid",
                json!({ "prompt_tokens": 600_000, "completion_tokens": 10 }),
            ),
            "lead",
        );
        assert_eq!(t.peak_lead_context, 0, "no lead line yet");
        t.add(
            &reply(
                "lead",
                json!({ "prompt_tokens": 2_000, "completion_tokens": 5 }),
            ),
            "lead",
        );
        t.add(
            &reply("lead", json!({ "input_tokens": 1_000, "output_tokens": 5 })),
            "lead",
        );
        t.add(
            &reply(
                "kid",
                json!({ "input_tokens": 900_000, "output_tokens": 1 }),
            ),
            "lead",
        );
        assert_eq!(t.peak_lead_context, 2_000);
        assert_eq!((t.lead_in, t.sub_in), (3_000, 1_500_000));
    }

    /// Round 2 (section 4.2): `cost()`'s arms in order. Replies without
    /// usage make it unknown whatever else is known; then the provider's
    /// cost; then the price; only with neither is a run without replies
    /// free ("none") and one with replies unknown. Pinned: a price with no
    /// replies is (0, "price"), not "none".
    #[test]
    fn cost_arm_order_with_no_replies_and_no_price() {
        let price = Some(Price {
            input_per_million: 2.0,
            output_per_million: 10.0,
        });
        assert_eq!(Totals::default().cost(None), (Some(0.0), "none"));
        assert_eq!(Totals::default().cost(price), (Some(0.0), "price"));
        let missing = Totals {
            missing_usage: 1,
            ..Totals::default()
        };
        assert_eq!(missing.cost(None), (None, "unknown"), "not none");
        assert_eq!(missing.cost(price), (None, "unknown"), "not price");
        let reported = Totals {
            lead_in: 10,
            replies: 1,
            reported_cost: Some(0.5),
            ..Totals::default()
        };
        assert_eq!(reported.cost(price), (Some(0.5), "provider"));
        assert_eq!(
            Totals {
                missing_usage: 1,
                ..reported
            }
            .cost(price),
            (None, "unknown")
        );
        let counted = Totals {
            lead_in: 10,
            replies: 1,
            ..Totals::default()
        };
        assert_eq!(counted.cost(None), (None, "unknown"));
    }

    /// Round 2 (section 4.2), P6.4: a subagent's line is the run's by its
    /// `parentDialogId` before its `subagentStarted` is seen; one naming no
    /// parent counts once the child is known; another run's never do.
    #[test]
    fn of_run_takes_a_childs_line_by_its_parent_before_it_starts() {
        let none: Vec<String> = Vec::new();
        let known = vec!["kid".to_string()];
        let lead = json!({ "event": "reply", "dialogId": "lead" });
        let child = json!({ "event": "reply", "dialogId": "kid", "parentDialogId": "lead" });
        let bare = json!({ "event": "reply", "dialogId": "kid" });
        assert!(of_run(&lead, "lead", &none));
        assert!(of_run(&child, "lead", &none), "before subagentStarted");
        assert!(!of_run(&bare, "lead", &none));
        assert!(of_run(&bare, "lead", &known));
        for other in [
            json!({ "event": "reply", "dialogId": "other" }),
            json!({ "event": "reply", "dialogId": "kid2", "parentDialogId": "other" }),
            json!({ "event": "run" }),
        ] {
            assert!(!of_run(&other, "lead", &known), "{other}");
        }
    }

    /// Round 2 (section 4.2), P6.4: a run that fails early returns the error
    /// and writes no report: a missing `--project`, and a database that
    /// can't open (its `attachments` folder is a file).
    #[tokio::test]
    async fn an_early_error_writes_no_report() {
        let dir = std::env::temp_dir().join(format!("headless-early-{}", uuid::Uuid::new_v4()));
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(
            &settings,
            json!({ "profile": { "id": "p::m", "label": "P/m", "model": "m", "baseUrl": "http://127.0.0.1:9/v1" } })
                .to_string(),
        )
        .unwrap();
        let args = |project: PathBuf| Args {
            project,
            settings: settings.clone(),
            policy: None,
            prompt: "Hi".into(),
            out: dir.join("report.json"),
            data: Some(data.clone()),
            timeout: Duration::from_secs(5),
            max_tokens: None,
            mode: None,
            effort: None,
            steers: Vec::new(),
        };
        let err = run(&args(dir.join("missing"))).await.unwrap_err();
        assert!(err.contains("missing"), "{err}");
        std::fs::write(data.join("attachments"), "").unwrap();
        assert!(run(&args(dir.clone())).await.is_err());
        assert!(!dir.join("report.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Round 2, P6.3: the rename seen before the new file exists: the rest
    /// of agent-log.1.jsonl, offset 0, and the .1 file isn't read again
    /// once the new file comes.
    #[test]
    fn the_log_watcher_between_the_rename_and_the_new_file() {
        let dir = std::env::temp_dir().join(format!("headless-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("agent-log.jsonl");
        std::fs::write(&log, "{\"n\":1}\n").unwrap();
        let mut offset = 0;
        assert_eq!(new_log_lines(&log, &mut offset), [json!({ "n": 1 })]);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"{\"n\":2}\n").unwrap();
        drop(f);
        std::fs::rename(&log, dir.join("agent-log.1.jsonl")).unwrap();
        // No agent-log.jsonl yet: the old file's rest, from the start next time.
        assert_eq!(new_log_lines(&log, &mut offset), [json!({ "n": 2 })]);
        assert_eq!(offset, 0);
        assert!(
            new_log_lines(&log, &mut offset).is_empty(),
            "nothing read twice"
        );
        assert_eq!(offset, 0);
        std::fs::write(&log, "{\"n\":3}\n").unwrap();
        assert_eq!(new_log_lines(&log, &mut offset), [json!({ "n": 3 })]);
        assert_eq!(offset, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

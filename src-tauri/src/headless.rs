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
use crate::agent_run::log::token_count;
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
    let n = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| token_count(u.get(*k)?))
            .unwrap_or(0)
    };
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
        // P6.2: an object without token counts reports nothing either.
        let counted = |u: &&Value| {
            [
                "prompt_tokens",
                "input_tokens",
                "completion_tokens",
                "output_tokens",
            ]
            .iter()
            .any(|k| u.get(*k).and_then(token_count).is_some())
        };
        let Some(u) = line.get("usage").filter(|u| u.is_object()).filter(counted) else {
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

/// Tests: the run under way, its app and lead chat, to act on it as the
/// user would (pick another effort mid-request). Cleared as the run ends.
#[cfg(test)]
pub(crate) static TEST_RUN: std::sync::Mutex<
    Option<(tauri::AppHandle<tauri::test::MockRuntime>, String)>,
> = std::sync::Mutex::new(None);

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
    crate::project_profile::set_agent_log_dir(Some(log_dir.clone()));
    // P6.4: reset however the run ends (an error return included).
    struct LogDir;
    impl Drop for LogDir {
        fn drop(&mut self) {
            crate::project_profile::set_agent_log_dir(None);
            #[cfg(test)]
            {
                *TEST_RUN.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }
    let log_dir_guard = LogDir;

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
    #[cfg(test)]
    {
        *TEST_RUN.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((handle.clone(), dialog_id.clone()));
    }
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
                    if of_run(&line, &dialog_id, &children) {
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
    // P6.4: the log is written off the run's threads: wait for the run's own
    // last line (bounded: a run that never started writes none).
    let wait_until = Instant::now() + Duration::from_secs(5);
    while !has_run_line(&log_dir.join("agent-log.jsonl"), &dialog_id) && Instant::now() < wait_until
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    drop(log_dir_guard);

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
    drop(app);
    // Closed first: Windows won't remove an open database's folder.
    drop(db);
    if own_data {
        let _ = std::fs::remove_dir_all(&data);
    }
    Ok(report)
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

    /// Round 2 (section 4.2), P6.4: a run that fails early leaves the agent
    /// log's folder as it found it: a missing `--project` fails before it's
    /// set; a database that can't open (its `attachments` folder is a file)
    /// fails after, and the folder is reset.
    #[tokio::test]
    async fn the_log_dir_is_reset_after_an_early_project_error() {
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
        };
        // Other runs may set their own folder meanwhile: only ours is checked.
        let ours = Some(data.join("log"));
        let err = run(&args(dir.join("missing"))).await.unwrap_err();
        assert!(err.contains("missing"), "{err}");
        assert_ne!(crate::project_profile::agent_log_dir(), ours);
        std::fs::write(data.join("attachments"), "").unwrap();
        assert!(run(&args(dir.clone())).await.is_err());
        assert_ne!(crate::project_profile::agent_log_dir(), ours);
        assert!(!dir.join("report.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

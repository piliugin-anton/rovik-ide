//! Starting a run and the settings it follows, and what's going on right
//! now (the core's agent_run). The frontend builds these as
//! src/lib/agentRuns.ts types them.

use crate::autopilot::AutopilotConfig;
use crate::diff::Hunk;
use crate::events::{ApprovalRequest, QuestionsRequest};
use crate::network::HostNetwork;
use crate::permissions::Permissions;
use crate::subagents::{ModelChoice, SubagentConfig};
use serde::{Deserialize, Serialize};

/// A model, resolved by the frontend from the providers in its settings.
/// Its `Debug` leaves the key out, so a request that gets logged can't
/// leak it (plan rule 9).
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    /// `${providerId}::${modelId}`.
    pub id: String,
    /// "Provider/developer/model", for the user.
    pub label: String,
    pub model: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub vision: bool,
    /// The model takes PDF files (the settings' PDF).
    #[serde(default)]
    pub pdf: bool,
    pub context_tokens: Option<usize>,
    #[serde(default)]
    pub no_tools: bool,
    pub first_byte_timeout_ms: Option<u64>,
    /// The API route saved with the model ("chat_completions" or
    /// "responses"), once found (`chat_request::responses`).
    #[serde(default)]
    pub api_route: Option<String>,
}

impl std::fmt::Debug for ModelProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelProfile")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "…" })
            .field("stream", &self.stream)
            .field("vision", &self.vision)
            .field("pdf", &self.pdf)
            .field("context_tokens", &self.context_tokens)
            .field("no_tools", &self.no_tools)
            .field("first_byte_timeout_ms", &self.first_byte_timeout_ms)
            .field("api_route", &self.api_route)
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn level(self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct OpenAIChatOptions {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub reasoning_effort: Option<ReasoningEffort>, // add more as needed: frequency_penalty, presence_penalty, etc.
    /// Which forms of the effort to send (`REASONING_FIELDS`). Absent: the
    /// older default (`reasoning_effort` plus `chat_template_kwargs`), still
    /// used by background requests.
    pub reasoning_formats: Option<Vec<String>>,
}

/// The settings runs follow, sent by the frontend at startup and whenever
/// they change; a running run picks changes up at its next tool call.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConfig {
    pub permissions: Permissions,
    /// The AI reviewer, then its fallbacks (enabled models only).
    pub approval_profiles: Vec<ModelProfile>,
    /// Subagents: off unless the user turns them on.
    pub subagents: SubagentConfig,
    /// How Autopilot's tasks run: read only by chats in Autopilot.
    pub autopilot: AutopilotConfig,
    /// How a host connects for this computer's chats (Settings → Network,
    /// "Cloud hosts"). The app sets it for its hosts, the frontend never
    /// does; a host keeps the proxies' passwords in memory only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<HostNetwork>,
}

impl AgentConfig {
    /// The models it names: the AI reviewer and its fallbacks, and each
    /// subagent kind's.
    pub fn models(&self) -> impl Iterator<Item = &ModelProfile> {
        let kinds = self.subagents.kinds.values().flat_map(|k| &k.models);
        self.approval_profiles
            .iter()
            .chain(kinds.filter_map(|choice| match choice {
                ModelChoice::Profile(model) => Some(model),
                ModelChoice::Chat { .. } => None,
            }))
    }

    pub fn models_mut(&mut self) -> impl Iterator<Item = &mut ModelProfile> {
        let kinds = self
            .subagents
            .kinds
            .values_mut()
            .flat_map(|k| &mut k.models);
        self.approval_profiles
            .iter_mut()
            .chain(kinds.filter_map(|choice| match choice {
                ModelChoice::Profile(model) => Some(model),
                ModelChoice::Chat { .. } => None,
            }))
    }

    /// The configuration with no model's key: what a daemon keeps, and
    /// what the app sends it outside a start (D11).
    pub fn without_keys(&self) -> AgentConfig {
        let mut config = self.clone();
        for model in config.models_mut() {
            model.api_key.clear();
        }
        config
    }
}

/// The user's decision on a tool call waiting for approval.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    /// Some of an edit's changes accepted: the file with only those.
    Partial {
        content: String,
        total: usize,
        rejected: Vec<Hunk>,
    },
}

/// How a run starts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StartKind {
    /// A new message from the user (stored here, first).
    Send { message: UserMessage },
    /// Another try after a failed turn.
    Retry,
    /// Continue an interrupted reply.
    ContinueReply,
    /// Continue a run that paused (a checkpoint or a loop check).
    ContinueRun,
    /// Run the checks of a verification record that waited for approval.
    RunChecks { record_id: String },
    /// A chat whose last tool calls never ran (the app closed meanwhile).
    Resume,
    /// The project's newer changes were merged into these files of the
    /// chat's worktree on its server (6.6, D13): its checks run on the
    /// result first, and a failure goes on as a fix round.
    CheckMerged {
        files: Vec<String>,
        /// What the project changed in each, in `files`' order: its diff
        /// from the chat's landing base, which a fix round shows.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diffs: Vec<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
    /// Images and PDFs attached to the message (chat_store_attachment ids),
    /// in order; its meta names the PDFs (`pdfs`).
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub dialog_id: String,
    pub start: StartKind,
    pub profile: ModelProfile,
    /// The reasoning effort, in the forms the provider takes (src/lib/reasoning.ts).
    pub options: Option<OpenAIChatOptions>,
    /// The mode selected in the chat, for a run whose message didn't record one.
    #[serde(default)]
    pub mode: Option<String>,
    /// "trimming" or "clm".
    #[serde(default)]
    pub context_mode: Option<String>,
    pub root: Option<String>,
    /// The request's user message as sent (with editor context), when it
    /// isn't the stored one.
    pub current_user_content: Option<String>,
    /// Today, as the user's clock has it ("2026-10-04"), for plan file names.
    pub local_date: Option<String>,
    pub config: Option<AgentConfig>,
    /// Work in the chat's overlay (src/overlay). `None`: the app's setting.
    #[serde(default)]
    pub isolate: Option<bool>,
    /// The project's settings as the user saved them (K03): a daemon keeps
    /// them for the folder the run works in, as the app does for its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_settings: Option<serde_json::Value>,
}

/// A running subagent as its block shows it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChildInfo {
    pub kind: String,
    pub goal: String,
    pub model: String,
    /// Milliseconds since the epoch.
    pub started_at: u64,
    pub tool_calls: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunningRun {
    pub dialog_id: String,
    pub run_id: String,
    /// The run takes messages while it works (`AgentSteer`): an Agent-mode
    /// lead's. Absent: false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub steerable: bool,
}

/// What became of a message sent to a run (`AgentSteer`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SteerReply {
    /// It waits for the run's next step, which reads it.
    Queued,
    /// The run is ending, was stopped, or none goes: the message is for
    /// the chat's next run, sent the usual way once this one has ended.
    Ended,
    /// The run takes no messages (Plan mode, an Autopilot task's), or too
    /// many wait already: why, for the user.
    Refused { reason: String },
}

/// A message waiting for its run's next step (`AgentSteer`), for a page
/// loaded meanwhile.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSteer {
    pub dialog_id: String,
    pub message: UserMessage,
    /// When it was sent, in milliseconds since the epoch.
    pub queued_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunningSubagent {
    pub dialog_id: String,
    pub parent_dialog_id: String,
    pub parent_call_id: String,
    /// Its kind, goal, model, start and tool calls (once it's launched).
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub info: Option<ChildInfo>,
}

/// What's going on right now, for a frontend that (re)starts mid-run.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub runs: Vec<RunningRun>,
    pub subagents: Vec<RunningSubagent>,
    pub approvals: Vec<ApprovalRequest>,
    pub questions: Vec<QuestionsRequest>,
    /// The chats whose Autopilot driver is at work (`TaskDriving`). Absent
    /// when none is, as from a daemon before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driving: Vec<String>,
    /// The messages waiting for their run's next step, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steering: Vec<QueuedSteer>,
}

//! Starting a run and the settings it follows, and what's going on right
//! now (the core's agent_run). The frontend builds these as
//! src/lib/agentRuns.ts types them.

use crate::diff::Hunk;
use crate::events::{ApprovalRequest, QuestionsRequest};
use crate::permissions::Permissions;
use crate::subagents::SubagentConfig;
use serde::{Deserialize, Serialize};

/// A model, resolved by the frontend from the providers in its settings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
}

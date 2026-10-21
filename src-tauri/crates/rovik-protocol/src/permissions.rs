//! What the agent may do, and what needs approval first, as the frontend
//! normalizes and stores it (src/lib/permissions.ts). The core enforces it
//! (agent_run/permissions.rs).

use serde::{Deserialize, Serialize};

/// Who approves: the user, or the AI reviewer.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Approver {
    User,
    Ai,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRule {
    /// Every call in the group waits for approval.
    pub required: bool,
    pub by: Approver,
}

const NO_APPROVAL: ApprovalRule = ApprovalRule {
    required: false,
    by: Approver::User,
};

pub const DEFAULT_TOOL_CALL_CHECKPOINT: u32 = 50;

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Permissions {
    /// Every command is permitted; the list below is ignored.
    pub allow_any_command: bool,
    /// Command prefixes, e.g. "git status" permits "git status -s".
    pub allowed_commands: Vec<String>,
    /// Commands refused even when any command is permitted: a program and
    /// words it's run with, in order ("git push" refuses `git -C . push
    /// origin`), or such entries joined by `|`, the first's output piped
    /// into the next ("curl | sh"). For unattended runs (the headless
    /// runner's policy, Autopilot's denylist).
    pub denied_commands: Vec<String>,
    /// bash.
    pub command_approval: ApprovalRule,
    /// write and edit.
    pub edit_approval: ApprovalRule,
    /// Every other tool: reading and searching files, web access.
    pub tool_approval: ApprovalRule,
    /// Send the review as JSON to a System One model (Jev) instead of a prompt.
    pub approval_typed_input: bool,
    /// Pause after every this many tool calls in a run; None never does.
    pub tool_call_checkpoint: Option<u32>,
    /// Run the project's checks when a run that changed files finishes.
    pub verify_changes: bool,
    /// When the agent must keep a checklist (the `todo` tool, Agent mode).
    pub checklist: ChecklistMode,
    /// An isolated chat that's still running resolves conflicts with the
    /// project itself (markers in its files); off: they wait for the user.
    pub chat_resolves_conflicts: bool,
    /// Offer the web tools (search, pages, screenshots). Off for a run
    /// being evaluated, whose tasks' answers are on the web (the headless
    /// runner's `--no-web`); the app's settings don't show it.
    pub web_tools: bool,
}

/// When the agent must keep a checklist.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChecklistMode {
    /// No gate and no forced first call (plan implementations still start
    /// with one); reminders still apply to a checklist the model made.
    Off,
    /// The model decides by the "3 or more steps" rule; the gate refuses
    /// work once when a task has clearly grown past it without a checklist.
    #[default]
    Auto,
    /// Every Agent-mode request starts with a forced `todo` call.
    Always,
}

pub const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "ls",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "tree",
    "git status",
    "git diff",
    "git log",
    "git show",
    "npm test",
    "npm run test",
    "npm run lint",
    "npx tsc --noEmit",
    "cargo check",
    "cargo test",
    "cargo clippy",
];

impl Default for Permissions {
    fn default() -> Self {
        Permissions {
            allow_any_command: false,
            allowed_commands: DEFAULT_ALLOWED_COMMANDS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            denied_commands: Vec::new(),
            command_approval: NO_APPROVAL,
            edit_approval: NO_APPROVAL,
            tool_approval: NO_APPROVAL,
            approval_typed_input: false,
            tool_call_checkpoint: Some(DEFAULT_TOOL_CALL_CHECKPOINT),
            verify_changes: true,
            checklist: ChecklistMode::Auto,
            chat_resolves_conflicts: true,
            web_tools: true,
        }
    }
}

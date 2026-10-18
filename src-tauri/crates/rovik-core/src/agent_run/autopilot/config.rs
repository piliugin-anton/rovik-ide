//! What an Autopilot task's runs are configured with: the subagents its
//! stages need (A07, D17) and the rules of an unattended run (U01, D6).

use crate::agent_run::permissions::{ChecklistMode, Permissions};
use crate::agent_run::subagent::{Kind, SubagentConfig};
use crate::agent_run::AgentConfig;
use rovik_protocol::autopilot::{AutopilotConfig, Effort};
use serde_json::Value;

/// The kinds an Autopilot task's stages need (D17), of those built: the
/// plan reviewer (the plan's review), the change reviewer and the failure
/// triage (the implementation's review and checks), the test writer, the
/// step workers and the acceptance checker; in Thorough also the planners,
/// the judge and the adversarial tester (T01, T02, T04).
pub fn needed_kinds(effort: Effort) -> &'static [Kind] {
    const BALANCED: &[Kind] = &[
        Kind::PlanReviewer,
        Kind::Reviewer,
        Kind::Triage,
        Kind::TestWriter,
        Kind::Worker,
        Kind::Acceptance,
    ];
    const THOROUGH: &[Kind] = &[
        Kind::PlanReviewer,
        Kind::Reviewer,
        Kind::Triage,
        Kind::TestWriter,
        Kind::Worker,
        Kind::Acceptance,
        Kind::Planner,
        Kind::Judge,
        Kind::Breaker,
    ];
    match effort {
        Effort::Balanced => BALANCED,
        Effort::Thorough => THOROUGH,
    }
}

/// The subagent settings an Autopilot task's runs follow (D17): the kinds
/// its stages need are on, each with the models the user's row on the
/// Subagents page gives (the app resolves them: the row's model and
/// fallbacks, the default subagent model, the chat's), or the chat's model
/// when the row sent none. The user's other kinds stay as they are when
/// subagents are on, and are off when they're off: the user's switches
/// still govern what isn't Autopilot's. An ordinary chat's are untouched
/// (`config_for` calls this for Autopilot's runs only).
pub fn subagent_config(user: &SubagentConfig, effort: Effort) -> SubagentConfig {
    let mut config = user.clone();
    if !user.enabled {
        for kind in config.kinds.values_mut() {
            kind.enabled = false;
        }
    }
    config.enabled = true;
    for kind in needed_kinds(effort) {
        config
            .kinds
            .entry(kind.id().to_string())
            .or_default()
            .enabled = true;
    }
    config
}

/// The permissions an Autopilot task's runs and their subagents follow
/// (3.4), whatever the user's say: nothing in them waits for a person (D6).
///
/// - No tool-call checkpoint: a pause nobody continues would never end.
/// - Any command but the denylist's: the user's denied commands and the
///   Autopilot settings' (`DEFAULT_AUTOPILOT_DENYLIST` unless changed).
///   The fixed refusals are where they're checked: a command naming the
///   server's own folder (`execute.rs`), git writes on a server
///   (`server.rs`).
/// - The checks run when files changed, and the checklist rule is Auto.
///
/// The approval rules stay the user's; who answers them is U02's.
pub fn permissions_for(user: &Permissions, config: &AutopilotConfig) -> Permissions {
    let mut denied = user.denied_commands.clone();
    for entry in &config.denied_commands {
        if !denied.iter().any(|d| d.trim() == entry.trim()) {
            denied.push(entry.clone());
        }
    }
    Permissions {
        allow_any_command: true,
        denied_commands: denied,
        tool_call_checkpoint: None,
        verify_changes: true,
        checklist: ChecklistMode::Auto,
        ..user.clone()
    }
}

/// Why an Autopilot run doesn't run a command naming the server's own
/// folder, which always waits for the user's approval elsewhere (D8).
pub const DAEMON_FOLDER_REFUSED: &str = "Error: commands that name the server's own folder (~/.rovik) aren't run in Autopilot: they always need a person's approval, and nobody is there to give it. Do without it.";

/// The configuration an Autopilot task's runs follow: the user's, with
/// the kinds its stages need on (A07) and the rules of an unattended run
/// (U01). The same however often it's applied.
pub fn run_config(user: AgentConfig, effort: Effort) -> AgentConfig {
    AgentConfig {
        subagents: subagent_config(&user.subagents, effort),
        permissions: permissions_for(&user.permissions, &user.autopilot),
        ..user
    }
}

/// The mode of a message sent in Autopilot (`meta.mode`, `StartRequest.mode`).
pub const MODE: &str = "autopilot";

/// What `ask_user` answers in Autopilot (D5): nobody is there.
pub const NO_QUESTIONS: &str = "Error: nobody answers questions in Autopilot. Decide yourself and say what you assumed, under **Assumptions** in the plan's \"## Criteria\".";

/// An Autopilot run's tools: never `ask_user` (D5).
pub fn without_questions(tools: &mut Vec<Value>) {
    tools.retain(|t| t.pointer("/function/name").and_then(Value::as_str) != Some("ask_user"));
}

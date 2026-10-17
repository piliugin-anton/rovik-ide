//! Autopilot's settings (docs/design/autopilot-plan.md, 3.5), sent by the
//! frontend in `AgentConfig::autopilot` (src/lib/autopilotSettings.ts).
//! Autopilot is a chat mode in which one message is a whole task: the core
//! drives it through its stages (agent_run/autopilot.rs) with nobody at the
//! keyboard. Nothing here changes a chat that isn't in Autopilot.

use serde::{Deserialize, Serialize};

/// How much a task spends on independence (D4): Balanced runs each stage
/// once, with the subagents that exist; Thorough adds candidates, a second
/// model for every judgment and an adversarial tester.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Effort {
    #[default]
    Balanced,
    Thorough,
}

impl Effort {
    pub fn id(self) -> &'static str {
        match self {
            Effort::Balanced => "balanced",
            Effort::Thorough => "thorough",
        }
    }
}

/// Where a task stops on its own (D16): after this many hours, or once it
/// has cost this much in the provider's currency. None: no limit.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Budget {
    pub max_hours: Option<f64>,
    pub max_cost: Option<f64>,
}

/// Commands an Autopilot run never runs, whatever the command rules say
/// (U01): each is a program and the words it's run with, or such entries
/// joined by `|` (a download run by a shell), as
/// `Permissions::denied_commands` matches them.
pub const DEFAULT_AUTOPILOT_DENYLIST: &[&str] = &[
    "rm -rf /",
    "git push",
    "git commit --amend",
    "curl | sh",
    "curl | bash",
    "wget | sh",
    "wget | bash",
    "sudo",
    "shutdown",
    "dd",
];

/// The Autopilot section of Settings → Subagents (3.5). The effort shapes
/// the pipeline (`next_stage`); the budget (U04), the prepare stage (S03)
/// and the denylist (U01) are read by the tasks that build them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutopilotConfig {
    pub effort: Effort,
    pub budget: Budget,
    /// Install the dependencies a lockfile implies before the first check
    /// (D19).
    pub prepare: bool,
    pub denied_commands: Vec<String>,
}

impl Default for AutopilotConfig {
    fn default() -> Self {
        AutopilotConfig {
            effort: Effort::Balanced,
            budget: Budget::default(),
            prepare: true,
            denied_commands: DEFAULT_AUTOPILOT_DENYLIST
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

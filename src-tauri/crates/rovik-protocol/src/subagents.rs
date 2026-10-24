//! Subagents' settings, sent by the frontend in `AgentConfig::subagents`
//! (src/lib/subagentSettings.ts). The core reads them with its defaults and
//! bounds (agent_run/subagent.rs, `SubagentSettings`).

use crate::agent::{ModelProfile, OpenAIChatOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A model in a kind's list: a resolved model, or "the chat's model". The
/// profile is boxed so a list of "the chat's model" stays small; serde
/// reads and writes it the same.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum ModelChoice {
    Profile(Box<ModelProfile>),
    Chat { chat: bool },
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct KindConfig {
    pub enabled: bool,
    /// The kind's model, then its fallbacks, resolved by the frontend (its
    /// model → the default subagent model → the chat's model).
    pub models: Vec<ModelChoice>,
    /// The reasoning effort for this kind's requests (K20).
    pub options: Option<OpenAIChatOptions>,
    pub max_tool_calls: Option<u32>,
    pub max_minutes: Option<f64>,
    /// The core's tests only: tools the kind gets beyond its own (a scout
    /// that writes stands in for the writing kinds to come).
    #[cfg(feature = "testing")]
    pub extra_tools: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SubagentConfig {
    /// The master switch (D14): off, no `task` tool and nothing else changes.
    pub enabled: bool,
    /// By kind id ("scout").
    pub kinds: HashMap<String, KindConfig>,
    pub max_at_once: Option<usize>,
    pub max_per_run: Option<usize>,
    /// The change reviewer runs on changes of at least this many lines.
    pub review_min_lines: Option<usize>,
    /// Plans with at least this many steps are carried out by step workers.
    pub worker_min_steps: Option<usize>,
    /// Plan groups run as parallel workers in child overlays (milestone F).
    pub parallel_workers: bool,
    /// The test writer's new tests also run on the code before the change;
    /// one that passes there is flagged (E04).
    pub red_check: bool,
    /// The repair loop fixes causes in different workers' scopes, each
    /// rated high by triage, at the same time in child overlays (R07).
    pub parallel_repair: bool,
    /// Tokens the subagents of one run may use, as their providers report
    /// them; past it, no new subagent starts (S17). None: no limit.
    pub max_run_tokens: Option<u64>,
}

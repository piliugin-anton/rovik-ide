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

/// A "Works on its own" host's settings (3.5, H01): what its Autopilot
/// tasks follow over the computer's own settings, and where their results
/// go. None: as in the computer's settings. The deploy key is a path on
/// the server, never its contents; the notify URL may carry a token, and
/// is never logged whole.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutonomySettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// A limit left empty: as in the computer's settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    pub delivery: Delivery,
    /// Called when a task ends or is blocked (D18).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_url: Option<String>,
}

/// Where a "Works on its own" host's results go (D15).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Delivery {
    /// On the server, for this computer to land, as today.
    #[default]
    KeepOnHost,
    /// Pushed to a branch `rovik/<task>` on the project's remote, with the
    /// deploy key at this path on the server; a pull request opened when
    /// `open_pr` and `gh` is logged in there.
    PushBranch {
        deploy_key: String,
        #[serde(default)]
        open_pr: bool,
    },
}

impl AutonomySettings {
    /// The settings as kept, or why they can't be: a deploy key to push
    /// with, a notify URL that's a web address, a budget's limits above 0
    /// (0 and below: as in the computer's settings, and a budget with no
    /// limit left: none).
    pub fn checked(mut self) -> Result<AutonomySettings, String> {
        if let Delivery::PushBranch { deploy_key, .. } = &mut self.delivery {
            *deploy_key = deploy_key.trim().to_string();
            if deploy_key.is_empty() {
                return Err("Pushing a branch needs the deploy key's path on the server.".into());
            }
            if deploy_key.contains(['\n', '\r', '\0']) {
                return Err("The deploy key's path is one line.".into());
            }
        }
        self.notify_url = self
            .notify_url
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        if let Some(url) = &self.notify_url {
            let web = url.starts_with("https://") || url.starts_with("http://");
            if !web || url.contains(char::is_whitespace) {
                return Err("The notify URL is an http:// or https:// address.".into());
            }
        }
        if let Some(budget) = &mut self.budget {
            let limit = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
            budget.max_hours = limit(budget.max_hours);
            budget.max_cost = limit(budget.max_cost);
        }
        self.budget = self.budget.filter(|b| *b != Budget::default());
        Ok(self)
    }
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

#[cfg(test)]
mod autonomy_tests {
    use super::*;
    use serde_json::json;

    /// H01: the block as the hosts page saves it, and an old host's
    /// settings without it.
    #[test]
    fn autonomy_settings_round_trip_and_default_to_keep_on_the_host() {
        let push = AutonomySettings {
            effort: Some(Effort::Thorough),
            budget: Some(Budget {
                max_hours: Some(3.0),
                max_cost: None,
            }),
            delivery: Delivery::PushBranch {
                deploy_key: "~/.ssh/rovik_deploy".into(),
                open_pr: true,
            },
            notify_url: Some("https://ntfy.sh/rovik-abc".into()),
        };
        let json = serde_json::to_value(&push).unwrap();
        assert_eq!(
            json,
            json!({
                "effort": "thorough",
                "budget": { "maxHours": 3.0, "maxCost": null },
                "delivery": { "kind": "pushBranch", "deployKey": "~/.ssh/rovik_deploy", "openPr": true },
                "notifyUrl": "https://ntfy.sh/rovik-abc",
            })
        );
        assert_eq!(
            serde_json::from_value::<AutonomySettings>(json).unwrap(),
            push
        );
        let none: AutonomySettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(none, AutonomySettings::default());
        assert_eq!(none.delivery, Delivery::KeepOnHost);
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            json!({ "delivery": { "kind": "keepOnHost" } })
        );
    }

    #[test]
    fn autonomy_settings_are_checked_before_they_are_kept() {
        let push = |key: &str| AutonomySettings {
            delivery: Delivery::PushBranch {
                deploy_key: key.into(),
                open_pr: false,
            },
            ..Default::default()
        };
        assert!(push("  ").checked().unwrap_err().contains("deploy key"));
        assert!(push("a\nb").checked().is_err());
        assert_eq!(
            push(" ~/.ssh/k ").checked().unwrap().delivery,
            Delivery::PushBranch {
                deploy_key: "~/.ssh/k".into(),
                open_pr: false
            }
        );
        let notify = |url: &str| AutonomySettings {
            notify_url: Some(url.into()),
            ..Default::default()
        };
        assert!(notify("ftp://x").checked().unwrap_err().contains("http"));
        assert!(notify("https://a b").checked().is_err());
        assert_eq!(notify("  ").checked().unwrap().notify_url, None);
        let budget = AutonomySettings {
            budget: Some(Budget {
                max_hours: Some(0.0),
                max_cost: Some(-2.0),
            }),
            ..Default::default()
        };
        assert_eq!(budget.checked().unwrap().budget, None);
        let hours = AutonomySettings {
            budget: Some(Budget {
                max_hours: Some(2.0),
                max_cost: Some(0.0),
            }),
            ..Default::default()
        };
        assert_eq!(
            hours.checked().unwrap().budget,
            Some(Budget {
                max_hours: Some(2.0),
                max_cost: None
            })
        );
    }
}

//! Autopilot's settings (docs/design/autopilot-plan.md, 3.5), sent by the
//! frontend in `AgentConfig::autopilot` (src/lib/autopilotSettings.ts).
//! Autopilot is a chat mode in which one message is a whole task: the core
//! drives it through its stages (agent_run/autopilot/) with nobody at the
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

/// Where a task stops on its own (D16): after this many hours, once it
/// has cost this much in the provider's currency, or once its requests
/// used this many tokens, as the providers report them (most do, even
/// those that report no cost). None: no limit.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Budget {
    pub max_hours: Option<f64>,
    pub max_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// A "Works on its own" host's settings (3.5, H01): what its Autopilot
/// tasks follow over the computer's own settings, and where their results
/// go. None: as in the computer's settings. The deploy key is a path on
/// the server, never its contents; the notify URL may carry a token, and
/// is never logged whole.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
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

/// What a server that works on its own keeps of a computer's settings
/// (3.5, H02): where its tasks' results go and whom to tell, for runs that
/// go on with that computer away. Sent beside `AgentConfig`
/// (`Request::AutonomyConfigure`) and kept per computer by the daemon; the
/// default, a host that doesn't work on its own: results stay there, and
/// nobody is called.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutonomyConfig {
    pub delivery: Delivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_url: Option<String>,
}

impl AutonomyConfig {
    pub fn is_default(&self) -> bool {
        *self == AutonomyConfig::default()
    }
}

/// What "Test delivery" found (3.5, H05): the branch the server pushed to
/// the remote and deleted again, or why it couldn't; and, when the
/// computer asked for pull requests, whether `gh` can open them there.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DeliveryTest {
    pub remote: String,
    pub branch: String,
    /// Why the push or the delete didn't go: none when both did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Asked for pull requests: whether they'd open, in a line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<String>,
}

/// A chat on a server and its latest Autopilot task (H08), as
/// `rovik-agentd status --json` and the hosts page show it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatTask {
    pub chat: String,
    pub title: String,
    /// The computer that started it there, or moved it there.
    pub computer: String,
    /// The run of it going now, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<String>,
    /// How its last run ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Its latest task; none for a chat that never ran one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskState>,
}

/// Where a task is: its latest stage record, and what its runs spent.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskState {
    /// The task: the id of the message that started it.
    pub id: String,
    /// Its latest stage ("plan" … "deliver"), and that stage's status
    /// ("running", "done", "failed", "skipped"), outcome and line.
    pub stage: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// What its runs spent: their time, and their replies' cost in the
    /// providers' currency, with how many replies reported none, and the
    /// tokens they used.
    pub ms: u64,
    pub cost: f64,
    pub uncosted: u64,
    #[serde(default)]
    pub tokens: u64,
}

/// A URL as a log or a message shows it: its scheme and host, never its
/// path or query, which may carry a token (3.5).
pub fn url_for_logs(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Credentials before the host are a token too.
    let host = host.rsplit('@').next().unwrap_or(host);
    let cut = host.len() < rest.len() || rest.contains('@');
    match (scheme.is_empty(), cut) {
        (true, _) => "(a URL)".into(),
        (false, true) => format!("{scheme}://{host}/…"),
        (false, false) => format!("{scheme}://{host}"),
    }
}

/// Where a server pushes a task's result (D15, H04), from the project's
/// `origin`: an SSH address, which the deploy key opens. An `https://`
/// (or `http://`, `git://`) remote becomes `git@<host>:<path>`, as the
/// forges take it, its credentials and port left behind; an SSH address
/// keeps its user and loses a password; a `file://` URL or a path, a
/// remote on the server itself, stays. None: nothing to push to.
pub fn push_url(origin: &str) -> Option<String> {
    let origin = origin.trim();
    if origin.is_empty() || origin.contains(char::is_whitespace) {
        return None;
    }
    if origin.starts_with("file://") || origin.starts_with('/') {
        return Some(origin.to_string());
    }
    if let Some(rest) = origin.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let user = authority.rsplit_once('@').map(|(user, _)| user);
        let host = authority.rsplit('@').next()?;
        let user = user.map(|u| u.split(':').next().unwrap_or(u));
        return Some(match user {
            Some(user) if !user.is_empty() => format!("ssh://{user}@{host}/{path}"),
            _ => format!("ssh://{host}/{path}"),
        });
    }
    for scheme in ["https://", "http://", "git://"] {
        if let Some(rest) = origin.strip_prefix(scheme) {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            let host = host.split(':').next()?;
            let path = path.trim_end_matches('/');
            if host.is_empty() || path.is_empty() {
                return None;
            }
            return Some(format!("git@{host}:{path}"));
        }
    }
    // `user@host:path`, as scp writes it.
    let (authority, path) = origin.split_once(':')?;
    let fine = !authority.is_empty() && !path.is_empty() && !authority.contains('/');
    fine.then(|| origin.to_string())
}

/// The notify URL as `Debug` shows it: with `url_for_logs`.
struct LoggedUrl<'a>(&'a Option<String>);

impl std::fmt::Debug for LoggedUrl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(url) => write!(f, "Some({:?})", url_for_logs(url)),
            None => f.write_str("None"),
        }
    }
}

impl std::fmt::Debug for AutonomySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutonomySettings")
            .field("effort", &self.effort)
            .field("budget", &self.budget)
            .field("delivery", &self.delivery)
            .field("notify_url", &LoggedUrl(&self.notify_url))
            .finish()
    }
}

impl std::fmt::Debug for AutonomyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutonomyConfig")
            .field("delivery", &self.delivery)
            .field("notify_url", &LoggedUrl(&self.notify_url))
            .finish()
    }
}

impl AutonomySettings {
    /// What the server keeps of them for its runs (H02).
    pub fn config(&self) -> AutonomyConfig {
        AutonomyConfig {
            delivery: self.delivery.clone(),
            notify_url: self.notify_url.clone(),
        }
    }

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
            budget.max_tokens = budget.max_tokens.filter(|t| *t > 0);
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
                max_tokens: None,
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

    /// H02: what the server keeps, and how a log shows the notify URL,
    /// which may carry a token: never whole.
    #[test]
    fn the_servers_part_round_trips_and_its_url_isnt_logged_whole() {
        let settings = AutonomySettings {
            effort: Some(Effort::Thorough),
            delivery: Delivery::PushBranch {
                deploy_key: "~/.ssh/k".into(),
                open_pr: false,
            },
            notify_url: Some("https://ntfy.sh/rovik-secret-topic?auth=tok".into()),
            ..Default::default()
        };
        let config = settings.config();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            json!({
                "delivery": { "kind": "pushBranch", "deployKey": "~/.ssh/k", "openPr": false },
                "notifyUrl": "https://ntfy.sh/rovik-secret-topic?auth=tok",
            })
        );
        assert_eq!(
            serde_json::from_value::<AutonomyConfig>(json).unwrap(),
            config
        );
        let none: AutonomyConfig = serde_json::from_value(json!({})).unwrap();
        assert!(none.is_default());
        assert!(!config.is_default());
        for shown in [format!("{config:?}"), format!("{settings:?}")] {
            assert!(!shown.contains("secret"), "{shown}");
            assert!(!shown.contains("tok"), "{shown}");
            assert!(shown.contains("https://ntfy.sh/…"), "{shown}");
        }
        assert_eq!(
            url_for_logs("https://hooks.example.com"),
            "https://hooks.example.com"
        );
        assert_eq!(
            url_for_logs("https://user:pw@example.com/x"),
            "https://example.com/…"
        );
        assert_eq!(url_for_logs("example.com/token"), "(a URL)");
    }

    /// H04: what a server pushes to, from the project's `origin`: an SSH
    /// address, no credentials.
    #[test]
    fn the_push_url_is_the_origin_as_ssh_without_credentials() {
        let cases = [
            (
                "git@github.com:acme/app.git",
                Some("git@github.com:acme/app.git"),
            ),
            (
                "https://github.com/acme/app.git",
                Some("git@github.com:acme/app.git"),
            ),
            (
                "https://me:tok@github.com/acme/app",
                Some("git@github.com:acme/app"),
            ),
            (
                "https://gitlab.example.com:8443/team/app.git/",
                Some("git@gitlab.example.com:team/app.git"),
            ),
            (
                "ssh://git@host:2222/srv/app.git",
                Some("ssh://git@host:2222/srv/app.git"),
            ),
            (
                "ssh://me:pw@host/srv/app.git",
                Some("ssh://me@host/srv/app.git"),
            ),
            ("file:///srv/git/app.git", Some("file:///srv/git/app.git")),
            ("/srv/git/app.git", Some("/srv/git/app.git")),
            ("", None),
            ("https://github.com", None),
            ("not a url", None),
        ];
        for (origin, expected) in cases {
            assert_eq!(push_url(origin).as_deref(), expected, "{origin}");
        }
    }

    #[test]
    fn a_budget_says_its_token_limit_only_when_it_has_one() {
        let plain = Budget {
            max_hours: Some(2.0),
            max_cost: Some(5.0),
            max_tokens: None,
        };
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({ "maxHours": 2.0, "maxCost": 5.0 })
        );
        let tokens = Budget {
            max_tokens: Some(2_000_000),
            ..plain.clone()
        };
        let json = serde_json::to_value(&tokens).unwrap();
        assert_eq!(json["maxTokens"], 2_000_000);
        assert_eq!(serde_json::from_value::<Budget>(json).unwrap(), tokens);
        let older: Budget =
            serde_json::from_value(serde_json::json!({ "maxHours": 1.5, "maxCost": null }))
                .unwrap();
        assert_eq!(older.max_tokens, None);
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
                max_tokens: Some(0),
            }),
            ..Default::default()
        };
        assert_eq!(budget.checked().unwrap().budget, None);
        let tokens = AutonomySettings {
            budget: Some(Budget {
                max_tokens: Some(5_000_000),
                ..Budget::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            tokens.checked().unwrap().budget.and_then(|b| b.max_tokens),
            Some(5_000_000)
        );
        let hours = AutonomySettings {
            budget: Some(Budget {
                max_hours: Some(2.0),
                max_cost: Some(0.0),
                max_tokens: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            hours.checked().unwrap().budget,
            Some(Budget {
                max_hours: Some(2.0),
                max_cost: None,
                max_tokens: None,
            })
        );
    }
}

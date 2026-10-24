//! Connectors: coding harnesses (Claude Code first) a chat can use in place
//! of a model provider. A connector is a program on the computer the chat
//! runs on; it brings its own model loop and tools, so the IDE hands it the
//! task and shows what it did.
//!
//! A connector's models go through the same settings as a provider's:
//! each is a `Provider` entry in the AI settings whose base URL is
//! `connector://<kind>`, so the chat's model picker, the saved model of a
//! chat, the AI reviewer and the keychain (its token is the entry's
//! `apiKey`) all work as they do for a provider. What the harness itself
//! needs (its program, its permission mode, its arguments) travels in
//! `ModelProfile::connector`, read by the connector's own code in the core.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The scheme of a connector's base URL: `connector://claude-code`.
pub const SCHEME: &str = "connector://";

/// The harnesses Rovik can connect. Each is implemented in the core
/// (`rovik_core::connectors`); the frontend lists the same kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectorKind {
    ClaudeCode,
}

impl ConnectorKind {
    pub const ALL: [ConnectorKind; 1] = [ConnectorKind::ClaudeCode];

    /// Its id, as the base URL names it: `claude-code`.
    pub fn id(self) -> &'static str {
        match self {
            ConnectorKind::ClaudeCode => "claude-code",
        }
    }

    /// For the user: `Claude Code`.
    pub fn title(self) -> &'static str {
        match self {
            ConnectorKind::ClaudeCode => "Claude Code",
        }
    }

    /// The kind this id names.
    pub fn from_id(id: &str) -> Option<ConnectorKind> {
        Self::ALL.into_iter().find(|k| k.id() == id.trim())
    }

    /// Its base URL: `connector://claude-code`.
    pub fn base_url(self) -> String {
        format!("{SCHEME}{}", self.id())
    }
}

/// The connector a base URL names, if it names one.
pub fn kind_of(base_url: &str) -> Option<ConnectorKind> {
    let rest = base_url.trim().strip_prefix(SCHEME)?;
    ConnectorKind::from_id(rest.trim_end_matches('/'))
}

/// Whether a base URL is a connector's rather than a provider's endpoint.
pub fn is_connector(base_url: &str) -> bool {
    kind_of(base_url).is_some()
}

/// What "Test & refresh models" finds out about a connector on the
/// computer it was asked on (`Request::ConnectorProbe`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorProbe {
    /// The program that answered, as it was run (`claude`, or a path).
    pub program: String,
    /// Its version, as `--version` printed it.
    pub version: String,
    /// The models it offers: aliases and the names it knows, with a
    /// context window where known.
    pub models: Vec<ConnectorModel>,
    /// A request was made and answered: the connector is signed in and
    /// can be used. None when the probe didn't try one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<ProbeRequest>,
    /// Advice for the user that isn't a failure: a shared installation on
    /// a server, a token that will expire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Whether it's signed in, as the program says (`ConnectorAuth`);
    /// none when the program couldn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ConnectorAuth>,
    /// A newer version than `version` is published: this one. None when
    /// it's the newest, or the newest couldn't be found out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
}

/// Whether a connector's program is signed in on the computer asked
/// (`Request::ConnectorAuthStatus`), as it would be for a chat's run: with
/// the connector's token or key and settings folder. The program's own
/// check, which makes no request: a token it has is taken as good until a
/// request says it expired or was revoked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorAuth {
    pub logged_in: bool,
    /// How, as the program names it: `claude.ai`, `oauth_token`, `api_key`…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The settings folder its login is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// For the user: why it isn't signed in and what to do, or a caveat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A model a connector offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorModel {
    /// What the connector is told: an alias (`sonnet`) or a full name.
    pub id: String,
    /// Who made it, as the model list shows it (`anthropic`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// A line for the user: what the alias means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The test request a probe made, and what it cost.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRequest {
    /// The model that answered, as the harness named it.
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub ms: u64,
}

/// What a probe is asked with: the connector, its settings as the frontend
/// keeps them (`Provider.settings`), and its token or key, which travels
/// as a profile's `api_key` does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRequestArgs {
    pub kind: ConnectorKind,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub api_key: String,
    /// Whether to make a small request too (it costs a few tokens).
    #[serde(default)]
    pub request: bool,
}

/// A step of a connector's "Set up" that runs its program, or its
/// installer, on the computer asked (`Request::ConnectorSetup`). Which
/// steps a connector has, and what each runs, is its own
/// (`rovik_core::connectors::Connector::setup_command`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupAction {
    /// Installs the program for the user who asks, without admin rights.
    Install,
    /// Signs the program in, through a web page the user opens.
    Login,
    /// Signs it out.
    Logout,
    /// Updates it to the newest version (`ConnectorProbe::update`).
    Update,
}

/// What `Request::ConnectorSetup` does with a setup step: each answers with
/// the step as it then stands (`SetupJob`). A step runs on the computer
/// asked and is followed from there by its id, so the same calls follow
/// one here and on a cloud host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum SetupOp {
    /// Starts `action` for the connector `args` names, stopping any step
    /// of that connector still running.
    Start {
        args: ProbeRequestArgs,
        action: SetupAction,
    },
    /// The step as it stands.
    Poll { id: String },
    /// A line for the program, typed by the user: a sign-in code.
    Send { id: String, text: String },
    /// Stops the step.
    Cancel { id: String },
}

/// A setup step, as the computer running it reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupJob {
    pub id: String,
    pub action: SetupAction,
    /// What it runs, for the user: `curl -fsSL … | bash`, `claude auth login`.
    pub command: String,
    /// What it printed so far (the latest of it), without terminal codes.
    pub output: String,
    /// The first web address it printed: the sign-in page to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub done: bool,
    /// Why it failed, once done; none when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connectors_base_url_names_its_kind() {
        assert_eq!(
            kind_of("connector://claude-code"),
            Some(ConnectorKind::ClaudeCode)
        );
        assert_eq!(
            kind_of(" connector://claude-code/ "),
            Some(ConnectorKind::ClaudeCode)
        );
        assert_eq!(kind_of("connector://codex"), None);
        assert_eq!(kind_of("https://api.openai.com/v1"), None);
        assert!(!is_connector("http://localhost:11434/v1"));
        assert_eq!(
            ConnectorKind::ClaudeCode.base_url(),
            "connector://claude-code"
        );
        assert_eq!(
            ConnectorKind::from_id("claude-code"),
            Some(ConnectorKind::ClaudeCode)
        );
    }

    #[test]
    fn setup_ops_travel_tagged() {
        let op = SetupOp::Send {
            id: "j".into(),
            text: "code#state".into(),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "op": "send", "id": "j", "text": "code#state" })
        );
        let start: SetupOp = serde_json::from_value(serde_json::json!({
            "op": "start",
            "action": "login",
            "args": { "kind": "claude-code" }
        }))
        .unwrap();
        assert!(matches!(
            start,
            SetupOp::Start {
                action: SetupAction::Login,
                ..
            }
        ));
    }

    #[test]
    fn kinds_travel_as_their_ids() {
        let json = serde_json::to_string(&ConnectorKind::ClaudeCode).unwrap();
        assert_eq!(json, "\"claude-code\"");
        let back: ConnectorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ConnectorKind::ClaudeCode);
    }
}

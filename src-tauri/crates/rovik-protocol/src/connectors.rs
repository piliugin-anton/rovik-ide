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
    fn kinds_travel_as_their_ids() {
        let json = serde_json::to_string(&ConnectorKind::ClaudeCode).unwrap();
        assert_eq!(json, "\"claude-code\"");
        let back: ConnectorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ConnectorKind::ClaudeCode);
    }
}

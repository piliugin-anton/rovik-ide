//! The MCP servers the user sets up (Settings → MCP servers), whose tools
//! the agent calls with its `mcp` tool (docs/design/mcp-plan.md, Q01). The
//! frontend keeps them in its AI settings (src/lib/mcpServers.ts) and sends
//! them with the agent's configuration.
//!
//! A secret's value (a token in an env var or a header) is never here: the
//! keychain has it, and the core reads it when it starts the server (Q02).

use serde::{Deserialize, Serialize};

/// The most characters a server's name has.
pub const MAX_NAME: usize = 32;

/// What Test on the settings page shows of a server (Q04).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tested {
    /// Its name and version, as it gives them.
    pub server: String,
    /// The protocol version Rovik speaks with it.
    pub protocol: String,
    pub tools: Vec<TestedTool>,
}

/// One tool in a `Tested`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestedTool {
    pub name: String,
    pub description: String,
    pub read_only: bool,
}

/// One MCP server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// Made when the server is added, never shown: its secrets are kept by
    /// it, and two windows' saves merge by it.
    pub id: String,
    /// What the agent calls it: `github` in `github.create_issue`
    /// (`is_valid_name`).
    pub name: String,
    /// Off keeps its settings and leaves it out.
    #[serde(default = "on")]
    pub enabled: bool,
    pub transport: McpTransport,
    /// The tools the user switched off, by name: left out of the agent's
    /// list.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

fn on() -> bool {
    true
}

/// How Rovik reaches a server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpTransport {
    /// A command Rovik starts and speaks to on its stdin and stdout.
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Its environment, besides the app's.
        #[serde(default)]
        env: Vec<McpValue>,
    },
    /// An address Rovik posts to (Streamable HTTP).
    Url {
        url: String,
        /// Sent with each request.
        #[serde(default)]
        headers: Vec<McpValue>,
    },
}

/// An environment variable of a command's server, or a header of a URL's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpValue {
    pub name: String,
    /// Empty for a secret, whose value the keychain has
    /// (`McpTransport::secret_key`).
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

impl McpTransport {
    /// Its env vars or headers.
    pub fn values(&self) -> &[McpValue] {
        match self {
            McpTransport::Command { env, .. } => env,
            McpTransport::Url { headers, .. } => headers,
        }
    }

    /// What the secret value `name` of it is kept under, besides the
    /// server's id: an env var's `env/NAME`, a header's `header/Name`.
    pub fn secret_key(&self, name: &str) -> String {
        match self {
            McpTransport::Command { .. } => format!("env/{name}"),
            McpTransport::Url { .. } => format!("header/{name}"),
        }
    }

    /// The keys its secret values are kept under: one for each named value
    /// marked secret.
    pub fn secret_keys(&self) -> Vec<String> {
        self.values()
            .iter()
            .filter(|v| v.secret && !v.name.trim().is_empty())
            .map(|v| self.secret_key(&v.name))
            .collect()
    }
}

/// Whether `key` is one [`McpTransport::secret_key`] makes: `env/NAME` or
/// `header/Name`, the name neither empty nor with a `/`. Kept under the
/// server's id, such a key never reaches another server's or another
/// value's place in the keychain.
pub fn is_secret_key(key: &str) -> bool {
    key.strip_prefix("env/")
        .or_else(|| key.strip_prefix("header/"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// Whether `name` can name a server: lower-case letters, digits, `-` and
/// `_`, starting with a letter, at most [`MAX_NAME`] characters. It never
/// has the dot that parts it from a tool's name in `github.create_issue`.
pub fn is_valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

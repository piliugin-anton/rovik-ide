//! The MCP servers the user sets up (Settings → MCP servers), whose tools
//! the agent calls with its `mcp` tool (docs/design/mcp-plan.md, Q01). The
//! frontend keeps them in its AI settings (src/lib/mcpServers.ts) and sends
//! them with the agent's configuration.
//!
//! A secret's value (a token in an env var or a header) is never here: the
//! keychain has it, and the core reads it when it starts the server (Q02).

use crate::Secret;
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
        /// The old HTTP+SSE transport (2024-11-05), as the server's setup
        /// said (`"type": "sse"`): a GET of `url` opens its event stream
        /// (mcp-plan-2.md, Q13). One without it that turns out to speak
        /// only that is spoken to so all the same.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        sse: bool,
        /// The OAuth client its sign-in uses, when the user entered one
        /// (Q14); else Rovik's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<McpOAuthClient>,
    },
}

/// An OAuth client a URL server's sign-in uses (mcp-plan-2.md, Q14): one
/// the user registered with its authorization server and entered on the
/// server's form.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthClient {
    pub client_id: String,
    /// It has a secret, which the keychain keeps with the server's other
    /// secrets, under [`CLIENT_SECRET_KEY`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// The key a server's OAuth client secret is kept under (Q14).
pub const CLIENT_SECRET_KEY: &str = "oauth/client-secret";

/// A sign-in to a URL server (mcp-plan-2.md, Q14): what its authorization
/// server issued for one place (this computer, a host), and what refreshes
/// it there. Its tokens and client secret never show in `Debug`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpGrant {
    /// The authorization server that issued it.
    pub issuer: String,
    /// Where it's refreshed.
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<Secret>,
    /// How the client proves itself at the token endpoint: `none`,
    /// `client_secret_basic` or `client_secret_post`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_method: String,
    /// The server the tokens are for (RFC 8707's `resource`).
    pub resource: String,
    /// The scopes asked for, space-separated.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    pub access_token: Secret,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<Secret>,
    /// When the access token expires, in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// The scopes the server asked for that this sign-in lacks (a 403's
    /// `insufficient_scope`), with those it has: the next sign-in for this
    /// place asks for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wants: Option<String>,
}

/// An MCP server signed in on a host (Q14), as `McpSignedIn` answers:
/// never its tokens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SignedInServer {
    /// The server's id.
    pub server: String,
    /// The scopes it asked for that its sign-in there lacks, as
    /// [`McpGrant::wants`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wants: Option<String>,
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
/// `header/Name`, the name neither empty nor with a `/`; or the OAuth
/// client's secret ([`CLIENT_SECRET_KEY`]). Kept under the server's id,
/// such a key never reaches another server's or another value's place in
/// the keychain.
pub fn is_secret_key(key: &str) -> bool {
    key == CLIENT_SECRET_KEY
        || key
            .strip_prefix("env/")
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

/// Where a project's servers' ids begin: `project/<project id>/<hash>`,
/// their variables' values kept under `project/<project id>` (Q12).
pub const PROJECT_SERVERS: &str = "project/";

/// A server a project's own file sets up (mcp-plan-2.md, Q12), as found:
/// its values as written, the variables they name not filled in.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectServer {
    /// Its name, made valid and apart from the user's servers and the
    /// project's others.
    pub name: String,
    /// The file it's in, from the project's root: `.mcp.json`,
    /// `.cursor/mcp.json` or `.vscode/mcp.json`.
    pub file: String,
    /// Its definition's hash, before its variables are filled: what an
    /// approval keeps, so a server changed is asked for again.
    pub hash: String,
    /// Its command or URL, the values as the file writes them.
    pub transport: McpTransport,
    /// What its values name that the user gives a value for, as they come.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<ProjectVariable>,
}

/// A variable a project's server names (`${GITHUB_TOKEN}`, VS Code's
/// `${input:token}`), which the user gives a value for when they approve
/// it: kept as a secret, by the project.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectVariable {
    /// As the project's values name it: `GITHUB_TOKEN` for `${GITHUB_TOKEN}`
    /// and `${env:GITHUB_TOKEN}`, `input:token` for `${input:token}`.
    pub name: String,
    /// What the file says of it: an input's description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What it is when given no value: `${VAR:-default}`'s, an input's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// An input marked as a password: its field hides what's typed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password: bool,
}

/// A POST a daemon has the user's computer make, to an MCP server on that
/// computer's loopback, which a host can't reach (mcp-plan-2.md, Q11): the
/// request whole, its headers' values kept out of `Debug`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayedPost {
    /// The server's URL, as its settings have it; or, on the HTTP+SSE
    /// transport, the endpoint its event stream named (Q13).
    pub url: String,
    /// Each header with its value, the settings' secrets among them.
    pub headers: Vec<(String, Secret)>,
    /// The JSON-RPC message.
    pub body: String,
    /// A GET of the server's event stream instead (HTTP+SSE, Q13), with
    /// no body: its answer goes on for as long as the stream does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

/// A part of the answer to a [`RelayedPost`], as the computer gets it: its
/// status and headers, then its body as it comes, then its end; or why the
/// POST failed there.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RelayPart {
    Head {
        status: u16,
        headers: Vec<(String, String)>,
    },
    Body {
        text: String,
    },
    End,
    Failed {
        error: String,
    },
}

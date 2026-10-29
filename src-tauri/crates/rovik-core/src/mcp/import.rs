//! Servers from JSON as other clients' setup instructions give them
//! (mcp-plan.md, Q01): Settings → MCP servers' **Add from JSON**, and the
//! servers a project's own files set up (mcp-plan-2.md, Q12). An
//! `mcpServers` object (Claude Code, Claude Desktop, Cursor), VS Code's
//! `servers`, one server's entry by its name, or the entry alone: a command
//! (`command`, `args`, `env`) or a URL (`url`, `headers`).

use super::{McpServer, McpTransport, McpValue};
use rovik_protocol::mcp::MAX_NAME;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// What a value's name says of a secret: a key, a token, a password.
static SECRET_NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)key|token|secret|password|passwd|auth|credential|cookie|pat\b")
        .expect("a valid pattern")
});

/// A value setup instructions show for the user's own: `<YOUR_TOKEN>`,
/// `${API_KEY}`, `{{token}}`, `your-key`.
static PLACEHOLDER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)<[^<>]*>|\$\{[^}]*\}|\{\{[^}]*\}\}|\byour[-_ ]")
        .expect("a valid pattern")
});

/// Whether a value named `name` (an env var, a header) looks like a
/// secret's, as the page marks one by default.
pub fn looks_secret(name: &str) -> bool {
    SECRET_NAME.is_match(name)
}

/// `name` made a valid server name (`is_valid_name`), not one in `taken`,
/// which it joins: lower-case, other characters dashes, `-2` after one
/// taken.
pub(crate) fn name_for(name: &str, taken: &mut BTreeSet<String>) -> String {
    // Each run of other characters is one dash.
    let mut dashed = String::new();
    let mut other = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            dashed.push(c);
            other = false;
        } else if !other {
            dashed.push('-');
            other = true;
        }
    }
    let trimmed = dashed
        .trim_start_matches(|c: char| !c.is_ascii_lowercase())
        .trim_end_matches('-');
    let mut base: String = trimmed.chars().take(MAX_NAME).collect();
    if base.is_empty() {
        base = "server".into();
    }
    let mut candidate = base.clone();
    let mut n = 2;
    while taken.contains(&candidate) {
        let suffix = format!("-{n}");
        let room = MAX_NAME - suffix.len();
        candidate = format!("{}{suffix}", base.chars().take(room).collect::<String>());
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

/// Whether `value` is a server's entry: it has a command or a URL.
fn is_entry(value: &Value) -> bool {
    let text = |key: &str| value.get(key).is_some_and(Value::is_string);
    value.is_object() && (text("command") || text("url"))
}

/// The server entries `root` holds, each with its name there.
pub(crate) fn entries(root: &Map<String, Value>) -> Vec<(String, &Map<String, Value>)> {
    let listed: Vec<(String, &Value)> = match (root.get("mcpServers"), root.get("servers")) {
        (Some(Value::Object(servers)), _) | (_, Some(Value::Object(servers))) => {
            servers.iter().map(|(n, v)| (n.clone(), v)).collect()
        }
        _ => {
            let whole = Value::Object(root.clone());
            if is_entry(&whole) {
                let name = root.get("name").and_then(Value::as_str).unwrap_or("server");
                return vec![(name.to_string(), root)];
            }
            root.iter().map(|(n, v)| (n.clone(), v)).collect()
        }
    };
    listed
        .into_iter()
        .filter(|(_, v)| is_entry(v))
        .filter_map(|(name, v)| v.as_object().map(|entry| (name, entry)))
        .collect()
}

/// A value of an entry's `env` or `headers` as text, as JavaScript writes
/// it: a number or a boolean as its digits or word.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// An entry's command or URL, its env vars or headers made by `value`
/// (from a value's name and its text).
pub(crate) fn transport_of(
    entry: &Map<String, Value>,
    value: impl Fn(&str, String) -> McpValue,
) -> McpTransport {
    let values = |key: &str| -> Vec<McpValue> {
        let Some(Value::Object(map)) = entry.get(key) else {
            return Vec::new();
        };
        (map.iter())
            .filter(|(name, _)| !name.trim().is_empty())
            .map(|(name, v)| value(name, text_of(v)))
            .collect()
    };
    match entry.get("command").and_then(Value::as_str) {
        Some(command) => McpTransport::Command {
            command: command.to_string(),
            args: match entry.get("args") {
                Some(Value::Array(args)) => args.iter().map(text_of).collect(),
                _ => Vec::new(),
            },
            env: values("env"),
        },
        None => McpTransport::Url {
            url: entry.get("url").map(text_of).unwrap_or_default(),
            headers: values("headers"),
        },
    }
}

/// The servers in `text`, JSON pasted on the settings page, named apart
/// from the `taken` names. A value whose name looks like a secret's is a
/// secret, its value typed (as a form has it), but for a placeholder
/// (`<YOUR_TOKEN>`), which is left for the user to enter. An entry of the
/// old SSE transport is refused: it goes through a bridge command.
pub fn import(text: &str, taken: &[String]) -> Result<Vec<McpServer>, String> {
    let parsed: Value = serde_json::from_str(text).map_err(|_| {
        "That isn’t JSON. Paste the server’s settings as its instructions give them.".to_string()
    })?;
    let Value::Object(root) = &parsed else {
        return Err("Paste an object: an \"mcpServers\" entry, or one server’s settings.".into());
    };
    let found = entries(root);
    if found.is_empty() {
        return Err("No server found: each needs a \"command\" or a \"url\".".into());
    }
    let mut taken: BTreeSet<String> = taken.iter().cloned().collect();
    let mut servers = Vec::new();
    for (name, entry) in found {
        if entry.get("type").and_then(Value::as_str) == Some("sse") {
            let url = entry.get("url").map(text_of);
            return Err(format!(
                "{name} uses the old SSE transport: add it as a command, through a bridge: npx \
                 mcp-remote {}",
                url.as_deref().unwrap_or("<url>")
            ));
        }
        let transport = transport_of(entry, |key, text| {
            let secret = looks_secret(key);
            McpValue {
                name: key.to_string(),
                value: if secret && PLACEHOLDER.is_match(&text) {
                    String::new()
                } else {
                    text
                },
                secret,
            }
        });
        servers.push(McpServer {
            id: uuid::Uuid::new_v4().to_string(),
            name: name_for(&name, &mut taken),
            enabled: entry.get("disabled") != Some(&Value::Bool(true)),
            transport,
            disabled_tools: Vec::new(),
        });
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn value(name: &str, value: &str, secret: bool) -> McpValue {
        McpValue {
            name: name.into(),
            value: value.into(),
            secret,
        }
    }

    /// Q01: Claude Desktop's, Cursor's and VS Code's JSON become servers:
    /// names made valid and apart from the user's, a secret's value typed
    /// in but a placeholder's left out, a server turned off as it was.
    #[test]
    fn other_clients_json_becomes_servers_their_secrets_typed_in() {
        let desktop = json!({
            "mcpServers": {
                "GitHub": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_123", "LOG_LEVEL": 3 },
                },
                "my.docs": {
                    "url": "https://docs.example.com/mcp",
                    "headers": { "Authorization": "Bearer <YOUR_TOKEN>" },
                    "disabled": true,
                },
            },
        });
        let servers = import(&desktop.to_string(), &["github".into()]).unwrap();
        let (gh, docs) = (&servers[0], &servers[1]);
        assert_eq!(gh.name, "github-2", "made valid, and not a name taken");
        assert_eq!(
            gh.transport,
            McpTransport::Command {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                env: vec![
                    value("GITHUB_PERSONAL_ACCESS_TOKEN", "ghp_123", true),
                    value("LOG_LEVEL", "3", false),
                ],
            }
        );
        assert!(gh.enabled);
        assert_eq!(docs.name, "my-docs");
        assert!(!docs.enabled, "turned off as it was");
        assert_eq!(
            docs.transport,
            McpTransport::Url {
                url: "https://docs.example.com/mcp".into(),
                headers: vec![value("Authorization", "", true)],
            },
            "the placeholder left for the user to enter"
        );
        assert_ne!(gh.id, docs.id);

        let vscode = json!({ "servers": { "fetch": { "type": "stdio", "command": "uvx", "args": ["mcp-server-fetch"] } } });
        let names = |text: Value| -> Vec<String> {
            let servers = import(&text.to_string(), &[]).unwrap();
            servers.into_iter().map(|s| s.name).collect()
        };
        assert_eq!(names(vscode), ["fetch"]);
        assert_eq!(
            names(json!({ "command": "uvx", "args": ["mcp-server-time"] })),
            ["server"]
        );
        assert_eq!(names(json!({ "time": { "command": "uvx" } })), ["time"]);
    }

    /// JSON with no server, or an SSE one, says what to do instead.
    #[test]
    fn json_without_a_server_or_with_an_sse_one_says_what_to_do() {
        let error = |text: &str| import(text, &[]).unwrap_err();
        assert!(error("{").starts_with("That isn’t JSON"));
        assert!(error("[]").starts_with("Paste an object"));
        assert!(error(r#"{"mcpServers": {"x": {"args": []}}}"#).starts_with("No server found"));
        let sse = json!({ "mcpServers": { "old": { "type": "sse", "url": "https://old.example.com/sse" } } });
        assert_eq!(
            error(&sse.to_string()),
            "old uses the old SSE transport: add it as a command, through a bridge: npx mcp-remote https://old.example.com/sse"
        );
    }

    /// A name keeps letters, digits, - and _, starts with a letter, fits,
    /// and is told apart from those taken.
    #[test]
    fn a_name_is_made_valid_and_apart() {
        let mut taken = BTreeSet::new();
        assert_eq!(name_for("My Docs!", &mut taken), "my-docs");
        assert_eq!(name_for("my docs", &mut taken), "my-docs-2");
        assert_eq!(name_for("42", &mut taken), "server");
        assert_eq!(name_for("--a--", &mut taken), "a");
        let long = name_for(&"x".repeat(40), &mut taken);
        assert_eq!(long.len(), MAX_NAME);
        let again = name_for(&"x".repeat(40), &mut taken);
        assert_eq!(again, format!("{}-2", "x".repeat(MAX_NAME - 2)));
        for name in &taken {
            assert!(rovik_protocol::mcp::is_valid_name(name), "{name}");
        }
    }

    /// A name says a secret's the way the page marks one: a key, a token,
    /// a password, a personal access token.
    #[test]
    fn a_secret_s_name_looks_like_one() {
        for name in [
            "API_KEY",
            "Authorization",
            "GITHUB_PAT",
            "x-session-cookie",
            "db_passwd",
        ] {
            assert!(looks_secret(name), "{name}");
        }
        for name in ["LOG_LEVEL", "REGION", "path", "PATH"] {
            assert!(!looks_secret(name), "{name}");
        }
    }
}

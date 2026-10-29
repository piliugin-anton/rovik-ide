//! The MCP servers a project's own files set up (mcp-plan-2.md, Q12):
//! `.mcp.json` (Claude Code), `.cursor/mcp.json` (Cursor) and
//! `.vscode/mcp.json` (VS Code, with its `inputs`), read where a run
//! starts, through the reader Settings uses for pasted JSON (import.rs).
//! Their values name variables rather than hold secrets: `${VAR}` and
//! `${VAR:-default}` (Claude Code), `${env:VAR}` (Cursor, VS Code),
//! `${input:id}` (VS Code), and the folders `${workspaceFolder}` and
//! `${userHome}`. The user gives each variable but the folders a value
//! when they approve the servers, and approves a server again once its
//! definition changes.

use super::import::{entries, looks_secret, name_for, transport_of};
use super::{McpServer, McpTransport, McpValue};
use rovik_protocol::mcp::{ProjectServer, ProjectVariable, PROJECT_SERVERS};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The project's files, in the order whose first keeps a duplicate's name.
pub const FILES: [&str; 3] = [".mcp.json", ".cursor/mcp.json", ".vscode/mcp.json"];

/// The key of the project's settings that keeps its approved servers.
const APPROVALS: &str = "mcpApprovals";

/// Whether `file` names an env var bare, `${VAR}`, as Claude Code's
/// `.mcp.json` does. Cursor's and VS Code's write `${env:VAR}`, a bare
/// name being one of their own (`${pathSeparator}`), which stays as written.
fn bare_vars(file: &str) -> bool {
    file == FILES[0]
}

/// What a project's files set up, as read.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    /// Its servers, duplicates merged.
    pub servers: Vec<ProjectServer>,
    /// Why each entry left out is: its name is one of the user's servers'.
    pub left_out: Vec<String>,
}

/// A variable a value names, as `${…}` writes it.
enum Named {
    /// The user gives it a value: an env var's (`${VAR}`, `${env:VAR}`),
    /// with its default (`${VAR:-default}`), or an input's (`input:id`).
    Given(String, Option<String>),
    /// The folder the server runs for (`${workspaceFolder}`).
    Folder,
    /// The user's home (`${userHome}`).
    Home,
}

/// What `${…}` names, its braces' inside, a bare name an env var where the
/// file writes them so (`bare`): none for one Rovik doesn't know, which
/// stays as written.
fn named(inside: &str, bare: bool) -> Option<Named> {
    let is_var = |name: &str| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    match inside {
        "workspaceFolder" => return Some(Named::Folder),
        "userHome" => return Some(Named::Home),
        _ => {}
    }
    if let Some(var) = inside.strip_prefix("env:") {
        return is_var(var).then(|| Named::Given(var.to_string(), None));
    }
    if let Some(id) = inside.strip_prefix("input:") {
        return (!id.trim().is_empty()).then(|| Named::Given(format!("input:{id}"), None));
    }
    let (var, default) = match inside.split_once(":-") {
        Some((var, default)) => (var, Some(default.to_string())),
        None => (inside, None),
    };
    (bare && is_var(var)).then(|| Named::Given(var.to_string(), default))
}

/// `text` with each variable it names (`bare` as `named` has it) replaced
/// by `fill`'s text for it, one Rovik doesn't know left as written.
fn substitute(text: &str, bare: bool, mut fill: impl FnMut(&Named) -> String) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[at..]);
            return out;
        };
        match named(&after[..end], bare) {
            Some(variable) => out.push_str(&fill(&variable)),
            None => out.push_str(&rest[at..at + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Whether `text` names a variable the user gives a value for.
fn names_a_value(text: &str, bare: bool) -> bool {
    let mut found = false;
    substitute(text, bare, |variable| {
        found |= matches!(variable, Named::Given(..));
        String::new()
    });
    found
}

/// Every text of a transport: its command and arguments, or its URL; then
/// its values.
fn texts(transport: &McpTransport) -> Vec<&str> {
    let mut texts: Vec<&str> = match transport {
        McpTransport::Command { command, args, .. } => std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect(),
        McpTransport::Url { url, .. } => vec![url.as_str()],
    };
    texts.extend(transport.values().iter().map(|v| v.value.as_str()));
    texts
}

/// The variables `transport` names that the user gives a value for, as
/// they come, described as VS Code's `inputs` describe them.
fn variables_of(transport: &McpTransport, inputs: &[Value], bare: bool) -> Vec<ProjectVariable> {
    let mut variables: Vec<ProjectVariable> = Vec::new();
    for text in texts(transport) {
        substitute(text, bare, |variable| {
            if let Named::Given(name, default) = variable {
                if !variables.iter().any(|v| v.name == *name) {
                    let input = (name.strip_prefix("input:"))
                        .and_then(|id| inputs.iter().find(|i| i["id"] == id));
                    let field = |key: &str| input.and_then(|i| i[key].as_str()).map(str::to_string);
                    variables.push(ProjectVariable {
                        name: name.clone(),
                        description: field("description").unwrap_or_default(),
                        default: default.clone().or_else(|| field("default")),
                        password: input.is_some_and(|i| i["password"] == true),
                    });
                }
            }
            String::new()
        });
    }
    variables
}

/// What makes two servers one (Q12): a URL server's address and its auth
/// headers (those whose names look secret, by name only when `values` is
/// false), or a command's program, arguments and environment (names only,
/// likewise).
fn identity(transport: &McpTransport, values: bool) -> String {
    let fields = |list: &[McpValue], all: bool| -> Vec<String> {
        let mut fields: Vec<String> = (list.iter())
            .filter(|v| all || looks_secret(&v.name))
            .map(|v| match values {
                true => format!("{}={}", v.name.to_lowercase(), v.value),
                false => v.name.to_lowercase(),
            })
            .collect();
        fields.sort();
        fields
    };
    let identity = match transport {
        McpTransport::Url { url, headers } => {
            let url =
                reqwest::Url::parse(url.trim()).map_or(url.trim().to_string(), |u| u.to_string());
            serde_json::json!(["url", url, fields(headers, false)])
        }
        McpTransport::Command { command, args, env } => {
            serde_json::json!(["command", command.trim(), args, fields(env, true)])
        }
    };
    identity.to_string()
}

/// The servers the files at `root` set up, but those turned off there and
/// those `user`'s servers already are; one named as one of the user's is
/// left out, saying so (Q12). A file that can't be read, or isn't JSON, is
/// passed over.
pub fn read(root: &Path, user: &[McpServer]) -> Found {
    let user_names: BTreeSet<String> = user.iter().map(|s| s.name.clone()).collect();
    let user_ids: Vec<String> = user.iter().map(|s| identity(&s.transport, false)).collect();
    let mut found = Found::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut taken = user_names.clone();
    for file in FILES {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let Ok(Value::Object(json)) = serde_json::from_str::<Value>(&text) else {
            log::info!("{file} of {} isn't JSON: passed over", root.display());
            continue;
        };
        let inputs = match json.get("inputs") {
            Some(Value::Array(inputs)) => inputs.clone(),
            _ => Vec::new(),
        };
        for (raw, entry) in entries(&json) {
            if entry.get("disabled") == Some(&Value::Bool(true)) {
                continue;
            }
            let bare = bare_vars(file);
            let transport = transport_of(entry, |key, text| McpValue {
                name: key.to_string(),
                secret: names_a_value(&text, bare),
                value: text,
            });
            if entry.get("type").and_then(Value::as_str) == Some("sse") {
                found.left_out.push(format!(
                    "{raw} ({file}) uses the old SSE transport, which Rovik doesn't speak."
                ));
                continue;
            }
            if user_ids.contains(&identity(&transport, false))
                || !seen.insert(identity(&transport, true))
            {
                continue;
            }
            let wanted = name_for(&raw, &mut BTreeSet::new());
            if user_names.contains(&wanted) {
                found.left_out.push(format!(
                    "{wanted} ({file}) isn't used: one of the user's MCP servers has its name, and \
                     the user's own wins."
                ));
                continue;
            }
            found.servers.push(ProjectServer {
                name: name_for(&raw, &mut taken),
                file: file.to_string(),
                hash: definition_hash(entry),
                variables: variables_of(&transport, &inputs, bare),
                transport,
            });
        }
    }
    found
}

/// A definition's hash: its entry as the file has it, keys in order.
fn definition_hash(entry: &Map<String, Value>) -> String {
    let canonical = serde_json::to_string(&Value::Object(entry.clone())).unwrap_or_default();
    crate::file_guard::sha256_hex(canonical.as_bytes())
}

/// The hashes of the project's servers the user approved, as its settings
/// keep them (`mcpApprovals`).
pub fn approved(settings: &Value) -> BTreeSet<String> {
    let approvals = settings.get(APPROVALS).and_then(Value::as_array);
    (approvals.into_iter().flatten())
        .filter_map(|a| a.get("hash").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The names of the variables the project's approved servers take, which
/// a start brings the values of to a host (Q11, Q12).
pub fn approved_variables(settings: &Value) -> BTreeSet<String> {
    let approvals = settings.get(APPROVALS).and_then(Value::as_array);
    (approvals.into_iter().flatten())
        .filter_map(|a| a.get("variables").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Whether `name` is a variable's, as `read` names them: an env var's
/// (`GITHUB_TOKEN`), or an input's (`input:token`), with no `/`: kept
/// under the project, it never reaches another's place in the keychain.
pub fn is_variable_name(name: &str) -> bool {
    let input = name.strip_prefix("input:");
    let plain = |n: &str| {
        n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    match input {
        Some(id) => !id.trim().is_empty() && !id.contains('/') && !id.chars().any(char::is_control),
        None => plain(name),
    }
}

/// Where a project's variables' values are kept, as a server's secrets
/// are: `project/<project id>`, by variable.
pub fn values_id(project: &str) -> String {
    format!("{PROJECT_SERVERS}{project}")
}

/// Where the values of `project`'s variables `names` go (`values_id`),
/// checked first as the app takes them from its page: an id with no `/`
/// and variables' names (`is_variable_name`), so that no value reaches
/// another's place in the keychain. Else what's wrong.
pub fn values_place<'a>(
    project: &str,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<String, String> {
    if project.is_empty() || project.contains('/') {
        return Err(format!("{project} isn't a project's id."));
    }
    if let Some(name) = names.into_iter().find(|name| !is_variable_name(name)) {
        return Err(format!("{name} isn't a variable's name."));
    }
    Ok(values_id(project))
}

/// What this computer's environment has for the variables `names`, to
/// fill their fields with when the user allows a project's servers: an
/// env var's value, as a server started here would see it; nothing for an
/// input or a variable not set.
pub fn from_environment<'a>(
    names: impl IntoIterator<Item = &'a String>,
) -> BTreeMap<String, String> {
    (names.into_iter())
        .filter(|name| is_variable_name(name) && !name.starts_with("input:"))
        .filter_map(|name| {
            let value = crate::process::child_var(name)?;
            Some((name.clone(), value.to_string_lossy().into_owned()))
        })
        .collect()
}

/// `server` as Rovik runs it for `project` in `folder`: its id
/// (`project/<project id>/<hash>`), its variables filled from `value_of`
/// (else their defaults), those of a value the user gives kept as secrets,
/// their text by key (`env/NAME`, `header/Name`). None while a variable
/// has neither a value nor a default: the user is asked for it.
pub fn resolved(
    server: &ProjectServer,
    project: &str,
    folder: &Path,
    value_of: impl Fn(&str) -> Option<String>,
) -> Option<(McpServer, BTreeMap<String, String>)> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let bare = bare_vars(&server.file);
    let mut missing = false;
    let mut fill = |text: &str| {
        substitute(text, bare, |variable| match variable {
            Named::Folder => folder.to_string_lossy().into_owned(),
            Named::Home => home.clone(),
            Named::Given(name, default) => {
                let given = value_of(name).or_else(|| default.clone());
                missing |= given.is_none();
                given.unwrap_or_default()
            }
        })
    };
    let mut secrets = BTreeMap::new();
    let mut values =
        |list: &[McpValue], transport: &McpTransport, fill: &mut dyn FnMut(&str) -> String| {
            (list.iter())
                .map(|v| {
                    let value = fill(&v.value);
                    if !names_a_value(&v.value, bare) {
                        return McpValue {
                            name: v.name.clone(),
                            value,
                            secret: false,
                        };
                    }
                    secrets.insert(transport.secret_key(&v.name), value);
                    McpValue {
                        name: v.name.clone(),
                        value: String::new(),
                        secret: true,
                    }
                })
                .collect::<Vec<_>>()
        };
    let transport = match &server.transport {
        McpTransport::Command { command, args, env } => McpTransport::Command {
            command: fill(command),
            args: args.iter().map(|a| fill(a)).collect(),
            env: values(env, &server.transport, &mut fill),
        },
        McpTransport::Url { url, headers } => McpTransport::Url {
            url: fill(url),
            headers: values(headers, &server.transport, &mut fill),
        },
    };
    if missing {
        return None;
    }
    let id = format!("{}/{}", values_id(project), server.hash);
    let resolved = McpServer {
        id,
        name: server.name.clone(),
        enabled: true,
        transport,
        disabled_tools: Vec::new(),
    };
    Some((resolved, secrets))
}

#[cfg(test)]
mod tests;

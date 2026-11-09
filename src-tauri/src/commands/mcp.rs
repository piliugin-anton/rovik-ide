//! Settings → MCP servers (docs/design/mcp-plan.md, Q02, Q04): where the
//! servers' secret values go, saving and forgetting them, Test, and the
//! servers in JSON pasted from another client's setup (Q01). The
//! servers themselves are AI settings; their secret values go in the
//! keychain (or the encrypted file in its place), never in a settings
//! file, and the page only learns where they go. The values the user
//! gives the variables of a project's own servers go there too
//! (mcp-plan-2.md, Q12), and so do the servers' sign-ins made here (Q14):
//! this computer's own, while a host's goes to its daemon.

use rovik_client::desktop_api::{mcp::McpRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_core::mcp::McpServer;
use rovik_protocol::Secret;
use std::collections::HashMap;
use tauri::State;

/// Where the secret values go: the keychain, or the encrypted file in its
/// place and whether its passphrase was given (Cloud hosts' `hosts_unlock`).
/// Off the async threads, as finding the keychain may take a moment.
#[tauri::command]
pub async fn mcp_store(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::Store))
        .await
}

/// Keeps the secret values typed for `server` (its id), by key (`env/NAME`,
/// `header/Name`): a value is saved, an empty one forgets the saved one, one
/// not given stays. The server starts again with them at its next use,
/// also when some couldn't be kept. Off the async threads, as the keychain
/// may take a moment or ask the user.
#[tauri::command]
pub async fn mcp_save_secrets(
    backend: State<'_, DesktopConnection>,
    server: String,
    values: HashMap<String, Secret>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::SaveSecrets {
            server,
            values,
        }))
        .await
}

/// Starts `server` as the page has it, with the secret values typed there
/// (by key) over the saved ones, lists its tools and stops it: a command
/// in `root`, the open project's folder (the home folder without one). With
/// a `host`, its daemon starts the server instead, with this computer's saved
/// secrets under the typed ones.
#[tauri::command]
pub async fn mcp_test(
    backend: State<'_, DesktopConnection>,
    server: McpServer,
    values: HashMap<String, Secret>,
    root: Option<String>,
    host: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::Test {
            server,
            values,
            root,
            host,
        }))
        .await
}

/// The servers in `text`, JSON pasted from another client's setup, named
/// apart from the `taken` names (Q01): a secret's value is in it, for the
/// page to save as a form's.
#[tauri::command]
pub async fn mcp_import(
    backend: State<'_, DesktopConnection>,
    text: String,
    taken: Vec<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::Import { text, taken }))
        .await
}

/// Keeps the values the user gave for `project`'s variables, by name
/// (mcp-plan-2.md, Q12): a value is saved, an empty one forgets the saved
/// one. The project's servers that run stop, also when some couldn't be
/// kept, and take them at their next run. Off the async threads, as the
/// keychain may take a moment or ask the user.
#[tauri::command]
pub async fn mcp_save_project_values(
    backend: State<'_, DesktopConnection>,
    project: String,
    values: HashMap<String, Secret>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::SaveProjectValues {
            project,
            values,
        }))
        .await
}

/// What the fields of a project's variables start with when the user is
/// asked to allow its servers (Q12).
/// The fields of `project`'s variables `names` (Q12): what this computer's
/// environment has for them, and which have a value saved already.
#[tauri::command]
pub async fn mcp_project_prefill(
    backend: State<'_, DesktopConnection>,
    project: String,
    names: Vec<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::ProjectPrefill {
            project,
            names,
        }))
        .await
}

/// Signs in through the requesting window's browser while the backend owns OAuth and credentials.
#[tauri::command]
pub async fn mcp_sign_in(
    backend: State<'_, DesktopConnection>,
    server: McpServer,
    values: HashMap<String, Secret>,
    host: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_task(
            |sink| {
                McpRequest::SignIn {
                    server,
                    values,
                    host,
                    sink,
                }
                .into()
            },
            open_sign_in,
        )
        .await
}

/// Resolves a project server and signs in through the requesting window.
#[tauri::command]
pub async fn mcp_project_sign_in(
    backend: State<'_, DesktopConnection>,
    root: String,
    project: String,
    hash: String,
    host: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_task(
            |sink| {
                McpRequest::ProjectSignIn {
                    root,
                    project,
                    hash,
                    host,
                    sink,
                }
                .into()
            },
            open_sign_in,
        )
        .await
}

fn open_sign_in(value: serde_json::Value) -> Result<(), String> {
    let url = value.as_str().ok_or("Invalid sign-in browser request")?;
    crate::open_url::open(url)
}

/// Signs MCP server `server` (its id) out of every place (Q14): this
/// computer, and each host, now or, for one that's away, once it next
/// connects. Only this computer's can fail, and says so.
#[tauri::command]
pub async fn mcp_sign_out(
    backend: State<'_, DesktopConnection>,
    server: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::SignOut { server }))
        .await
}

/// Where the MCP servers are signed in (Q14): which of `servers` (their
/// ids) here, and on each of `hosts` that can say, each with the scopes
/// it asked for that its sign-in there lacks.
/// Which of `servers` are signed in here, and on each of `hosts`; a host
/// that doesn't answer is left out, as not known.
#[tauri::command]
pub async fn mcp_signed_in(
    backend: State<'_, DesktopConnection>,
    servers: Vec<String>,
    hosts: Vec<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(McpRequest::SignedIn {
            servers,
            hosts,
        }))
        .await
}

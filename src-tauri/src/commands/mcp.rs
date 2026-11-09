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
use rovik_client::mcp_sign_in::{KeychainGrants, KeychainRegistrations};
use rovik_client::secrets::SecretStore;
use rovik_client::Remote;
use rovik_core::mcp::oauth::GrantStore;
use rovik_core::mcp::McpServer;
use rovik_core::Core;
use rovik_protocol::Secret;
use std::collections::HashMap;
use std::sync::Arc;
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

/// The values typed on the page that were given, by key.
fn typed(values: HashMap<String, Secret>) -> HashMap<String, String> {
    (values.into_iter())
        .filter(|(_, value)| !value.0.is_empty())
        .map(|(key, value)| (key, value.0.clone()))
        .collect()
}

/// Signs in to URL server `server` in the browser (mcp-plan-2.md, Q14),
/// its values as typed on the page over the saved ones: for this computer,
/// or for `host`, whose daemon keeps the grant and refreshes it from then
/// on, asking for the scopes the server asked that place for too. Each
/// place has a sign-in of its own: refresh tokens rotate, and two places
/// sharing one would sign each other out.
#[tauri::command]
pub async fn mcp_sign_in(
    secrets: State<'_, Arc<SecretStore>>,
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    server: McpServer,
    values: HashMap<String, Secret>,
    host: Option<String>,
) -> Result<(), String> {
    let wants = wanted(&secrets, &remote, &server.id, host.as_deref()).await?;
    let registrations = Arc::new(KeychainRegistrations(Arc::clone(&secrets)));
    let grant = core
        .mcp
        .sign_in(
            &server,
            &typed(values),
            wants.as_deref(),
            registrations,
            &crate::open_url::open,
        )
        .await?;
    if let Some(host) = host {
        return remote
            .mcp_sign_in(&host, &server.id, Some(grant))
            .await
            .map_err(|e| e.message);
    }
    let (grants, id) = (KeychainGrants(Arc::clone(&secrets)), server.id.clone());
    rovik_core::rt::spawn_blocking(move || grants.put(&id, &grant))
        .await
        .map_err(|e| e.to_string())??;
    core.mcp.forget(&server.id);
    Ok(())
}

/// The scopes the server `server` (its id) asked for at `host`, or here,
/// that its sign-in there lacks: what a new sign-in there asks for too. A
/// sign-in here that can't be read wants none: the new one replaces it.
async fn wanted(
    secrets: &Arc<SecretStore>,
    remote: &Remote,
    server: &str,
    host: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(host) = host {
        let signed = remote.mcp_signed_in(host).await.map_err(|e| e.message)?;
        let there = signed.into_iter().find(|s| s.server == server);
        return Ok(there.and_then(|s| s.wants));
    }
    let (grants, id) = (KeychainGrants(Arc::clone(secrets)), server.to_string());
    let kept = rovik_core::rt::spawn_blocking(move || grants.get(&id))
        .await
        .map_err(|e| e.to_string())?;
    Ok(kept.ok().flatten().and_then(|grant| grant.wants))
}

/// Signs in to the project's own URL server `hash` (Q12, Q14), as the
/// files at `root` set it up and the values saved for `project`'s
/// variables fill it: for this computer, or for `host`, as `mcp_sign_in`
/// does for the user's. Kept under the server's id there.
#[tauri::command]
pub async fn mcp_project_sign_in(
    secrets: State<'_, Arc<SecretStore>>,
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    root: String,
    project: String,
    hash: String,
    host: Option<String>,
) -> Result<(), String> {
    use rovik_core::mcp::project as files;
    let folder = std::path::PathBuf::from(&root);
    let read_from = folder.clone();
    let found = rovik_core::rt::spawn_blocking(move || files::read(&read_from, &[]))
        .await
        .map_err(|e| e.to_string())?;
    let server = (found.servers.into_iter())
        .find(|s| s.hash == hash)
        .ok_or("The project's files no longer set up this server as it was allowed.")?;
    let names = server.variables.iter().map(|v| v.name.clone()).collect();
    let values = core.mcp.project_values(&project, &names).await;
    let (server, given) =
        files::resolved(&server, &project, &folder, |name| values.get(name).cloned()).ok_or(
            "A variable of the server has no value: allow it again from its chat, giving one.",
        )?;
    let given: HashMap<String, Secret> = given.into_iter().map(|(k, v)| (k, Secret(v))).collect();
    mcp_sign_in(secrets, core, remote, server, given, host).await
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

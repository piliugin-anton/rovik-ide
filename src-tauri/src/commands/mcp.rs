//! Settings → MCP servers (docs/design/mcp-plan.md, Q02, Q04): where the
//! servers' secret values go, saving and forgetting them, Test, and the
//! servers in JSON pasted from another client's setup (Q01). The
//! servers themselves are AI settings; their secret values go in the
//! keychain (or the encrypted file in its place), never in a settings
//! file, and the page only learns where they go. The values the user
//! gives the variables of a project's own servers go there too
//! (mcp-plan-2.md, Q12).

use rovik_client::secrets::{SecretName, SecretStore, StoreKind};
use rovik_client::Remote;
use rovik_core::mcp::{McpServer, Tested};
use rovik_core::Core;
use rovik_protocol::mcp::is_secret_key;
use rovik_protocol::Secret;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tauri::State;

/// Where the secret values go: the keychain, or the encrypted file in its
/// place and whether its passphrase was given (Cloud hosts' `hosts_unlock`).
/// Off the async threads, as finding the keychain may take a moment.
#[tauri::command]
pub async fn mcp_store(secrets: State<'_, Arc<SecretStore>>) -> Result<StoreKind, String> {
    let secrets = Arc::clone(&secrets);
    rovik_core::rt::spawn_blocking(move || secrets.kind())
        .await
        .map_err(|e| e.to_string())
}

/// Keeps the secret values typed for `server` (its id), by key (`env/NAME`,
/// `header/Name`): a value is saved, an empty one forgets the saved one, one
/// not given stays. The server starts again with them at its next use,
/// also when some couldn't be kept. Off the async threads, as the keychain
/// may take a moment or ask the user.
#[tauri::command]
pub async fn mcp_save_secrets(
    secrets: State<'_, Arc<SecretStore>>,
    core: State<'_, Core>,
    server: String,
    values: HashMap<String, Secret>,
) -> Result<(), String> {
    if let Some(key) = values.keys().find(|key| !is_secret_key(key)) {
        return Err(format!("{key} isn't an env var's or a header's key."));
    }
    let secrets = Arc::clone(&secrets);
    let id = server.clone();
    let kept = rovik_core::rt::spawn_blocking(move || {
        for (key, value) in &values {
            let name = SecretName::McpValue {
                server: id.clone(),
                key: key.clone(),
            };
            match value.0.is_empty() {
                true => secrets.delete(&name),
                false => secrets.set(&name, value),
            }
            .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?;
    core.mcp.forget(&server);
    kept
}

/// Starts `server` as the page has it, with the secret values typed there
/// (by key) over the saved ones, lists its tools and stops it: a command
/// in `root`, the open project's folder (the home folder without one). With
/// a `host`, its daemon starts the server instead, with this computer's saved
/// secrets under the typed ones.
#[tauri::command]
pub async fn mcp_test(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    server: McpServer,
    values: HashMap<String, Secret>,
    root: Option<String>,
    host: Option<String>,
) -> Result<Tested, String> {
    if let Some(host) = host {
        let given = values.into_iter().collect();
        return remote
            .mcp_test(&host, server, given)
            .await
            .map_err(|e| e.message);
    }
    let given: HashMap<String, String> = values
        .iter()
        .filter(|(_, value)| !value.0.is_empty())
        .map(|(key, value)| (key.clone(), value.0.clone()))
        .collect();
    core.mcp
        .test(&server, &given, root.as_deref().map(Path::new), None)
        .await
}

/// The servers in `text`, JSON pasted from another client's setup, named
/// apart from the `taken` names (Q01): a secret's value is in it, for the
/// page to save as a form's.
#[tauri::command]
pub fn mcp_import(text: String, taken: Vec<String>) -> Result<Vec<McpServer>, String> {
    rovik_core::mcp::import(&text, &taken)
}

/// Keeps the values the user gave for `project`'s variables, by name
/// (mcp-plan-2.md, Q12): a value is saved, an empty one forgets the saved
/// one. The project's servers that run stop, also when some couldn't be
/// kept, and take them at their next run. Off the async threads, as the
/// keychain may take a moment or ask the user.
#[tauri::command]
pub async fn mcp_save_project_values(
    secrets: State<'_, Arc<SecretStore>>,
    core: State<'_, Core>,
    project: String,
    values: HashMap<String, Secret>,
) -> Result<(), String> {
    let server = rovik_core::mcp::project::values_place(&project, values.keys())?;
    let secrets = Arc::clone(&secrets);
    let place = server.clone();
    let kept = rovik_core::rt::spawn_blocking(move || {
        for (key, value) in &values {
            let name = SecretName::McpValue {
                server: server.clone(),
                key: key.clone(),
            };
            match value.0.is_empty() {
                true => secrets.delete(&name),
                false => secrets.set(&name, value),
            }
            .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?;
    core.mcp.forget(&place);
    kept
}

/// What the fields of a project's variables start with when the user is
/// asked to allow its servers (Q12).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPrefill {
    /// This computer's environment's values, by variable.
    values: HashMap<String, Secret>,
    /// The variables whose values the keychain has already: none is asked
    /// for again.
    saved: Vec<String>,
}

/// The fields of `project`'s variables `names` (Q12): what this computer's
/// environment has for them, and which have a value saved already.
#[tauri::command]
pub async fn mcp_project_prefill(
    core: State<'_, Core>,
    project: String,
    names: Vec<String>,
) -> Result<ProjectPrefill, String> {
    use rovik_core::mcp::project;
    project::values_place(&project, &names)?;
    let names: std::collections::BTreeSet<String> = names.into_iter().collect();
    let saved = core.mcp.project_values(&project, &names).await;
    let values = project::from_environment(&names);
    Ok(ProjectPrefill {
        values: values.into_iter().map(|(k, v)| (k, Secret(v))).collect(),
        saved: saved.into_keys().collect(),
    })
}

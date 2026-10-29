//! Settings → MCP servers (docs/design/mcp-plan.md, Q02, Q04): where the
//! servers' secret values go, saving and forgetting them, and Test. The
//! servers themselves are AI settings; their secret values go in the
//! keychain (or the encrypted file in its place), never in a settings
//! file, and the page only learns where they go.

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

//! Backend-owned mcp operations.

use crate::mcp_sign_in::KeychainGrants;
use crate::secrets::{SecretName, StoreKind};
use rovik_core::mcp::oauth::GrantStore;
use rovik_core::mcp::{McpServer, Tested};
use rovik_protocol::mcp::{is_secret_key, SignedInServer};
use rovik_protocol::Secret;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

/// Desktop mcp operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum McpRequest {
    /// mcp_store on the backend.
    Store,
    /// mcp_save_secrets on the backend.
    SaveSecrets {
        server: String,
        values: HashMap<String, Secret>,
    },
    /// mcp_test on the backend.
    Test {
        server: McpServer,
        values: HashMap<String, Secret>,
        root: Option<String>,
        host: Option<String>,
    },
    /// mcp_import on the backend.
    Import { text: String, taken: Vec<String> },
    /// mcp_save_project_values on the backend.
    SaveProjectValues {
        project: String,
        values: HashMap<String, Secret>,
    },
    /// mcp_project_prefill on the backend.
    ProjectPrefill { project: String, names: Vec<String> },
    /// mcp_sign_out on the backend.
    SignOut { server: String },
    /// mcp_signed_in on the backend.
    SignedIn {
        servers: Vec<String>,
        hosts: Vec<String>,
    },
}

impl McpRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::Store => super::encode_result(mcp_store(backend).await),
            Self::SaveSecrets { server, values } => {
                super::encode_result(mcp_save_secrets(backend, server, values).await)
            }
            Self::Test {
                server,
                values,
                root,
                host,
            } => super::encode_result(mcp_test(backend, server, values, root, host).await),
            Self::Import { text, taken } => {
                super::encode_result(mcp_import(backend, text, taken).await)
            }
            Self::SaveProjectValues { project, values } => {
                super::encode_result(mcp_save_project_values(backend, project, values).await)
            }
            Self::ProjectPrefill { project, names } => {
                super::encode_result(mcp_project_prefill(backend, project, names).await)
            }
            Self::SignOut { server } => super::encode_result(mcp_sign_out(backend, server).await),
            Self::SignedIn { servers, hosts } => {
                super::encode_result(mcp_signed_in(backend, servers, hosts).await)
            }
        }
    }
}

/// Variable values available to the project server setup form.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPrefill {
    /// This computer's environment's values, by variable.
    values: HashMap<String, Secret>,
    /// The variables whose values the keychain has already: none is asked
    /// for again.
    saved: Vec<String>,
}
/// Sign-ins reported locally and by reachable hosts.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedIn {
    here: Vec<SignedInServer>,
    hosts: HashMap<String, Vec<SignedInServer>>,
}
async fn mcp_store(backend: &DesktopBackend) -> Result<StoreKind, String> {
    let secrets = &backend.secrets;
    let secrets = Arc::clone(secrets);
    rovik_core::rt::spawn_blocking(move || secrets.kind())
        .await
        .map_err(|e| e.to_string())
}

async fn mcp_save_secrets(
    backend: &DesktopBackend,
    server: String,
    values: HashMap<String, Secret>,
) -> Result<(), String> {
    let secrets = &backend.secrets;
    let core = &backend.core;
    if let Some(key) = values.keys().find(|key| !is_secret_key(key)) {
        return Err(format!("{key} isn't an env var's or a header's key."));
    }
    let secrets = Arc::clone(secrets);
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

async fn mcp_test(
    backend: &DesktopBackend,
    server: McpServer,
    values: HashMap<String, Secret>,
    root: Option<String>,
    host: Option<String>,
) -> Result<Tested, String> {
    let core = &backend.core;
    let remote = &backend.remote;
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

async fn mcp_import(
    backend: &DesktopBackend,
    text: String,
    taken: Vec<String>,
) -> Result<Vec<McpServer>, String> {
    let _ = backend;
    rovik_core::mcp::import(&text, &taken)
}

async fn mcp_save_project_values(
    backend: &DesktopBackend,
    project: String,
    values: HashMap<String, Secret>,
) -> Result<(), String> {
    let secrets = &backend.secrets;
    let core = &backend.core;
    let server = rovik_core::mcp::project::values_place(&project, values.keys())?;
    let secrets = Arc::clone(secrets);
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

async fn mcp_project_prefill(
    backend: &DesktopBackend,
    project: String,
    names: Vec<String>,
) -> Result<ProjectPrefill, String> {
    let core = &backend.core;
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

async fn mcp_sign_out(backend: &DesktopBackend, server: String) -> Result<(), String> {
    let secrets = &backend.secrets;
    let core = &backend.core;
    let remote = &backend.remote;
    let (grants, id) = (KeychainGrants(Arc::clone(secrets)), server.clone());
    let here = rovik_core::rt::spawn_blocking(move || grants.forget(&id)).await;
    core.mcp.forget(&server);
    remote.mcp_sign_out(&server).await;
    (here.map_err(|e| e.to_string()).and_then(|forgot| forgot))
        .map_err(|e| format!("Not signed out on this computer: {e}"))
}

async fn mcp_signed_in(
    backend: &DesktopBackend,
    servers: Vec<String>,
    hosts: Vec<String>,
) -> Result<SignedIn, String> {
    let secrets = &backend.secrets;
    let remote = &backend.remote;
    let (grants, ids) = (KeychainGrants(Arc::clone(secrets)), servers.clone());
    let here = rovik_core::rt::spawn_blocking(move || {
        let signed = |id: String| match grants.get(&id) {
            Ok(Some(grant)) => Some(SignedInServer {
                server: id,
                wants: grant.wants,
            }),
            _ => None,
        };
        ids.into_iter().filter_map(signed).collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut on_hosts = HashMap::new();
    for host in hosts {
        match remote.mcp_signed_in(&host).await {
            Ok(signed) => {
                let ours = (signed.into_iter())
                    .filter(|s| servers.contains(&s.server))
                    .collect();
                on_hosts.insert(host, ours);
            }
            Err(e) => log::info!(
                "{} didn't say which MCP servers are signed in: {}",
                remote.host_name(&host),
                e.message
            ),
        }
    }
    Ok(SignedIn {
        here,
        hosts: on_hosts,
    })
}

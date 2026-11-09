//! Settings → Cloud hosts (U01): the hosts this computer knows, each one's
//! connection test, its deploy, upgrade and removal, and what its daemon
//! says. Thin wrappers over `rovik_client::hosts_page`. A connection
//! test's steps come on `host-test` as each one ends.

use rovik_client::connection_test::Step;
use rovik_client::deploy::Removal;
use rovik_client::desktop_api::{hosts::HostsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_client::host_settings::Host;
use rovik_client::hosts_page::{HostSecrets, HostsPage};
use rovik_client::ApprovalPolicy;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
/// The event a connection test's steps come on.
const TEST_EVENT: &str = "host-test";

/// A step of `host_id`'s connection test, as it ends.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TestStep {
    host_id: String,
    step: Step,
}

#[tauri::command]
pub async fn hosts_list(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::List))
        .await
}

/// The hosts' names and connections, for the chats.
#[tauri::command]
pub async fn hosts_summaries(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Summaries))
        .await
}

/// Adds or changes a host, and its secrets: none leaves one as it is, an
/// empty one forgets it. Off the async threads, as the keychain may take a
/// moment or ask the user.
#[tauri::command]
pub async fn hosts_save(
    backend: State<'_, DesktopConnection>,
    host: Host,
    secrets: Option<HostSecrets>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Save {
            host: Box::new(host),
            secrets,
        }))
        .await
}

#[tauri::command]
pub async fn hosts_remove(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Remove { id }))
        .await
}

/// Tests the host's connection, each step on `host-test` as it ends.
#[tauri::command]
pub async fn hosts_test(
    app: AppHandle,
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<Vec<Step>, String> {
    let host_id = id.clone();
    let report = move |step: &Step| {
        let told = TestStep {
            host_id: host_id.clone(),
            step: step.clone(),
        };
        if let Err(e) = app.emit(TEST_EVENT, told) {
            eprintln!("hosts: a test's step couldn't be told: {e}");
        }
    };
    page.test(&id, &report).await
}

/// Pins the host's key, once the user compared its fingerprint.
#[tauri::command]
pub async fn hosts_trust_key(
    backend: State<'_, DesktopConnection>,
    id: String,
    key: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::TrustKey { id, key }))
        .await
}

/// Trusts the build of Rovik the host runs now, once the user compared it.
#[tauri::command]
pub async fn hosts_trust_build(
    backend: State<'_, DesktopConnection>,
    id: String,
    sha256: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::TrustBuild {
            id,
            sha256,
        }))
        .await
}

#[tauri::command]
pub async fn hosts_set_policy(
    backend: State<'_, DesktopConnection>,
    id: String,
    policy: ApprovalPolicy,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::SetPolicy { id, policy }))
        .await
}

/// What the host's tasks follow while it works on its own (H01), as kept.
#[tauri::command]
pub async fn hosts_set_autonomy(
    backend: State<'_, DesktopConnection>,
    id: String,
    autonomy: rovik_protocol::autopilot::AutonomySettings,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::SetAutonomy {
            id,
            autonomy,
        }))
        .await
}

/// This computer's chats on the host and their tasks (autopilot-plan H08).
#[tauri::command]
pub async fn hosts_tasks(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Tasks { id }))
        .await
}

/// "Test delivery" (autopilot-plan H05): what the host's push found.
#[tauri::command]
pub async fn hosts_test_delivery(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::TestDelivery { id }))
        .await
}

/// Deploys Rovik for this user; `sudo_password` when sudo asked for one.
/// Returns the daemon's version.
#[tauri::command]
pub async fn hosts_deploy(
    backend: State<'_, DesktopConnection>,
    id: String,
    sudo_password: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Deploy {
            id,
            sudo_password,
        }))
        .await
}

#[tauri::command]
pub async fn hosts_upgrade(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Upgrade { id }))
        .await
}

/// Runs the host's setup script: after a deploy, and when the user asks.
#[tauri::command]
pub async fn hosts_run_setup(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::RunSetup { id }))
        .await
}

/// Opens the file that keeps secrets where there's no keychain, with its
/// passphrase, which isn't kept anywhere else. Its key takes a moment to
/// derive, so off the async threads.
#[tauri::command]
pub async fn hosts_unlock(
    backend: State<'_, DesktopConnection>,
    passphrase: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Unlock { passphrase }))
        .await
}

/// What removing Rovik for this user would delete on the host.
#[tauri::command]
pub async fn hosts_removal(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Removal { id }))
        .await
}

/// Removes Rovik for this user, as `hosts_removal` listed it.
#[tauri::command]
pub async fn hosts_remove_rovik(
    backend: State<'_, DesktopConnection>,
    id: String,
    removal: Removal,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::RemoveRovik {
            id,
            removal,
        }))
        .await
}

#[tauri::command]
pub async fn hosts_details(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Details { id }))
        .await
}

/// "Remove keys from this server": how many went.
#[tauri::command]
pub async fn hosts_remove_keys(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::RemoveKeys { id }))
        .await
}

/// "Clean up now" (R01): what Rovik's folders take on the host after, and
/// how much it freed.
#[tauri::command]
pub async fn hosts_clean_up(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::CleanUp { id }))
        .await
}

/// The most the host's chats' working copies may take, or none: what
/// Rovik's folders take there once it's fitted.
#[tauri::command]
pub async fn hosts_storage_budget(
    backend: State<'_, DesktopConnection>,
    id: String,
    bytes: Option<u64>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::StorageBudget {
            id,
            bytes,
        }))
        .await
}

/// Whether the host's storage budget leaves the chat ready.
#[tauri::command]
pub async fn hosts_keep_ready(
    backend: State<'_, DesktopConnection>,
    id: String,
    chat: String,
    keep: bool,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::KeepReady {
            id,
            chat,
            keep,
        }))
        .await
}

#[tauri::command]
pub async fn hosts_limits(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::Limits { id }))
        .await
}

#[tauri::command]
pub async fn hosts_set_limits(
    backend: State<'_, DesktopConnection>,
    id: String,
    changes: Vec<(String, String)>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HostsRequest::SetLimits {
            id,
            changes,
        }))
        .await
}

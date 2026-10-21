//! Settings → Cloud hosts (U01): the hosts this computer knows, each one's
//! connection test, its deploy, upgrade and removal, and what its daemon
//! says. Thin wrappers over `rovik_client::hosts_page`. A connection
//! test's steps come on `host-test` as each one ends.

use rovik_client::connection_test::Step;
use rovik_client::deploy::{Removal, SetupRun};
use rovik_client::host_settings::Host;
use rovik_client::hosts_page::{
    CleanedUp, DeployFailure, HostDetails, HostSecrets, HostSummary, HostsList, HostsPage,
};
use rovik_client::ApprovalPolicy;
use rovik_protocol::Secret;
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
pub async fn hosts_list(page: State<'_, Arc<HostsPage>>) -> Result<HostsList, String> {
    page.list().await
}

/// The hosts' names and connections, for the chats.
#[tauri::command]
pub async fn hosts_summaries(page: State<'_, Arc<HostsPage>>) -> Result<Vec<HostSummary>, String> {
    page.summaries()
}

/// Adds or changes a host, and its secrets: none leaves one as it is, an
/// empty one forgets it. Off the async threads, as the keychain may take a
/// moment or ask the user.
#[tauri::command]
pub async fn hosts_save(
    page: State<'_, Arc<HostsPage>>,
    host: Host,
    secrets: Option<HostSecrets>,
) -> Result<Host, String> {
    let page = Arc::clone(&page);
    rovik_core::rt::spawn_blocking(move || page.save(host, secrets.unwrap_or_default()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn hosts_remove(page: State<'_, Arc<HostsPage>>, id: String) -> Result<(), String> {
    page.remove(&id).await
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
    page: State<'_, Arc<HostsPage>>,
    id: String,
    key: String,
) -> Result<(), String> {
    page.trust_key(&id, &key)
}

/// Trusts the build of Rovik the host runs now, once the user compared it.
#[tauri::command]
pub async fn hosts_trust_build(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    sha256: String,
) -> Result<(), String> {
    page.trust_build(&id, &sha256)
}

#[tauri::command]
pub async fn hosts_set_policy(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    policy: ApprovalPolicy,
) -> Result<(), String> {
    page.set_policy(&id, policy).await
}

/// What the host's tasks follow while it works on its own (H01), as kept.
#[tauri::command]
pub async fn hosts_set_autonomy(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    autonomy: rovik_protocol::autopilot::AutonomySettings,
) -> Result<rovik_protocol::autopilot::AutonomySettings, String> {
    page.set_autonomy(&id, autonomy).await
}

/// This computer's chats on the host and their tasks (autopilot-plan H08).
#[tauri::command]
pub async fn hosts_tasks(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<Vec<rovik_protocol::autopilot::ChatTask>, String> {
    page.tasks(&id).await
}

/// "Test delivery" (autopilot-plan H05): what the host's push found.
#[tauri::command]
pub async fn hosts_test_delivery(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<rovik_protocol::autopilot::DeliveryTest, String> {
    page.test_delivery(&id).await
}

/// Deploys Rovik for this user; `sudo_password` when sudo asked for one.
/// Returns the daemon's version.
#[tauri::command]
pub async fn hosts_deploy(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    sudo_password: Option<String>,
) -> Result<String, DeployFailure> {
    page.deploy(&id, sudo_password.map(Secret)).await
}

#[tauri::command]
pub async fn hosts_upgrade(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<(), DeployFailure> {
    page.upgrade(&id).await
}

/// Runs the host's setup script: after a deploy, and when the user asks.
#[tauri::command]
pub async fn hosts_run_setup(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<SetupRun, DeployFailure> {
    page.run_setup(&id).await
}

/// Opens the file that keeps secrets where there's no keychain, with its
/// passphrase, which isn't kept anywhere else. Its key takes a moment to
/// derive, so off the async threads.
#[tauri::command]
pub async fn hosts_unlock(
    page: State<'_, Arc<HostsPage>>,
    passphrase: String,
) -> Result<(), String> {
    let page = Arc::clone(&page);
    rovik_core::rt::spawn_blocking(move || page.unlock(&Secret(passphrase)))
        .await
        .map_err(|e| e.to_string())?
}

/// What removing Rovik for this user would delete on the host.
#[tauri::command]
pub async fn hosts_removal(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<Removal, DeployFailure> {
    page.removal(&id).await
}

/// Removes Rovik for this user, as `hosts_removal` listed it.
#[tauri::command]
pub async fn hosts_remove_rovik(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    removal: Removal,
) -> Result<(), DeployFailure> {
    page.remove_rovik(&id, &removal).await
}

#[tauri::command]
pub async fn hosts_details(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<HostDetails, String> {
    page.details(&id).await
}

/// "Remove keys from this server": how many went.
#[tauri::command]
pub async fn hosts_remove_keys(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<usize, String> {
    page.remove_keys(&id).await
}

/// "Clean up now" (R01): what Rovik's folders take on the host after, and
/// how much it freed.
#[tauri::command]
pub async fn hosts_clean_up(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<CleanedUp, String> {
    page.clean_up(&id).await
}

#[tauri::command]
pub async fn hosts_limits(
    page: State<'_, Arc<HostsPage>>,
    id: String,
) -> Result<Vec<(String, String)>, DeployFailure> {
    page.limits(&id).await
}

#[tauri::command]
pub async fn hosts_set_limits(
    page: State<'_, Arc<HostsPage>>,
    id: String,
    changes: Vec<(String, String)>,
) -> Result<(), DeployFailure> {
    page.set_limits(&id, &changes).await
}

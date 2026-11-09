//! Backend-owned hosts operations.

use crate::deploy::{Removal, SetupRun};
use crate::host_settings::Host;
use crate::hosts_page::{
    CleanedUp, DeployFailure, HostDetails, HostSecrets, HostSummary, HostsList,
};
use crate::ApprovalPolicy;
use rovik_protocol::host::Storage;
use rovik_protocol::Secret;
use std::sync::Arc;

use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

/// Desktop hosts operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum HostsRequest {
    /// hosts_list on the backend.
    List,
    /// hosts_summaries on the backend.
    Summaries,
    /// hosts_save on the backend.
    Save {
        host: Box<Host>,
        secrets: Option<HostSecrets>,
    },
    /// hosts_remove on the backend.
    Remove { id: String },
    /// hosts_trust_key on the backend.
    TrustKey { id: String, key: String },
    /// hosts_trust_build on the backend.
    TrustBuild { id: String, sha256: String },
    /// hosts_set_policy on the backend.
    SetPolicy { id: String, policy: ApprovalPolicy },
    /// hosts_set_autonomy on the backend.
    SetAutonomy {
        id: String,
        autonomy: rovik_protocol::autopilot::AutonomySettings,
    },
    /// hosts_tasks on the backend.
    Tasks { id: String },
    /// hosts_test_delivery on the backend.
    TestDelivery { id: String },
    /// hosts_deploy on the backend.
    Deploy {
        id: String,
        sudo_password: Option<String>,
    },
    /// hosts_upgrade on the backend.
    Upgrade { id: String },
    /// hosts_run_setup on the backend.
    RunSetup { id: String },
    /// hosts_unlock on the backend.
    Unlock { passphrase: String },
    /// hosts_removal on the backend.
    Removal { id: String },
    /// hosts_remove_rovik on the backend.
    RemoveRovik { id: String, removal: Removal },
    /// hosts_details on the backend.
    Details { id: String },
    /// hosts_remove_keys on the backend.
    RemoveKeys { id: String },
    /// hosts_clean_up on the backend.
    CleanUp { id: String },
    /// hosts_storage_budget on the backend.
    StorageBudget { id: String, bytes: Option<u64> },
    /// hosts_keep_ready on the backend.
    KeepReady {
        id: String,
        chat: String,
        keep: bool,
    },
    /// hosts_limits on the backend.
    Limits { id: String },
    /// hosts_set_limits on the backend.
    SetLimits {
        id: String,
        changes: Vec<(String, String)>,
    },
}

impl HostsRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::List => super::encode_result(hosts_list(backend).await),
            Self::Summaries => super::encode_result(hosts_summaries(backend).await),
            Self::Save { host, secrets } => {
                super::encode_result(hosts_save(backend, *host, secrets).await)
            }
            Self::Remove { id } => super::encode_result(hosts_remove(backend, id).await),
            Self::TrustKey { id, key } => {
                super::encode_result(hosts_trust_key(backend, id, key).await)
            }
            Self::TrustBuild { id, sha256 } => {
                super::encode_result(hosts_trust_build(backend, id, sha256).await)
            }
            Self::SetPolicy { id, policy } => {
                super::encode_result(hosts_set_policy(backend, id, policy).await)
            }
            Self::SetAutonomy { id, autonomy } => {
                super::encode_result(hosts_set_autonomy(backend, id, autonomy).await)
            }
            Self::Tasks { id } => super::encode_result(hosts_tasks(backend, id).await),
            Self::TestDelivery { id } => {
                super::encode_result(hosts_test_delivery(backend, id).await)
            }
            Self::Deploy { id, sudo_password } => {
                super::encode_result(hosts_deploy(backend, id, sudo_password).await)
            }
            Self::Upgrade { id } => super::encode_result(hosts_upgrade(backend, id).await),
            Self::RunSetup { id } => super::encode_result(hosts_run_setup(backend, id).await),
            Self::Unlock { passphrase } => {
                super::encode_result(hosts_unlock(backend, passphrase).await)
            }
            Self::Removal { id } => super::encode_result(hosts_removal(backend, id).await),
            Self::RemoveRovik { id, removal } => {
                super::encode_result(hosts_remove_rovik(backend, id, removal).await)
            }
            Self::Details { id } => super::encode_result(hosts_details(backend, id).await),
            Self::RemoveKeys { id } => super::encode_result(hosts_remove_keys(backend, id).await),
            Self::CleanUp { id } => super::encode_result(hosts_clean_up(backend, id).await),
            Self::StorageBudget { id, bytes } => {
                super::encode_result(hosts_storage_budget(backend, id, bytes).await)
            }
            Self::KeepReady { id, chat, keep } => {
                super::encode_result(hosts_keep_ready(backend, id, chat, keep).await)
            }
            Self::Limits { id } => super::encode_result(hosts_limits(backend, id).await),
            Self::SetLimits { id, changes } => {
                super::encode_result(hosts_set_limits(backend, id, changes).await)
            }
        }
    }
}

async fn hosts_list(backend: &DesktopBackend) -> Result<HostsList, String> {
    let page = &backend.hosts;
    page.list().await
}

async fn hosts_summaries(backend: &DesktopBackend) -> Result<Vec<HostSummary>, String> {
    let page = &backend.hosts;
    page.summaries()
}

async fn hosts_save(
    backend: &DesktopBackend,
    host: Host,
    secrets: Option<HostSecrets>,
) -> Result<Host, String> {
    let page = &backend.hosts;
    let page = Arc::clone(page);
    rovik_core::rt::spawn_blocking(move || page.save(host, secrets.unwrap_or_default()))
        .await
        .map_err(|e| e.to_string())?
}

async fn hosts_remove(backend: &DesktopBackend, id: String) -> Result<(), String> {
    let page = &backend.hosts;
    page.remove(&id).await
}

async fn hosts_trust_key(backend: &DesktopBackend, id: String, key: String) -> Result<(), String> {
    let page = &backend.hosts;
    page.trust_key(&id, &key)
}

async fn hosts_trust_build(
    backend: &DesktopBackend,
    id: String,
    sha256: String,
) -> Result<(), String> {
    let page = &backend.hosts;
    page.trust_build(&id, &sha256)
}

async fn hosts_set_policy(
    backend: &DesktopBackend,
    id: String,
    policy: ApprovalPolicy,
) -> Result<(), String> {
    let page = &backend.hosts;
    page.set_policy(&id, policy).await
}

async fn hosts_set_autonomy(
    backend: &DesktopBackend,
    id: String,
    autonomy: rovik_protocol::autopilot::AutonomySettings,
) -> Result<rovik_protocol::autopilot::AutonomySettings, String> {
    let page = &backend.hosts;
    page.set_autonomy(&id, autonomy).await
}

async fn hosts_tasks(
    backend: &DesktopBackend,
    id: String,
) -> Result<Vec<rovik_protocol::autopilot::ChatTask>, String> {
    let page = &backend.hosts;
    page.tasks(&id).await
}

async fn hosts_test_delivery(
    backend: &DesktopBackend,
    id: String,
) -> Result<rovik_protocol::autopilot::DeliveryTest, String> {
    let page = &backend.hosts;
    page.test_delivery(&id).await
}

async fn hosts_deploy(
    backend: &DesktopBackend,
    id: String,
    sudo_password: Option<String>,
) -> Result<String, DeployFailure> {
    let page = &backend.hosts;
    page.deploy(&id, sudo_password.map(Secret)).await
}

async fn hosts_upgrade(backend: &DesktopBackend, id: String) -> Result<(), DeployFailure> {
    let page = &backend.hosts;
    page.upgrade(&id).await
}

async fn hosts_run_setup(backend: &DesktopBackend, id: String) -> Result<SetupRun, DeployFailure> {
    let page = &backend.hosts;
    page.run_setup(&id).await
}

async fn hosts_unlock(backend: &DesktopBackend, passphrase: String) -> Result<(), String> {
    let page = &backend.hosts;
    let page = Arc::clone(page);
    rovik_core::rt::spawn_blocking(move || page.unlock(&Secret(passphrase)))
        .await
        .map_err(|e| e.to_string())?
}

async fn hosts_removal(backend: &DesktopBackend, id: String) -> Result<Removal, DeployFailure> {
    let page = &backend.hosts;
    page.removal(&id).await
}

async fn hosts_remove_rovik(
    backend: &DesktopBackend,
    id: String,
    removal: Removal,
) -> Result<(), DeployFailure> {
    let page = &backend.hosts;
    page.remove_rovik(&id, &removal).await
}

async fn hosts_details(backend: &DesktopBackend, id: String) -> Result<HostDetails, String> {
    let page = &backend.hosts;
    page.details(&id).await
}

async fn hosts_remove_keys(backend: &DesktopBackend, id: String) -> Result<usize, String> {
    let page = &backend.hosts;
    page.remove_keys(&id).await
}

async fn hosts_clean_up(backend: &DesktopBackend, id: String) -> Result<CleanedUp, String> {
    let page = &backend.hosts;
    page.clean_up(&id).await
}

async fn hosts_storage_budget(
    backend: &DesktopBackend,
    id: String,
    bytes: Option<u64>,
) -> Result<Storage, String> {
    let page = &backend.hosts;
    page.set_storage_budget(&id, bytes).await
}

async fn hosts_keep_ready(
    backend: &DesktopBackend,
    id: String,
    chat: String,
    keep: bool,
) -> Result<(), String> {
    let page = &backend.hosts;
    page.keep_ready(&id, &chat, keep).await
}

async fn hosts_limits(
    backend: &DesktopBackend,
    id: String,
) -> Result<Vec<(String, String)>, DeployFailure> {
    let page = &backend.hosts;
    page.limits(&id).await
}

async fn hosts_set_limits(
    backend: &DesktopBackend,
    id: String,
    changes: Vec<(String, String)>,
) -> Result<(), DeployFailure> {
    let page = &backend.hosts;
    page.set_limits(&id, &changes).await
}

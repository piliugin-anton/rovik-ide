//! Settings → This computer → WSL (Windows): the WSL distribution Rovik's
//! Linux side runs in, set up, tested, updated with the app and removed.
//! Thin wrappers over `rovik_client::hosts_page`'s WSL part; the test's
//! steps come on `host-test` with the host id `wsl`, as a cloud host's do.

use rovik_client::connection_test::Step;
use rovik_client::hosts_page::{DeployFailure, HostsPage, WslStatus, WSL_HOST};
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn wsl_status(page: State<'_, Arc<HostsPage>>) -> Result<WslStatus, String> {
    page.wsl_status().await
}

/// Sets `distro` up as this computer's Linux side, as `user` in it (its
/// default user when none). Returns the daemon's version.
#[tauri::command]
pub async fn wsl_setup(
    page: State<'_, Arc<HostsPage>>,
    distro: String,
    user: Option<String>,
) -> Result<String, DeployFailure> {
    page.wsl_setup(distro, user).await
}

#[tauri::command]
pub async fn wsl_test(
    app: AppHandle,
    page: State<'_, Arc<HostsPage>>,
) -> Result<Vec<Step>, String> {
    super::hosts::hosts_test(app, page, WSL_HOST.into()).await
}

/// Installs WSL itself, with `distro` (Ubuntu when none), as an
/// administrator: Windows asks for permission. The computer may have to
/// restart before it's set up.
#[tauri::command]
pub async fn wsl_install(distro: Option<String>) -> Result<rovik_client::wsl::Installed, String> {
    rovik_client::wsl::install(distro.as_deref()).await
}

/// Rovik in the distribution brought to this app's version.
#[tauri::command]
pub async fn wsl_update(page: State<'_, Arc<HostsPage>>) -> Result<(), DeployFailure> {
    page.wsl_update().await
}

#[tauri::command]
pub async fn wsl_remove(page: State<'_, Arc<HostsPage>>) -> Result<(), DeployFailure> {
    page.wsl_remove().await
}

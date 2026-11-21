//! Native command adapters for shared backend operations.

pub mod agent;
pub mod browser;
pub mod chat;
pub mod clm;
pub mod hosts;
pub mod mcp;
pub mod memory;
pub mod network;
pub mod overlay;
pub mod pages;
pub mod project;
pub mod sandbox;
pub mod tools;
pub mod vfs;
pub mod workload;
pub mod wsl;

/// Manages app-wide repository provider accounts.
#[tauri::command]
pub async fn repository_account(
    backend: tauri::State<'_, rovik_client::desktop_connection::DesktopConnection>,
    provider: rovik_client::forge_accounts::Provider,
    action: rovik_client::forge_accounts::AccountAction,
) -> Result<serde_json::Value, serde_json::Value> {
    use rovik_client::desktop_api::{settings::SettingsRequest, DesktopRequest};
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::RepositoryAccount {
            provider,
            action,
        }))
        .await
}

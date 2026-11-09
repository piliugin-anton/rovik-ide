//! A project as the agent sees it (project_profile.rs): its tools and
//! checks, the files it may not read or change (restricted_files.rs), and
//! the user's saved settings for it.

use rovik_client::desktop_api::{settings::SettingsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;

#[tauri::command]
pub async fn project_profile(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::ProjectProfile {
            root,
        }))
        .await
}

/// The user's settings for one project (e.g. corrected check commands), as
/// the JSON the frontend stored; null when there are none.
#[tauri::command]
pub async fn load_project_settings(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::LoadProject { root }))
        .await
}

/// Saves one project's settings; null removes them.
#[tauri::command]
pub async fn save_project_settings(
    backend: State<'_, DesktopConnection>,
    root: String,
    settings: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::SaveProject {
            root,
            settings,
        }))
        .await
}

/// Settings → Restricted files' table for one project: the files found
/// whose names look like they hold credentials, and the user's list, each
/// with what applies to it.
#[tauri::command]
pub async fn restricted_files(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::RestrictedFiles {
            root,
        }))
        .await
}

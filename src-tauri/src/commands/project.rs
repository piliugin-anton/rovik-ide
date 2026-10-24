//! A project as the agent sees it (project_profile.rs): its tools and
//! checks, and the user's saved settings for it.

use rovik_core::project_profile::{self, ProjectProfile};
use rovik_core::Core;
use tauri::State;

#[tauri::command]
pub async fn project_profile(root: String) -> Result<ProjectProfile, String> {
    // Off the main thread: it reads the project's manifests and lockfiles.
    crate::util::blocking(move || project_profile::project_profile(&root)).await
}

/// The user's settings for one project (e.g. corrected check commands), as
/// the JSON the frontend stored; null when there are none.
#[tauri::command]
pub fn load_project_settings(
    core: State<'_, Core>,
    root: String,
) -> Result<serde_json::Value, String> {
    Ok(project_profile::load_project_settings(
        &core.paths.config_dir,
        &root,
    ))
}

/// Saves one project's settings; null removes them.
#[tauri::command]
pub fn save_project_settings(
    core: State<'_, Core>,
    root: String,
    settings: serde_json::Value,
) -> Result<(), String> {
    project_profile::save_project_settings(&core.paths.config_dir, root, settings)
}

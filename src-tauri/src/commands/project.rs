//! A project as the agent sees it (project_profile.rs): its tools and
//! checks, the files it may not read or change (restricted_files.rs), and
//! the user's saved settings for it.

use rovik_core::project_profile::{self, ProjectProfile};
use rovik_core::restricted_files::{rows, RestrictedFiles, Row};
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

/// Settings → Restricted files' table for one project: the files found
/// whose names look like they hold credentials, and the user's list, each
/// with what applies to it.
#[tauri::command]
pub async fn restricted_files(core: State<'_, Core>, root: String) -> Result<Vec<Row>, String> {
    let config_dir = core.paths.config_dir.clone();
    let settings_root = root.clone();
    // Off the main thread: it walks the project.
    crate::util::blocking_in(root, move |folder| {
        let restricted = RestrictedFiles::load(&config_dir, &settings_root);
        rows(folder, &restricted)
    })
    .await
}

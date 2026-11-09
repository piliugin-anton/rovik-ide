//! Shared AI and project settings, resolved only by the backend owner.
use super::{blocking, encode_result, DesktopResult};
use crate::desktop_backend::DesktopBackend;
use rovik_core::project_profile;
use rovik_core::restricted_files::{rows, RestrictedFiles};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Settings operations with the same JSON contract as the desktop commands.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum SettingsRequest {
    /// Load AI settings and provider credentials.
    LoadAi,
    /// The AI settings modification stamp.
    AiStamp,
    /// Merge changes over the supplied base when another client saved first.
    SaveAi { json: String, base: Option<String> },
    /// Load the user's settings for a project.
    LoadProject { root: String },
    /// Save or remove a project's settings.
    SaveProject { root: String, settings: Value },
    /// Discover a project's tools and checks.
    ProjectProfile { root: String },
    /// Discover files whose access should be restricted.
    RestrictedFiles { root: String },
}

impl SettingsRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> DesktopResult {
        match self {
            Self::LoadAi => {
                let keys = backend.keys.clone();
                encode_result(blocking(move || keys.load()).await)
            }
            Self::AiStamp => encode_result(Ok::<_, String>(backend.keys.stamp())),
            Self::SaveAi { json, base } => {
                let keys = backend.keys.clone();
                encode_result(
                    blocking(move || match base {
                        Some(base) => keys.save_onto(&json, &base),
                        None => keys.save(&json),
                    })
                    .await,
                )
            }
            Self::LoadProject { root } => {
                let config = backend.core.paths.config_dir.clone();
                encode_result(
                    blocking(move || Ok(project_profile::load_project_settings(&config, &root)))
                        .await,
                )
            }
            Self::SaveProject { root, settings } => {
                let config = backend.core.paths.config_dir.clone();
                encode_result(
                    blocking(move || {
                        project_profile::save_project_settings(&config, root, settings)
                    })
                    .await,
                )
            }
            Self::ProjectProfile { root } => {
                encode_result(blocking(move || project_profile::project_profile(&root)).await)
            }
            Self::RestrictedFiles { root } => {
                let config = backend.core.paths.config_dir.clone();
                encode_result(
                    blocking(move || {
                        let folder = PathBuf::from(&root);
                        if !folder.is_dir() {
                            return Err(format!("{}: not a folder", folder.display()));
                        }
                        Ok(rows(&folder, &RestrictedFiles::load(&config, &root)))
                    })
                    .await,
                )
            }
        }
    }
}

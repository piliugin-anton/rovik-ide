//! Shared settings commands.
use rovik_client::desktop_api::{settings::SettingsRequest, DesktopRequest};
use rovik_client::desktop_connection::DesktopConnection;
use tauri::State;
// `ai-settings.json`, its API keys in the keychain (H06): off the main
// thread, as the keychain may take a moment, or ask the user.
#[tauri::command]
pub async fn load_ai_settings(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::LoadAi))
        .await
}

/// When the AI settings were last saved, by any window (ProviderKeys::stamp).
#[tauri::command]
pub async fn ai_settings_stamp(
    backend: State<'_, DesktopConnection>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::AiStamp))
        .await
}

/// `base`: the settings the frontend made `json` of, as it loaded them:
/// saved onto the settings as they are now, which another window may have
/// saved meanwhile (ProviderKeys::save_onto).
#[tauri::command]
pub async fn save_ai_settings(
    backend: State<'_, DesktopConnection>,
    json: String,
    base: Option<String>,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(SettingsRequest::SaveAi { json, base }))
        .await
}

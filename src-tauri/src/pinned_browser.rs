//! Native adapters observe the backend's shared Chrome installation.
use rovik_client::{desktop_api::browser::BrowserRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Whether the backend's pinned browser is installed, and its download size.
#[tauri::command]
pub async fn browser_status(backend: State<'_, DesktopConnection>) -> Result<Value, String> {
    backend.desktop(BrowserRequest::Status.into()).await
}

/// Observes installation progress in this window without owning the download.
#[tauri::command]
pub async fn browser_install(
    backend: State<'_, DesktopConnection>,
    on_progress: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| BrowserRequest::Install { sink }.into(),
            move |progress| on_progress.send(progress).map_err(|e| e.to_string()),
        )
        .await
}

#[cfg(test)]
mod tests {
    /// The browser tools look for the app's copy in the folder named after
    /// the app's identifier, without asking Tauri.
    #[test]
    fn the_app_and_the_browser_tools_agree_on_its_folder() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["identifier"],
            rovik_core::pinned_browser::APP_ID,
            "pinned_browser::app_root() names the app's data folder"
        );
    }
}

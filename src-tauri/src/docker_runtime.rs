//! Window adapters for the local backend's Docker installer.
use rovik_client::{desktop_api::docker::DockerRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Report local installation and daemon readiness separately.
#[tauri::command]
pub async fn docker_status(backend: State<'_, DesktopConnection>) -> Result<Value, String> {
    backend.desktop(DockerRequest::Status.into()).await
}

/// Observe a shared backend installation through this window's channel.
#[tauri::command]
pub async fn docker_install(
    backend: State<'_, DesktopConnection>,
    on_progress: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| DockerRequest::Install { sink }.into(),
            move |progress| on_progress.send(progress).map_err(|e| e.to_string()),
        )
        .await
}

//! UI adapters for the backend's shared workload queue.
use rovik_client::{desktop_api::workload::WorkloadRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use tauri::State;

/// Promotes this client's visible chat, or releases its selection.
#[tauri::command]
pub async fn workload_focus(
    backend: State<'_, DesktopConnection>,
    dialog_id: Option<String>,
) -> Result<Value, String> {
    backend
        .desktop(WorkloadRequest::Focus { dialog_id }.into())
        .await
}
/// Persists the backend's shared limit; None restores automatic sizing.
#[tauri::command]
pub async fn workload_set_slots(
    backend: State<'_, DesktopConnection>,
    slots: Option<usize>,
) -> Result<Value, String> {
    backend
        .desktop(WorkloadRequest::SetSlots { slots }.into())
        .await
}
/// Reads the same queue used by backend execution and terminals.
#[tauri::command]
pub async fn workload_status(backend: State<'_, DesktopConnection>) -> Result<Value, String> {
    backend.desktop(WorkloadRequest::Status.into()).await
}

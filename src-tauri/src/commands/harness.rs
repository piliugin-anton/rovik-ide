//! Native UI adapter for the shared polyglot verification service.

use rovik_client::desktop_api::{
    harness::{ExecutionHost, HarnessRequest},
    DesktopRequest,
};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_protocol::harness::Operation;
use tauri::State;

/// Configure a project or manage verification where the selected chat executes.
#[tauri::command]
pub async fn polyglot_harness(
    backend: State<'_, DesktopConnection>,
    root: String,
    execution_host: ExecutionHost,
    dialog_id: Option<String>,
    request: Operation,
) -> Result<serde_json::Value, serde_json::Value> {
    backend
        .desktop_raw(DesktopRequest::from(HarnessRequest {
            root,
            execution_host,
            dialog_id,
            request,
        }))
        .await
}

//! Native adapters for client-owned language servers in the backend.
pub mod rust_analyzer;
use rovik_client::{desktop_api::lsp::LspRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Starts a server with private output registered before the process starts.
#[tauri::command]
pub async fn lsp_start(
    backend: State<'_, DesktopConnection>,
    command: String,
    args: Vec<String>,
    cwd: String,
    on_event: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_stream(
            |sink| {
                LspRequest::Start {
                    command,
                    args,
                    cwd,
                    sink,
                }
                .into()
            },
            move |event| on_event.send(event).is_ok(),
        )
        .await
}

/// Sends without blocking the command connection behind a full stdin pipe.
#[tauri::command]
pub async fn lsp_send(
    backend: State<'_, DesktopConnection>,
    id: String,
    message: String,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| LspRequest::Send { id, message, sink }.into(),
            |_| Ok(()),
        )
        .await
}

/// Stops only a server belonging to this window.
#[tauri::command]
pub async fn lsp_stop(backend: State<'_, DesktopConnection>, id: String) -> Result<Value, String> {
    backend.desktop(LspRequest::Stop { id }.into()).await
}

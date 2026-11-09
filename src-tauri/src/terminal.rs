//! Native terminal channels forward backend-owned shell output.
use rovik_client::desktop_api::terminal::TerminalRequest;
use rovik_client::desktop_connection::DesktopConnection;
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Starts a local shell through the shared backend.
#[tauri::command]
pub async fn spawn_terminal(
    backend: State<'_, DesktopConnection>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_stream(
            |sink| {
                TerminalRequest::Spawn {
                    cwd,
                    cols,
                    rows,
                    sink,
                }
                .into()
            },
            move |event| on_event.send(event).is_ok(),
        )
        .await
}

/// Starts a remote shell through the shared backend.
#[tauri::command]
pub async fn spawn_remote_terminal(
    backend: State<'_, DesktopConnection>,
    host: String,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_stream(
            |sink| {
                TerminalRequest::SpawnRemote {
                    host,
                    cols,
                    rows,
                    sink,
                }
                .into()
            },
            move |event| on_event.send(event).is_ok(),
        )
        .await
}

/// Sends input to a terminal owned by this client.
#[tauri::command]
pub async fn write_to_terminal(
    backend: State<'_, DesktopConnection>,
    id: String,
    data: String,
) -> Result<Value, String> {
    backend
        .desktop(TerminalRequest::Write { id, data }.into())
        .await
}

/// Resizes a terminal owned by this client.
#[tauri::command]
pub async fn resize_terminal(
    backend: State<'_, DesktopConnection>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<Value, String> {
    backend
        .desktop(TerminalRequest::Resize { id, cols, rows }.into())
        .await
}

/// Closes a terminal owned by this client.
#[tauri::command]
pub async fn close_terminal(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<Value, String> {
    backend.desktop(TerminalRequest::Close { id }.into()).await
}

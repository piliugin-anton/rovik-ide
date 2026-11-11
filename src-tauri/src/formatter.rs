//! Native adapters for editor formatting in the shared backend.
use rovik_client::{
    desktop_api::formatter::FormatterRequest, desktop_connection::DesktopConnection,
};
use serde_json::Value;
use tauri::State;

/// Discovers the project's own formatter before falling back to the backend PATH.
#[tauri::command]
pub async fn formatter_for_file(
    backend: State<'_, DesktopConnection>,
    path: String,
    language: String,
    root: Option<String>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| {
                FormatterRequest::Lookup {
                    path,
                    language,
                    root,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

/// Formats the unsaved buffer without writing the source file.
#[tauri::command]
pub async fn format_file(
    backend: State<'_, DesktopConnection>,
    path: String,
    language: String,
    root: Option<String>,
    content: String,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| {
                FormatterRequest::Format {
                    path,
                    language,
                    root,
                    content,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

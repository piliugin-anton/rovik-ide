//! Browser tools execute in the shared backend, independently of the command connection.
use rovik_client::{desktop_api::browser::BrowserRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use tauri::State;

/// Searches through the backend's existing browser service.
#[tauri::command]
pub async fn web_search(
    backend: State<'_, DesktopConnection>,
    query: String,
    max_results: Option<usize>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| {
                BrowserRequest::Search {
                    query,
                    max_results,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

/// Reads a page through the backend's URL and network checks.
#[tauri::command]
pub async fn browse_url(
    backend: State<'_, DesktopConnection>,
    url: String,
    max_chars: Option<usize>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| {
                BrowserRequest::Browse {
                    url,
                    max_chars,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

/// Captures a browser screenshot and stores it in the backend's attachments.
#[tauri::command]
pub async fn browser_screenshot(
    backend: State<'_, DesktopConnection>,
    url: Option<String>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| BrowserRequest::Screenshot { url, sink }.into(),
            |_| Ok(()),
        )
        .await
}

//! Private callbacks for the window responsible for a remote page.
use rovik_client::{
    desktop_api::pages::PagesRequest, desktop_connection::DesktopConnection, desktop_journal::Scope,
};
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Registers the window's current project and retained chats.
#[tauri::command]
pub async fn desktop_pages_subscribe(
    backend: State<'_, DesktopConnection>,
    scope: Scope,
    on_event: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_stream(
            |sink| PagesRequest::Subscribe { scope, sink }.into(),
            move |event| on_event.send(event).is_ok(),
        )
        .await
}

/// Removes only this client's callback, without affecting newer subscriptions.
#[tauri::command]
pub async fn desktop_pages_unsubscribe(
    backend: State<'_, DesktopConnection>,
    id: String,
) -> Result<Value, String> {
    backend
        .desktop(PagesRequest::Unsubscribe { id }.into())
        .await
}

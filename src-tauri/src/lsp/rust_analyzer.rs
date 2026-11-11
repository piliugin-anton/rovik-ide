//! Rust-analyzer setup executes in the backend, with private download progress.
use rovik_client::{
    desktop_api::lsp::LspRequest, desktop_connection::DesktopConnection,
    lsp::rust_analyzer::RaRelease,
};
use serde_json::Value;
use tauri::{ipc::Channel, State};

/// Finds the binary using the backend's managed directory and environment.
#[tauri::command]
pub async fn ra_find(
    backend: State<'_, DesktopConnection>,
    custom_path: Option<String>,
    prefer_system: bool,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| {
                LspRequest::Find {
                    custom_path,
                    prefer_system,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

/// Resolves the latest official release in the backend.
#[tauri::command]
pub async fn ra_latest_release(backend: State<'_, DesktopConnection>) -> Result<Value, Value> {
    backend
        .desktop_task(|sink| LspRequest::Latest { sink }.into(), |_| Ok(()))
        .await
}

/// Installs a verified release, forwarding download progress to this window.
#[tauri::command]
pub async fn ra_install(
    backend: State<'_, DesktopConnection>,
    release: RaRelease,
    on_progress: Channel<Value>,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| LspRequest::Install { release, sink }.into(),
            move |progress| on_progress.send(progress).map_err(|e| e.to_string()),
        )
        .await
}

/// Checks rust-src using the project's own toolchain.
#[tauri::command]
pub async fn ra_rust_src_status(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| LspRequest::RustSrcStatus { root, sink }.into(),
            |_| Ok(()),
        )
        .await
}

/// Installs rust-src using the project's own toolchain.
#[tauri::command]
pub async fn ra_install_rust_src(
    backend: State<'_, DesktopConnection>,
    root: String,
) -> Result<Value, Value> {
    backend
        .desktop_task(
            |sink| LspRequest::InstallRustSrc { root, sink }.into(),
            |_| Ok(()),
        )
        .await
}

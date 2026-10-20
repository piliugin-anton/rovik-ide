//! The sandbox the agent's commands run in (rovik_core::overlay::confine):
//! whether this machine can make one, and installing what it needs.

use rovik_core::overlay::confine::{self, InstallOutcome, SandboxStatus};

/// Finding out may run the sandbox's program once: off the async threads.
#[tauri::command]
pub async fn sandbox_status() -> Result<SandboxStatus, String> {
    rovik_core::rt::spawn_blocking(confine::status)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sandbox_install() -> Result<InstallOutcome, String> {
    confine::install().await
}

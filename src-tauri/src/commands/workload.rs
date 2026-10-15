//! Heavy commands' slots (workload): which chat comes first, how many run at
//! once, and what runs now.

use super::call;
use rovik_core::workload::{self, WorkloadStatus};
use rovik_core::Core;
use rovik_protocol::Request;
use tauri::State;

/// The chat the user is looking at: its commands come first.
#[tauri::command]
pub async fn workload_focus(
    core: State<'_, Core>,
    dialog_id: Option<String>,
) -> Result<(), String> {
    call(&core, Request::WorkloadFocus { dialog_id }).await
}

/// `None`: auto. A number: that many slots, as the user set it; kept for
/// the next start.
#[tauri::command]
pub fn workload_set_slots(slots: Option<usize>) -> Result<(), String> {
    workload::workload_set_slots(slots)
}

/// For Settings → Builds & checks.
#[tauri::command]
pub fn workload_status() -> WorkloadStatus {
    workload::workload_status()
}

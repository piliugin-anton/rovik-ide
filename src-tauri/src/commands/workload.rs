//! Heavy commands' slots (workload): which chat comes first, how many run at
//! once, and what runs now.

use super::answer;
use rovik_client::Remote;
use rovik_core::workload::{self, WorkloadStatus};
use rovik_protocol::Request;
use std::sync::Arc;
use tauri::State;

/// The chat the user is looking at: its commands come first, here and on
/// every connected host.
#[tauri::command]
pub async fn workload_focus(
    remote: State<'_, Arc<Remote>>,
    dialog_id: Option<String>,
) -> Result<(), String> {
    answer(
        remote
            .everywhere(Request::WorkloadFocus { dialog_id })
            .await,
    )
}

/// `None`: auto. A number: that many slots, as the user set it; kept for
/// the next start.
#[tauri::command]
pub fn workload_set_slots(slots: Option<usize>) -> Result<(), String> {
    workload::workload_set_slots(slots)
}

/// For Settings → Heavy commands.
#[tauri::command]
pub fn workload_status() -> WorkloadStatus {
    workload::workload_status()
}

//! Heavy commands' slots (workload): which chat comes first, how many run at
//! once, and what runs now.

use rovik_core::workload::{self, WorkloadStatus};

#[tauri::command]
pub fn workload_focus(dialog_id: Option<String>) {
    workload::workload_focus(dialog_id);
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

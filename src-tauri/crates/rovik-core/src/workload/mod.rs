//! A queue for CPU-heavy work, so several chats (and the user's terminal)
//! building at once don't freeze the machine.
//!
//! - classify.rs guesses from a command's text whether it will be heavy;
//!   history.rs remembers what the same command cost before.
//! - queue.rs hands out slots: a command predicted heavy waits for one
//!   before it starts. One slot by default ("auto", leaning low): two only on
//!   machines with 16+ cores and 32 GB+ memory while they have room.
//! - tracker.rs watches every running command's process tree. A command that
//!   turns heavy without a slot is given lower priority (paused only when
//!   memory runs out); a slot whose command has gone idle is freed. The
//!   terminal panel's commands are watched too and take a slot when heavy,
//!   but are never slowed.
//!
//! See docs/design/chat-isolation-plan.md, track Q.

pub mod classify;
pub mod history;
pub mod queue;
mod tracker;

pub use queue::{begin, set_focus, JobKind, Waited};

/// Points history at the app's data folder, and applies the slots the
/// user set (Settings → Builds & checks), if they set any.
pub fn init(data_dir: &std::path::Path) {
    history::init(data_dir);
    let file = data_dir.join("workload-settings.json");
    if let Some(slots) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        .and_then(|s| s.slots)
    {
        queue::set_manual_slots(Some(slots.clamp(1, MAX_SLOTS)));
    }
    *SETTINGS_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
}

/// At exit: paused commands resume, and history is written.
pub fn shutdown() {
    tracker::release_all();
    history::flush();
}

/// The most slots the user can set.
const MAX_SLOTS: usize = 8;

static SETTINGS_FILE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Saved {
    slots: Option<usize>,
}

/// "1 min 20 s", "45 s".
pub fn format_duration(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    if s >= 60 {
        let (m, s) = (s / 60, s % 60);
        if s == 0 {
            format!("{m} min")
        } else {
            format!("{m} min {s} s")
        }
    } else {
        format!("{s} s")
    }
}

/// A command for a note: one line, cut short.
pub fn short_command(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(60).collect();
    if cut.len() < line.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

pub fn workload_focus(dialog_id: Option<String>) {
    set_focus(dialog_id);
}

/// `None`: auto. A number: that many slots, as the user set it; kept for
/// the next start.
pub fn workload_set_slots(slots: Option<usize>) -> Result<(), String> {
    let slots = slots.map(|n| n.clamp(1, MAX_SLOTS));
    // Saved first: a setting that can't be kept isn't applied either.
    let file = SETTINGS_FILE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(file) = file {
        let bytes = serde_json::to_vec(&Saved { slots }).map_err(|e| e.to_string())?;
        crate::atomic_file::write_atomic(&file, &bytes).map_err(|e| e.to_string())?;
    }
    queue::set_manual_slots(slots);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadStatus {
    /// As the user set them; None: auto.
    pub slots: Option<usize>,
    /// What auto gives on this machine now.
    pub auto_slots: usize,
    pub max_slots: usize,
    /// Commands holding a slot, and ones waiting for one.
    pub busy: usize,
    pub waiting: usize,
    pub cores: usize,
    pub memory_gb: u64,
}

/// For Settings → Builds & checks.
pub fn workload_status() -> WorkloadStatus {
    let s = queue::slots();
    let cores = if s.cores > 0 {
        s.cores
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    };
    let memory = if s.total_memory > 0 {
        s.total_memory
    } else {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.total_memory()
    };
    WorkloadStatus {
        slots: s.manual,
        auto_slots: s.auto,
        max_slots: MAX_SLOTS,
        busy: s.busy,
        waiting: s.waiting,
        cores,
        memory_gb: (memory + (1 << 29)) >> 30,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_read_naturally() {
        assert_eq!(super::format_duration(45_200), "45 s");
        assert_eq!(super::format_duration(80_000), "1 min 20 s");
        assert_eq!(super::format_duration(120_000), "2 min");
    }
}

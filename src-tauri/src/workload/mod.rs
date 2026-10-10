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

/// Points history at the app's data folder.
pub fn init(data_dir: &std::path::Path) {
    history::init(data_dir);
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

#[tauri::command]
pub fn workload_focus(dialog_id: Option<String>) {
    set_focus(dialog_id);
}

/// `None`: auto. A number: that many slots, as the user set it.
#[tauri::command]
pub fn workload_set_slots(slots: Option<usize>) {
    queue::set_manual_slots(slots);
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

//! Heavy-work slots shared by Rovik's processes. Each window is a process
//! with a queue of its own (queue.rs), and the machine is one: a slot in
//! use in one window counts in all. While a job of this process holds a
//! slot, it holds an OS lock on `<data>/heavy-slots/slot-<n>.lock`
//! (os_lock.rs); a busy terminal holds one too, beyond the slots there are
//! if it must, as it never waits. The other processes count the locks they
//! find held; a crash lets go of its own.
//!
//! Without a folder (the daemon, tests) nothing is shared: this process's
//! queue is the machine's, as it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// The most slots held at once on the machine: the most the user can set,
/// and a busy terminal beyond them in a few windows.
const MOST: usize = 16;

/// A process's slots: the global one, or a test's (another "process").
#[derive(Default)]
pub(super) struct Slots {
    dir: Option<PathBuf>,
    /// By job: the slot it holds, and the lock that keeps it.
    held: HashMap<u64, (usize, crate::os_lock::Held)>,
}

static SLOTS: LazyLock<Mutex<Slots>> = LazyLock::new(Default::default);

fn slots() -> MutexGuard<'static, Slots> {
    SLOTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The slots' locks go in `dir`, shared with the other processes.
pub fn init(dir: PathBuf) {
    slots().set_dir(Some(dir));
}

/// The slots other processes hold now.
pub(super) fn held_elsewhere() -> usize {
    slots().held_elsewhere()
}

/// Takes a slot for job `id` among the first `within` (a chat's job: the
/// slots there are; a busy terminal: any). False when none is free.
pub(super) fn take(id: u64, within: usize) -> bool {
    slots().take(id, within)
}

/// Job `id`'s slot, if it holds one, goes.
pub(super) fn release(id: u64) {
    slots().release(id);
}

/// Whether job `id` holds a slot.
pub(super) fn holds(id: u64) -> bool {
    slots().held.contains_key(&id)
}

impl Slots {
    pub(super) fn set_dir(&mut self, dir: Option<PathBuf>) {
        if let Some(dir) = &dir {
            let _ = std::fs::create_dir_all(dir);
        }
        self.dir = dir;
        self.held.clear();
    }

    fn path(dir: &Path, n: usize) -> PathBuf {
        dir.join(format!("slot-{n}.lock"))
    }

    pub(super) fn held_elsewhere(&self) -> usize {
        let Some(dir) = &self.dir else {
            return 0;
        };
        let mine: Vec<usize> = self.held.values().map(|(n, _)| *n).collect();
        (1..=MOST)
            .filter(|n| !mine.contains(n))
            .filter(|n| crate::os_lock::held_elsewhere(&Self::path(dir, *n)))
            .count()
    }

    pub(super) fn take(&mut self, id: u64, within: usize) -> bool {
        if self.held.contains_key(&id) {
            return true;
        }
        let Some(dir) = self.dir.clone() else {
            return true;
        };
        let mine: Vec<usize> = self.held.values().map(|(n, _)| *n).collect();
        let free: Vec<usize> = (1..=within.min(MOST))
            .filter(|n| !mine.contains(n))
            .collect();
        let paths: Vec<PathBuf> = free.iter().map(|n| Self::path(&dir, *n)).collect();
        // All tried at once: this runs under the queue's lock, which every
        // other job waits on meanwhile.
        match crate::os_lock::try_hold_first(&paths) {
            Ok(Some((i, lock))) => {
                self.held.insert(id, (free[i], lock));
                true
            }
            Ok(None) => false,
            // A slot that can't be locked isn't counted anywhere: this
            // process goes on as if alone.
            Err(e) => {
                eprintln!("heavy slots: {e}");
                true
            }
        }
    }

    pub(super) fn release(&mut self, id: u64) {
        self.held.remove(&id);
    }
}

/// The number of slots anyone may hold, as `within` for a busy terminal.
pub(super) const ANY: usize = MOST;

/// Tests: nothing shared again.
#[cfg(test)]
pub(super) fn reset() {
    slots().set_dir(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("heavy-slots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn process(dir: &Path) -> Slots {
        let mut slots = Slots::default();
        slots.set_dir(Some(dir.to_path_buf()));
        slots
    }

    // Two `Slots` in one test process stand for two processes: each holds
    // its locks through its own open files.
    #[test]
    fn a_slot_held_in_one_window_counts_in_the_other() {
        let dir = temp_dir("counts");
        let mut a = process(&dir);
        let mut b = process(&dir);
        assert!(a.take(1, 1), "the only slot, free");
        assert_eq!(b.held_elsewhere(), 1);
        assert_eq!(a.held_elsewhere(), 0, "its own isn't another's");
        assert!(!b.take(7, 1), "taken in the other window");
        // A busy terminal takes one beyond them.
        assert!(b.take(8, ANY));
        assert_eq!(a.held_elsewhere(), 1);
        a.release(1);
        assert_eq!(b.held_elsewhere(), 0);
        assert!(b.take(7, 1), "free again");
        assert_eq!(a.held_elsewhere(), 2);
    }

    #[test]
    fn a_window_that_ends_lets_go_of_its_slots() {
        let dir = temp_dir("ends");
        let mut a = process(&dir);
        assert!(a.take(1, 2));
        assert!(a.take(2, 2));
        let mut b = process(&dir);
        assert!(!b.take(5, 2), "both taken");
        drop(a);
        assert_eq!(b.held_elsewhere(), 0);
        assert!(b.take(5, 2));
    }

    #[test]
    fn without_a_folder_only_this_process_counts() {
        let mut alone = Slots::default();
        assert!(alone.take(1, 1));
        assert!(alone.take(2, 1), "nothing shared, nothing refused here");
        assert_eq!(alone.held_elsewhere(), 0);
    }
}

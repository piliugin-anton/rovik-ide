//! OS file locks shared by Rovik's processes. Several can run at once on
//! the same app data, one window each: a lock says one of them is at work
//! on something (a chat, an overlay, a landing) until it lets go or ends.
//! A crash lets go too, as the OS releases a dead process's locks, so
//! nothing stays stuck. The lock belongs to the open file, not to the
//! process: each is held through one open file, as a second of the
//! process's own would find it taken.
//!
//! Letting go unlocks the file before closing it. Closed alone, it would
//! stay locked while a program this process starts at that moment is
//! between its fork and its exec (it shares the open file until then),
//! and Windows unlocks a closed file's locks when it gets to it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// A lock found taken is tried again this often, this far apart: a
/// process only looking at it (`held_elsewhere`) shares it for a moment.
const TRIES: u32 = 6;
const APART: Duration = Duration::from_millis(15);

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

/// A lock held, until it's dropped.
#[derive(Debug)]
pub struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl std::ops::Deref for Held {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}

fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Holds the lock at `path`, the file made if missing; None while another
/// process holds it.
pub fn try_hold(path: &Path) -> io::Result<Option<Held>> {
    let file = open(path)?;
    for attempt in 0..TRIES {
        if attempt > 0 {
            std::thread::sleep(APART);
        }
        if try_lock(&file)? {
            return Ok(Some(Held(file)));
        }
    }
    Ok(None)
}

/// Holds the first lock of `paths` free, with its index; None while
/// others hold them all. Each is tried once a round, and a round again a
/// moment later as `try_hold` does, so a caller holding a lock of its own
/// meanwhile (a queue's) waits as long for many as for one.
pub fn try_hold_first(paths: &[std::path::PathBuf]) -> io::Result<Option<(usize, Held)>> {
    let files = paths
        .iter()
        .map(|p| open(p))
        .collect::<io::Result<Vec<File>>>()?;
    first_unlocked(files, || std::thread::sleep(APART))
}

/// The first of `files` whose lock is free, held, with its index: each
/// tried once a round, `pause` between the rounds.
fn first_unlocked(files: Vec<File>, mut pause: impl FnMut()) -> io::Result<Option<(usize, Held)>> {
    for attempt in 0..TRIES {
        if attempt > 0 {
            pause();
        }
        for (i, file) in files.iter().enumerate() {
            if try_lock(file)? {
                let file = files.into_iter().nth(i).expect("an index of files");
                return Ok(Some((i, Held(file))));
            }
        }
    }
    Ok(None)
}

/// Holds the lock at `path`, once whoever has it lets go.
pub fn hold(path: &Path) -> io::Result<Held> {
    let file = open(path)?;
    file.lock()?;
    Ok(Held(file))
}

/// Whether another process holds the lock at `path`: looked at without
/// taking it from anyone, as a shared lock for a moment. Only for a lock
/// this process doesn't hold, as its own would look taken.
pub fn held_elsewhere(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// This process's slot: its number among the Rovik processes running on
/// this computer (the lowest free), and the lock that keeps it.
static SLOT: OnceLock<(usize, Option<Held>)> = OnceLock::new();
/// Where the slots are kept.
static SLOTS: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Takes this process's slot among those kept in `dir`: 1 for the first
/// process, 2 for one started while it runs, and so on. Taken once; what's
/// asked after that is the slot taken first.
pub fn take_slot(dir: &Path) -> usize {
    let _ = SLOTS.set(dir.to_path_buf());
    SLOT.get_or_init(|| first_free(dir)).0
}

/// Whether no other Rovik process runs now (none holds a slot): what they
/// all share, and one may still use, can be cleaned up then. True for a
/// process that took no slot (the daemon, tests).
pub fn alone() -> bool {
    let (Some(dir), Some((mine, _))) = (SLOTS.get(), SLOT.get()) else {
        return true;
    };
    alone_in(dir, *mine)
}

/// Whether no process but the one with slot `mine` holds a slot in `dir`.
fn alone_in(dir: &Path, mine: usize) -> bool {
    !(1..=64)
        .filter(|n| *n != mine)
        .any(|n| held_elsewhere(&dir.join(format!("slot-{n}.lock"))))
}

/// The lowest slot in `dir` no process holds, held.
fn first_free(dir: &Path) -> (usize, Option<Held>) {
    for n in 1..=64 {
        let taken = open(&dir.join(format!("slot-{n}.lock")))
            .and_then(|file| Ok(try_lock(&file)?.then_some(Held(file))));
        match taken {
            Ok(Some(file)) => return (n, Some(file)),
            Ok(None) => continue,
            Err(e) => {
                eprintln!("slots: {e}");
                break;
            }
        }
    }
    // None to be had: one no other process has, unkept.
    (64 + std::process::id() as usize, None)
}

/// This process's slot: 1 until one is taken (a process on its own).
pub fn slot() -> usize {
    SLOT.get().map_or(1, |(n, _)| *n)
}

/// `name` for something only one process at a time can have (a mount
/// point, a socket): as it is for the first, `name-2` for the second.
pub fn slotted(name: &str) -> String {
    slotted_as(name, slot())
}

fn slotted_as(name: &str, slot: usize) -> String {
    match slot {
        1 => name.to_string(),
        n => format!("{name}-{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("os-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A program that starts and ends at once.
    fn nothing() -> std::process::Command {
        if cfg!(windows) {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "exit"]);
            cmd
        } else {
            std::process::Command::new("true")
        }
    }

    // A program another thread starts shares the open files until its exec:
    // a lock closed then stayed held until the program got there.
    #[test]
    fn a_lock_let_go_is_free_at_once_while_programs_start() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        let path = temp_dir("let-go").join("busy.lock");
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let starting = {
            let (stop, started) = (Arc::clone(&stop), Arc::clone(&started));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = nothing().status();
                    started.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        let (mut lingered, mut tries) = (0, 0);
        while started.load(Ordering::Relaxed) < 50 && tries < 1_000_000 {
            tries += 1;
            drop(try_hold(&path).unwrap().expect("free"));
            lingered += usize::from(held_elsewhere(&path));
        }
        stop.store(true, Ordering::Relaxed);
        starting.join().unwrap();
        assert_eq!(lingered, 0, "let go, yet held, {lingered} times in {tries}");
    }

    // Two open files in one test process stand for two processes: the lock
    // is the open file's.
    #[test]
    fn a_held_lock_is_another_processs_until_it_lets_go() {
        let dir = temp_dir("held");
        let path = dir.join("sub/chat.lock");
        assert!(!held_elsewhere(&path), "no file: no one's");
        let mine = try_hold(&path).unwrap().expect("free");
        assert!(held_elsewhere(&path));
        assert!(try_hold(&path).unwrap().is_none());
        drop(mine);
        assert!(!held_elsewhere(&path));
        let theirs = try_hold(&path).unwrap().expect("free again");
        drop(theirs);
    }

    /// The first of several locks free is held, and all taken costs one
    /// round of waits, not one per lock.
    #[test]
    fn the_first_free_of_several_locks_is_held_and_all_taken_waits_once() {
        let dir = temp_dir("first");
        let paths: Vec<std::path::PathBuf> = (1..=8)
            .map(|n| dir.join(format!("slot-{n}.lock")))
            .collect();
        let taken: Vec<Held> = paths[..3]
            .iter()
            .map(|p| try_hold(p).unwrap().unwrap())
            .collect();
        // Held through other open files, as another process would.
        let (i, _fourth) = try_hold_first(&paths).unwrap().unwrap();
        assert_eq!(i, 3);
        let more: Vec<Held> = paths[4..]
            .iter()
            .map(|p| try_hold(p).unwrap().unwrap())
            .collect();
        // All taken: as many pauses as one lock's tries take, not one
        // lock's tries for each.
        let files: Vec<File> = paths.iter().map(|p| open(p).unwrap()).collect();
        let mut pauses = 0;
        assert!(first_unlocked(files, || pauses += 1).unwrap().is_none());
        assert_eq!(pauses, TRIES - 1);
        drop((taken, more));
    }

    #[test]
    fn looking_at_a_lock_doesnt_keep_it_from_a_process_taking_it() {
        let dir = temp_dir("look");
        let path = dir.join("chat.lock");
        let looking = File::open({
            drop(try_hold(&path).unwrap());
            &path
        })
        .unwrap();
        looking.try_lock_shared().unwrap();
        // Taken while another looks: tried again once it's done looking.
        let looker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            drop(looking);
        });
        assert!(try_hold(&path).unwrap().is_some());
        looker.join().unwrap();
    }

    #[test]
    fn each_process_takes_the_lowest_slot_free() {
        let dir = temp_dir("slots");
        let (first, a) = first_free(&dir);
        let (second, b) = first_free(&dir);
        assert_eq!((first, second), (1, 2));
        // The first ends: the next one started takes its slot.
        drop(a);
        assert_eq!(first_free(&dir).0, 1);
        drop(b);
    }

    #[test]
    fn a_process_is_alone_once_the_others_end() {
        let dir = temp_dir("alone");
        let (mine, _held) = first_free(&dir);
        assert!(alone_in(&dir, mine));
        let (_, other) = first_free(&dir);
        assert!(!alone_in(&dir, mine), "another window runs");
        drop(other);
        assert!(alone_in(&dir, mine));
    }

    #[test]
    fn names_carry_the_slot_after_the_first() {
        // The test process took none: it's a process on its own.
        assert_eq!(slot(), 1);
        assert_eq!(slotted("vfs"), "vfs");
        assert_eq!(slotted_as("vfs", 2), "vfs-2");
        assert_eq!(slotted_as("RovikVirtualFiles", 3), "RovikVirtualFiles-3");
    }
}

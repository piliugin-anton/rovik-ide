//! Which Rovik process is at work on a chat. Several can run at once on the
//! same chats, each with projects of its own (the app's instances.rs), but
//! a chat's run, its Autopilot driver between two stages and a stage
//! waiting out a rate limit live in one process's memory. While one of
//! them holds the chat, another process doesn't run it, resume it, or
//! judge its unanswered message. It's an OS lock per chat (os_lock.rs),
//! counted here: the chat is let go once the last of this process's holds
//! on it goes. Without a folder for the locks (the daemon, the tests) holds
//! are only counted, and no chat is at work anywhere else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

/// Why a run doesn't start in a chat another process is at work on.
pub const AT_WORK_ELSEWHERE: &str =
    "This chat is at work in another Rovik window: go on with it there.";

/// A process's holds: the global one, or a test's own (another "process").
#[derive(Default)]
pub struct Holds {
    dir: Mutex<Option<PathBuf>>,
    /// By chat: how many holds, and the lock they share.
    held: Mutex<HashMap<String, (usize, Option<crate::os_lock::Held>)>>,
}

static HOLDS: LazyLock<Arc<Holds>> = LazyLock::new(Default::default);

/// A hold on a chat, let go when dropped.
pub struct Hold {
    holds: Arc<Holds>,
    chat: String,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = self.holds.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((count, _)) = held.get_mut(&self.chat) {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.chat);
            }
        }
    }
}

impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hold({})", self.chat)
    }
}

/// The app: the chats' locks go in `dir`.
pub fn init(dir: PathBuf) {
    HOLDS.set_dir(dir);
}

/// Holds `chat` for this process: None while another process is at work
/// on it.
pub fn take(chat: &str) -> Option<Hold> {
    Holds::take(&HOLDS, chat)
}

/// Whether another process is at work on `chat`.
pub fn elsewhere(chat: &str) -> bool {
    HOLDS.elsewhere(chat)
}

/// The chats this process holds.
pub fn held_here() -> Vec<String> {
    HOLDS.held_here()
}

/// A deleted chat: its lock goes.
pub fn forget(chat: &str) {
    HOLDS.forget(chat)
}

impl Holds {
    pub fn set_dir(&self, dir: PathBuf) {
        let _ = std::fs::create_dir_all(&dir);
        *self.dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    }

    fn path(&self, chat: &str) -> Option<PathBuf> {
        let dir = self.dir.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        Some(dir.join(file_name(chat)))
    }

    pub fn take(this: &Arc<Self>, chat: &str) -> Option<Hold> {
        let mut held = this.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((count, _)) = held.get_mut(chat) {
            *count += 1;
        } else {
            let lock = match this.path(chat).map(|p| crate::os_lock::try_hold(&p)) {
                Some(Ok(Some(file))) => Some(file),
                Some(Ok(None)) => return None,
                // Better a chat held here only than no run at all.
                Some(Err(e)) => {
                    eprintln!("chat holds: {chat}: {e}");
                    None
                }
                None => None,
            };
            held.insert(chat.to_string(), (1, lock));
        }
        Some(Hold {
            holds: this.clone(),
            chat: chat.to_string(),
        })
    }

    pub fn elsewhere(&self, chat: &str) -> bool {
        // Looked at under the count, so a hold taken here meanwhile isn't
        // mistaken for another process's.
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        !held.contains_key(chat)
            && self
                .path(chat)
                .is_some_and(|p| crate::os_lock::held_elsewhere(&p))
    }

    pub fn held_here(&self) -> Vec<String> {
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.keys().cloned().collect()
    }

    fn forget(&self, chat: &str) {
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.contains_key(chat) {
            return;
        }
        // Deleted while this process holds it: deleted from under another
        // process's hold, the file would come back fresh for a third one,
        // and two would hold the chat.
        if let Some(path) = self.path(chat) {
            if let Ok(Some(_holding)) = crate::os_lock::try_hold(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// A chat id names its lock as it is; anything else, by its hash.
fn file_name(chat: &str) -> String {
    let plain = !chat.is_empty()
        && chat.len() <= 128
        && chat
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        return format!("{chat}.lock");
    }
    let hash = crate::file_guard::hash(chat.as_bytes());
    let name: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{name}.lock")
}

/// Whether `path` is a chat's lock file (for tests).
#[cfg(test)]
fn is_lock(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e == "lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two "processes" sharing one folder of locks.
    fn two(tag: &str) -> (Arc<Holds>, Arc<Holds>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("chat-hold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (Arc::new(Holds::default()), Arc::new(Holds::default()));
        a.set_dir(dir.clone());
        b.set_dir(dir.clone());
        (a, b, dir)
    }

    #[test]
    fn a_chat_one_process_is_at_work_on_is_held_from_the_other() {
        let (a, b, _) = two("work");
        let run = Holds::take(&a, "chat-1").expect("free");
        assert!(b.elsewhere("chat-1"));
        assert!(!a.elsewhere("chat-1"), "not elsewhere for the one at work");
        assert!(Holds::take(&b, "chat-1").is_none());
        // Other chats are free.
        assert!(Holds::take(&b, "chat-2").is_some());
        drop(run);
        assert!(!b.elsewhere("chat-1"));
        assert!(Holds::take(&b, "chat-1").is_some());
    }

    #[test]
    fn a_chat_is_let_go_once_the_last_of_its_holds_goes() {
        let (a, b, _) = two("count");
        // A run, and the driver between two stages.
        let run = Holds::take(&a, "chat-1").unwrap();
        let driver = Holds::take(&a, "chat-1").unwrap();
        assert_eq!(a.held_here(), vec!["chat-1".to_string()]);
        drop(run);
        assert!(b.elsewhere("chat-1"));
        drop(driver);
        assert!(!b.elsewhere("chat-1"));
        assert!(a.held_here().is_empty());
    }

    #[test]
    fn a_deleted_chats_lock_goes_unless_its_held() {
        let (a, b, dir) = two("forget");
        let held = Holds::take(&a, "chat-1").unwrap();
        b.forget("chat-1");
        assert!(dir.join("chat-1.lock").is_file(), "held by A");
        drop(held);
        drop(Holds::take(&a, "chat-2").unwrap());
        a.forget("chat-2");
        assert!(!dir.join("chat-2.lock").exists());
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| is_lock(&e.path()))
            .collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn without_a_folder_holds_are_only_counted() {
        let (a, b) = (Arc::new(Holds::default()), Arc::new(Holds::default()));
        let _held = Holds::take(&a, "chat-1").unwrap();
        assert!(!b.elsewhere("chat-1"));
        assert!(Holds::take(&b, "chat-1").is_some());
    }

    #[test]
    fn odd_ids_are_named_by_their_hash() {
        assert_eq!(file_name("3f2a-b_c"), "3f2a-b_c.lock");
        let odd = file_name("../x");
        assert!(odd.ends_with(".lock") && !odd.contains('/') && odd.len() == 37);
    }
}

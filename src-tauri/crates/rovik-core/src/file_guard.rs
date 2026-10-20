//! Keeps one writer from silently overwriting another's change to a file.
//!
//! Several chats can work on one project at once, and the user edits it in
//! the editor at the same time. A write built from an old copy of a file
//! (a chat's read from minutes ago, an editor tab loaded before the agent
//! changed the file) would erase whatever changed since. Here every write
//! by the app checks first that the file is still what the writer last saw:
//!
//! - [`lock`]: one writer at a time per file, for the read → check → write.
//!   Held only for that, never across an `.await` or an approval wait.
//! - [`Seen`]: per chat run, the content hash of each file as the run last
//!   read or wrote it. A whole-file write must match it.
//! - The ledger: who last wrote each file through the app and what it was.
//!   A run writing a file it never read is refused only when the file is
//!   another writer's work it hasn't seen.
//!
//! Commands a chat runs, and programs outside the app, write without going
//! through here. A write by them is caught at the next checked write, not
//! prevented (chat isolation, docs/design/chat-isolation-plan.md, covers it).

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

pub type Hash = [u8; 32];

pub fn hash(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}

/// `hash` (or the first bytes of one) in lowercase hex.
pub fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `bytes` in lowercase hex, as `sha256sum` writes it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&hash(bytes))
}

/// Who wrote a file through the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Writer {
    /// A chat's run, by dialog id.
    Chat(String),
    /// The user, saving in the editor.
    Editor,
}

/// The key a file is locked and recorded under: one per file, whichever
/// spelling of its path a caller uses. The folder is resolved (symlinks,
/// `.`), and on the case-insensitive file systems macOS and Windows use by
/// default, case is ignored.
pub fn key(path: &Path) -> PathBuf {
    // A symlinked file is the file it points at.
    if let Ok(target) = path.canonicalize() {
        return fold_case(target);
    }
    // Not there yet: the nearest folder that is, resolved, and the rest as
    // written, so the key doesn't change once the folders are created.
    let mut rest = Vec::new();
    let mut at = path;
    let resolved = loop {
        match at.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                if let Some(name) = at.file_name() {
                    rest.push(name.to_os_string());
                }
                if let Ok(dir) = dir.canonicalize() {
                    let mut p = dir;
                    p.extend(rest.iter().rev());
                    break p;
                }
                at = dir;
            }
            _ => break path.to_path_buf(),
        }
    };
    fold_case(resolved)
}

fn fold_case(resolved: PathBuf) -> PathBuf {
    if cfg!(any(target_os = "macos", windows)) {
        PathBuf::from(resolved.to_string_lossy().to_lowercase())
    } else {
        resolved
    }
}

// ---------------------------------------------------------------------------
// One writer at a time per file
// ---------------------------------------------------------------------------

struct Locks {
    held: Mutex<HashSet<PathBuf>>,
    freed: Condvar,
}

static LOCKS: LazyLock<Locks> = LazyLock::new(|| Locks {
    held: Mutex::new(HashSet::new()),
    freed: Condvar::new(),
});

/// Holds a file's lock until dropped.
pub struct PathLock {
    key: PathBuf,
}

impl Drop for PathLock {
    fn drop(&mut self) {
        LOCKS
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        LOCKS.freed.notify_all();
    }
}

/// Waits until no one else holds `path`'s lock, then takes it.
pub fn lock(path: &Path) -> PathLock {
    lock_key(key(path))
}

/// [`lock`] for each file of `paths`, once, in the order of their keys, so
/// that two callers never wait for each other: two paths of one file (a
/// link and its target; names that differ only in case on macOS and
/// Windows) are one lock, which a second `lock` would wait for forever.
pub fn lock_all<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Vec<PathLock> {
    let mut keys: Vec<PathBuf> = paths.into_iter().map(key).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter().map(lock_key).collect()
}

fn lock_key(key: PathBuf) -> PathLock {
    let mut held = LOCKS.held.lock().unwrap_or_else(|e| e.into_inner());
    while held.contains(&key) {
        held = LOCKS.freed.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    held.insert(key.clone());
    PathLock { key }
}

// ---------------------------------------------------------------------------
// What each writer has seen, and who wrote last
// ---------------------------------------------------------------------------

/// What one chat's run has seen of each file: the content hash when it
/// last read or wrote it.
pub struct Seen {
    owner: String,
    files: Mutex<HashMap<PathBuf, Hash>>,
}

impl Seen {
    pub fn new(owner: impl Into<String>) -> Arc<Seen> {
        Arc::new(Seen {
            owner: owner.into(),
            files: Mutex::new(HashMap::new()),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn record(&self, path: &Path, content: &[u8]) {
        self.record_hash(path, hash(content));
    }

    pub fn record_hash(&self, path: &Path, hash: Hash) {
        self.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(path), hash);
    }

    pub fn get(&self, path: &Path) -> Option<Hash> {
        self.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(path))
            .copied()
    }

    fn writer(&self) -> Writer {
        // No owner: a write from the UI (the editor's tools), not a chat's.
        if self.owner.is_empty() {
            Writer::Editor
        } else {
            Writer::Chat(self.owner.clone())
        }
    }
}

/// Forgets what was written under `dir` (a chat's view that's gone).
pub fn forget_under(dir: &Path) {
    let dir = key(dir);
    LEDGER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|path, _| !path.starts_with(&dir));
}

static LEDGER: LazyLock<Mutex<HashMap<PathBuf, (Writer, Hash)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn note_write(path: &Path, writer: Writer, hash: Hash) {
    LEDGER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key(path), (writer, hash));
}

/// Who wrote the file's current content through the app, if anyone did.
fn writer_of(path: &Path, current: Hash) -> Option<Writer> {
    let ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    ledger
        .get(&key(path))
        .filter(|(_, h)| *h == current)
        .map(|(w, _)| w.clone())
}

// ---------------------------------------------------------------------------
// Checked writes
// ---------------------------------------------------------------------------

/// A write that wasn't made: the file changed since the writer saw it.
#[derive(Debug)]
pub struct Changed {
    /// The file as it is now.
    pub current: Vec<u8>,
    /// Who changed it, when that was a write through the app.
    pub by: Option<Writer>,
}

#[derive(Debug)]
pub enum WriteError {
    Changed(Changed),
    Io(io::Error),
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

/// Who changed a file, in words, for the one who was refused.
pub fn describe_changer(by: Option<&Writer>, me: Option<&str>) -> &'static str {
    match by {
        Some(Writer::Chat(id)) if Some(id.as_str()) != me => "by another chat",
        Some(Writer::Editor) => "by the user in the editor",
        _ => "outside this chat: by a command, the user or another program",
    }
}

/// Writes a file in place (keeping its inode: the editor's file id), as
/// the app's other writes do.
fn write_file(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, content)
}

fn read_existing(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A chat run's whole-file write. Refused when the file exists and either
/// - the run has seen it, and it changed since; or
/// - the run never saw it, and its content is another writer's change.
///
/// The run then counts as having seen the new content.
pub fn write_for_chat(path: &Path, content: &[u8], seen: &Seen) -> Result<(), WriteError> {
    let _lock = lock(path);
    if let Some(current) = read_existing(path)? {
        let now = hash(&current);
        let refused = match seen.get(path) {
            Some(saw) => saw != now,
            None => matches!(writer_of(path, now), Some(w) if w != seen.writer()),
        };
        if refused {
            return Err(WriteError::Changed(Changed {
                by: writer_of(path, now),
                current,
            }));
        }
    }
    write_file(path, content)?;
    let new = hash(content);
    seen.record_hash(path, new);
    note_write(path, seen.writer(), new);
    Ok(())
}

/// A chat run's change to part of a file: `change` gets the file as it is
/// now (under the lock, so nothing writes in between) and returns the new
/// content. Its own check (the edit's text still being there) decides.
pub fn update_for_chat<T>(
    path: &Path,
    seen: &Seen,
    change: impl FnOnce(&str) -> Result<(String, T), String>,
) -> Result<(String, T), String> {
    let _lock = lock(path);
    let current = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (content, extra) = change(&current)?;
    write_file(path, content.as_bytes()).map_err(|e| e.to_string())?;
    let new = hash(content.as_bytes());
    seen.record_hash(path, new);
    note_write(path, seen.writer(), new);
    Ok((content, extra))
}

/// The editor's save: refused when the file isn't what the tab loaded
/// (`expected`, the hash of the loaded content). `None` writes regardless:
/// a new file, or the user chose to overwrite.
pub fn write_for_editor(
    path: &Path,
    content: &[u8],
    expected: Option<Hash>,
) -> Result<(), WriteError> {
    let _lock = lock(path);
    if let Some(expected) = expected {
        if let Some(current) = read_existing(path)? {
            let now = hash(&current);
            if now != expected {
                return Err(WriteError::Changed(Changed {
                    by: writer_of(path, now),
                    current,
                }));
            }
        }
    }
    write_file(path, content)?;
    note_write(path, Writer::Editor, hash(content));
    Ok(())
}

/// A file the app writes itself (a saved plan): created with its folders,
/// in place, under the file's lock, but not as the user's save.
pub fn write_app_file(path: &Path, content: &[u8]) -> Result<(), String> {
    let _lock = lock(path);
    write_file(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

/// A chat's changes that landed in the project (overlay/land.rs): written
/// by that chat, as far as other writers' checks go.
pub fn note_landing(path: &Path, dialog_id: &str, content: &[u8]) {
    note_write(path, Writer::Chat(dialog_id.to_string()), hash(content));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "file-guard-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn one_key_per_file_whatever_the_spelling() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let a = key(&dir.join("sub").join("f.txt"));
        let b = key(&dir.join("sub").join(".").join("f.txt"));
        let c = key(&dir.join("sub").join("..").join("sub").join("f.txt"));
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    /// Each file's lock once, whatever paths name it: a second lock of a
    /// file this thread holds would wait for itself.
    #[test]
    fn lock_all_takes_each_files_lock_once() {
        let dir = temp_dir();
        let file = dir.join("f.txt");
        fs::write(&file, "f").unwrap();
        let mut paths = vec![file.clone(), dir.join(".").join("f.txt"), dir.join("g.txt")];
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            paths.push(link);
        }
        if cfg!(any(target_os = "macos", windows)) {
            paths.push(dir.join("F.TXT"));
        }
        let (done, taken) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let locks = lock_all(paths.iter().map(PathBuf::as_path));
            let _ = done.send(locks.len());
        });
        let taken = taken.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(taken, Ok(2), "f.txt's lock and g.txt's");
    }

    #[test]
    fn a_write_after_reading_succeeds_and_counts_as_seen() {
        let dir = temp_dir();
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let seen = Seen::new("chat-a");
        seen.record(&path, b"one\n");
        write_for_chat(&path, b"two\n", &seen).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        // Its own write is what it saw last: it may write again.
        write_for_chat(&path, b"three\n", &seen).unwrap();
    }

    #[test]
    fn a_change_since_reading_refuses_the_write() {
        let dir = temp_dir();
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let a = Seen::new("chat-a");
        let b = Seen::new("chat-b");
        a.record(&path, b"one\n");
        b.record(&path, b"one\n");
        write_for_chat(&path, b"b's change\n", &b).unwrap();
        let Err(WriteError::Changed(changed)) = write_for_chat(&path, b"a's change\n", &a) else {
            panic!("a's write should be refused")
        };
        assert_eq!(changed.current, b"b's change\n");
        assert_eq!(changed.by, Some(Writer::Chat("chat-b".into())));
        assert_eq!(
            describe_changer(changed.by.as_ref(), Some("chat-a")),
            "by another chat"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "b's change\n");
    }

    #[test]
    fn a_change_by_a_command_refuses_the_write() {
        let dir = temp_dir();
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let a = Seen::new("chat-a");
        a.record(&path, b"one\n");
        fs::write(&path, "formatted\n").unwrap();
        let Err(WriteError::Changed(changed)) = write_for_chat(&path, b"x\n", &a) else {
            panic!("refused")
        };
        assert_eq!(changed.by, None);
    }

    #[test]
    fn a_file_never_read_is_refused_only_over_another_writers_change() {
        let dir = temp_dir();
        let path = dir.join("a.txt");
        fs::write(&path, "untouched by the app\n").unwrap();
        let a = Seen::new("chat-a");
        // Nobody wrote it through the app: as before, the write goes through.
        write_for_chat(&path, b"a\n", &a).unwrap();

        let b = Seen::new("chat-b");
        let Err(WriteError::Changed(_)) = write_for_chat(&path, b"b\n", &b) else {
            panic!("b never saw a's change")
        };
        // A new file has nothing to lose.
        write_for_chat(&dir.join("new.txt"), b"new\n", &b).unwrap();
    }

    #[test]
    fn racing_partial_updates_lose_nothing() {
        let dir = temp_dir();
        let path = dir.join("count.txt");
        fs::write(&path, "").unwrap();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let seen = Seen::new(format!("chat-{i}"));
                    for _ in 0..25 {
                        update_for_chat(&path, &seen, |cur| Ok((format!("{cur}x"), ()))).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap().len(), 200);
    }

    #[test]
    fn the_editor_save_checks_what_the_tab_loaded() {
        let dir = temp_dir();
        let path = dir.join("a.txt");
        fs::write(&path, "loaded\n").unwrap();
        let loaded = hash(b"loaded\n");
        let chat = Seen::new("chat-a");
        chat.record(&path, b"loaded\n");
        write_for_chat(&path, b"agent\n", &chat).unwrap();
        let Err(WriteError::Changed(changed)) = write_for_editor(&path, b"user\n", Some(loaded))
        else {
            panic!("the save should be refused")
        };
        assert_eq!(changed.current, b"agent\n");
        // Overwrite on purpose.
        write_for_editor(&path, b"user\n", None).unwrap();
        // The chat hasn't seen the user's save: its next write is refused.
        assert!(matches!(
            write_for_chat(&path, b"agent 2\n", &chat),
            Err(WriteError::Changed(_))
        ));
    }
}

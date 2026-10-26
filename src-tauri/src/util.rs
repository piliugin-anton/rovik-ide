//! What the app's modules share: work off the async threads, an error as
//! the text a command returns, and the live things a command addresses by
//! id (terminals, language servers).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// An error as the text a command returns: every command fails with a
/// `String`, which the frontend shows.
pub trait ErrText<T> {
    fn err_text(self) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ErrText<T> for Result<T, E> {
    fn err_text(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
    }
}

/// `work` on a worker thread, off the async runtime: disk, git and child
/// processes block, and as plain commands they ran on the app's main
/// thread, so a slow commit or a push waiting on the network froze the
/// whole window. A worker that panics is an error like any other.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    rovik_core::rt::spawn_blocking(work).await.err_text()?
}

/// `load`, off the async threads, on the folder `root`: walking and reading
/// thousands of files must not stall the other commands. An error when
/// `root` isn't a folder.
pub async fn blocking_in<T: Send + 'static>(
    root: String,
    load: impl FnOnce(&Path) -> T + Send + 'static,
) -> Result<T, String> {
    let root = PathBuf::from(root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    blocking(move || Ok(load(&root))).await
}

/// `work` off the main thread while the repository's folders wait: the
/// working tree at `path` is being rewritten (rovik_core::overlay::chats).
/// The hold is on the repository's working folder, not on `path` alone: a
/// checkout, a pull or a discard rewrites every folder the repository holds,
/// so the isolated chats of a workspace's folders beside it hold off too.
pub async fn rewriting<T: Send + 'static>(
    path: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let held = working_folder_of(Path::new(path));
    let _busy = rovik_core::overlay::chats::hold_busy(&held);
    blocking(work).await
}

/// The working folder of the repository `folder` is in, or `folder` itself
/// outside one: what a rewrite of the working tree touches.
fn working_folder_of(folder: &Path) -> PathBuf {
    git2::Repository::discover(folder)
        .ok()
        .and_then(|repo| repo.workdir().map(Path::to_path_buf))
        .unwrap_or_else(|| folder.to_path_buf())
}

/// Live things by id, each shared with the threads that serve it. The lock
/// is only held to look an entry up.
pub struct Registry<T> {
    /// What they are, for the error naming one that isn't there.
    what: &'static str,
    items: Mutex<HashMap<String, Arc<T>>>,
}

impl<T> Registry<T> {
    pub fn new(what: &'static str) -> Self {
        Registry {
            what,
            items: Mutex::new(HashMap::new()),
        }
    }

    fn items(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<T>>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, id: &str) -> Result<Arc<T>, String> {
        self.items()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("{} {id} isn't running", self.what))
    }

    pub fn insert(&self, id: String, item: T) -> Arc<T> {
        let item = Arc::new(item);
        self.items().insert(id, item.clone());
        item
    }

    pub fn remove(&self, id: &str) -> Option<Arc<T>> {
        self.items().remove(id)
    }

    /// Every item, taken out: the registry is left empty.
    pub fn take_all(&self) -> Vec<Arc<T>> {
        self.items().drain().map(|(_, item)| item).collect()
    }
}

/// `path` as Windows programs show it: `canonicalize` there gives the
/// verbatim form (`\\?\C:\p`, `\\?\UNC\server\share`), which other
/// paths and the user's own spelling never match. Any other path as it is.
pub fn plain_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        // A drive's path only: `\\?\Volume{…}` has no plain form.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_windows_path_is_shown_plain() {
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\C:\Users\me\app")),
            PathBuf::from(r"C:\Users\me\app")
        );
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\UNC\server\share\app")),
            PathBuf::from(r"\\server\share\app")
        );
        for kept in [r"\\?\Volume{1234}\app", "/home/me/app", r"C:\app"] {
            assert_eq!(plain_path(PathBuf::from(kept)), PathBuf::from(kept));
        }
    }

    #[test]
    fn a_registry_hands_out_what_it_holds() {
        let terminals: Registry<u32> = Registry::new("terminal");
        assert_eq!(
            terminals.get("t1").unwrap_err(),
            "terminal t1 isn't running"
        );
        let held = terminals.insert("t1".into(), 7);
        assert!(Arc::ptr_eq(&held, &terminals.get("t1").unwrap()));
        assert_eq!(*terminals.remove("t1").unwrap(), 7);
        assert!(terminals.remove("t1").is_none());
        assert!(terminals.get("t1").is_err());
    }

    #[test]
    fn taking_all_from_a_registry_leaves_it_empty() {
        let terminals: Registry<u32> = Registry::new("terminal");
        terminals.insert("t1".into(), 1);
        terminals.insert("t2".into(), 2);
        let mut taken: Vec<u32> = terminals.take_all().iter().map(|t| **t).collect();
        taken.sort();
        assert_eq!(taken, [1, 2]);
        assert!(terminals.get("t1").is_err() && terminals.get("t2").is_err());
        assert!(terminals.take_all().is_empty());
    }

    #[test]
    fn errors_become_their_text() {
        let failed: Result<(), std::io::Error> = Err(std::io::Error::other("the disk is full"));
        assert_eq!(failed.err_text().unwrap_err(), "the disk is full");
        assert_eq!(Ok::<_, std::io::Error>(1).err_text(), Ok(1));
        let out = rovik_core::rt::block_on(blocking(|| Ok::<_, String>(2)));
        assert_eq!(out, Ok(2));
        let missing = rovik_core::rt::block_on(blocking_in("/no/such/folder".into(), |_| 0));
        assert!(missing.unwrap_err().ends_with("not a folder"));
    }
}

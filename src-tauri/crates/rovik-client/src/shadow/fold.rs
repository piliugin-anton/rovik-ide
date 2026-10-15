//! A chat's overlay folded into the snapshot that takes the chat to a server
//! (6.4, T09). The landing base, `S0'`, is the project as it is now with
//! every path the chat changed as it was when the chat first changed it,
//! from the overlay's base copies. The start, `C0`, is the chat's changes on
//! that base, from its upper files. A file the user edited after the chat
//! did then lands merged with the user's edit, as it would have from the
//! overlay.

use super::{git, hold, Result, Shadow};
use git2::build::TreeUpdateBuilder;
use git2::{FileMode, Oid, Tree};
use rovik_core::overlay::fs::OverlayChange;
use rovik_core::vfs::store::Kind;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;

/// The two commits a chat moves with, `C0` on `S0'`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Folded {
    /// `S0'`: the landing base.
    pub base: Oid,
    /// `C0`: the chat's changes.
    pub start: Oid,
}

impl Shadow {
    /// Folds the chat's overlay changes (`overlay::chats::changes`) into a
    /// snapshot of the project. A file the chat deleted that changed here
    /// since is a conflict the user settles first.
    pub fn fold(&self, chat: &str, changes: &[OverlayChange]) -> Result<Folded> {
        let snapshot = self.snapshot(chat)?;
        let _held = hold(&self.busy);
        let now = self.repo.find_tree(snapshot.tree).map_err(git)?;
        let changes: Vec<&OverlayChange> = changes.iter().filter(|c| !c.by_product()).collect();
        let paths = |write: bool| -> HashSet<&str> {
            let wanted = changes
                .iter()
                .filter(|c| matches!(c, OverlayChange::Write { .. }) == write);
            wanted.map(|c| c.path()).collect()
        };
        let (written, deleted) = (paths(true), paths(false));

        let mut base = TreeUpdateBuilder::new();
        for change in &changes {
            let OverlayChange::Write {
                path,
                kind,
                origin,
                base_copy,
                ..
            } = change
            else {
                continue;
            };
            if let (Some(origin), Some(copy)) = (origin, base_copy) {
                let at = self.in_tree(origin);
                let mode = mode_in(&now, &at).unwrap_or(FileMode::Blob);
                base.upsert(at.as_str(), self.blob(copy, &at)?, mode);
            }
            // What the chat made wasn't there before it, unless it replaced
            // the project's own.
            let made = origin.as_deref() != Some(path.as_str()) && !deleted.contains(path.as_str());
            let at = self.in_tree(path);
            if made && *kind != Kind::Dir && has(&now, &at) {
                base.remove(at.as_str());
            }
        }
        let base_tree = base.create_updated(&self.repo, &now).map_err(git)?;
        let base_tree = self.repo.find_tree(base_tree).map_err(git)?;

        let mut start = TreeUpdateBuilder::new();
        for change in &changes {
            match change {
                OverlayChange::Delete {
                    path, base: was, ..
                } => {
                    // Replaced: what's there is its write's.
                    if written.contains(path.as_str()) {
                        continue;
                    }
                    let abs = self.root.join(path);
                    let there = fs::symlink_metadata(&abs).is_ok();
                    if there && was.as_ref().is_some_and(|was| !was.still(&abs)) {
                        return Err(format!(
                            "{path}: the chat deleted it, and it changed here since. Settle that first."
                        ));
                    }
                    let at = self.in_tree(path);
                    if has(&base_tree, &at) {
                        start.remove(at.as_str());
                    }
                }
                OverlayChange::Write {
                    kind: Kind::Dir, ..
                } => {}
                OverlayChange::Write {
                    path,
                    kind: Kind::Symlink,
                    target,
                    ..
                } => {
                    let target = target.as_deref().unwrap_or_default();
                    let link = self.repo.blob(target.as_bytes()).map_err(git)?;
                    start.upsert(self.in_tree(path).as_str(), link, FileMode::Link);
                }
                OverlayChange::Write {
                    path,
                    origin,
                    upper,
                    perm,
                    ..
                } => {
                    let at = self.in_tree(path);
                    let content = match (upper, origin) {
                        (Some(file), _) => self.blob(file, &at)?,
                        // Moved as it was: the project's file it came from.
                        (None, Some(origin)) => {
                            let from = now.get_path(Path::new(&self.in_tree(origin)));
                            from.map_err(git)?.id()
                        }
                        (None, None) => self.repo.blob(b"").map_err(git)?,
                    };
                    let mode = if perm & 0o111 != 0 {
                        FileMode::BlobExecutable
                    } else {
                        FileMode::Blob
                    };
                    start.upsert(at.as_str(), content, mode);
                }
            }
        }
        let start_tree = start.create_updated(&self.repo, &base_tree).map_err(git)?;
        let base = self.commit(base_tree.id(), Some(snapshot.commit), chat)?;
        let start = self.commit(start_tree, Some(base), chat)?;
        Ok(Folded { base, start })
    }

    /// A path of the project, as the snapshots' tree has it.
    fn in_tree(&self, path: &str) -> String {
        if self.subpath.is_empty() {
            path.to_string()
        } else {
            format!("{}/{path}", self.subpath)
        }
    }

    /// A file of the overlay, stored as `git add` would store it at `path`:
    /// with the project's filters, such as its line endings (K40).
    fn blob(&self, file: &Path, path: &str) -> Result<Oid> {
        let bytes = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut writer = self.repo.blob_writer(Some(Path::new(path))).map_err(git)?;
        writer.write_all(&bytes).map_err(|e| e.to_string())?;
        writer.commit().map_err(git)
    }
}

fn has(tree: &Tree, path: &str) -> bool {
    tree.get_path(Path::new(path)).is_ok()
}

/// A file's mode in a tree.
fn mode_in(tree: &Tree, path: &str) -> Option<FileMode> {
    let entry = tree.get_path(Path::new(path)).ok()?;
    Some(match entry.filemode() {
        0o100755 => FileMode::BlobExecutable,
        0o120000 => FileMode::Link,
        _ => FileMode::Blob,
    })
}

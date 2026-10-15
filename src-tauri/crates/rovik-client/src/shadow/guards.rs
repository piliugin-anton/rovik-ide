//! What a project's first push would carry that the user hears about first
//! (6.4's guards): large files the project doesn't track, Git LFS files
//! that go up with their content, the size of its history, nested
//! repositories, and scripts with CRLF line endings. The move's preflight
//! (M06) lists them and asks.

use super::{git, slashed, Result, Shadow, Snapshot};
use git2::{Delta, ObjectType, TreeWalkMode, TreeWalkResult};
use std::fs;
use std::path::Path;

/// Untracked files above this are listed before the first push.
pub const LARGE_FILE: u64 = 50 << 20;
/// Above this, the project can go without its history.
pub const LARGE_HISTORY: u64 = 1 << 30;
/// Scripts are smaller: bigger files aren't read for line endings.
const SCRIPT_MAX: usize = 1 << 20;
/// How a Git LFS pointer starts; pointers are smaller than a KiB.
const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";
const LFS_POINTER_MAX: usize = 1024;
const SCRIPTS: [&str; 4] = ["sh", "bash", "zsh", "ksh"];
const GITLINK: i32 = 0o160000;
const EXECUTABLE: i32 = 0o100755;

#[derive(Debug, Default)]
pub struct Guards {
    /// Files over `LARGE_FILE` that `HEAD` doesn't have, with their sizes.
    pub large_files: Vec<(String, u64)>,
    /// Git LFS files whose content the snapshot carries in place of
    /// `HEAD`'s pointer, with their sizes: libgit2 runs no filter drivers,
    /// so they go up whole, and the server has them as they are here.
    pub lfs: Vec<(String, u64)>,
    /// The project's history, as the size of its object store: above
    /// `LARGE_HISTORY`, the user can send the project without it.
    pub history: u64,
    /// Nested repositories and submodules: v1 doesn't carry them.
    pub nested: Vec<String>,
    /// Scripts whose CRLF line endings the project doesn't normalise: they
    /// won't run on Linux.
    pub crlf_scripts: Vec<String>,
}

impl Shadow {
    /// The guards for pushing `snapshot`.
    pub fn guards(&self, snapshot: &Snapshot) -> Result<Guards> {
        let tree = self.repo.find_tree(snapshot.tree).map_err(git)?;
        let head = self.head().and_then(|commit| commit.tree().ok());
        let odb = self.repo.odb().map_err(git)?;
        let size = |id| odb.read_header(id).map_or(0, |(size, _)| size);
        let mut guards = Guards {
            history: self.history_size(),
            nested: snapshot.nested.clone(),
            ..Guards::default()
        };
        let diff = self
            .repo
            .diff_tree_to_tree(head.as_ref(), Some(&tree), None)
            .map_err(git)?;
        for delta in diff.deltas() {
            let file = delta.new_file();
            let (Some(path), bytes) = (file.path(), size(file.id()) as u64) else {
                continue;
            };
            match delta.status() {
                Delta::Added if bytes > LARGE_FILE => {
                    guards.large_files.push((slashed(path), bytes));
                }
                Delta::Modified
                    if self.is_lfs_pointer(delta.old_file().id())
                        && !self.is_lfs_pointer(file.id()) =>
                {
                    guards.lfs.push((slashed(path), bytes));
                }
                _ => {}
            }
        }
        // Submodules are `HEAD`'s gitlinks: snapshots leave them out.
        if let Some(head) = &head {
            head.walk(TreeWalkMode::PreOrder, |dir, entry| {
                if entry.filemode() == GITLINK {
                    let name = String::from_utf8_lossy(entry.name_bytes());
                    guards.nested.push(format!("{dir}{name}"));
                }
                TreeWalkResult::Ok
            })
            .map_err(git)?;
        }
        tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
            let name = String::from_utf8_lossy(entry.name_bytes());
            if entry.kind() == Some(ObjectType::Blob)
                && is_script(&name, entry.filemode())
                && size(entry.id()) <= SCRIPT_MAX
                && self.has_crlf(entry.id())
            {
                guards.crlf_scripts.push(format!("{dir}{name}"));
            }
            TreeWalkResult::Ok
        })
        .map_err(git)?;
        guards.nested.sort();
        guards.nested.dedup();
        Ok(guards)
    }

    /// The size of the project's object store, which a pack of its history
    /// can't much exceed; nothing for a project without a repository.
    fn history_size(&self) -> u64 {
        self.project
            .as_ref()
            .map_or(0, |repo| size_of(&repo.commondir().join("objects")))
    }

    fn is_lfs_pointer(&self, blob: git2::Oid) -> bool {
        self.repo.find_blob(blob).is_ok_and(|blob| {
            blob.size() < LFS_POINTER_MAX && blob.content().starts_with(LFS_POINTER)
        })
    }

    fn has_crlf(&self, blob: git2::Oid) -> bool {
        self.repo.find_blob(blob).is_ok_and(|blob| {
            !blob.is_binary() && blob.content().windows(2).any(|pair| pair == b"\r\n")
        })
    }
}

/// A script by its mode or its name.
fn is_script(name: &str, mode: i32) -> bool {
    mode == EXECUTABLE
        || name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| SCRIPTS.contains(&extension))
}

fn size_of(dir: &Path) -> u64 {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size_of(&entry.path()),
            _ => entry.metadata().map_or(0, |meta| meta.len()),
        })
        .sum()
}

//! A work tree staged into its repository's index as `git add -A` would,
//! against a base commit's tree (K30). The client's shadow stages the
//! project against its `HEAD` for a snapshot (6.4); a daemon stages a
//! chat's worktree against its landing base for a result (6.5).
//!
//! - Entries the base has and the index lacks go in with their modes, for
//!   the files that are there, so a tracked file that matches an ignore
//!   rule stays in, and an OS without file modes keeps the base's.
//!   Submodules aren't files, and stay out. Entries the base doesn't have
//!   that an ignore rule matches now come out.
//! - One diff from the index reads the work tree. Files whose stat data
//!   hasn't changed aren't read. Those whose stat data has are hashed, and
//!   keep their new stat data when their content is the same, so the next
//!   staging reads only what changed.
//! - Nested repositories are left out: v1 doesn't carry them.

use git2::{Delta, DiffFile, DiffOptions, Index, IndexEntry, IndexTime, Oid, Repository, Tree};
use std::fs;
use std::path::{Path, PathBuf};

/// What a work tree is staged against.
pub enum Base<'a> {
    /// A commit's tree: what it has is tracked.
    Tree(&'a Tree<'a>),
    /// Nothing is tracked: a folder without a repository, or one without a
    /// commit yet.
    Nothing,
    /// A base that can't be read: nothing is known to be tracked or not,
    /// and the index stays as it is.
    Unknown,
}

/// What staging made.
pub struct Staged {
    pub tree: Oid,
    /// The files read and stored this time: those that changed since the
    /// last staging.
    pub hashed: usize,
    /// Nested repositories, left out.
    pub nested: Vec<String>,
}

/// Stages the repository's work tree into its index, against `base`, and
/// writes the index and its tree.
pub fn stage(repo: &Repository, base: Base) -> Result<Staged, String> {
    let workdir = repo
        .workdir()
        .ok_or("The repository has no work tree.")?
        .to_path_buf();
    let mut index = repo.index().map_err(git)?;
    match base {
        Base::Tree(tree) => align(repo, &mut index, Some(tree), &workdir)?,
        Base::Nothing => align(repo, &mut index, None, &workdir)?,
        Base::Unknown => {}
    }
    let (hashed, nested) = read_changes(repo, &mut index, &workdir)?;
    index.write().map_err(git)?;
    let tree = index.write_tree().map_err(git)?;
    Ok(Staged {
        tree,
        hashed,
        nested,
    })
}

/// Brings the index in line with the base before the work tree is read.
fn align(
    repo: &Repository,
    index: &mut Index,
    base: Option<&Tree>,
    workdir: &Path,
) -> Result<(), String> {
    let diff = repo
        .diff_tree_to_index(base, Some(index), None)
        .map_err(git)?;
    let mut missing = Vec::new();
    let mut untracked = Vec::new();
    for delta in diff.deltas() {
        match delta.status() {
            Delta::Deleted => missing.extend(unseen(&delta.old_file())),
            Delta::Added => untracked.extend(delta.new_file().path().map(Path::to_path_buf)),
            _ => {}
        }
    }
    drop(diff);
    for entry in missing {
        if has_file(workdir, &entry.path) {
            index.add(&entry).map_err(git)?;
        }
    }
    for path in untracked {
        if repo.is_path_ignored(&path).map_err(git)? {
            index.remove_path(&path).map_err(git)?;
        }
    }
    Ok(())
}

/// Reads the work tree into the index: untracked files go in unless an
/// ignore rule matches them, and deleted ones come out. Returns how many
/// files were stored, and the nested repositories left out.
fn read_changes(
    repo: &Repository,
    index: &mut Index,
    workdir: &Path,
) -> Result<(usize, Vec<String>), String> {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_typechange(true)
        .ignore_submodules(true)
        .update_index(true);
    let diff = repo
        .diff_index_to_workdir(Some(index), Some(&mut opts))
        .map_err(git)?;
    let changes: Vec<(Delta, PathBuf)> = diff
        .deltas()
        .filter_map(|delta| Some((delta.status(), delta.new_file().path()?.to_path_buf())))
        .collect();
    drop(diff);
    let mut hashed = 0;
    let mut nested = Vec::new();
    for (status, path) in changes {
        match status {
            Delta::Deleted => index.remove_path(&path).map_err(git)?,
            Delta::Untracked | Delta::Modified | Delta::Typechange => {
                let meta = fs::symlink_metadata(workdir.join(&path));
                if meta.is_ok_and(|meta| meta.is_dir()) {
                    let name = path.to_string_lossy().replace('\\', "/");
                    nested.push(name.trim_end_matches('/').to_string());
                    continue;
                }
                index
                    .add_path(&path)
                    .map_err(|e| format!("Can't read {}: {}", path.display(), e.message()))?;
                hashed += 1;
            }
            _ => {}
        }
    }
    Ok((hashed, nested))
}

/// An index entry for a file the base has, with no stat data: the next
/// read of the work tree hashes the file once, and an OS without file
/// modes keeps the base's.
fn unseen(file: &DiffFile) -> Option<IndexEntry> {
    let zero = IndexTime::new(0, 0);
    Some(IndexEntry {
        ctime: zero,
        mtime: zero,
        dev: 0,
        ino: 0,
        mode: u32::from(file.mode()),
        uid: 0,
        gid: 0,
        file_size: 0,
        id: file.id(),
        flags: 0,
        flags_extended: 0,
        path: file.path_bytes()?.to_vec(),
    })
}

/// Whether the work tree has a file or a link at `path`, from its root.
fn has_file(workdir: &Path, path: &[u8]) -> bool {
    let path = String::from_utf8_lossy(path);
    fs::symlink_metadata(workdir.join(&*path)).is_ok_and(|meta| !meta.is_dir())
}

fn git(e: git2::Error) -> String {
    e.message().to_string()
}

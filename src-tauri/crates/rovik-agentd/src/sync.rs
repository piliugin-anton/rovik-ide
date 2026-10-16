//! A chat's worktree brought up to date with the project before a message
//! (6.7, D14). Each path the project changed since the chat's landing base
//! is merged three ways, from the base, the new snapshot and the worktree
//! (`merge::merge_bytes`): a file the chat didn't change takes the
//! project's version, and one both changed, on other lines, takes both.
//!
//! - Nothing conflicts: every path is applied, the chat's branch moves to
//!   the snapshot and its index follows (mixed), so the agent's
//!   `git status` still shows exactly the chat's changes.
//! - A binary conflict, or a file deleted on one side and changed on the
//!   other: nothing is applied, whatever the setting. Those stay with the
//!   user, as they do locally.
//! - Text conflicts, and the chat resolves conflicts: every path is
//!   applied, with the local kind of markers in the files that conflict
//!   (`overlay::markers`).
//! - Text conflicts otherwise: nothing is applied, and the base stays.
//!
//! The run goes on in every case.

use crate::store::Kept;
use git2::{Oid, Repository, ResetType, Tree};
use rovik_core::merge;
use rovik_core::overlay::{chats, markers};
use rovik_protocol::work::SyncState;
use rovik_protocol::{Fail, FailCode};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// What a sync did: its state, and the files that state is about.
pub struct Merged {
    pub state: SyncState,
    pub files: Vec<String>,
    /// For the files of a sync that merged cleanly: what the project
    /// changed in each, its diff from the landing base (6.6).
    pub diffs: Vec<String>,
    /// What the merge replaced in the chat's files, once applied.
    pub kept: Vec<(String, Kept)>,
}

impl Merged {
    /// The worktree is on the snapshot now.
    pub fn applied(&self) -> bool {
        matches!(self.state, SyncState::Synced | SyncState::Marked)
    }
}

/// A path on one side: a file, a link, or something no merge takes (a
/// folder, a submodule).
#[derive(Clone, PartialEq)]
enum Entry {
    File { bytes: Vec<u8>, exec: bool },
    Link(Vec<u8>),
    Other,
}

/// Merges `snapshot` into the worktree, whose landing base is `base`.
/// `resolves`: the chat resolves conflicts itself, with markers.
pub fn merge(worktree: &Path, base: &str, snapshot: Oid, resolves: bool) -> Result<Merged, Fail> {
    let git = |e: git2::Error| internal(e.message().to_string());
    let repo = Repository::open(worktree).map_err(git)?;
    let base = (Oid::from_str(base).and_then(|base| repo.find_commit(base))).map_err(git)?;
    let snapshot = repo
        .find_commit(snapshot)
        .map_err(|_| bad_request("The snapshot isn't on this server: push it first."))?;
    if base.id() == snapshot.id() {
        return Ok(Merged {
            state: SyncState::Synced,
            files: Vec::new(),
            diffs: Vec::new(),
            kept: Vec::new(),
        });
    }
    let (old, new) = (base.tree().map_err(git)?, snapshot.tree().map_err(git)?);
    let diff = repo
        .diff_tree_to_tree(Some(&old), Some(&new), None)
        .map_err(git)?;
    let paths: BTreeSet<String> = diff
        .deltas()
        .filter_map(|delta| delta.new_file().path().or(delta.old_file().path()))
        .map(|path| path.to_string_lossy().into_owned())
        .collect();

    let (mut apply, mut merged, mut marked, mut stuck) = (Vec::new(), vec![], vec![], vec![]);
    let mut diffs = Vec::new();
    // The chat's version of each file both changed, its base, and what
    // replaces them.
    let mut replaced = Vec::new();
    for path in paths {
        let side = |tree: &Tree| in_tree(&repo, tree, &path).map_err(git);
        let (was, now) = (side(&old)?, side(&new)?);
        let chats = on_disk(&worktree.join(&path));
        if chats == was {
            apply.push((path, now));
            continue;
        }
        if chats == now {
            continue;
        }
        // Deleted on one side, a link or a folder: the user's to settle.
        let both = now
            .as_ref()
            .and_then(file)
            .zip(chats.as_ref().and_then(file));
        let base = was.as_ref().map(file);
        let (Some(((project, project_exec), (chat, chat_exec))), false) =
            (both, base == Some(None))
        else {
            stuck.push(path);
            continue;
        };
        let (base_bytes, base_exec) = base.flatten().unzip();
        // The chat's own change of mode stands.
        let exec = if Some(chat_exec) != base_exec {
            chat_exec
        } else {
            project_exec
        };
        let base = base_bytes.zip(base_exec);
        let kept = |written: &[u8]| Replaced::new(&path, chat, base, written);
        match merge::merge_bytes(base_bytes.unwrap_or_default(), chat, project) {
            Ok(Some(bytes)) => {
                replaced.push(kept(&bytes));
                apply.push((path.clone(), Some(Entry::File { bytes, exec })));
                diffs.push(chats::short_diff(base_bytes.unwrap_or_default(), project));
                merged.push(path);
            }
            Ok(None) => match markers::mark(base_bytes, project, chat) {
                Some(bytes) => {
                    replaced.push(kept(&bytes));
                    marked.push((path, Entry::File { bytes, exec }));
                }
                None => stuck.push(path),
            },
            Err(_) => stuck.push(path),
        }
    }

    let marked_paths = || {
        marked
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>()
    };
    if !stuck.is_empty() {
        stuck.extend(marked_paths());
        stuck.sort();
        return Ok(Merged {
            state: SyncState::Unmergeable,
            files: stuck,
            diffs: Vec::new(),
            kept: Vec::new(),
        });
    }
    if !marked.is_empty() && !resolves {
        return Ok(Merged {
            state: SyncState::Conflicts,
            files: marked_paths(),
            diffs: Vec::new(),
            kept: Vec::new(),
        });
    }
    let files = marked_paths();
    let kept = replaced.iter().map(|r| r.keep(&repo));
    let kept = kept.collect::<Result<Vec<_>, _>>().map_err(git)?;
    let markers = marked.into_iter().map(|(path, entry)| (path, Some(entry)));
    for (path, entry) in apply.into_iter().chain(markers) {
        write(&worktree.join(&path), entry.as_ref())
            .map_err(|e| internal(format!("{path}: {e}")))?;
    }
    repo.reset(snapshot.as_object(), ResetType::Mixed, None)
        .map_err(git)?;
    Ok(if files.is_empty() {
        Merged {
            state: SyncState::Synced,
            files: merged,
            diffs,
            kept,
        }
    } else {
        Merged {
            state: SyncState::Marked,
            files,
            diffs: Vec::new(),
            kept,
        }
    })
}

/// What a merge or conflict markers replace in a file: the chat's version,
/// its base (and whether that's executable), and what's written there.
pub struct Replaced {
    path: String,
    chat: Vec<u8>,
    base: Option<(Vec<u8>, bool)>,
    written: Vec<u8>,
}

impl Replaced {
    pub fn new(path: &str, chat: &[u8], base: Option<(&[u8], bool)>, written: &[u8]) -> Self {
        Replaced {
            path: path.to_string(),
            chat: chat.to_vec(),
            base: base.map(|(bytes, exec)| (bytes.to_vec(), exec)),
            written: written.to_vec(),
        }
    }

    /// Kept as blobs in the repository, for Undo the merge (6.1).
    pub fn keep(&self, repo: &Repository) -> Result<(String, Kept), git2::Error> {
        let base = self.base.as_ref().map(|(bytes, _)| repo.blob(bytes));
        let kept = Kept {
            chat: repo.blob(&self.chat)?.to_string(),
            base: base.transpose()?.map(|id| id.to_string()),
            base_exec: self.base.as_ref().is_some_and(|(_, exec)| *exec),
            written: repo.blob(&self.written)?.to_string(),
        };
        Ok((self.path.clone(), kept))
    }
}

/// A file's bytes, and whether it's executable: not a link or a folder.
fn file(entry: &Entry) -> Option<(&[u8], bool)> {
    match entry {
        Entry::File { bytes, exec } => Some((bytes, *exec)),
        _ => None,
    }
}

/// A path as a tree has it.
fn in_tree(repo: &Repository, tree: &Tree, path: &str) -> Result<Option<Entry>, git2::Error> {
    let Ok(entry) = tree.get_path(Path::new(path)) else {
        return Ok(None);
    };
    let blob = || {
        repo.find_blob(entry.id())
            .map(|blob| blob.content().to_vec())
    };
    Ok(Some(match entry.filemode() {
        0o100644 | 0o100664 => Entry::File {
            bytes: blob()?,
            exec: false,
        },
        0o100755 => Entry::File {
            bytes: blob()?,
            exec: true,
        },
        0o120000 => Entry::Link(blob()?),
        _ => Entry::Other,
    }))
}

/// A path as the worktree has it.
fn on_disk(path: &Path) -> Option<Entry> {
    let meta = fs::symlink_metadata(path).ok()?;
    Some(if meta.file_type().is_symlink() {
        Entry::Link(fs::read_link(path).ok()?.as_os_str().as_bytes().to_vec())
    } else if meta.is_file() {
        Entry::File {
            bytes: fs::read(path).ok()?,
            exec: meta.permissions().mode() & 0o111 != 0,
        }
    } else {
        Entry::Other
    })
}

/// Puts `entry` at `path`, in place of what was there; None deletes it.
fn write(path: &Path, entry: Option<&Entry>) -> io::Result<()> {
    let is_file = fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file());
    if !matches!(entry, Some(Entry::File { .. })) || !is_file {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    if let (Some(parent), Some(_)) = (path.parent(), entry) {
        fs::create_dir_all(parent)?;
    }
    match entry {
        Some(Entry::File { bytes, exec }) => {
            fs::write(path, bytes)?;
            let mode = if *exec { 0o755 } else { 0o644 };
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
        }
        Some(Entry::Link(target)) => {
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), path)
        }
        Some(Entry::Other) | None => Ok(()),
    }
}

fn bad_request(message: &str) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

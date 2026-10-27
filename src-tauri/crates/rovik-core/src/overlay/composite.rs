//! A workspace chat's view (docs/design/workspaces-plan.md, 4.2, D22): one
//! folder whose top-level folders are the chat's folders by name, each its
//! folder's own overlay (a part). Every call goes to the part an inode or a
//! path is in; only these need more than that:
//!
//! - **Inode numbers**: a part's inode `n` is `(n << 16) | slot`, a slot per
//!   part never given to another while the view is mounted; the root keeps
//!   its number, and a part's start at 2 and are never reused, so the
//!   kernel's caches stay right. Renumbered, they stay below 2^53.
//! - **The root** lists the folders by name and is read-only: nothing is
//!   created, linked, removed or renamed there, and its attributes don't
//!   change, whoever asks (macFUSE's FSKit asks as root).
//! - **Across folders**, a rename or a link is a cross-device error (EXDEV),
//!   which `mv` takes as a copy and a delete.
//! - **Changes** are named from the view's root: a change's path and where
//!   it came from both start with its folder's name.
//! - **Folders that join or leave** (`matched`) get a new slot and a root
//!   entry, or leave theirs, the kernel told; nothing is mounted again.

use super::fs::{separator, Budget, ChatFs, OverlayChange, OverlayFs};
use crate::folders::{Folder, Folders};
use crate::vfs::backend::Backend;
use crate::vfs::store::{
    Attr, Change, DirEntry, Ino, Kind, Listener, RenameMode, SetAttr, VfsError, VfsResult, ROOT,
};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

/// The low bits of a view's inode number: its part's slot.
const SLOT_BITS: u32 = 16;
const SLOT_MASK: Ino = (1 << SLOT_BITS) - 1;

/// The view's number for a part's inode `n`.
fn view_ino(slot: u16, n: Ino) -> Ino {
    (n << SLOT_BITS) | slot as Ino
}

/// A part's slot and its own number for one of the view's inodes (not the
/// root's).
fn part_ino(ino: Ino) -> (u16, Ino) {
    ((ino & SLOT_MASK) as u16, ino >> SLOT_BITS)
}

/// The view's numbers for what a part tells the kernel.
fn renumbered(slot: u16, changes: &[Change]) -> Vec<Change> {
    changes
        .iter()
        .map(|c| match c {
            Change::Data(n) => Change::Data(view_ino(slot, *n)),
            Change::Entry { parent, name } => Change::Entry {
                parent: view_ino(slot, *parent),
                name: name.clone(),
            },
        })
        .collect()
}

/// Whether two of the view's names are one: as the file system the
/// overlays sit on takes them (it ignores case on macOS and Windows).
fn same_name(a: &str, b: &str) -> bool {
    a == b || (cfg!(any(target_os = "macos", windows)) && a.to_lowercase() == b.to_lowercase())
}

/// The first part of a chat path, and the rest.
fn split(path: &str) -> (&str, &str) {
    let path = path.trim_start_matches(separator);
    match path.find(separator) {
        Some(at) => (&path[..at], path[at + 1..].trim_end_matches(separator)),
        None => (path, ""),
    }
}

/// One of the chat's folders in the view.
struct Part {
    name: String,
    slot: u16,
    fs: Arc<OverlayFs>,
}

pub struct CompositeFs {
    parts: RwLock<Vec<Part>>,
    /// The next part's slot: never one given before while the view lives.
    next_slot: Mutex<u16>,
    /// The chat's room, which its parts share.
    budget: Arc<Budget>,
    /// What the kernel is told, through which each part's renumbered
    /// notifications go too.
    listener: Mutex<Option<Arc<Listener>>>,
    /// Folders that joined or left: the view's generation moves with them.
    epoch: AtomicU64,
    uid: u32,
    gid: u32,
    made: SystemTime,
}

impl CompositeFs {
    /// An empty view whose parts share `budget`.
    pub fn new(budget: Arc<Budget>) -> CompositeFs {
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0, 0);
        CompositeFs {
            parts: RwLock::new(Vec::new()),
            next_slot: Mutex::new(1),
            budget,
            listener: Mutex::new(None),
            epoch: AtomicU64::new(0),
            uid,
            gid,
            made: SystemTime::now(),
        }
    }

    /// The chat's room, for its parts' overlays to be opened in.
    pub fn budget(&self) -> Arc<Budget> {
        self.budget.clone()
    }

    /// The folders in the view now: each name and the project its part is
    /// of.
    pub fn folders(&self) -> Vec<(String, PathBuf)> {
        self.current()
            .iter()
            .map(|p| (p.name.clone(), p.fs.base().to_path_buf()))
            .collect()
    }

    /// The overlay of the folder named `name`.
    pub fn part(&self, name: &str) -> Option<Arc<OverlayFs>> {
        self.current()
            .iter()
            .find(|p| same_name(&p.name, name))
            .map(|p| p.fs.clone())
    }

    /// The view matched to `folders`: a folder it has at the same place
    /// stays as it is (its inodes valid); one that's new, or that's now
    /// somewhere else, gets a new slot and an overlay `open` makes; one
    /// that's no longer among them leaves. The kernel is told of every name
    /// that came or went at the root. Returns the parts that left, for their
    /// stores to go. Nothing changes if an overlay can't be opened.
    pub fn matched(
        &self,
        folders: &Folders,
        mut open: impl FnMut(&Folder) -> io::Result<Arc<OverlayFs>>,
    ) -> io::Result<Vec<(String, Arc<OverlayFs>)>> {
        // Opened before the view changes: one that can't be leaves it as it
        // was, and nothing waits on the view while the others are read.
        let had = self.folders();
        let mut opened: Vec<(String, Arc<OverlayFs>)> = Vec::new();
        for folder in folders.iter() {
            if !had
                .iter()
                .any(|(n, p)| *n == folder.name && *p == folder.path)
            {
                opened.push((folder.name.clone(), open(folder)?));
            }
        }
        let mut parts = self.parts.write().unwrap_or_else(|e| e.into_inner());
        let mut next: Vec<Part> = Vec::new();
        let mut came: Vec<String> = Vec::new();
        for folder in folders.iter() {
            let kept = parts
                .iter()
                .position(|p| p.name == folder.name && p.fs.base() == folder.path);
            match kept {
                Some(at) => next.push(parts.remove(at)),
                None => {
                    let at = opened.iter().position(|(name, _)| *name == folder.name);
                    let Some(at) = at else {
                        return Err(io::Error::other(format!(
                            "{} changed while the chat's view was matched to its folders",
                            folder.name
                        )));
                    };
                    let (name, fs) = opened.remove(at);
                    next.push(Part {
                        name: name.clone(),
                        slot: self.slot()?,
                        fs,
                    });
                    came.push(name);
                }
            }
        }
        let left: Vec<Part> = std::mem::replace(&mut *parts, next);
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for part in parts.iter().filter(|p| came.contains(&p.name)) {
            part.fs.set_listener(Self::forward(&listener, part.slot));
        }
        drop(parts);
        for part in &left {
            part.fs.set_listener(None);
        }
        let names: Vec<Change> = left
            .iter()
            .map(|p| p.name.clone())
            .chain(came)
            .map(|name| Change::Entry { parent: ROOT, name })
            .collect();
        if !names.is_empty() {
            self.epoch.fetch_add(1, Ordering::Relaxed);
            if let Some(listener) = &listener {
                listener(&names);
            }
        }
        Ok(left.into_iter().map(|p| (p.name, p.fs)).collect())
    }

    /// A slot no part had before.
    fn slot(&self) -> io::Result<u16> {
        let mut next = self.next_slot.lock().unwrap_or_else(|e| e.into_inner());
        let slot = *next;
        *next = next.checked_add(1).ok_or_else(|| {
            io::Error::other("the chat's folders changed too often while it was open: close it and open it again")
        })?;
        Ok(slot)
    }

    /// A part's listener: what it tells the kernel, in the view's numbers.
    fn forward(listener: &Option<Arc<Listener>>, slot: u16) -> Option<Listener> {
        let listener = listener.clone()?;
        Some(Box::new(move |changes: &[Change]| {
            listener(&renumbered(slot, changes))
        }))
    }

    /// The view's parts now.
    fn current(&self) -> std::sync::RwLockReadGuard<'_, Vec<Part>> {
        self.parts.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The part one of the view's inodes is in, and its own number for it.
    fn part_of(&self, ino: Ino) -> VfsResult<(u16, Arc<OverlayFs>, Ino)> {
        let (slot, n) = part_ino(ino);
        let parts = self.current();
        let part = parts
            .iter()
            .find(|p| p.slot == slot)
            .ok_or(VfsError::NotFound)?;
        Ok((slot, part.fs.clone(), n))
    }

    /// The part a chat path is in, and its path in that part.
    fn part_at<'a>(&self, path: &'a str) -> VfsResult<(u16, Arc<OverlayFs>, &'a str)> {
        let (name, rest) = split(path);
        let parts = self.current();
        let part = parts
            .iter()
            .find(|p| same_name(&p.name, name))
            .ok_or(VfsError::NotFound)?;
        Ok((part.slot, part.fs.clone(), rest))
    }

    fn root_attr(&self) -> Attr {
        let folders = self.current().len() as u32;
        Attr {
            ino: ROOT,
            kind: Kind::Dir,
            size: 0,
            perm: 0o555,
            nlink: 2 + folders,
            uid: self.uid,
            gid: self.gid,
            atime: self.made,
            mtime: self.made,
            ctime: self.made,
            crtime: self.made,
            attributes: 0,
        }
    }

    /// A part's attributes in the view's numbers.
    fn attr(slot: u16, mut attr: Attr) -> Attr {
        attr.ino = view_ino(slot, attr.ino);
        attr
    }

    /// The two parts two of the view's inodes are in, when one: a rename or
    /// a link across folders is across file systems.
    fn same_part(&self, a: Ino, b: Ino) -> VfsResult<(u16, Arc<OverlayFs>, Ino, Ino)> {
        if a == ROOT || b == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (slot, fs, a) = self.part_of(a)?;
        let (other, _, b) = self.part_of(b)?;
        if slot != other {
            return Err(VfsError::CrossDevice);
        }
        Ok((slot, fs, a, b))
    }
}

impl Backend for CompositeFs {
    fn getattr(&self, ino: Ino) -> VfsResult<Attr> {
        if ino == ROOT {
            return Ok(self.root_attr());
        }
        let (slot, fs, n) = self.part_of(ino)?;
        fs.getattr(n).map(|a| Self::attr(slot, a))
    }

    fn lookup(&self, parent: Ino, name: &str) -> VfsResult<Attr> {
        if parent == ROOT {
            let (slot, fs, _) = self.part_at(name)?;
            return fs.getattr(ROOT).map(|a| Self::attr(slot, a));
        }
        let (slot, fs, n) = self.part_of(parent)?;
        fs.lookup(n, name).map(|a| Self::attr(slot, a))
    }

    fn parent_of(&self, dir: Ino) -> VfsResult<Ino> {
        if dir == ROOT {
            return Ok(ROOT);
        }
        let (slot, fs, n) = self.part_of(dir)?;
        if n == ROOT {
            return Ok(ROOT);
        }
        fs.parent_of(n).map(|p| view_ino(slot, p))
    }

    fn create(
        &self,
        parent: Ino,
        name: &str,
        kind: Kind,
        perm: u16,
        uid: u32,
        gid: u32,
        target: Option<&str>,
    ) -> VfsResult<Attr> {
        if parent == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (slot, fs, n) = self.part_of(parent)?;
        fs.create(n, name, kind, perm, uid, gid, target)
            .map(|a| Self::attr(slot, a))
    }

    fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()> {
        if parent == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (_, fs, n) = self.part_of(parent)?;
        fs.unlink(n, name)
    }

    fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()> {
        if parent == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (_, fs, n) = self.part_of(parent)?;
        fs.rmdir(n, name)
    }

    fn rename(
        &self,
        parent: Ino,
        name: &str,
        new_parent: Ino,
        new_name: &str,
        mode: RenameMode,
    ) -> VfsResult<()> {
        let (_, fs, from, to) = self.same_part(parent, new_parent)?;
        fs.rename(from, name, to, new_name, mode)
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr> {
        let (slot, fs, n, to) = self.same_part(ino, new_parent)?;
        fs.link(n, to, new_name).map(|a| Self::attr(slot, a))
    }

    fn readlink(&self, ino: Ino) -> VfsResult<String> {
        if ino == ROOT {
            return Err(VfsError::Invalid);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.readlink(n)
    }

    fn read(&self, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        if ino == ROOT {
            return Err(VfsError::IsDir);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.read(n, offset, size)
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if ino == ROOT {
            return Err(VfsError::IsDir);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.write(n, offset, data)
    }

    fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
        if ino == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (slot, fs, n) = self.part_of(ino)?;
        fs.setattr(n, set).map(|a| Self::attr(slot, a))
    }

    fn dir_is_empty(&self, ino: Ino) -> VfsResult<bool> {
        if ino == ROOT {
            return Ok(self.current().is_empty());
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.dir_is_empty(n)
    }

    fn readdir_plus(&self, ino: Ino) -> VfsResult<Vec<(DirEntry, Attr)>> {
        if ino == ROOT {
            let parts: Vec<(String, u16, Arc<OverlayFs>)> = self
                .current()
                .iter()
                .map(|p| (p.name.clone(), p.slot, p.fs.clone()))
                .collect();
            return Ok(parts
                .into_iter()
                .filter_map(|(name, slot, fs)| {
                    let attr = Self::attr(slot, fs.getattr(ROOT).ok()?);
                    let entry = DirEntry {
                        name,
                        ino: attr.ino,
                        kind: Kind::Dir,
                    };
                    Some((entry, attr))
                })
                .collect());
        }
        let (slot, fs, n) = self.part_of(ino)?;
        Ok(fs
            .readdir_plus(n)?
            .into_iter()
            .map(|(mut entry, attr)| {
                entry.ino = view_ino(slot, entry.ino);
                (entry, Self::attr(slot, attr))
            })
            .collect())
    }

    fn open(&self, ino: Ino, write: bool) -> VfsResult<Attr> {
        if ino == ROOT {
            return Ok(self.root_attr());
        }
        let (slot, fs, n) = self.part_of(ino)?;
        fs.open(n, write).map(|a| Self::attr(slot, a))
    }

    fn release(&self, ino: Ino) {
        if ino == ROOT {
            return;
        }
        if let Ok((_, fs, n)) = self.part_of(ino) {
            fs.release(n);
        }
    }

    fn getxattr(&self, ino: Ino, name: &str) -> VfsResult<Vec<u8>> {
        if ino == ROOT {
            return Err(VfsError::NoAttr);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.getxattr(n, name)
    }

    fn setxattr(
        &self,
        ino: Ino,
        name: &str,
        value: &[u8],
        create_only: bool,
        replace_only: bool,
    ) -> VfsResult<()> {
        if ino == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.setxattr(n, name, value, create_only, replace_only)
    }

    fn listxattr(&self, ino: Ino) -> VfsResult<Vec<String>> {
        if ino == ROOT {
            return Ok(Vec::new());
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.listxattr(n)
    }

    fn removexattr(&self, ino: Ino, name: &str) -> VfsResult<()> {
        if ino == ROOT {
            return Err(VfsError::NotPermitted);
        }
        let (_, fs, n) = self.part_of(ino)?;
        fs.removexattr(n, name)
    }

    /// The chat's room, and its parts' nodes added up.
    fn usage(&self) -> (u64, u64, u64) {
        let nodes: u64 = self.current().iter().map(|p| p.fs.usage().2).sum();
        (self.budget.limit(), self.budget.used(), nodes + 1)
    }

    fn set_listener(&self, listener: Option<Listener>) {
        let listener = listener.map(Arc::new);
        *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = listener.clone();
        for part in self.current().iter() {
            part.fs.set_listener(Self::forward(&listener, part.slot));
        }
    }

    fn resolve(&self, path: &str) -> VfsResult<Ino> {
        if split(path).0.is_empty() {
            return Ok(ROOT);
        }
        let (slot, fs, rest) = self.part_at(path)?;
        fs.resolve(rest).map(|n| view_ino(slot, n))
    }

    fn resolve_parent<'a>(&self, path: &'a str) -> VfsResult<(Ino, &'a str)> {
        let (name, rest) = split(path);
        if rest.is_empty() {
            return Ok((ROOT, name));
        }
        let (slot, fs, rest) = self.part_at(path)?;
        let (parent, last) = fs.resolve_parent(rest)?;
        Ok((view_ino(slot, parent), last))
    }

    fn path_of(&self, ino: Ino) -> VfsResult<String> {
        if ino == ROOT {
            return Ok(String::new());
        }
        let (slot, fs, n) = self.part_of(ino)?;
        let name = self
            .current()
            .iter()
            .find(|p| p.slot == slot)
            .map(|p| p.name.clone())
            .ok_or(VfsError::NotFound)?;
        match fs.path_of(n)? {
            rest if rest.is_empty() => Ok(name),
            rest => Ok(format!("{name}/{rest}")),
        }
    }
}

impl ChatFs for CompositeFs {
    /// Every folder's changes, in the workspace's order, each named from the
    /// view's root.
    fn changes(&self) -> Vec<OverlayChange> {
        let parts: Vec<(String, Arc<OverlayFs>)> = self
            .current()
            .iter()
            .map(|p| (p.name.clone(), p.fs.clone()))
            .collect();
        parts
            .into_iter()
            .flat_map(|(name, fs)| {
                let changes = fs.changes();
                changes.into_iter().map(move |c| c.under(&name))
            })
            .collect()
    }

    fn versions(&self, rel: &str) -> Option<(PathBuf, PathBuf)> {
        let (_, fs, rest) = self.part_at(rel).ok()?;
        fs.versions(rest)
    }

    fn rebase_file(&self, rel: &str, content: Option<&[u8]>, base: Option<&[u8]>) -> VfsResult<()> {
        let (_, fs, rest) = self.part_at(rel)?;
        if rest.is_empty() {
            return Err(VfsError::NotPermitted);
        }
        fs.rebase_file(rest, content, base)
    }

    fn revert_file(&self, rel: &str) -> VfsResult<bool> {
        let (_, fs, rest) = self.part_at(rel)?;
        if rest.is_empty() {
            return Ok(false);
        }
        fs.revert_file(rest)
    }

    fn flush(&self) {
        for part in self.current().iter() {
            part.fs.flush();
        }
    }

    fn close_handles(&self) {
        for part in self.current().iter() {
            part.fs.close_handles();
        }
    }

    fn used(&self) -> (u64, u64) {
        (self.budget.used(), self.budget.limit())
    }

    fn by_product_bytes(&self) -> u64 {
        self.current().iter().map(|p| p.fs.by_product_bytes()).sum()
    }

    /// Different whenever a folder's changes moved, or a folder joined or
    /// left.
    fn generation(&self) -> u64 {
        let changes: u64 = self.current().iter().map(|p| p.fs.generation()).sum();
        (self.epoch.load(Ordering::Relaxed) << 40).wrapping_add(changes)
    }
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;

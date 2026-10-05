//! What the virtual filesystem holds: an in-memory tree of inodes.
//!
//! Both drivers (FUSE on Linux/macOS, WinFsp on Windows) translate kernel
//! requests into calls on [`VirtualFs`], and the app reaches the same files
//! through the path-based API at the bottom. Nothing here is platform
//! specific, so all the filesystem semantics are tested on any OS.
//!
//! The semantics follow POSIX closely enough for ordinary tools: hard links,
//! symlinks, extended attributes, rename that replaces atomically, and a file
//! that's unlinked while open stays readable until its last handle closes.
//! One lock guards the whole tree — every operation is a memory copy, so
//! contention isn't worth the complexity of finer locking.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::SystemTime;

pub type Ino = u64;
pub const ROOT: Ino = 1;

/// Longest name of one path component, in bytes.
pub const MAX_NAME: usize = 255;
/// Symlinks deeper than this are a loop.
const MAX_SYMLINK_HOPS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfsError {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    Invalid,
    NameTooLong,
    NoSpace,
    NotPermitted,
    Loop,
    NoAttr,
    Range,
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VfsError::NotFound => "no such file or directory",
            VfsError::Exists => "file exists",
            VfsError::NotDir => "not a directory",
            VfsError::IsDir => "is a directory",
            VfsError::NotEmpty => "directory not empty",
            VfsError::Invalid => "invalid argument",
            VfsError::NameTooLong => "file name too long",
            VfsError::NoSpace => "no space left in the virtual filesystem",
            VfsError::NotPermitted => "operation not permitted",
            VfsError::Loop => "too many levels of symbolic links",
            VfsError::NoAttr => "no such attribute",
            VfsError::Range => "buffer too small",
        })
    }
}

impl std::error::Error for VfsError {}

pub type VfsResult<T> = Result<T, VfsError>;

/// A node's metadata, as the drivers report it.
#[derive(Clone, Debug, PartialEq)]
pub struct Attr {
    pub ino: Ino,
    pub kind: Kind,
    pub size: u64,
    /// Permission bits (`0o7777`).
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,
    /// Windows `FILE_ATTRIBUTE_*` flags other than the directory bit
    /// (read-only, hidden, archive…). Unused by the FUSE driver.
    pub attributes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub ino: Ino,
    pub kind: Kind,
}

/// What `setattr` changes; `None` leaves a field as it is.
#[derive(Clone, Debug, Default)]
pub struct SetAttr {
    pub perm: Option<u16>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
    pub crtime: Option<SystemTime>,
    pub attributes: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameMode {
    /// POSIX `rename`: an existing target is replaced.
    Replace,
    /// Fail with `Exists` if the target exists.
    NoReplace,
    /// Swap the two entries; both must exist.
    Exchange,
}

/// An app-side change the driver should tell the kernel about, so it drops
/// what it cached. Changes made through the kernel are never reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A file's contents or size changed.
    Data(Ino),
    /// An entry in a directory appeared, disappeared or now points elsewhere.
    Entry { parent: Ino, name: String },
}

type Listener = Box<dyn Fn(&[Change]) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct Options {
    /// Total bytes of file contents the filesystem may hold.
    pub capacity: u64,
    /// Windows: names match regardless of case (and keep the case they were
    /// created with), as every Windows program expects.
    pub case_insensitive: bool,
    /// Owner of the root and of files the app creates.
    pub uid: u32,
    pub gid: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { capacity: 1 << 30, case_insensitive: cfg!(windows), uid: 0, gid: 0 }
    }
}

struct Child {
    /// The name as created (the map key may be case-folded).
    name: String,
    ino: Ino,
}

struct Node {
    kind: Kind,
    /// File contents, or a symlink's target.
    data: Vec<u8>,
    /// Directories only, keyed by the (possibly folded) name: sorted, so
    /// listings are stable.
    children: BTreeMap<String, Child>,
    /// Directories only: the containing directory (the root's is itself).
    parent: Ino,
    perm: u16,
    uid: u32,
    gid: u32,
    nlink: u32,
    /// Open handles; an unlinked file lives until this is zero.
    open: u32,
    attributes: u32,
    xattrs: BTreeMap<String, Vec<u8>>,
    atime: SystemTime,
    mtime: SystemTime,
    ctime: SystemTime,
    crtime: SystemTime,
}

impl Node {
    fn new(kind: Kind, perm: u16, uid: u32, gid: u32) -> Self {
        let now = SystemTime::now();
        Node {
            kind,
            data: Vec::new(),
            children: BTreeMap::new(),
            parent: ROOT,
            perm: perm & 0o7777,
            uid,
            gid,
            nlink: if kind == Kind::Dir { 2 } else { 1 },
            open: 0,
            attributes: 0,
            xattrs: BTreeMap::new(),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
        }
    }

    fn attr(&self, ino: Ino) -> Attr {
        Attr {
            ino,
            kind: self.kind,
            size: self.data.len() as u64,
            perm: self.perm,
            nlink: self.nlink,
            uid: self.uid,
            gid: self.gid,
            atime: self.atime,
            mtime: self.mtime,
            ctime: self.ctime,
            crtime: self.crtime,
            attributes: self.attributes,
        }
    }

    fn touch_modified(&mut self) {
        let now = SystemTime::now();
        self.mtime = now;
        self.ctime = now;
    }
}

struct Tree {
    nodes: BTreeMap<Ino, Node>,
    next_ino: Ino,
    /// Bytes of file contents held, counted against `Options::capacity`.
    used: u64,
    opts: Options,
}

impl Tree {
    fn node(&self, ino: Ino) -> VfsResult<&Node> {
        self.nodes.get(&ino).ok_or(VfsError::NotFound)
    }

    fn node_mut(&mut self, ino: Ino) -> VfsResult<&mut Node> {
        self.nodes.get_mut(&ino).ok_or(VfsError::NotFound)
    }

    fn dir(&self, ino: Ino) -> VfsResult<&Node> {
        let node = self.node(ino)?;
        if node.kind != Kind::Dir {
            return Err(VfsError::NotDir);
        }
        Ok(node)
    }

    fn key(&self, name: &str) -> String {
        if self.opts.case_insensitive {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    }

    fn child(&self, parent: Ino, name: &str) -> VfsResult<Ino> {
        if name == "." {
            return self.dir(parent).map(|_| parent);
        }
        if name == ".." {
            return self.dir(parent).map(|d| d.parent);
        }
        self.dir(parent)?.children.get(&self.key(name)).map(|c| c.ino).ok_or(VfsError::NotFound)
    }

    fn alloc(&mut self, node: Node) -> Ino {
        let ino = self.next_ino;
        self.next_ino += 1;
        self.nodes.insert(ino, node);
        ino
    }

    /// Makes room for `new_len` bytes in a node that holds `old_len`.
    fn reserve(&mut self, old_len: usize, new_len: usize) -> VfsResult<()> {
        if new_len > old_len {
            let grow = (new_len - old_len) as u64;
            if self.used + grow > self.opts.capacity {
                return Err(VfsError::NoSpace);
            }
            self.used += grow;
        } else {
            self.used -= (old_len - new_len) as u64;
        }
        Ok(())
    }

    /// Frees a node nothing refers to any more.
    fn collect(&mut self, ino: Ino) {
        let dead = self.nodes.get(&ino).is_some_and(|n| n.nlink == 0 && n.open == 0);
        if dead {
            if let Some(node) = self.nodes.remove(&ino) {
                if node.kind == Kind::File {
                    self.used -= node.data.len() as u64;
                }
            }
        }
    }

    fn insert_child(&mut self, parent: Ino, name: &str, ino: Ino) -> VfsResult<()> {
        let key = self.key(name);
        let is_dir = self.node(ino)?.kind == Kind::Dir;
        let dir = self.node_mut(parent)?;
        dir.children.insert(key, Child { name: name.to_string(), ino });
        if is_dir {
            dir.nlink += 1;
        }
        dir.touch_modified();
        if is_dir {
            self.node_mut(ino)?.parent = parent;
        }
        Ok(())
    }

    /// Takes an entry out of its directory without freeing the node.
    fn detach(&mut self, parent: Ino, name: &str) -> VfsResult<Ino> {
        let key = self.key(name);
        let dir = self.node_mut(parent)?;
        let child = dir.children.remove(&key).ok_or(VfsError::NotFound)?;
        dir.touch_modified();
        if self.node(child.ino)?.kind == Kind::Dir {
            self.node_mut(parent)?.nlink -= 1;
        }
        Ok(child.ino)
    }

    /// Whether `ino` is `ancestor` or somewhere inside it.
    fn is_within(&self, mut ino: Ino, ancestor: Ino) -> bool {
        loop {
            if ino == ancestor {
                return true;
            }
            match self.nodes.get(&ino) {
                Some(node) if node.kind == Kind::Dir && ino != ROOT => ino = node.parent,
                _ => return false,
            }
        }
    }

    /// Drops one link to `ino`, which was just detached from a directory.
    fn unlink_node(&mut self, ino: Ino) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            if node.kind == Kind::Dir {
                node.nlink = 0;
            } else {
                node.nlink = node.nlink.saturating_sub(1);
            }
            node.ctime = SystemTime::now();
        }
        self.collect(ino);
    }
}

pub struct VirtualFs {
    tree: RwLock<Tree>,
    listener: RwLock<Option<Listener>>,
}

fn check_name(name: &str) -> VfsResult<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(VfsError::Invalid);
    }
    if cfg!(windows) && name.contains('\\') {
        return Err(VfsError::Invalid);
    }
    if name.len() > MAX_NAME {
        return Err(VfsError::NameTooLong);
    }
    Ok(())
}

/// A path's components, accepting `/` and `\` and ignoring empty and `.`
/// parts. `..` is kept: it's resolved against the tree.
fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".")
}

impl VirtualFs {
    pub fn new(opts: Options) -> Arc<Self> {
        let mut root = Node::new(Kind::Dir, 0o755, opts.uid, opts.gid);
        root.parent = ROOT;
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, root);
        Arc::new(VirtualFs {
            tree: RwLock::new(Tree { nodes, next_ino: ROOT + 1, used: 0, opts }),
            listener: RwLock::new(None),
        })
    }

    fn tree(&self) -> RwLockReadGuard<'_, Tree> {
        self.tree.read().unwrap_or_else(|e| e.into_inner())
    }

    fn tree_mut(&self) -> RwLockWriteGuard<'_, Tree> {
        self.tree.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Called with what the app changed (see [`Change`]); the mounted driver
    /// uses it to invalidate the kernel's caches.
    pub fn set_listener(&self, listener: Option<Listener>) {
        *self.listener.write().unwrap_or_else(|e| e.into_inner()) = listener;
    }

    fn notify(&self, changes: &[Change]) {
        if changes.is_empty() {
            return;
        }
        if let Some(listener) = self.listener.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            listener(changes);
        }
    }

    // --- Inode operations (what the drivers call) -------------------------

    pub fn getattr(&self, ino: Ino) -> VfsResult<Attr> {
        let tree = self.tree();
        Ok(tree.node(ino)?.attr(ino))
    }

    pub fn lookup(&self, parent: Ino, name: &str) -> VfsResult<Attr> {
        let tree = self.tree();
        let ino = tree.child(parent, name)?;
        Ok(tree.node(ino)?.attr(ino))
    }

    /// Parent directory of a directory (the root's is itself).
    pub fn parent_of(&self, dir: Ino) -> VfsResult<Ino> {
        Ok(self.tree().dir(dir)?.parent)
    }

    /// Creates a file, directory or symlink (`target` for symlinks).
    pub fn create(&self, parent: Ino, name: &str, kind: Kind, perm: u16, uid: u32, gid: u32, target: Option<&str>) -> VfsResult<Attr> {
        check_name(name)?;
        let mut tree = self.tree_mut();
        tree.dir(parent)?;
        if tree.child(parent, name).is_ok() {
            return Err(VfsError::Exists);
        }
        let mut node = Node::new(kind, perm, uid, gid);
        if kind == Kind::Symlink {
            let target = target.ok_or(VfsError::Invalid)?;
            if target.is_empty() {
                return Err(VfsError::Invalid);
            }
            node.data = target.as_bytes().to_vec();
            node.perm = 0o777;
        }
        if kind == Kind::File {
            // FILE_ATTRIBUTE_ARCHIVE: a new file "needs backing up".
            node.attributes = 0x20;
        }
        let ino = tree.alloc(node);
        tree.insert_child(parent, name, ino)?;
        Ok(tree.node(ino)?.attr(ino))
    }

    /// Removes a non-directory entry.
    pub fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()> {
        let mut tree = self.tree_mut();
        let ino = tree.child(parent, name)?;
        if tree.node(ino)?.kind == Kind::Dir {
            return Err(VfsError::IsDir);
        }
        tree.detach(parent, name)?;
        tree.unlink_node(ino);
        Ok(())
    }

    /// Removes an empty directory.
    pub fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()> {
        if name == "." {
            return Err(VfsError::Invalid);
        }
        if name == ".." {
            return Err(VfsError::NotEmpty);
        }
        let mut tree = self.tree_mut();
        let ino = tree.child(parent, name)?;
        let node = tree.node(ino)?;
        if node.kind != Kind::Dir {
            return Err(VfsError::NotDir);
        }
        if !node.children.is_empty() {
            return Err(VfsError::NotEmpty);
        }
        tree.detach(parent, name)?;
        tree.unlink_node(ino);
        Ok(())
    }

    pub fn rename(&self, parent: Ino, name: &str, new_parent: Ino, new_name: &str, mode: RenameMode) -> VfsResult<()> {
        check_name(new_name)?;
        if name == "." || name == ".." {
            return Err(VfsError::Invalid);
        }
        let mut tree = self.tree_mut();
        tree.dir(new_parent)?;
        let src = tree.child(parent, name)?;
        let src_is_dir = tree.node(src)?.kind == Kind::Dir;
        let dst = tree.child(new_parent, new_name).ok();

        if src_is_dir && tree.is_within(new_parent, src) {
            // Into itself or its own subdirectory.
            return Err(VfsError::Invalid);
        }

        if mode == RenameMode::Exchange {
            let dst = dst.ok_or(VfsError::NotFound)?;
            if tree.node(dst)?.kind == Kind::Dir && tree.is_within(parent, dst) {
                return Err(VfsError::Invalid);
            }
            let dst_name = tree.node(new_parent)?.children[&tree.key(new_name)].name.clone();
            let src_name = tree.node(parent)?.children[&tree.key(name)].name.clone();
            tree.detach(parent, &src_name)?;
            tree.detach(new_parent, &dst_name)?;
            tree.insert_child(parent, &src_name, dst)?;
            tree.insert_child(new_parent, &dst_name, src)?;
            let now = SystemTime::now();
            tree.node_mut(src)?.ctime = now;
            tree.node_mut(dst)?.ctime = now;
            return Ok(());
        }

        if let Some(dst) = dst {
            if dst == src {
                // Same file (a hard link, or a case-only rename on a
                // case-insensitive volume): only the spelling may change.
                if parent == new_parent && tree.key(name) == tree.key(new_name) {
                    let key = tree.key(new_name);
                    if let Some(child) = tree.node_mut(new_parent)?.children.get_mut(&key) {
                        child.name = new_name.to_string();
                    }
                }
                return Ok(());
            }
            if mode == RenameMode::NoReplace {
                return Err(VfsError::Exists);
            }
            let dst_node = tree.node(dst)?;
            match (src_is_dir, dst_node.kind == Kind::Dir) {
                (true, false) => return Err(VfsError::NotDir),
                (false, true) => return Err(VfsError::IsDir),
                (true, true) if !dst_node.children.is_empty() => return Err(VfsError::NotEmpty),
                _ => {}
            }
            tree.detach(new_parent, new_name)?;
            tree.unlink_node(dst);
        }

        tree.detach(parent, name)?;
        tree.insert_child(new_parent, new_name, src)?;
        tree.node_mut(src)?.ctime = SystemTime::now();
        Ok(())
    }

    /// Adds a hard link to a file.
    pub fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr> {
        check_name(new_name)?;
        let mut tree = self.tree_mut();
        let node = tree.node(ino)?;
        if node.kind == Kind::Dir {
            return Err(VfsError::NotPermitted);
        }
        if node.nlink == 0 {
            return Err(VfsError::NotFound);
        }
        tree.dir(new_parent)?;
        if tree.child(new_parent, new_name).is_ok() {
            return Err(VfsError::Exists);
        }
        tree.insert_child(new_parent, new_name, ino)?;
        let node = tree.node_mut(ino)?;
        node.nlink += 1;
        node.ctime = SystemTime::now();
        Ok(node.attr(ino))
    }

    pub fn readlink(&self, ino: Ino) -> VfsResult<String> {
        let tree = self.tree();
        let node = tree.node(ino)?;
        if node.kind != Kind::Symlink {
            return Err(VfsError::Invalid);
        }
        Ok(String::from_utf8_lossy(&node.data).into_owned())
    }

    pub fn read(&self, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let tree = self.tree();
        let node = tree.node(ino)?;
        match node.kind {
            Kind::Dir => return Err(VfsError::IsDir),
            Kind::Symlink => return Err(VfsError::Invalid),
            Kind::File => {}
        }
        let len = node.data.len() as u64;
        if offset >= len {
            return Ok(Vec::new());
        }
        let end = len.min(offset.saturating_add(size as u64));
        Ok(node.data[offset as usize..end as usize].to_vec())
    }

    /// Writes at `offset`, filling any gap with zeros. Returns the bytes written.
    pub fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize> {
        let mut tree = self.tree_mut();
        let node = tree.node(ino)?;
        match node.kind {
            Kind::Dir => return Err(VfsError::IsDir),
            Kind::Symlink => return Err(VfsError::Invalid),
            Kind::File => {}
        }
        let old_len = node.data.len();
        let end = usize::try_from(offset).ok().and_then(|o| o.checked_add(data.len())).ok_or(VfsError::NoSpace)?;
        let new_len = old_len.max(end);
        tree.reserve(old_len, new_len)?;
        let node = tree.node_mut(ino)?;
        if new_len > old_len {
            node.data.resize(new_len, 0);
        }
        node.data[offset as usize..end].copy_from_slice(data);
        node.touch_modified();
        node.attributes |= 0x20;
        Ok(data.len())
    }

    pub fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
        let mut tree = self.tree_mut();
        let node = tree.node(ino)?;
        if let Some(size) = set.size {
            if node.kind == Kind::Dir {
                return Err(VfsError::IsDir);
            }
            if node.kind != Kind::File {
                return Err(VfsError::Invalid);
            }
            let new_len = usize::try_from(size).map_err(|_| VfsError::NoSpace)?;
            let old_len = node.data.len();
            tree.reserve(old_len, new_len)?;
            let node = tree.node_mut(ino)?;
            node.data.resize(new_len, 0);
            if new_len < node.data.capacity() / 2 {
                node.data.shrink_to_fit();
            }
            node.touch_modified();
        }
        let node = tree.node_mut(ino)?;
        let now = SystemTime::now();
        if let Some(perm) = set.perm {
            node.perm = perm & 0o7777;
            node.ctime = now;
        }
        if let Some(uid) = set.uid {
            node.uid = uid;
            node.ctime = now;
        }
        if let Some(gid) = set.gid {
            node.gid = gid;
            node.ctime = now;
        }
        if let Some(atime) = set.atime {
            node.atime = atime;
            node.ctime = now;
        }
        if let Some(mtime) = set.mtime {
            node.mtime = mtime;
            node.ctime = now;
        }
        if let Some(crtime) = set.crtime {
            node.crtime = crtime;
        }
        if let Some(attributes) = set.attributes {
            node.attributes = attributes;
            node.ctime = now;
        }
        Ok(node.attr(ino))
    }

    /// A directory's entries, without `.` and `..`.
    pub fn readdir(&self, ino: Ino) -> VfsResult<Vec<DirEntry>> {
        let tree = self.tree();
        let dir = tree.dir(ino)?;
        Ok(dir
            .children
            .values()
            .filter_map(|c| Some(DirEntry { name: c.name.clone(), ino: c.ino, kind: tree.nodes.get(&c.ino)?.kind }))
            .collect())
    }

    /// Notes an open handle, so the node outlives an unlink until `release`.
    pub fn open(&self, ino: Ino) -> VfsResult<Attr> {
        let mut tree = self.tree_mut();
        let node = tree.node_mut(ino)?;
        node.open += 1;
        Ok(node.attr(ino))
    }

    pub fn release(&self, ino: Ino) {
        let mut tree = self.tree_mut();
        if let Some(node) = tree.nodes.get_mut(&ino) {
            node.open = node.open.saturating_sub(1);
        }
        tree.collect(ino);
    }

    pub fn getxattr(&self, ino: Ino, name: &str) -> VfsResult<Vec<u8>> {
        self.tree().node(ino)?.xattrs.get(name).cloned().ok_or(VfsError::NoAttr)
    }

    pub fn setxattr(&self, ino: Ino, name: &str, value: &[u8], create_only: bool, replace_only: bool) -> VfsResult<()> {
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(VfsError::Invalid);
        }
        if value.len() > 64 * 1024 {
            return Err(VfsError::Range);
        }
        let mut tree = self.tree_mut();
        let node = tree.node_mut(ino)?;
        let exists = node.xattrs.contains_key(name);
        if create_only && exists {
            return Err(VfsError::Exists);
        }
        if replace_only && !exists {
            return Err(VfsError::NoAttr);
        }
        node.xattrs.insert(name.to_string(), value.to_vec());
        node.ctime = SystemTime::now();
        Ok(())
    }

    pub fn listxattr(&self, ino: Ino) -> VfsResult<Vec<String>> {
        Ok(self.tree().node(ino)?.xattrs.keys().cloned().collect())
    }

    pub fn removexattr(&self, ino: Ino, name: &str) -> VfsResult<()> {
        let mut tree = self.tree_mut();
        let node = tree.node_mut(ino)?;
        node.xattrs.remove(name).ok_or(VfsError::NoAttr)?;
        node.ctime = SystemTime::now();
        Ok(())
    }

    /// (capacity, bytes used, number of nodes)
    pub fn usage(&self) -> (u64, u64, u64) {
        let tree = self.tree();
        (tree.opts.capacity, tree.used, tree.nodes.len() as u64)
    }

    // --- Path operations (what the app calls) -----------------------------

    /// Follows a path from the root. Symlinks in the middle of the path are
    /// followed (absolute targets can't be: they point outside the volume);
    /// the last component is followed only if `follow` is set.
    fn resolve_in(tree: &Tree, path: &str, follow: bool, hops: &mut usize) -> VfsResult<Ino> {
        let parts: Vec<&str> = components(path).collect();
        let mut ino = ROOT;
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let parent = ino;
            ino = tree.child(ino, part)?;
            let node = tree.node(ino)?;
            if node.kind == Kind::Symlink && (!last || follow) {
                *hops += 1;
                if *hops > MAX_SYMLINK_HOPS {
                    return Err(VfsError::Loop);
                }
                let target = String::from_utf8_lossy(&node.data).into_owned();
                if target.starts_with('/') {
                    return Err(VfsError::NotFound);
                }
                let base = Self::path_of_dir(tree, parent)?;
                ino = Self::resolve_in(tree, &format!("{base}/{target}"), true, hops)?;
            }
        }
        Ok(ino)
    }

    fn path_of_dir(tree: &Tree, mut dir: Ino) -> VfsResult<String> {
        let mut parts = Vec::new();
        while dir != ROOT {
            let parent = tree.dir(dir)?.parent;
            let name = tree.dir(parent)?.children.values().find(|c| c.ino == dir).ok_or(VfsError::NotFound)?.name.clone();
            parts.push(name);
            dir = parent;
        }
        parts.reverse();
        Ok(parts.join("/"))
    }

    /// The node at `path` (relative to the volume root), following symlinks.
    pub fn resolve(&self, path: &str) -> VfsResult<Ino> {
        Self::resolve_in(&self.tree(), path, true, &mut 0)
    }

    /// The node at `path` without following a final symlink.
    pub fn resolve_nofollow(&self, path: &str) -> VfsResult<Ino> {
        Self::resolve_in(&self.tree(), path, false, &mut 0)
    }

    /// The parent directory of `path` and the last component's name.
    pub fn resolve_parent<'a>(&self, path: &'a str) -> VfsResult<(Ino, &'a str)> {
        let path = path.trim_end_matches(['/', '\\']);
        let (dir, name) = match path.rfind(['/', '\\']) {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        check_name(name)?;
        Ok((self.resolve(dir)?, name))
    }

    /// The path of a node relative to the root, using `/` (the first link,
    /// for a file with several). Unlinked nodes have none.
    pub fn path_of(&self, ino: Ino) -> VfsResult<String> {
        let tree = self.tree();
        if tree.node(ino)?.kind == Kind::Dir {
            return Self::path_of_dir(&tree, ino);
        }
        // Not kept per node: hard links make it a set. Files are found from
        // their directory, which is rare enough to search for.
        let mut stack = vec![ROOT];
        while let Some(dir) = stack.pop() {
            for child in tree.dir(dir)?.children.values() {
                if child.ino == ino {
                    let base = Self::path_of_dir(&tree, dir)?;
                    return Ok(if base.is_empty() { child.name.clone() } else { format!("{base}/{}", child.name) });
                }
                if tree.nodes.get(&child.ino).is_some_and(|n| n.kind == Kind::Dir) {
                    stack.push(child.ino);
                }
            }
        }
        Err(VfsError::NotFound)
    }

    /// Creates a directory and any missing parents.
    pub fn create_dir_all(&self, path: &str) -> VfsResult<Ino> {
        let mut changes = Vec::new();
        let result = (|| {
            let (uid, gid) = {
                let tree = self.tree();
                (tree.opts.uid, tree.opts.gid)
            };
            let mut ino = ROOT;
            for part in components(path) {
                match self.lookup(ino, part) {
                    Ok(attr) if attr.kind == Kind::Dir => ino = attr.ino,
                    Ok(attr) if attr.kind == Kind::Symlink => ino = self.resolve(&format!("{}/{part}", self.path_of(ino)?))?,
                    Ok(_) => return Err(VfsError::NotDir),
                    Err(VfsError::NotFound) => {
                        let attr = self.create(ino, part, Kind::Dir, 0o755, uid, gid, None)?;
                        changes.push(Change::Entry { parent: ino, name: part.to_string() });
                        ino = attr.ino;
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(ino)
        })();
        self.notify(&changes);
        result
    }

    /// Creates or replaces the file at `path` with `data`, creating parent
    /// directories. An existing file keeps its inode (open handles see the new
    /// contents), its owner and permissions unless `perm` is given.
    pub fn write_file(&self, path: &str, data: &[u8], perm: Option<u16>) -> VfsResult<Attr> {
        let (dir_path, name) = {
            let trimmed = path.trim_end_matches(['/', '\\']);
            match trimmed.rfind(['/', '\\']) {
                Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
                None => ("", trimmed),
            }
        };
        check_name(name)?;
        let parent = self.create_dir_all(dir_path)?;
        let mut changes = Vec::new();
        let result = (|| {
            let mut tree = self.tree_mut();
            let ino = match tree.child(parent, name) {
                Ok(ino) => {
                    let mut ino = ino;
                    if tree.node(ino)?.kind == Kind::Symlink {
                        drop(tree);
                        ino = self.resolve(path)?;
                        tree = self.tree_mut();
                    }
                    if tree.node(ino)?.kind == Kind::Dir {
                        return Err(VfsError::IsDir);
                    }
                    ino
                }
                Err(VfsError::NotFound) => {
                    let (uid, gid) = (tree.opts.uid, tree.opts.gid);
                    let mut node = Node::new(Kind::File, perm.unwrap_or(0o644), uid, gid);
                    node.attributes = 0x20;
                    let ino = tree.alloc(node);
                    tree.insert_child(parent, name, ino)?;
                    changes.push(Change::Entry { parent, name: name.to_string() });
                    ino
                }
                Err(e) => return Err(e),
            };
            let old_len = tree.node(ino)?.data.len();
            if let Err(e) = tree.reserve(old_len, data.len()) {
                if changes.is_empty() {
                    return Err(e);
                }
                // Don't leave a new, empty file behind.
                drop(tree);
                let _ = self.unlink(parent, name);
                return Err(e);
            }
            let node = tree.node_mut(ino)?;
            node.data = data.to_vec();
            node.touch_modified();
            if let Some(perm) = perm {
                node.perm = perm & 0o7777;
            }
            changes.push(Change::Data(ino));
            Ok(node.attr(ino))
        })();
        self.notify(&changes);
        result
    }

    pub fn read_file(&self, path: &str) -> VfsResult<Vec<u8>> {
        let ino = self.resolve(path)?;
        self.read(ino, 0, usize::MAX)
    }

    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn metadata(&self, path: &str) -> VfsResult<Attr> {
        self.getattr(self.resolve_nofollow(path)?)
    }

    pub fn list(&self, path: &str) -> VfsResult<Vec<DirEntry>> {
        self.readdir(self.resolve(path)?)
    }

    /// Removes a file, symlink or a whole directory tree.
    pub fn remove(&self, path: &str) -> VfsResult<()> {
        let (parent, name) = self.resolve_parent(path)?;
        let name = name.to_string();
        let ino = self.lookup(parent, &name)?.ino;
        let result = (|| {
            let mut tree = self.tree_mut();
            if tree.node(ino)?.kind == Kind::Dir {
                // Unlink everything inside, depth first.
                let mut stack = vec![ino];
                let mut order = Vec::new();
                while let Some(dir) = stack.pop() {
                    order.push(dir);
                    for child in tree.dir(dir)?.children.values() {
                        if tree.nodes.get(&child.ino).is_some_and(|n| n.kind == Kind::Dir) {
                            stack.push(child.ino);
                        }
                    }
                }
                for dir in order.into_iter().rev() {
                    let names: Vec<String> = tree.dir(dir)?.children.values().map(|c| c.name.clone()).collect();
                    for child_name in names {
                        let child = tree.detach(dir, &child_name)?;
                        tree.unlink_node(child);
                    }
                }
            }
            tree.detach(parent, &name)?;
            tree.unlink_node(ino);
            Ok(())
        })();
        if result.is_ok() {
            self.notify(&[Change::Entry { parent, name }]);
        }
        result
    }

    /// Renames within the volume (paths relative to the root), replacing an
    /// existing target like `rename(2)`.
    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn rename_path(&self, from: &str, to: &str) -> VfsResult<()> {
        let (parent, name) = self.resolve_parent(from)?;
        let (new_parent, new_name) = self.resolve_parent(to)?;
        self.rename(parent, name, new_parent, new_name, RenameMode::Replace)?;
        self.notify(&[
            Change::Entry { parent, name: name.to_string() },
            Change::Entry { parent: new_parent, name: new_name.to_string() },
        ]);
        Ok(())
    }

    /// Creates a symlink at `path` pointing to `target`.
    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn symlink(&self, path: &str, target: &str) -> VfsResult<Attr> {
        let (parent, name) = self.resolve_parent(path)?;
        let (uid, gid) = {
            let tree = self.tree();
            (tree.opts.uid, tree.opts.gid)
        };
        let attr = self.create(parent, name, Kind::Symlink, 0o777, uid, gid, Some(target))?;
        self.notify(&[Change::Entry { parent, name: name.to_string() }]);
        Ok(attr)
    }

    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn set_permissions(&self, path: &str, perm: u16) -> VfsResult<Attr> {
        let ino = self.resolve(path)?;
        self.setattr(ino, &SetAttr { perm: Some(perm), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> Arc<VirtualFs> {
        VirtualFs::new(Options { capacity: 1024, case_insensitive: false, uid: 1000, gid: 1000 })
    }

    fn file(fs: &VirtualFs, parent: Ino, name: &str) -> Attr {
        fs.create(parent, name, Kind::File, 0o644, 1000, 1000, None).unwrap()
    }

    fn dir(fs: &VirtualFs, parent: Ino, name: &str) -> Attr {
        fs.create(parent, name, Kind::Dir, 0o755, 1000, 1000, None).unwrap()
    }

    #[test]
    fn writes_and_reads_with_gaps() {
        let fs = fs();
        let f = file(&fs, ROOT, "a.txt");
        assert_eq!(fs.write(f.ino, 0, b"hello").unwrap(), 5);
        assert_eq!(fs.write(f.ino, 8, b"!").unwrap(), 1);
        assert_eq!(fs.read(f.ino, 0, 100).unwrap(), b"hello\0\0\0!");
        assert_eq!(fs.read(f.ino, 3, 2).unwrap(), b"lo");
        assert!(fs.read(f.ino, 50, 10).unwrap().is_empty());
        assert_eq!(fs.getattr(f.ino).unwrap().size, 9);
    }

    #[test]
    fn truncates_and_counts_space() {
        let fs = fs();
        let f = file(&fs, ROOT, "a");
        fs.write(f.ino, 0, &[1; 1000]).unwrap();
        assert_eq!(fs.write(f.ino, 1000, &[1; 100]), Err(VfsError::NoSpace));
        fs.setattr(f.ino, &SetAttr { size: Some(10), ..Default::default() }).unwrap();
        assert_eq!(fs.usage().1, 10);
        fs.write(f.ino, 10, &[2; 500]).unwrap();
        fs.unlink(ROOT, "a").unwrap();
        assert_eq!(fs.usage().1, 0);
    }

    #[test]
    fn create_rejects_duplicates_and_bad_names() {
        let fs = fs();
        file(&fs, ROOT, "a");
        assert_eq!(fs.create(ROOT, "a", Kind::File, 0o644, 0, 0, None), Err(VfsError::Exists));
        assert_eq!(fs.create(ROOT, "a/b", Kind::File, 0o644, 0, 0, None), Err(VfsError::Invalid));
        assert_eq!(fs.create(ROOT, "..", Kind::Dir, 0o755, 0, 0, None), Err(VfsError::Invalid));
        assert_eq!(fs.create(ROOT, &"x".repeat(256), Kind::File, 0o644, 0, 0, None), Err(VfsError::NameTooLong));
    }

    #[test]
    fn rmdir_needs_an_empty_directory() {
        let fs = fs();
        let d = dir(&fs, ROOT, "d");
        file(&fs, d.ino, "f");
        assert_eq!(fs.rmdir(ROOT, "d"), Err(VfsError::NotEmpty));
        assert_eq!(fs.unlink(ROOT, "d"), Err(VfsError::IsDir));
        fs.unlink(d.ino, "f").unwrap();
        fs.rmdir(ROOT, "d").unwrap();
        assert_eq!(fs.lookup(ROOT, "d"), Err(VfsError::NotFound));
        assert_eq!(fs.getattr(ROOT).unwrap().nlink, 2);
    }

    #[test]
    fn rename_replaces_like_posix() {
        let fs = fs();
        let a = file(&fs, ROOT, "a");
        fs.write(a.ino, 0, b"new").unwrap();
        let b = file(&fs, ROOT, "b");
        fs.write(b.ino, 0, b"old").unwrap();
        fs.rename(ROOT, "a", ROOT, "b", RenameMode::Replace).unwrap();
        assert_eq!(fs.read_file("b").unwrap(), b"new");
        assert_eq!(fs.lookup(ROOT, "a"), Err(VfsError::NotFound));
        assert_eq!(fs.getattr(b.ino), Err(VfsError::NotFound), "replaced file is freed");
        assert_eq!(fs.usage().1, 3);
    }

    #[test]
    fn rename_checks_kinds_and_cycles() {
        let fs = fs();
        let d = dir(&fs, ROOT, "d");
        let sub = dir(&fs, d.ino, "sub");
        file(&fs, ROOT, "f");
        dir(&fs, ROOT, "full");
        file(&fs, fs.lookup(ROOT, "full").unwrap().ino, "x");
        assert_eq!(fs.rename(ROOT, "d", sub.ino, "d2", RenameMode::Replace), Err(VfsError::Invalid));
        assert_eq!(fs.rename(ROOT, "d", ROOT, "f", RenameMode::Replace), Err(VfsError::NotDir));
        assert_eq!(fs.rename(ROOT, "f", ROOT, "d", RenameMode::Replace), Err(VfsError::IsDir));
        assert_eq!(fs.rename(ROOT, "d", ROOT, "full", RenameMode::Replace), Err(VfsError::NotEmpty));
        assert_eq!(fs.rename(ROOT, "f", ROOT, "full", RenameMode::NoReplace), Err(VfsError::Exists));
        // A directory moves with its contents, and `..` follows.
        let other = dir(&fs, ROOT, "other");
        fs.rename(ROOT, "d", other.ino, "moved", RenameMode::Replace).unwrap();
        assert_eq!(fs.resolve("other/moved/sub").unwrap(), sub.ino);
        assert_eq!(fs.parent_of(d.ino).unwrap(), other.ino);
        assert_eq!(fs.getattr(other.ino).unwrap().nlink, 3);
        assert_eq!(fs.getattr(ROOT).unwrap().nlink, 4); // "." and the root's own entry + full + other
    }

    #[test]
    fn exchange_swaps_entries() {
        let fs = fs();
        let a = file(&fs, ROOT, "a");
        let d = dir(&fs, ROOT, "d");
        fs.rename(ROOT, "a", ROOT, "d", RenameMode::Exchange).unwrap();
        assert_eq!(fs.lookup(ROOT, "a").unwrap().ino, d.ino);
        assert_eq!(fs.lookup(ROOT, "d").unwrap().ino, a.ino);
    }

    #[test]
    fn unlinked_open_file_stays_readable() {
        let fs = fs();
        let f = file(&fs, ROOT, "tmp");
        fs.write(f.ino, 0, b"data").unwrap();
        fs.open(f.ino).unwrap();
        fs.unlink(ROOT, "tmp").unwrap();
        assert_eq!(fs.read(f.ino, 0, 10).unwrap(), b"data");
        assert_eq!(fs.getattr(f.ino).unwrap().nlink, 0);
        fs.release(f.ino);
        assert_eq!(fs.getattr(f.ino), Err(VfsError::NotFound));
        assert_eq!(fs.usage().1, 0);
    }

    #[test]
    fn hard_links_share_contents() {
        let fs = fs();
        let f = file(&fs, ROOT, "a");
        fs.link(f.ino, ROOT, "b").unwrap();
        fs.write(f.ino, 0, b"x").unwrap();
        assert_eq!(fs.read_file("b").unwrap(), b"x");
        fs.unlink(ROOT, "a").unwrap();
        assert_eq!(fs.getattr(f.ino).unwrap().nlink, 1);
        assert_eq!(fs.path_of(f.ino).unwrap(), "b");
        let d = dir(&fs, ROOT, "d");
        assert_eq!(fs.link(d.ino, ROOT, "d2"), Err(VfsError::NotPermitted));
    }

    #[test]
    fn symlinks_resolve_relative_to_their_directory() {
        let fs = fs();
        fs.write_file("dir/target.txt", b"hi", None).unwrap();
        fs.symlink("dir/link", "target.txt").unwrap();
        fs.symlink("up", "dir").unwrap();
        assert_eq!(fs.read_file("dir/link").unwrap(), b"hi");
        assert_eq!(fs.read_file("up/link").unwrap(), b"hi");
        assert_eq!(fs.metadata("dir/link").unwrap().kind, Kind::Symlink);
        assert_eq!(fs.readlink(fs.resolve_nofollow("dir/link").unwrap()).unwrap(), "target.txt");
        fs.symlink("loop", "loop").unwrap();
        assert_eq!(fs.read_file("loop"), Err(VfsError::Loop));
    }

    #[test]
    fn path_api_creates_parents_and_keeps_inodes() {
        let fs = fs();
        let first = fs.write_file("a/b/c.txt", b"one", None).unwrap();
        let second = fs.write_file("/a/b/c.txt", b"two!", Some(0o755)).unwrap();
        assert_eq!(first.ino, second.ino);
        assert_eq!(second.perm, 0o755);
        assert_eq!(fs.read_file("a\\b\\c.txt").unwrap(), b"two!");
        let names: Vec<String> = fs.list("a").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["b"]);
        assert_eq!(fs.path_of(second.ino).unwrap(), "a/b/c.txt");
        assert_eq!(fs.write_file("a/b", b"x", None), Err(VfsError::IsDir));
        assert_eq!(fs.write_file("a/b/c.txt/d", b"x", None), Err(VfsError::NotDir));
    }

    #[test]
    fn failed_write_file_leaves_nothing_behind() {
        let fs = fs();
        assert_eq!(fs.write_file("big", &[0; 2000], None), Err(VfsError::NoSpace));
        assert_eq!(fs.metadata("big"), Err(VfsError::NotFound));
    }

    #[test]
    fn remove_deletes_whole_trees() {
        let fs = fs();
        fs.write_file("t/a/1", b"12", None).unwrap();
        fs.write_file("t/b/2", b"34", None).unwrap();
        fs.write_file("keep", b"5", None).unwrap();
        fs.remove("t").unwrap();
        assert_eq!(fs.metadata("t"), Err(VfsError::NotFound));
        assert_eq!(fs.usage(), (1024, 1, 2));
    }

    #[test]
    fn reports_app_changes() {
        let fs = fs();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        fs.set_listener(Some(Box::new(move |changes| sink.lock().unwrap().extend_from_slice(changes))));
        let attr = fs.write_file("d/f", b"x", None).unwrap();
        fs.write_file("d/f", b"y", None).unwrap();
        let d = fs.resolve("d").unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                Change::Entry { parent: ROOT, name: "d".into() },
                Change::Entry { parent: d, name: "f".into() },
                Change::Data(attr.ino),
                Change::Data(attr.ino),
            ]
        );
    }

    #[test]
    fn case_insensitive_lookup_keeps_spelling() {
        let fs = VirtualFs::new(Options { capacity: 1024, case_insensitive: true, uid: 0, gid: 0 });
        let f = fs.create(ROOT, "ReadMe.md", Kind::File, 0o644, 0, 0, None).unwrap();
        assert_eq!(fs.lookup(ROOT, "README.MD").unwrap().ino, f.ino);
        assert_eq!(fs.create(ROOT, "readme.md", Kind::File, 0o644, 0, 0, None), Err(VfsError::Exists));
        fs.rename(ROOT, "readme.md", ROOT, "README.md", RenameMode::Replace).unwrap();
        assert_eq!(fs.readdir(ROOT).unwrap()[0].name, "README.md");
        assert_eq!(fs.getattr(f.ino).unwrap().nlink, 1);
    }

    #[test]
    fn xattrs() {
        let fs = fs();
        let f = file(&fs, ROOT, "a");
        fs.setxattr(f.ino, "user.k", b"v", true, false).unwrap();
        assert_eq!(fs.setxattr(f.ino, "user.k", b"v", true, false), Err(VfsError::Exists));
        assert_eq!(fs.setxattr(f.ino, "user.x", b"v", false, true), Err(VfsError::NoAttr));
        assert_eq!(fs.getxattr(f.ino, "user.k").unwrap(), b"v");
        assert_eq!(fs.listxattr(f.ino).unwrap(), ["user.k"]);
        fs.removexattr(f.ino, "user.k").unwrap();
        assert_eq!(fs.getxattr(f.ino, "user.k"), Err(VfsError::NoAttr));
    }
}

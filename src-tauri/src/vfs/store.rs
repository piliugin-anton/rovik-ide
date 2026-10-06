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

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::SystemTime;

use super::events::{Callback, EventMask, FsEvent, Hub, Op, Pending, Source, SubscriptionId};
use super::history::{Histories, HistoryLimits, RevisionInfo};

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
    /// Names match regardless of case (and keep the case they were created
    /// with). On by default on Windows and macOS, as their programs expect
    /// (and as macFUSE is told, by fuser).
    pub case_insensitive: bool,
    /// Owner of the root and of files the app creates.
    pub uid: u32,
    pub gid: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { capacity: 1 << 30, case_insensitive: cfg!(any(windows, target_os = "macos")), uid: 0, gid: 0 }
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
    /// Where the node is linked: (directory, name), one per hard link. Kept
    /// so a node's path is found by walking up, not by searching the tree.
    links: Vec<(Ino, String)>,
    /// The last link, once all are gone: events about a deleted file that's
    /// still open name the path it had.
    last_link: Option<(Ino, String)>,
    perm: u16,
    uid: u32,
    gid: u32,
    nlink: u32,
    /// Open handles; an unlinked file lives until this is zero.
    open: u32,
    /// Written since the last `Closed` event.
    dirty: bool,
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
            links: Vec::new(),
            last_link: None,
            perm: perm & 0o7777,
            uid,
            gid,
            nlink: if kind == Kind::Dir { 2 } else { 1 },
            open: 0,
            dirty: false,
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
    nodes: HashMap<Ino, Node>,
    next_ino: Ino,
    /// Bytes of file contents held, counted against `Options::capacity`.
    used: u64,
    opts: Options,
    /// Revisions (see `history.rs`).
    history: Histories,
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

    fn same_name(&self, a: &str, b: &str) -> bool {
        if self.opts.case_insensitive {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
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

    /// A node's path, walking up its first link: O(depth). `strict`: only
    /// for a node that's still linked; otherwise its last known path.
    fn path_in(&self, ino: Ino, strict: bool) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = ino;
        while cur != ROOT {
            let node = self.nodes.get(&cur)?;
            let (parent, name) = match (node.links.first(), &node.last_link) {
                (Some(link), _) => link,
                (None, Some(link)) if !strict => link,
                _ => return None,
            };
            parts.push(name);
            cur = *parent;
            if parts.len() > 4096 {
                return None;
            }
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// Path for an event: best effort, never fails.
    fn event_path(&self, ino: Ino) -> String {
        self.path_in(ino, false).unwrap_or_default()
    }

    /// Path of `name` in directory `dir`, for an event.
    fn join(&self, dir: Ino, name: &str) -> String {
        let base = self.event_path(dir);
        if base.is_empty() {
            name.to_string()
        } else {
            format!("{base}/{name}")
        }
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
            if self.history.contains(ino) {
                let path = self.event_path(ino);
                self.history.mark_deleted(ino, &path);
            }
            if let Some(node) = self.nodes.remove(&ino) {
                if node.kind == Kind::File {
                    self.used -= node.data.len() as u64;
                }
            }
        }
    }

    /// Records the file's current content as a revision (if versioned).
    fn record_revision(&mut self, ino: Ino, source: Source) {
        let size = self.nodes.get(&ino).map_or(0, |n| n.data.len());
        if !self.history.accepts(size) {
            return;
        }
        let path = self.event_path(ino);
        let Tree { nodes, history, .. } = self;
        if let Some(node) = nodes.get(&ino).filter(|n| n.kind == Kind::File) {
            history.record(ino, &path, &node.data, source);
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
        let node = self.node_mut(ino)?;
        node.links.push((parent, name.to_string()));
        node.last_link = None;
        if is_dir {
            node.parent = parent;
        }
        Ok(())
    }

    /// Takes an entry out of its directory without freeing the node.
    fn detach(&mut self, parent: Ino, name: &str) -> VfsResult<Ino> {
        let key = self.key(name);
        let dir = self.node_mut(parent)?;
        let child = dir.children.remove(&key).ok_or(VfsError::NotFound)?;
        dir.touch_modified();
        let case_insensitive = self.opts.case_insensitive;
        let node = self.node_mut(child.ino)?;
        let at = node.links.iter().position(|(p, n)| {
            *p == parent && if case_insensitive { n.to_lowercase() == key } else { *n == key }
        });
        if let Some(at) = at {
            let link = node.links.swap_remove(at);
            if node.links.is_empty() {
                node.last_link = Some(link);
            }
        }
        if node.kind == Kind::Dir {
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
    hub: Hub,
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
        let mut nodes = HashMap::new();
        nodes.insert(ROOT, root);
        Arc::new(VirtualFs {
            tree: RwLock::new(Tree { nodes, next_ino: ROOT + 1, used: 0, opts, history: Histories::new(HistoryLimits::default()) }),
            listener: RwLock::new(None),
            hub: Hub::default(),
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

    // --- Events -------------------------------------------------------------

    /// Calls `callback` for every event whose kind is in `mask` (see
    /// `events.rs` for what to keep out of a callback).
    pub fn subscribe(&self, mask: EventMask, callback: impl Fn(&FsEvent) + Send + Sync + 'static) -> SubscriptionId {
        self.hub.subscribe(mask, Arc::new(callback))
    }

    /// Same, with a shared callback (e.g. from [`super::events::batched`]).
    pub fn subscribe_shared(&self, mask: EventMask, callback: Callback) -> SubscriptionId {
        self.hub.subscribe(mask, callback)
    }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.hub.unsubscribe(id)
    }

    fn events(&self, source: Source) -> Pending<'_> {
        Pending::new(&self.hub, source)
    }

    /// Runs `op` collecting events as `source`, and delivers them after
    /// `op` (and the lock it took) is done.
    fn with_events<T>(&self, source: Source, op: impl FnOnce(&mut Pending<'_>) -> T) -> T {
        let mut events = self.events(source);
        let result = op(&mut events);
        events.send();
        result
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
        self.with_events(Source::Process, |ev| self.create_in(ev, parent, name, kind, perm, uid, gid, target))
    }

    #[allow(clippy::too_many_arguments)]
    fn create_in(
        &self,
        ev: &mut Pending<'_>,
        parent: Ino,
        name: &str,
        kind: Kind,
        perm: u16,
        uid: u32,
        gid: u32,
        target: Option<&str>,
    ) -> VfsResult<Attr> {
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
        ev.add(ino, Op::Created { kind }, || tree.join(parent, name));
        Ok(tree.node(ino)?.attr(ino))
    }

    /// Removes a non-directory entry.
    pub fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()> {
        self.with_events(Source::Process, |ev| {
            let mut tree = self.tree_mut();
            let ino = tree.child(parent, name)?;
            let kind = tree.node(ino)?.kind;
            if kind == Kind::Dir {
                return Err(VfsError::IsDir);
            }
            ev.add(ino, Op::Removed { kind }, || tree.join(parent, name));
            tree.detach(parent, name)?;
            tree.unlink_node(ino);
            Ok(())
        })
    }

    /// Removes an empty directory.
    pub fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()> {
        if name == "." {
            return Err(VfsError::Invalid);
        }
        if name == ".." {
            return Err(VfsError::NotEmpty);
        }
        self.with_events(Source::Process, |ev| {
            let mut tree = self.tree_mut();
            let ino = tree.child(parent, name)?;
            let node = tree.node(ino)?;
            if node.kind != Kind::Dir {
                return Err(VfsError::NotDir);
            }
            if !node.children.is_empty() {
                return Err(VfsError::NotEmpty);
            }
            ev.add(ino, Op::Removed { kind: Kind::Dir }, || tree.join(parent, name));
            tree.detach(parent, name)?;
            tree.unlink_node(ino);
            Ok(())
        })
    }

    pub fn rename(&self, parent: Ino, name: &str, new_parent: Ino, new_name: &str, mode: RenameMode) -> VfsResult<()> {
        self.with_events(Source::Process, |ev| self.rename_in(ev, parent, name, new_parent, new_name, mode))
    }

    fn rename_in(
        &self,
        ev: &mut Pending<'_>,
        parent: Ino,
        name: &str,
        new_parent: Ino,
        new_name: &str,
        mode: RenameMode,
    ) -> VfsResult<()> {
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
        // Paths before the move, only if someone will be told.
        let wants = ev.wants(EventMask::RENAMED);
        let from = wants.then(|| tree.join(parent, name));

        if mode == RenameMode::Exchange {
            let dst = dst.ok_or(VfsError::NotFound)?;
            if tree.node(dst)?.kind == Kind::Dir && tree.is_within(parent, dst) {
                return Err(VfsError::Invalid);
            }
            let to = wants.then(|| tree.join(new_parent, new_name));
            let dst_name = tree.node(new_parent)?.children[&tree.key(new_name)].name.clone();
            let src_name = tree.node(parent)?.children[&tree.key(name)].name.clone();
            tree.detach(parent, &src_name)?;
            tree.detach(new_parent, &dst_name)?;
            tree.insert_child(parent, &src_name, dst)?;
            tree.insert_child(new_parent, &dst_name, src)?;
            let now = SystemTime::now();
            tree.node_mut(src)?.ctime = now;
            tree.node_mut(dst)?.ctime = now;
            if let (Some(from), Some(to)) = (from, to) {
                ev.add(src, Op::Renamed { from: from.clone() }, || to.clone());
                ev.add(dst, Op::Renamed { from: to }, || from);
            }
            return Ok(());
        }

        if let Some(dst) = dst {
            if dst == src {
                // Same file (a hard link, or a case-only rename on a
                // case-insensitive volume): only the spelling may change.
                if parent == new_parent && tree.same_name(name, new_name) && name != new_name {
                    let key = tree.key(new_name);
                    if let Some(child) = tree.node_mut(new_parent)?.children.get_mut(&key) {
                        child.name = new_name.to_string();
                    }
                    let case_insensitive = tree.opts.case_insensitive;
                    for link in tree.node_mut(src)?.links.iter_mut() {
                        let same = if case_insensitive { link.1.to_lowercase() == key } else { link.1 == key };
                        if link.0 == parent && same {
                            link.1 = new_name.to_string();
                        }
                    }
                    if let Some(from) = from {
                        ev.add(src, Op::Renamed { from }, || tree.join(new_parent, new_name));
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
            // A file renamed over another (a "safe save") continues the
            // replaced file's history.
            if !src_is_dir && dst_node.kind == Kind::File {
                tree.history.merge_into(dst, src);
            }
            tree.detach(new_parent, new_name)?;
            tree.unlink_node(dst);
        }

        tree.detach(parent, name)?;
        tree.insert_child(new_parent, new_name, src)?;
        tree.node_mut(src)?.ctime = SystemTime::now();
        if let Some(from) = from {
            ev.add(src, Op::Renamed { from }, || tree.join(new_parent, new_name));
        }
        Ok(())
    }

    /// Adds a hard link to a file.
    pub fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr> {
        check_name(new_name)?;
        self.with_events(Source::Process, |ev| {
            let mut tree = self.tree_mut();
            let node = tree.node(ino)?;
            let kind = node.kind;
            if kind == Kind::Dir {
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
            ev.add(ino, Op::Created { kind }, || tree.join(new_parent, new_name));
            let node = tree.node_mut(ino)?;
            node.nlink += 1;
            node.ctime = SystemTime::now();
            Ok(node.attr(ino))
        })
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
        self.with_events(Source::Process, |ev| self.read_in(ev, ino, offset, size))
    }

    fn read_in(&self, ev: &mut Pending<'_>, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let tree = self.tree();
        let node = tree.node(ino)?;
        match node.kind {
            Kind::Dir => return Err(VfsError::IsDir),
            Kind::Symlink => return Err(VfsError::Invalid),
            Kind::File => {}
        }
        let len = node.data.len() as u64;
        let data = if offset >= len {
            Vec::new()
        } else {
            let end = len.min(offset.saturating_add(size as u64));
            node.data[offset as usize..end as usize].to_vec()
        };
        ev.add(ino, Op::Read { offset, len: data.len() as u64 }, || tree.event_path(ino));
        Ok(data)
    }

    /// Writes at `offset`, filling any gap with zeros. Returns the bytes written.
    pub fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize> {
        self.with_events(Source::Process, |ev| {
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
            node.dirty = true;
            ev.add(ino, Op::Written { offset, len: data.len() as u64 }, || tree.event_path(ino));
            Ok(data.len())
        })
    }

    pub fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
        self.with_events(Source::Process, |ev| self.setattr_in(ev, ino, set))
    }

    fn setattr_in(&self, ev: &mut Pending<'_>, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
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
            node.dirty = true;
            ev.add(ino, Op::Truncated { size }, || tree.event_path(ino));
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

    /// Whether a directory has no entries (without listing them).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn dir_is_empty(&self, ino: Ino) -> VfsResult<bool> {
        Ok(self.tree().dir(ino)?.children.is_empty())
    }

    /// A directory's entries (without `.` and `..`) with their attributes,
    /// under one lock instead of a `getattr` per entry.
    pub fn readdir_plus(&self, ino: Ino) -> VfsResult<Vec<(DirEntry, Attr)>> {
        let tree = self.tree();
        let dir = tree.dir(ino)?;
        Ok(dir
            .children
            .values()
            .filter_map(|c| {
                let node = tree.nodes.get(&c.ino)?;
                Some((DirEntry { name: c.name.clone(), ino: c.ino, kind: node.kind }, node.attr(c.ino)))
            })
            .collect())
    }

    /// Notes an open handle, so the node outlives an unlink until `release`.
    /// `write`: opened for writing (reported in the `Opened` event).
    pub fn open(&self, ino: Ino, write: bool) -> VfsResult<Attr> {
        self.with_events(Source::Process, |ev| {
            let mut tree = self.tree_mut();
            let node = tree.node_mut(ino)?;
            node.open += 1;
            let attr = node.attr(ino);
            if attr.kind == Kind::File {
                ev.add(ino, Op::Opened { write }, || tree.event_path(ino));
            }
            Ok(attr)
        })
    }

    pub fn release(&self, ino: Ino) {
        self.with_events(Source::Process, |ev| {
            let mut tree = self.tree_mut();
            let Some(node) = tree.nodes.get_mut(&ino) else { return };
            node.open = node.open.saturating_sub(1);
            if node.kind == Kind::File {
                let modified = std::mem::take(&mut node.dirty);
                if modified {
                    tree.record_revision(ino, Source::Process);
                }
                ev.add(ino, Op::Closed { modified }, || tree.event_path(ino));
            }
            tree.collect(ino);
        })
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
                let base = tree.path_in(parent, true).ok_or(VfsError::NotFound)?;
                ino = Self::resolve_in(tree, &format!("{base}/{target}"), true, hops)?;
            }
        }
        Ok(ino)
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
    /// for a file with several). Unlinked nodes have none. O(depth).
    pub fn path_of(&self, ino: Ino) -> VfsResult<String> {
        self.tree().path_in(ino, true).ok_or(VfsError::NotFound)
    }

    /// Creates a directory and any missing parents.
    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn create_dir_all(&self, path: &str) -> VfsResult<Ino> {
        self.with_events(Source::App, |ev| self.create_dir_all_in(ev, path))
    }

    fn create_dir_all_in(&self, ev: &mut Pending<'_>, path: &str) -> VfsResult<Ino> {
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
                        let attr = self.create_in(ev, ino, part, Kind::Dir, 0o755, uid, gid, None)?;
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
    ///
    /// Reported as `Created` (if new), `Written` (the whole contents) and
    /// `Closed { modified: true }`, so "a file was finished" looks the same
    /// whether a program or the app wrote it.
    pub fn write_file(&self, path: &str, data: &[u8], perm: Option<u16>) -> VfsResult<Attr> {
        self.with_events(Source::App, |ev| self.write_file_in(ev, path, data, perm))
    }

    fn write_file_in(&self, ev: &mut Pending<'_>, path: &str, data: &[u8], perm: Option<u16>) -> VfsResult<Attr> {
        let (dir_path, name) = {
            let trimmed = path.trim_end_matches(['/', '\\']);
            match trimmed.rfind(['/', '\\']) {
                Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
                None => ("", trimmed),
            }
        };
        check_name(name)?;
        let parent = self.create_dir_all_in(ev, dir_path)?;
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
                let _ = tree.detach(parent, name).map(|ino| tree.unlink_node(ino));
                return Err(e);
            }
            if !changes.is_empty() {
                ev.add(ino, Op::Created { kind: Kind::File }, || tree.join(parent, name));
            }
            let node = tree.node_mut(ino)?;
            node.data = data.to_vec();
            node.touch_modified();
            if let Some(perm) = perm {
                node.perm = perm & 0o7777;
            }
            let attr = node.attr(ino);
            tree.record_revision(ino, Source::App);
            if ev.wants(EventMask::WRITTEN | EventMask::CLOSED) {
                let path = tree.event_path(ino);
                ev.add(ino, Op::Written { offset: 0, len: data.len() as u64 }, || path.clone());
                ev.add(ino, Op::Closed { modified: true }, || path);
            }
            changes.push(Change::Data(ino));
            Ok(attr)
        })();
        self.notify(&changes);
        result
    }

    pub fn read_file(&self, path: &str) -> VfsResult<Vec<u8>> {
        let ino = self.resolve(path)?;
        self.with_events(Source::App, |ev| self.read_in(ev, ino, 0, usize::MAX))
    }

    pub fn metadata(&self, path: &str) -> VfsResult<Attr> {
        self.getattr(self.resolve_nofollow(path)?)
    }

    pub fn list(&self, path: &str) -> VfsResult<Vec<(DirEntry, Attr)>> {
        self.readdir_plus(self.resolve(path)?)
    }

    /// Removes a file, symlink or a whole directory tree (reported as one
    /// `Removed` for `path`).
    pub fn remove(&self, path: &str) -> VfsResult<()> {
        let (parent, name) = self.resolve_parent(path)?;
        let name = name.to_string();
        let ino = self.lookup(parent, &name)?.ino;
        let result = self.with_events(Source::App, |ev| {
            let mut tree = self.tree_mut();
            let kind = tree.node(ino)?.kind;
            ev.add(ino, Op::Removed { kind }, || tree.join(parent, &name));
            if kind == Kind::Dir {
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
        });
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
        self.with_events(Source::App, |ev| self.rename_in(ev, parent, name, new_parent, new_name, RenameMode::Replace))?;
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
        let attr =
            self.with_events(Source::App, |ev| self.create_in(ev, parent, name, Kind::Symlink, 0o777, uid, gid, Some(target)))?;
        self.notify(&[Change::Entry { parent, name: name.to_string() }]);
        Ok(attr)
    }

    #[allow(dead_code)] // app-side API, not used by the app yet
    pub fn set_permissions(&self, path: &str, perm: u16) -> VfsResult<Attr> {
        let ino = self.resolve(path)?;
        self.with_events(Source::App, |ev| self.setattr_in(ev, ino, &SetAttr { perm: Some(perm), ..Default::default() }))
    }
}


// --- Revisions (see `history.rs`) ---------------------------------------------

impl VirtualFs {
    /// The file a history lookup means: the one at `path`, or the latest
    /// deleted one that was there.
    fn history_ino(&self, path: &str) -> VfsResult<Ino> {
        match self.resolve(path) {
            Ok(ino) => Ok(ino),
            Err(VfsError::NotFound) => {
                let tree = self.tree();
                let normalized = components(path).collect::<Vec<_>>().join("/");
                tree.history.find_deleted(&normalized, tree.opts.case_insensitive).ok_or(VfsError::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    /// The revisions of the file at `path` (or of the deleted file that was
    /// there), oldest first.
    pub fn revisions(&self, path: &str) -> VfsResult<Vec<RevisionInfo>> {
        let ino = self.history_ino(path)?;
        Ok(self.tree().history.list(ino))
    }

    /// The content of revision `number` of the file at `path`.
    pub fn read_revision(&self, path: &str, number: u64) -> VfsResult<Arc<[u8]>> {
        let ino = self.history_ino(path)?;
        self.tree().history.get(ino, number).ok_or(VfsError::NotFound)
    }

    /// Writes revision `number` back as the file's content (a new revision
    /// itself). A deleted file is recreated at `path`, keeping its history.
    pub fn restore_revision(&self, path: &str, number: u64) -> VfsResult<Attr> {
        let old = self.history_ino(path)?;
        let data = self.tree().history.get(old, number).ok_or(VfsError::NotFound)?;
        let attr = self.write_file(path, &data, None)?;
        if attr.ino != old {
            let mut tree = self.tree_mut();
            tree.history.merge_into(old, attr.ino);
        }
        Ok(attr)
    }

    pub fn history_limits(&self) -> HistoryLimits {
        self.tree().history.limits()
    }

    /// Changes the limits, dropping what no longer fits (`max_bytes: 0`
    /// turns versioning off and drops all revisions).
    pub fn set_history_limits(&self, limits: HistoryLimits) {
        self.tree_mut().history.set_limits(limits);
    }

    /// (revisions, bytes) held.
    pub fn history_usage(&self) -> (usize, u64) {
        self.tree().history.usage()
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
        fs.open(f.ino, false).unwrap();
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
        let names: Vec<String> = fs.list("a").unwrap().into_iter().map(|(e, _)| e.name).collect();
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
        assert_eq!(fs.readdir_plus(ROOT).unwrap()[0].0.name, "README.md");
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

    use crate::vfs::events::{EventMask, FsEvent, Op, Source};
    use std::sync::Mutex;

    fn record(fs: &VirtualFs, mask: EventMask) -> Arc<Mutex<Vec<FsEvent>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        fs.subscribe(mask, move |e| sink.lock().unwrap().push(e.clone()));
        seen
    }

    fn ops(seen: &Mutex<Vec<FsEvent>>) -> Vec<(String, Op)> {
        seen.lock().unwrap().drain(..).map(|e| (e.path, e.op)).collect()
    }

    #[test]
    fn reports_a_programs_file_lifecycle() {
        let fs = fs();
        let seen = record(&fs, EventMask::ALL);
        let d = dir(&fs, ROOT, "d");
        let f = file(&fs, d.ino, "a.txt");
        fs.open(f.ino, true).unwrap();
        fs.write(f.ino, 0, b"hi").unwrap();
        fs.read(f.ino, 0, 10).unwrap();
        fs.setattr(f.ino, &SetAttr { size: Some(1), ..Default::default() }).unwrap();
        fs.release(f.ino);
        fs.open(f.ino, false).unwrap();
        fs.release(f.ino);
        fs.rename(d.ino, "a.txt", ROOT, "b.txt", RenameMode::Replace).unwrap();
        fs.unlink(ROOT, "b.txt").unwrap();
        assert_eq!(
            ops(&seen),
            [
                ("d".into(), Op::Created { kind: Kind::Dir }),
                ("d/a.txt".into(), Op::Created { kind: Kind::File }),
                ("d/a.txt".into(), Op::Opened { write: true }),
                ("d/a.txt".into(), Op::Written { offset: 0, len: 2 }),
                ("d/a.txt".into(), Op::Read { offset: 0, len: 2 }),
                ("d/a.txt".into(), Op::Truncated { size: 1 }),
                ("d/a.txt".into(), Op::Closed { modified: true }),
                ("d/a.txt".into(), Op::Opened { write: false }),
                ("d/a.txt".into(), Op::Closed { modified: false }),
                ("b.txt".into(), Op::Renamed { from: "d/a.txt".into() }),
                ("b.txt".into(), Op::Removed { kind: Kind::File }),
            ]
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn masks_limit_what_is_built() {
        let fs = fs();
        let seen = record(&fs, EventMask::CLOSED);
        let f = file(&fs, ROOT, "a");
        fs.open(f.ino, true).unwrap();
        fs.write(f.ino, 0, b"x").unwrap();
        fs.release(f.ino);
        assert_eq!(ops(&seen), [("a".into(), Op::Closed { modified: true })]);
    }

    #[test]
    fn app_writes_are_reported_as_the_app() {
        let fs = fs();
        let seen = record(&fs, EventMask::ALL);
        fs.write_file("x/y.txt", b"abc", None).unwrap();
        fs.write_file("x/y.txt", b"abcd", None).unwrap();
        fs.remove("x").unwrap();
        let events = seen.lock().unwrap().clone();
        assert!(events.iter().all(|e| e.source == Source::App));
        let got: Vec<(String, Op)> = events.into_iter().map(|e| (e.path, e.op)).collect();
        assert_eq!(
            got,
            [
                ("x".into(), Op::Created { kind: Kind::Dir }),
                ("x/y.txt".into(), Op::Created { kind: Kind::File }),
                ("x/y.txt".into(), Op::Written { offset: 0, len: 3 }),
                ("x/y.txt".into(), Op::Closed { modified: true }),
                ("x/y.txt".into(), Op::Written { offset: 0, len: 4 }),
                ("x/y.txt".into(), Op::Closed { modified: true }),
                ("x".into(), Op::Removed { kind: Kind::Dir }),
            ]
        );
    }

    #[test]
    fn deleted_open_files_keep_their_last_path() {
        let fs = fs();
        let f = file(&fs, ROOT, "tmp");
        fs.link(f.ino, ROOT, "other").unwrap();
        fs.open(f.ino, false).unwrap();
        let seen = record(&fs, EventMask::REMOVED | EventMask::CLOSED);
        fs.unlink(ROOT, "tmp").unwrap();
        assert_eq!(fs.path_of(f.ino).unwrap(), "other", "the remaining link");
        fs.unlink(ROOT, "other").unwrap();
        assert_eq!(fs.path_of(f.ino), Err(VfsError::NotFound));
        fs.release(f.ino);
        assert_eq!(
            ops(&seen),
            [
                ("tmp".into(), Op::Removed { kind: Kind::File }),
                ("other".into(), Op::Removed { kind: Kind::File }),
                ("other".into(), Op::Closed { modified: false }),
            ]
        );
    }

    #[test]
    fn paths_follow_renamed_directories() {
        let fs = fs();
        let f = fs.write_file("a/b/c/f.txt", b"", None).unwrap();
        fs.rename_path("a/b", "z").unwrap();
        assert_eq!(fs.path_of(f.ino).unwrap(), "z/c/f.txt");
        let ci = VirtualFs::new(Options { capacity: 1024, case_insensitive: true, uid: 0, gid: 0 });
        let g = ci.write_file("Dir/file.TXT", b"", None).unwrap();
        ci.rename_path("dir/FILE.txt", "dir/File.txt").unwrap();
        assert_eq!(ci.path_of(g.ino).unwrap(), "Dir/File.txt");
    }

    /// Lookups and paths cost the same with many files: nothing scans the
    /// tree. Generous bounds, so it only fails on a real regression.
    #[test]
    fn scales_to_many_files() {
        let fs = VirtualFs::new(Options { capacity: 1 << 30, case_insensitive: false, uid: 0, gid: 0 });
        let n = 100_000;
        for i in 0..n {
            fs.write_file(&format!("d{}/f{i}.txt", i % 100), b"x", None).unwrap();
        }
        let deep = fs.write_file("a/b/c/d/e/f/g/h/deep.txt", b"y", None).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            assert_eq!(fs.path_of(deep.ino).unwrap().len(), 24);
            fs.open(deep.ino, false).unwrap();
            fs.read(deep.ino, 0, 1).unwrap();
            fs.release(deep.ino);
        }
        let quiet = started.elapsed();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = seen.clone();
        fs.subscribe(EventMask::ALL, move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            fs.open(deep.ino, false).unwrap();
            fs.read(deep.ino, 0, 1).unwrap();
            fs.release(deep.ino);
        }
        let observed = started.elapsed();
        assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 30_000);
        assert!(quiet < std::time::Duration::from_secs(2), "{quiet:?}");
        assert!(observed < std::time::Duration::from_secs(2), "{observed:?}");
    }

    /// What a program does: open, write, close.
    fn program_writes(fs: &VirtualFs, ino: Ino, data: &[u8]) {
        fs.open(ino, true).unwrap();
        fs.setattr(ino, &SetAttr { size: Some(0), ..Default::default() }).unwrap();
        fs.write(ino, 0, data).unwrap();
        fs.release(ino);
    }

    fn contents(fs: &VirtualFs, path: &str) -> Vec<(u64, String)> {
        fs.revisions(path)
            .unwrap()
            .into_iter()
            .map(|r| (r.number, String::from_utf8_lossy(&fs.read_revision(path, r.number).unwrap()).into_owned()))
            .collect()
    }

    #[test]
    fn a_revision_per_finished_change() {
        let fs = fs();
        let f = file(&fs, ROOT, "notes.md");
        program_writes(&fs, f.ino, b"one");
        fs.open(f.ino, false).unwrap();
        fs.release(f.ino); // read only: no revision
        program_writes(&fs, f.ino, b"one"); // same content: no revision
        program_writes(&fs, f.ino, b"two");
        fs.write_file("notes.md", b"three", None).unwrap();
        assert_eq!(contents(&fs, "notes.md"), [(1, "one".into()), (2, "two".into()), (3, "three".into())]);
        let sources: Vec<Source> = fs.revisions("notes.md").unwrap().into_iter().map(|r| r.source).collect();
        assert_eq!(sources, [Source::Process, Source::Process, Source::App]);
        assert_eq!(fs.read_revision("notes.md", 9), Err(VfsError::NotFound));
    }

    #[test]
    fn safe_saves_and_renames_keep_the_history() {
        let fs = fs();
        fs.write_file("f.txt", b"v1", None).unwrap();
        // An editor: write a temporary file, rename it over the original.
        let tmp = file(&fs, ROOT, ".f.txt.swp");
        program_writes(&fs, tmp.ino, b"v2");
        fs.rename(ROOT, ".f.txt.swp", ROOT, "f.txt", RenameMode::Replace).unwrap();
        assert_eq!(contents(&fs, "f.txt"), [(1, "v1".into()), (2, "v2".into())]);
        fs.rename_path("f.txt", "g.txt").unwrap();
        assert_eq!(fs.revisions("g.txt").unwrap().len(), 2);
        assert_eq!(fs.revisions("f.txt"), Err(VfsError::NotFound));
    }

    #[test]
    fn deleted_files_can_be_restored() {
        let fs = fs();
        fs.write_file("d/a", b"first", None).unwrap();
        fs.write_file("d/a", b"second", None).unwrap();
        fs.remove("d/a").unwrap();
        assert_eq!(contents(&fs, "d/a"), [(1, "first".into()), (2, "second".into())]);
        let restored = fs.restore_revision("d/a", 1).unwrap();
        assert_eq!(fs.read_file("d/a").unwrap(), b"first");
        assert_eq!(fs.resolve("d/a").unwrap(), restored.ino);
        assert_eq!(
            contents(&fs, "d/a"),
            [(1, "first".into()), (2, "second".into()), (3, "first".into())],
            "the restore is a revision too"
        );
    }

    #[test]
    fn history_has_limits() {
        let fs = fs();
        fs.set_history_limits(HistoryLimits { max_bytes: 10, per_file: 2, max_file_size: 8 });
        for v in ["a1", "a2", "a3"] {
            fs.write_file("a", v.as_bytes(), None).unwrap();
        }
        assert_eq!(contents(&fs, "a"), [(2, "a2".into()), (3, "a3".into())]);
        fs.write_file("big", b"123456789", None).unwrap();
        assert!(fs.revisions("big").unwrap().is_empty(), "over max_file_size");
        assert!(fs.history_usage().1 <= 10);
        fs.set_history_limits(HistoryLimits { max_bytes: 0, ..fs.history_limits() });
        fs.write_file("a", b"a4", None).unwrap();
        assert_eq!(fs.history_usage(), (0, 0));
    }
}

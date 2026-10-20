//! A chat's view of the project: the project underneath, read as it is on
//! disk, and the chat's changes on top.
//!
//! Only changes are stored. A file the chat hasn't changed is read from the
//! project each time (so it shows the project's current content). The first
//! write copies it into the overlay's folder (`upper/`), together with a
//! copy of what it was (`base/`), which landing merges against. Folders are
//! listed from the project when first looked at; a chat's deletes and
//! renames are kept per folder. Content lives on disk, in files the OS
//! caches, not in the app's memory.
//!
//! It serves the drivers (vfs/backend.rs), so a chat's commands and tools
//! use it as an ordinary folder, and it lists its changes for landing
//! ([`OverlayFs::changes`]). The list is also saved (`manifest.json`) after
//! every change to the folder structure and every closed write, so an
//! overlay survives a restart ([`OverlayFs::open`]).

use super::tiers::Tiers;
use crate::vfs::backend::Backend;
use crate::vfs::store::{
    Attr, Change, DirEntry, Ino, Kind, Listener, RenameMode, SetAttr, VfsError, VfsResult,
    MAX_NAME, ROOT,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// A chat's changes beyond this many bytes are refused (`NoSpace`).
pub const DEFAULT_LIMIT: u64 = 512 << 20;
/// At this share of the limit, the chat is warned.
pub const WARN_AT: f64 = 0.8;
/// A folder is listed from the project again on a missed lookup at most
/// this often (tools look up many names that don't exist).
const RELIST_AFTER: Duration = Duration::from_secs(2);
/// The list of changes is saved at most this often while things change;
/// `flush` saves at once.
const SAVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
struct Meta {
    perm: u16,
    uid: u32,
    gid: u32,
    atime: SystemTime,
    mtime: SystemTime,
    ctime: SystemTime,
    crtime: SystemTime,
    attributes: u32,
}

#[derive(Clone, Debug, PartialEq)]
enum Data {
    /// A folder or a symlink.
    None,
    /// Read from the project, at the node's origin.
    Base,
    /// The chat's copy: `file` in the overlay's folder; `base_copy`, what
    /// the project's file was when the chat first changed it.
    Upper {
        file: PathBuf,
        size: u64,
        base_copy: Option<PathBuf>,
    },
}

struct Node {
    kind: Kind,
    /// Names it has: (folder, name). The first is the one `path_of` uses.
    links: Vec<(Ino, String)>,
    /// Where in the project it came from ("" is the root); `None` if the
    /// chat created it.
    origin: Option<String>,
    data: Data,
    meta: Meta,
    /// Its permission bits were changed: a change to land.
    perm_changed: bool,
    /// Folders: entries by name; `None` until listed from the project.
    children: Option<BTreeMap<String, Ino>>,
    /// Folders: names the chat removed (or moved away, or replaced).
    removed: BTreeSet<String>,
    /// Folders: when last listed from the project.
    listed_at: Option<Instant>,
    target: Option<String>,
    xattrs: BTreeMap<String, Vec<u8>>,
    open: u32,
    /// Build output, dependencies or `.git` (tiers.rs).
    by_product: bool,
}

impl Node {
    fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}

struct Inner {
    nodes: HashMap<Ino, Node>,
    next_ino: Ino,
    next_file: u64,
    /// Bytes of changed files, by-products not counted.
    used: u64,
    warned: bool,
    /// Project entries the chat deleted, replaced or moved away: what each
    /// was then, by its project path.
    deleted: HashMap<String, DeletedBase>,
    /// Files of changes that are gone, deleted after the next save.
    trash: Vec<PathBuf>,
    /// Files being copied into the overlay right now (copy_up).
    copying: std::collections::HashSet<Ino>,
    /// Changed since the list of changes was last saved.
    dirty: bool,
    saved_at: Option<Instant>,
    /// Saved changes whose folder can't be found now (a branch without
    /// it): kept as they are, saved again and named to the sweep, and
    /// tried again at the next open.
    parked: Vec<OverlayChange>,
}

/// What a project entry was when the chat deleted (or replaced, or moved)
/// it: landing deletes it only if it's still that, so a change made in the
/// project meanwhile isn't lost (land.rs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeletedBase {
    /// A file, by its content's hash (hex).
    File {
        hash: String,
    },
    /// A file too large to hash: by size and modification time.
    Big {
        size: u64,
        mtime: u64,
    },
    Link {
        target: String,
    },
    /// A folder: what was in it, by path, kind and size/mtime. `complete`
    /// false: too many entries to list; it can't be checked.
    Dir {
        entries: Vec<(String, u64, u64)>,
        complete: bool,
        /// Files in it the chat had changed (and deleted with the folder):
        /// what they were when the chat first changed them, by path and
        /// hash. A change the project made to one since is a conflict too.
        #[serde(default)]
        files: Vec<(String, String)>,
    },
}

/// Files above this are recorded by size and time, not hashed.
const HASH_UP_TO: u64 = 50 << 20;
/// Folders with more entries than this can't be checked when deleted.
const LIST_UP_TO: usize = 5000;

fn mtime_ns(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

fn hex(h: &crate::file_guard::Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

impl DeletedBase {
    /// What the project has at `abs` now, in the same form; None: nothing.
    pub fn of(abs: &Path) -> Option<DeletedBase> {
        let m = fs::symlink_metadata(abs).ok()?;
        if m.file_type().is_symlink() {
            return Some(DeletedBase::Link {
                target: fs::read_link(abs).ok()?.to_string_lossy().into_owned(),
            });
        }
        if m.is_dir() {
            let mut entries = Vec::new();
            let mut complete = true;
            let mut stack = vec![(abs.to_path_buf(), String::new())];
            while let Some((dir, rel)) = stack.pop() {
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    if entries.len() >= LIST_UP_TO {
                        complete = false;
                        break;
                    }
                    let name = e.file_name().to_string_lossy().into_owned();
                    let child = if rel.is_empty() {
                        name
                    } else {
                        format!("{rel}/{name}")
                    };
                    let Ok(cm) = e.path().symlink_metadata() else {
                        continue;
                    };
                    if cm.is_dir() {
                        entries.push((format!("{child}/"), 0, 0));
                        stack.push((e.path(), child));
                    } else {
                        entries.push((child, cm.len(), mtime_ns(&cm)));
                    }
                }
            }
            entries.sort();
            return Some(DeletedBase::Dir {
                entries,
                complete,
                files: Vec::new(),
            });
        }
        if m.len() > HASH_UP_TO {
            return Some(DeletedBase::Big {
                size: m.len(),
                mtime: mtime_ns(&m),
            });
        }
        Some(DeletedBase::file(&fs::read(abs).ok()?))
    }

    /// A file with this content.
    pub fn file(bytes: &[u8]) -> DeletedBase {
        DeletedBase::File {
            hash: hex(&crate::file_guard::hash(bytes)),
        }
    }

    /// Whether the project's entry at `abs` is still what was deleted.
    pub fn still(&self, abs: &Path) -> bool {
        match (self, DeletedBase::of(abs)) {
            (
                DeletedBase::Dir {
                    complete: false, ..
                },
                _,
            ) => false,
            (
                DeletedBase::Dir { entries, files, .. },
                Some(DeletedBase::Dir { entries: now, .. }),
            ) => {
                *entries == now
                    && files.iter().all(|(rel, hash)| {
                        fs::read(abs.join(rel))
                            .is_ok_and(|b| hex(&crate::file_guard::hash(&b)) == *hash)
                    })
            }
            (a, Some(b)) => *a == b,
            (_, None) => false,
        }
    }
}

/// One change in a chat's overlay, in the order to apply them: a folder's
/// deletes, then its entries, parents before children.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum OverlayChange {
    /// The project's entry here is gone (or replaced by what follows).
    #[serde(rename_all = "camelCase")]
    Delete {
        path: String,
        by_product: bool,
        /// What it was when the chat deleted it (None: not known, as in
        /// lists saved before this was kept).
        #[serde(default)]
        base: Option<DeletedBase>,
    },
    /// An entry here: created, changed, or moved from `origin`.
    #[serde(rename_all = "camelCase")]
    Write {
        path: String,
        kind: Kind,
        /// Where it came from in the project, if anywhere.
        origin: Option<String>,
        /// The chat's content (files it wrote); otherwise the content is the
        /// project's file at `origin`.
        upper: Option<PathBuf>,
        /// The project's file when the chat first changed it.
        base_copy: Option<PathBuf>,
        perm: u16,
        perm_changed: bool,
        target: Option<String>,
        by_product: bool,
    },
}

impl OverlayChange {
    pub fn path(&self) -> &str {
        match self {
            OverlayChange::Delete { path, .. } | OverlayChange::Write { path, .. } => path,
        }
    }

    pub fn by_product(&self) -> bool {
        match self {
            OverlayChange::Delete { by_product, .. } | OverlayChange::Write { by_product, .. } => {
                *by_product
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    next_file: u64,
    changes: Vec<OverlayChange>,
}

/// Saves a store's list of changes, which [`OverlayFs::open`] replays.
/// `next_file` names the next file of `upper/` and `base/`.
pub(super) fn write_manifest(
    store: &Path,
    next_file: u64,
    changes: Vec<OverlayChange>,
) -> io::Result<()> {
    let manifest = Manifest {
        version: 1,
        next_file,
        changes,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    crate::atomic_file::write_atomic(&store.join("manifest.json"), &bytes)
}

/// What a chat's overlay calls as its changes near their limit: the bytes
/// used, and the limit.
pub type OnWarn = Box<dyn Fn(u64, u64) + Send + Sync>;

pub struct OverlayFs {
    base: PathBuf,
    store: PathBuf,
    tiers: Tiers,
    limit: u64,
    uid: u32,
    gid: u32,
    inner: Mutex<Inner>,
    listener: Mutex<Option<Listener>>,
    on_warn: Mutex<Option<OnWarn>>,
    /// One save at a time, so an older list is never written after a
    /// newer one.
    saving: Mutex<()>,
    /// Counts changes: the comparison with other chats is skipped while it
    /// hasn't moved (chats.rs).
    generation: std::sync::atomic::AtomicU64,
    /// A copy into the overlay finished (copy_up).
    copied: std::sync::Condvar,
    /// The chat's files kept open between reads and writes (a build reads
    /// and writes the same files many times).
    handles: Mutex<HashMap<PathBuf, std::sync::Arc<fs::File>>>,
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

pub(super) fn check_name(name: &str) -> VfsResult<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(VfsError::Invalid);
    }
    if name.len() > MAX_NAME {
        return Err(VfsError::NameTooLong);
    }
    Ok(())
}

fn io_err(e: io::Error) -> VfsError {
    match e.kind() {
        io::ErrorKind::NotFound => VfsError::NotFound,
        io::ErrorKind::AlreadyExists => VfsError::Exists,
        io::ErrorKind::PermissionDenied => VfsError::NotPermitted,
        _ => VfsError::Invalid,
    }
}

#[cfg(unix)]
fn read_at(file: &fs::File, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut buf = vec![0; size];
    let mut done = 0;
    while done < size {
        let n = file.read_at(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    buf.truncate(done);
    Ok(buf)
}

#[cfg(windows)]
fn read_at(file: &fs::File, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    use std::os::windows::fs::FileExt;
    let mut buf = vec![0; size];
    let mut done = 0;
    while done < size {
        let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    buf.truncate(done);
    Ok(buf)
}

#[cfg(unix)]
fn write_at(file: &fs::File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(windows)]
fn write_at(file: &fs::File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < data.len() {
        done += file.seek_write(&data[done..], offset + done as u64)?;
    }
    Ok(())
}

impl OverlayFs {
    /// The overlay of `base` (the project) kept in `store` (its own folder),
    /// with what was saved there before, if anything.
    pub fn open(base: &Path, store: &Path, limit: u64) -> io::Result<OverlayFs> {
        fs::create_dir_all(store.join("upper"))?;
        fs::create_dir_all(store.join("base"))?;
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0, 0);
        let root_meta = fs::metadata(base)?;
        let mut nodes = HashMap::new();
        let fs_ = OverlayFs {
            base: base.to_path_buf(),
            store: store.to_path_buf(),
            tiers: Tiers::new(base),
            limit,
            uid,
            gid,
            inner: Mutex::new(Inner {
                nodes: HashMap::new(),
                next_ino: ROOT + 1,
                next_file: 1,
                used: 0,
                warned: false,
                deleted: HashMap::new(),
                trash: Vec::new(),
                copying: std::collections::HashSet::new(),
                dirty: false,
                saved_at: None,
                parked: Vec::new(),
            }),
            listener: Mutex::new(None),
            saving: Mutex::new(()),
            generation: std::sync::atomic::AtomicU64::new(0),
            copied: std::sync::Condvar::new(),
            handles: Mutex::new(HashMap::new()),
            on_warn: Mutex::new(None),
        };
        nodes.insert(
            ROOT,
            Node {
                kind: Kind::Dir,
                links: vec![(ROOT, String::new())],
                origin: Some(String::new()),
                data: Data::None,
                meta: fs_.meta_of(&root_meta),
                perm_changed: false,
                children: None,
                removed: BTreeSet::new(),
                listed_at: None,
                target: None,
                xattrs: BTreeMap::new(),
                open: 0,
                by_product: false,
            },
        );
        {
            let mut inner = fs_.inner.lock().unwrap();
            inner.nodes = nodes;
            // A saved list that can't be read isn't an empty one: opened
            // as empty, the sweep would delete every file it names.
            let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
            match fs::read(store.join("manifest.json")) {
                Ok(bytes) => {
                    let manifest: Manifest = serde_json::from_slice(&bytes)
                        .map_err(|e| invalid(format!("manifest.json: {e}")))?;
                    if manifest.version != 1 {
                        return Err(invalid(format!(
                            "manifest.json: version {} is a newer Rovik's",
                            manifest.version
                        )));
                    }
                    inner.next_file = manifest.next_file;
                    fs_.replay(&mut inner, &manifest.changes);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(fs_)
    }

    /// Called once when the chat's changes pass WARN_AT of its limit:
    /// (used, limit).
    pub fn on_warn(&self, f: OnWarn) {
        *self.on_warn.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Bytes of the chat's changes, and its limit.
    pub fn used(&self) -> (u64, u64) {
        (self.lock().used, self.limit)
    }

    /// Bytes of build output and dependencies the chat's commands wrote
    /// (not limited, not landed: shown so it doesn't grow unnoticed).
    pub fn by_product_bytes(&self) -> u64 {
        self.lock()
            .nodes
            .values()
            .filter(|n| n.by_product)
            .map(|n| match &n.data {
                Data::Upper { size, .. } => *size,
                _ => 0,
            })
            .sum()
    }

    /// One of the chat's files, open for reading and writing.
    fn handle(&self, path: &Path) -> io::Result<std::sync::Arc<fs::File>> {
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = handles.get(path) {
            return Ok(f.clone());
        }
        // Few files at once in practice; past this, start over.
        if handles.len() >= 256 {
            handles.clear();
        }
        let f = std::sync::Arc::new(fs::OpenOptions::new().read(true).write(true).open(path)?);
        handles.insert(path.to_path_buf(), f.clone());
        Ok(f)
    }

    /// Closes the files kept open (before they're deleted, or the overlay
    /// closes: Windows can't delete an open file).
    pub fn close_handles(&self) {
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self, changes: &[Change]) {
        if changes.is_empty() {
            return;
        }
        if let Some(l) = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            l(changes);
        }
    }

    fn meta_of(&self, m: &fs::Metadata) -> Meta {
        #[cfg(unix)]
        let (perm, uid, gid) = {
            use std::os::unix::fs::MetadataExt;
            ((m.mode() & 0o7777) as u16, m.uid(), m.gid())
        };
        #[cfg(not(unix))]
        let (perm, uid, gid) = (if m.is_dir() { 0o755 } else { 0o644 }, self.uid, self.gid);
        #[cfg(windows)]
        let attributes = {
            use std::os::windows::fs::MetadataExt;
            m.file_attributes() & !0x10
        };
        #[cfg(not(windows))]
        let attributes = 0;
        let mtime = m.modified().unwrap_or_else(|_| now());
        Meta {
            perm,
            uid,
            gid,
            atime: m.accessed().unwrap_or(mtime),
            mtime,
            ctime: mtime,
            crtime: m.created().unwrap_or(mtime),
            attributes,
        }
    }

    fn abs(&self, origin: &str) -> PathBuf {
        let mut p = self.base.clone();
        p.extend(origin.split('/').filter(|s| !s.is_empty()));
        p
    }

    /// The node's path in the view, with `/`; "" for the root.
    fn path_in(inner: &Inner, ino: Ino) -> Option<String> {
        let mut parts = Vec::new();
        let mut cur = ino;
        while cur != ROOT {
            let node = inner.nodes.get(&cur)?;
            let (parent, name) = node.links.first()?;
            parts.push(name.clone());
            cur = *parent;
            if parts.len() > 4096 {
                return None;
            }
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    fn attr(&self, ino: Ino, node: &Node) -> Attr {
        let size = match &node.data {
            Data::Upper { size, .. } => *size,
            Data::Base => node
                .origin
                .as_deref()
                .and_then(|o| fs::symlink_metadata(self.abs(o)).ok())
                .map(|m| m.len())
                .unwrap_or(0),
            Data::None => match node.kind {
                Kind::Symlink => node.target.as_ref().map_or(0, |t| t.len() as u64),
                _ => 4096,
            },
        };
        let nlink = if node.is_dir() {
            2
        } else {
            node.links.len().max(1) as u32
        };
        let m = &node.meta;
        Attr {
            ino,
            kind: node.kind,
            size,
            perm: m.perm,
            nlink,
            uid: m.uid,
            gid: m.gid,
            atime: m.atime,
            mtime: m.mtime,
            ctime: m.ctime,
            crtime: m.crtime,
            attributes: m.attributes,
        }
    }

    /// A node for an entry of the project at `origin`.
    fn node_from_disk(&self, origin: &str, parent: Ino, name: &str) -> Option<Node> {
        let abs = self.abs(origin);
        let meta = fs::symlink_metadata(&abs).ok()?;
        let ft = meta.file_type();
        let (kind, data, target) = if ft.is_symlink() {
            (
                Kind::Symlink,
                Data::None,
                fs::read_link(&abs)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned()),
            )
        } else if ft.is_dir() {
            (Kind::Dir, Data::None, None)
        } else if ft.is_file() {
            (Kind::File, Data::Base, None)
        } else {
            return None;
        };
        Some(Node {
            kind,
            links: vec![(parent, name.to_string())],
            origin: Some(origin.to_string()),
            data,
            meta: self.meta_of(&meta),
            perm_changed: false,
            children: None,
            removed: BTreeSet::new(),
            listed_at: None,
            target,
            xattrs: BTreeMap::new(),
            open: 0,
            by_product: self.tiers.is_by_product(origin, kind == Kind::Dir),
        })
    }

    fn add_node(inner: &mut Inner, node: Node) -> Ino {
        let ino = inner.next_ino;
        inner.next_ino += 1;
        inner.nodes.insert(ino, node);
        ino
    }

    /// Whether a folder's listing may be out of date.
    fn stale(inner: &Inner, ino: Ino) -> bool {
        inner
            .nodes
            .get(&ino)
            .is_none_or(|n| n.listed_at.is_none_or(|t| t.elapsed() >= RELIST_AFTER))
    }

    fn dir_node(inner: &Inner, ino: Ino) -> VfsResult<&Node> {
        let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
        if !node.is_dir() {
            return Err(VfsError::NotDir);
        }
        Ok(node)
    }

    /// Lists a folder from the project the first time it's needed. With
    /// `refresh`, also picks up entries that appeared in the project since
    /// and drops untouched ones that left it (the project underneath is
    /// live). Returns kernel invalidations for what changed.
    fn load_dir(&self, inner: &mut Inner, ino: Ino, refresh: bool) -> VfsResult<Vec<Change>> {
        let node = Self::dir_node(inner, ino)?;
        let loaded = node.children.is_some();
        if loaded && !refresh {
            return Ok(Vec::new());
        }
        let Some(path) = Self::path_in(inner, ino) else {
            return Ok(Vec::new());
        };
        // Only a folder still at its place in the project mirrors it.
        let listing: Vec<String> = match &node.origin {
            Some(origin) if *origin == path => fs::read_dir(self.abs(origin))
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut changes = Vec::new();
        let node = inner.nodes.get(&ino).unwrap();
        let mut children = node.children.clone().unwrap_or_default();
        let removed = node.removed.clone();
        let names: BTreeSet<&String> = listing.iter().collect();
        if loaded {
            // Untouched entries that left the project leave the view.
            let gone: Vec<(String, Ino)> = children
                .iter()
                .filter(|(name, _)| !names.contains(name))
                .filter(|(name, c)| {
                    inner.nodes.get(c).is_some_and(|n| {
                        n.origin.as_deref() == Some(join(&path, name).as_str())
                            && !n.perm_changed
                            && n.open == 0
                            && match n.kind {
                                Kind::Dir => n.children.is_none(),
                                _ => n.data != Data::None || n.kind == Kind::Symlink,
                            }
                            && !matches!(n.data, Data::Upper { .. })
                    })
                })
                .map(|(n, c)| (n.clone(), *c))
                .collect();
            for (name, c) in gone {
                children.remove(&name);
                // A hard link the chat made keeps the node alive under its
                // other names; only this name goes.
                let node = inner.nodes.get_mut(&c).unwrap();
                node.links.retain(|(p, n)| !(*p == ino && *n == name));
                if node.links.is_empty() {
                    inner.nodes.remove(&c);
                }
                changes.push(Change::Entry { parent: ino, name });
            }
        }
        for name in listing {
            if children.contains_key(&name) || removed.contains(&name) {
                continue;
            }
            if let Some(child) = self.node_from_disk(&join(&path, &name), ino, &name) {
                let c = Self::add_node(inner, child);
                children.insert(name.clone(), c);
                if loaded {
                    changes.push(Change::Entry { parent: ino, name });
                }
            }
        }
        let dir = inner.nodes.get_mut(&ino).unwrap();
        dir.children = Some(children);
        dir.listed_at = Some(Instant::now());
        Ok(changes)
    }

    /// A folder and everything under it listed: before it moves, so what
    /// it holds keeps pointing at the project's files.
    fn load_tree(&self, inner: &mut Inner, ino: Ino) {
        let _ = self.load_dir(inner, ino, false);
        let kids: Vec<Ino> = inner.nodes[&ino]
            .children
            .as_ref()
            .map(|c| c.values().copied().collect())
            .unwrap_or_default();
        for c in kids {
            if inner.nodes.get(&c).is_some_and(|n| n.is_dir()) {
                self.load_tree(inner, c);
            }
        }
    }

    fn find(&self, inner: &mut Inner, parent: Ino, name: &str) -> VfsResult<Ino> {
        self.load_dir(inner, parent, false)?;
        let lookup = |inner: &Inner| -> Option<Ino> {
            let children = inner.nodes.get(&parent)?.children.as_ref()?;
            if let Some(ino) = children.get(name) {
                return Some(*ino);
            }
            // The project's file system ignores case on macOS and Windows.
            if cfg!(any(target_os = "macos", windows)) {
                let lower = name.to_lowercase();
                return children
                    .iter()
                    .find(|(n, _)| n.to_lowercase() == lower)
                    .map(|(_, i)| *i);
            }
            None
        };
        if let Some(ino) = lookup(inner) {
            return Ok(ino);
        }
        // Maybe it appeared in the project since the folder was listed.
        let stale = inner
            .nodes
            .get(&parent)
            .ok_or(VfsError::NotFound)?
            .listed_at
            .is_none_or(|t| t.elapsed() >= RELIST_AFTER);
        if stale {
            self.load_dir(inner, parent, true)?;
        }
        lookup(inner).ok_or(VfsError::NotFound)
    }

    /// Makes a file the chat's own before it changes: its content copied
    /// into the overlay, and kept as it was for landing to merge against.
    /// `keep_content`: false when it's about to be emptied anyway.
    ///
    /// The copies are made with the lock released (a large file takes a
    /// while, and every other access to the chat's view would wait): the
    /// node is marked as copying, and another writer of the same file waits
    /// for it. Takes the lock and gives it back.
    fn copy_up<'a>(
        &'a self,
        mut inner: std::sync::MutexGuard<'a, Inner>,
        ino: Ino,
        keep_content: bool,
    ) -> VfsResult<std::sync::MutexGuard<'a, Inner>> {
        loop {
            let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
            if node.kind != Kind::File {
                return Err(VfsError::IsDir);
            }
            if !matches!(node.data, Data::Base) {
                return Ok(inner);
            }
            if !inner.copying.contains(&ino) {
                break;
            }
            inner = self.copied.wait(inner).unwrap_or_else(|e| e.into_inner());
        }
        let node = &inner.nodes[&ino];
        let origin = node.origin.clone().unwrap_or_default();
        let by_product = node.by_product;
        let src = self.abs(&origin);
        // Over the limit before copying anything, not after.
        let expected = if keep_content {
            fs::metadata(&src).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        if !by_product && inner.used + expected > self.limit {
            return Err(VfsError::NoSpace);
        }
        let n = inner.next_file;
        inner.next_file += 1;
        inner.copying.insert(ino);
        drop(inner);

        let file = self.store.join("upper").join(n.to_string());
        let base = (!by_product).then(|| self.store.join("base").join(n.to_string()));
        let copied = (|| -> io::Result<u64> {
            if let Some(b) = &base {
                fs::copy(&src, b)?;
            }
            if !keep_content {
                return fs::write(&file, b"").map(|_| 0);
            }
            let n = fs::copy(&src, &file)?;
            // The copy came with the project file's mode. The chat's copy is
            // written through (`handle` opens it read and write): a
            // read-only project file (generated code, a vendored one)
            // stays read-only to the chat as `perm` says, but the copy
            // itself must take the writes `chmod u+w` then allows.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = fs::metadata(&file)?.permissions();
                perm.set_mode(perm.mode() | 0o600);
                fs::set_permissions(&file, perm)?;
            }
            Ok(n)
        })();

        let mut inner = self.lock();
        inner.copying.remove(&ino);
        self.copied.notify_all();
        let cleanup = |file: &PathBuf, base: &Option<PathBuf>| {
            let _ = fs::remove_file(file);
            if let Some(b) = base {
                let _ = fs::remove_file(b);
            }
        };
        let size = match copied {
            Ok(size) => size,
            Err(e) => {
                cleanup(&file, &base);
                return Err(io_err(e));
            }
        };
        // Gone or changed while copying (deleted, renamed over): the copy
        // isn't needed; the caller finds the node as it is now.
        if !inner
            .nodes
            .get(&ino)
            .is_some_and(|n| matches!(n.data, Data::Base))
        {
            cleanup(&file, &base);
            return if inner.nodes.contains_key(&ino) {
                Ok(inner)
            } else {
                Err(VfsError::NotFound)
            };
        }
        // Moved into or out of a build folder while copying: counted (and
        // given a base copy) as it is now.
        let by_product_now = inner.nodes[&ino].by_product;
        let base = if by_product_now {
            if let Some(b) = &base {
                let _ = fs::remove_file(b);
            }
            None
        } else {
            base
        };
        if !by_product_now {
            if let Err(e) = self.charge(&mut inner, size) {
                cleanup(&file, &base);
                return Err(e);
            }
        }
        inner.nodes.get_mut(&ino).unwrap().data = Data::Upper {
            file,
            size,
            base_copy: base,
        };
        Ok(inner)
    }

    /// Counts `more` bytes against the chat's limit, or refuses them.
    fn charge(&self, inner: &mut Inner, more: u64) -> VfsResult<()> {
        let used = inner.used + more;
        if used > self.limit {
            return Err(VfsError::NoSpace);
        }
        inner.used = used;
        if !inner.warned && used as f64 >= self.limit as f64 * WARN_AT {
            inner.warned = true;
            if let Some(f) = self
                .on_warn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                f(used, self.limit);
            }
        }
        Ok(())
    }

    fn release_space(inner: &mut Inner, node: &Node) {
        if let (Data::Upper { size, .. }, false) = (&node.data, node.by_product) {
            inner.used = inner.used.saturating_sub(*size);
        }
    }

    /// A node with no names left and no open handles goes, with its files.
    fn collect(&self, inner: &mut Inner, ino: Ino) {
        let gone = inner
            .nodes
            .get(&ino)
            .is_some_and(|n| n.links.is_empty() && n.open == 0);
        if gone {
            let node = inner.nodes.remove(&ino).unwrap();
            Self::release_space(inner, &node);
            // Deleted once the list of changes no longer names them: a
            // crash before the next save must find them still there.
            if let Data::Upper {
                file, base_copy, ..
            } = &node.data
            {
                inner.trash.push(file.clone());
                if let Some(b) = base_copy {
                    inner.trash.push(b.clone());
                }
            }
            if let Some(children) = node.children {
                for (_, c) in children {
                    if let Some(child) = inner.nodes.get_mut(&c) {
                        child.links.retain(|(p, _)| *p != ino);
                    }
                    self.collect(inner, c);
                }
            }
        }
    }

    /// A "safe save" (`sed -i`, most editors): a new file renamed over a
    /// project file. It continues that file: same origin, and what the file
    /// was before the chat changed it stays the base to merge against.
    fn inherit(&self, inner: &mut Inner, src: Ino, dst: Ino) -> VfsResult<()> {
        let (s, d) = (&inner.nodes[&src], &inner.nodes[&dst]);
        if s.kind != Kind::File
            || d.kind != Kind::File
            || s.origin.is_some()
            || d.origin.is_none()
            || d.by_product
        {
            return Ok(());
        }
        let origin = d.origin.clone();
        let base_copy = match &d.data {
            Data::Upper { base_copy, .. } => base_copy.clone(),
            Data::Base => {
                let n = inner.next_file;
                inner.next_file += 1;
                let copy = self.store.join("base").join(n.to_string());
                fs::copy(self.abs(origin.as_deref().unwrap_or("")), &copy).map_err(io_err)?;
                Some(copy)
            }
            Data::None => None,
        };
        // The replaced file's copy now belongs to the new one.
        if let Data::Upper { base_copy: old, .. } = &mut inner.nodes.get_mut(&dst).unwrap().data {
            *old = None;
        }
        let node = inner.nodes.get_mut(&src).unwrap();
        node.origin = origin;
        if let Data::Upper { base_copy: b, .. } = &mut node.data {
            *b = base_copy;
        }
        Ok(())
    }

    /// Before a project entry at its own place leaves the view (deleted,
    /// replaced, moved away): what it is now in the project, for landing.
    fn remember_deleted(&self, inner: &mut Inner, parent: Ino, name: &str, ino: Ino) {
        let Some(node) = inner.nodes.get(&ino) else {
            return;
        };
        let Some(path) = Self::path_in(inner, parent).map(|p| join(&p, name)) else {
            return;
        };
        if node.origin.as_deref() != Some(path.as_str())
            || node.by_product
            || inner.deleted.contains_key(&path)
        {
            return;
        }
        // A file the chat changed: what it changed it from.
        let mut base = match &node.data {
            Data::Upper {
                base_copy: Some(b), ..
            } => fs::read(b).ok().map(|bytes| DeletedBase::File {
                hash: hex(&crate::file_guard::hash(&bytes)),
            }),
            Data::Upper {
                base_copy: None, ..
            } => None,
            _ => DeletedBase::of(&self.abs(&path)),
        };
        // A folder (`rm -rf`): files in it the chat had changed were deleted
        // first, each with what it was when the chat changed it. Only the
        // folder's delete lands, so it carries those.
        if let Some(DeletedBase::Dir { files, .. }) = &mut base {
            let prefix = format!("{path}/");
            for (p, b) in &inner.deleted {
                if let (Some(rel), DeletedBase::File { hash }) = (p.strip_prefix(&prefix), b) {
                    files.push((rel.to_string(), hash.clone()));
                }
            }
        }
        if let Some(b) = base {
            inner.deleted.insert(path, b);
        }
    }

    /// Takes `name` out of folder `parent`; returns the node.
    fn detach(inner: &mut Inner, parent: Ino, name: &str) -> Option<Ino> {
        let dir = inner.nodes.get_mut(&parent)?;
        let ino = dir.children.as_mut()?.remove(name)?;
        dir.removed.insert(name.to_string());
        if let Some(node) = inner.nodes.get_mut(&ino) {
            if let Some(i) = node
                .links
                .iter()
                .position(|(p, n)| *p == parent && n == name)
            {
                node.links.remove(i);
            }
        }
        Some(ino)
    }

    /// Puts node `ino` in folder `parent` as `name`.
    fn attach(&self, inner: &mut Inner, parent: Ino, name: &str, ino: Ino) {
        let path = Self::path_in(inner, parent)
            .map(|p| join(&p, name))
            .unwrap_or_default();
        let node = inner.nodes.get_mut(&ino).unwrap();
        node.links.insert(0, (parent, name.to_string()));
        let back_home = node.origin.as_deref() == Some(path.as_str());
        let dir = inner.nodes.get_mut(&parent).unwrap();
        dir.children
            .get_or_insert_with(BTreeMap::new)
            .insert(name.to_string(), ino);
        if back_home {
            dir.removed.remove(name);
            // Back where it came from: not deleted after all (a later
            // delete records it afresh).
            inner.deleted.remove(&path);
        }
        self.retier(inner, ino, &path);
    }

    /// After a move: build output in its new place, or not.
    fn retier(&self, inner: &mut Inner, ino: Ino, path: &str) {
        let Some(node) = inner.nodes.get(&ino) else {
            return;
        };
        let is_dir = node.is_dir();
        let by_product = self.tiers.is_by_product(path, is_dir);
        if by_product != node.by_product {
            let size = match &node.data {
                Data::Upper { size, .. } => *size,
                _ => 0,
            };
            if by_product {
                inner.used = inner.used.saturating_sub(size);
            } else {
                inner.used += size;
            }
            inner.nodes.get_mut(&ino).unwrap().by_product = by_product;
        }
        let kids: Vec<(String, Ino)> = inner.nodes[&ino]
            .children
            .as_ref()
            .map(|c| c.iter().map(|(n, i)| (n.clone(), *i)).collect())
            .unwrap_or_default();
        for (name, c) in kids {
            self.retier(inner, c, &join(path, &name));
        }
    }

    // -----------------------------------------------------------------
    // The project changing underneath
    // -----------------------------------------------------------------

    /// Paths in the project changed (another chat landed, the user saved, a
    /// pull). Untouched files show the new content: the kernel drops what
    /// it cached of them, and their folders are listed again. Returns the
    /// paths the chat has looked at (in its view) and those it changed
    /// itself (which may now conflict).
    pub fn project_changed(&self, rels: &[String]) -> (Vec<String>, Vec<String>) {
        let mut inner = self.lock();
        let mut seen = Vec::new();
        let mut touched = Vec::new();
        let mut kernel = Vec::new();
        // Not found at its own place in the view: maybe the chat moved it.
        let mut elsewhere: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rel in rels {
            let (dir_path, name) = rel.rsplit_once('/').unwrap_or(("", rel.as_str()));
            // Only what's already known: nothing is listed just for this.
            let mut ino = ROOT;
            let mut known = true;
            for part in dir_path.split('/').filter(|p| !p.is_empty()) {
                match inner
                    .nodes
                    .get(&ino)
                    .and_then(|n| n.children.as_ref())
                    .and_then(|c| c.get(part))
                {
                    Some(child) => ino = *child,
                    None => {
                        known = false;
                        break;
                    }
                }
            }
            if !known {
                elsewhere.insert(rel);
                continue;
            }
            let parent = ino;
            if let Some(dir) = inner.nodes.get_mut(&parent) {
                if dir.children.is_some() {
                    dir.listed_at = None;
                    kernel.push(Change::Entry {
                        parent,
                        name: name.to_string(),
                    });
                }
            }
            let child = inner
                .nodes
                .get(&parent)
                .and_then(|n| n.children.as_ref())
                .and_then(|c| c.get(name))
                .copied();
            if let Some(c) = child {
                match &inner.nodes[&c].data {
                    Data::Upper { .. } => touched.push(rel.clone()),
                    Data::Base => {
                        kernel.push(Change::Data(c));
                        seen.push(rel.clone());
                    }
                    Data::None => {}
                }
                if inner.nodes[&c].origin.as_deref() != Some(rel.as_str()) {
                    elsewhere.insert(rel);
                }
            } else {
                elsewhere.insert(rel);
            }
        }
        // An untouched file the chat moved still shows the project's file:
        // the kernel drops what it cached of it wherever it is now.
        if !elsewhere.is_empty() {
            for (ino, node) in &inner.nodes {
                if node.data == Data::Base
                    && node
                        .origin
                        .as_deref()
                        .is_some_and(|o| elsewhere.contains(o))
                {
                    kernel.push(Change::Data(*ino));
                }
            }
        }
        drop(inner);
        self.notify(&kernel);
        (seen, touched)
    }

    /// Too much changed in the project to say what (a branch switch):
    /// everything untouched is read afresh.
    pub fn project_changed_everywhere(&self) {
        let mut inner = self.lock();
        let mut kernel = Vec::new();
        for (ino, node) in inner.nodes.iter_mut() {
            if node.children.is_some() {
                node.listed_at = None;
            }
            if node.data == Data::Base {
                kernel.push(Change::Data(*ino));
            }
        }
        drop(inner);
        self.notify(&kernel);
    }

    /// The chat's version of a file it changed and the base it changed it
    /// from, for an early conflict check.
    pub fn versions(&self, rel: &str) -> Option<(PathBuf, PathBuf)> {
        let mut inner = self.lock();
        let ino = self.resolve_in(&mut inner, rel).ok()?;
        match &inner.nodes.get(&ino)?.data {
            Data::Upper {
                file,
                base_copy: Some(base),
                ..
            } => Some((file.clone(), base.clone())),
            _ => None,
        }
    }

    /// Settles a file the chat and the project both changed: the chat's
    /// version becomes `content` (None: deleted), and what it's merged
    /// against when landing becomes `base`, the project's file as it is now
    /// (None: not in the project). Used once a conflict is resolved, or once
    /// the project's newer changes are merged into the chat's view, so the
    /// landing takes the chat's version as it is.
    pub fn rebase_file(
        &self,
        rel: &str,
        content: Option<&[u8]>,
        base: Option<&[u8]>,
    ) -> VfsResult<()> {
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let parent = Backend::resolve(self, dir)?;
        let Some(bytes) = content else {
            match Backend::unlink(self, parent, name) {
                Ok(()) | Err(VfsError::NotFound) => {}
                Err(e) => return Err(e),
            }
            // Deleted, against the project as it is now: what landing checks
            // the project's file against.
            {
                let mut inner = self.lock();
                match base.and_then(|_| DeletedBase::of(&self.abs(rel))) {
                    Some(b) => {
                        inner.deleted.insert(rel.to_string(), b);
                    }
                    None => {
                        inner.deleted.remove(rel);
                    }
                }
                inner.dirty = true;
            }
            self.bump();
            self.notify(&[Change::Entry {
                parent,
                name: name.to_string(),
            }]);
            self.save();
            return Ok(());
        };
        let ino = match Backend::resolve(self, rel) {
            Ok(ino) => ino,
            Err(VfsError::NotFound) => {
                Backend::create(
                    self,
                    parent,
                    name,
                    Kind::File,
                    0o644,
                    self.uid,
                    self.gid,
                    None,
                )?
                .ino
            }
            Err(e) => return Err(e),
        };
        // The new content and base are written first, then swapped in at
        // once: the chat's processes never see a half-written file.
        {
            let mut inner = self.lock();
            let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
            if node.kind != Kind::File {
                return Err(VfsError::IsDir);
            }
            let by_product = node.by_product;
            let n = inner.next_file;
            inner.next_file += 2;
            let file = self.store.join("upper").join(n.to_string());
            fs::write(&file, bytes).map_err(io_err)?;
            let copy = match base {
                Some(b) => {
                    let copy = self.store.join("base").join((n + 1).to_string());
                    fs::write(&copy, b).map_err(io_err)?;
                    Some(copy)
                }
                None => None,
            };
            let size = bytes.len() as u64;
            let node = inner.nodes.get_mut(&ino).unwrap();
            let old = std::mem::replace(
                &mut node.data,
                Data::Upper {
                    file,
                    size,
                    base_copy: copy,
                },
            );
            let t = now();
            node.meta.mtime = t;
            node.meta.ctime = t;
            // The project has it: it's the project's file, changed (also
            // when the chat had deleted it and the resolution keeps it).
            let restored = base.is_some();
            if restored {
                node.origin = Some(rel.to_string());
            }
            if let Data::Upper {
                file,
                size: old_size,
                base_copy,
            } = old
            {
                inner.trash.push(file);
                inner.trash.extend(base_copy);
                if !by_product {
                    inner.used = inner.used.saturating_sub(old_size);
                }
            }
            if !by_product {
                inner.used += size;
            }
            if restored {
                inner.deleted.remove(rel);
                if let Some(dir) = inner.nodes.get_mut(&parent) {
                    dir.removed.remove(name);
                }
            }
            inner.dirty = true;
        }
        self.bump();
        self.notify(&[
            Change::Data(ino),
            Change::Entry {
                parent,
                name: name.to_string(),
            },
        ]);
        self.save();
        Ok(())
    }

    /// Drops the chat's own version of a file the project has, at the
    /// same path: the project's file shows again, with nothing of it to
    /// land. False when it isn't that (a file the chat created, moved or
    /// deleted, a folder, or no project file there).
    pub fn revert_file(&self, rel: &str) -> VfsResult<bool> {
        let ino = match Backend::resolve(self, rel) {
            Ok(ino) => ino,
            Err(VfsError::NotFound) => return Ok(false),
            Err(e) => return Err(e),
        };
        if !fs::metadata(self.abs(rel)).is_ok_and(|m| m.is_file()) {
            return Ok(false);
        }
        {
            let mut inner = self.lock();
            let Some(node) = inner.nodes.get_mut(&ino) else {
                return Ok(false);
            };
            if node.kind != Kind::File || node.origin.as_deref() != Some(rel) {
                return Ok(false);
            }
            let by_product = node.by_product;
            let old = std::mem::replace(&mut node.data, Data::Base);
            node.perm_changed = false;
            let t = now();
            node.meta.mtime = t;
            node.meta.ctime = t;
            if let Data::Upper {
                file,
                size,
                base_copy,
            } = old
            {
                inner.trash.push(file);
                inner.trash.extend(base_copy);
                if !by_product {
                    inner.used = inner.used.saturating_sub(size);
                }
            }
            inner.deleted.remove(rel);
            inner.dirty = true;
        }
        self.bump();
        self.notify(&[Change::Data(ino)]);
        self.save();
        Ok(true)
    }

    // -----------------------------------------------------------------
    // Changes, for landing and for saving
    // -----------------------------------------------------------------

    /// Everything the chat changed, in the order to apply it.
    pub fn changes(&self) -> Vec<OverlayChange> {
        let mut inner = self.lock();
        let mut out = Vec::new();
        self.changes_in(&mut inner, ROOT, "", &mut out);
        out.extend(inner.parked.iter().cloned());
        out
    }

    fn changes_in(&self, inner: &mut Inner, dir: Ino, path: &str, out: &mut Vec<OverlayChange>) {
        let Some(node) = inner.nodes.get(&dir) else {
            return;
        };
        let Some(children) = node.children.clone() else {
            return;
        };
        for name in node.removed.clone() {
            let p = join(path, &name);
            let replaced = children
                .get(&name)
                .is_some_and(|c| inner.nodes[c].origin.as_deref() != Some(p.as_str()));
            let gone = !children.contains_key(&name);
            if (gone || replaced) && fs::symlink_metadata(self.abs(&p)).is_ok() {
                out.push(OverlayChange::Delete {
                    by_product: self.tiers.is_by_product(&p, false),
                    base: inner.deleted.get(&p).cloned(),
                    path: p,
                });
            }
        }
        for (name, c) in children {
            let p = join(path, &name);
            let Some(n) = inner.nodes.get(&c) else {
                continue;
            };
            let moved = n.origin.as_deref() != Some(p.as_str());
            let changed = match n.kind {
                Kind::Dir => moved,
                Kind::File => moved || n.perm_changed || matches!(n.data, Data::Upper { .. }),
                Kind::Symlink => moved,
            };
            if changed {
                let (upper, base_copy) = match &n.data {
                    Data::Upper {
                        file, base_copy, ..
                    } => (Some(file.clone()), base_copy.clone()),
                    _ => (None, None),
                };
                out.push(OverlayChange::Write {
                    path: p.clone(),
                    kind: n.kind,
                    origin: n.origin.clone(),
                    upper,
                    base_copy,
                    perm: n.meta.perm,
                    perm_changed: n.perm_changed,
                    target: n.target.clone(),
                    by_product: n.by_product,
                });
            }
            if n.is_dir() && n.children.is_some() {
                self.changes_in(inner, c, &p, out);
            }
        }
    }

    fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Changes so far: different whenever anything changed.
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Notes a change; saves the list if it wasn't saved for a while.
    fn touched(&self) {
        self.bump();
        let due = {
            let mut inner = self.lock();
            inner.dirty = true;
            inner.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY)
        };
        if due {
            self.save();
        }
    }

    /// Saves the list of changes now if anything changed since it was.
    pub fn flush(&self) {
        if self.lock().dirty {
            self.save();
        }
    }

    /// Saves the list of changes, so the overlay can be opened again.
    pub fn save(&self) {
        let _one = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        // The list and the files it no longer names, taken together.
        let (changes, next_file, trash) = {
            let mut inner = self.lock();
            let mut changes = Vec::new();
            self.changes_in(&mut inner, ROOT, "", &mut changes);
            changes.extend(inner.parked.iter().cloned());
            inner.dirty = false;
            inner.saved_at = Some(Instant::now());
            (changes, inner.next_file, std::mem::take(&mut inner.trash))
        };
        if write_manifest(&self.store, next_file, changes).is_ok() {
            let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            for f in trash {
                handles.remove(&f);
                let _ = fs::remove_file(f);
            }
        } else {
            let mut inner = self.lock();
            inner.trash.extend(trash);
            inner.dirty = true;
        }
    }

    /// Deletes files in the overlay's folders that its list of changes
    /// doesn't name: left by a crash between a change and the next save.
    /// Only for the chat's own overlay, when nothing else has the same
    /// changes open (a second instance may have made files not saved yet).
    pub fn sweep_orphans(&self) {
        let changes = self.changes();
        self.sweep(&changes);
    }

    fn sweep(&self, changes: &[OverlayChange]) {
        // By folder and name: the saved paths are absolute, and the app's
        // data folder may have moved since.
        let named: std::collections::HashSet<(String, std::ffi::OsString)> = changes
            .iter()
            .flat_map(|c| match c {
                OverlayChange::Write {
                    upper, base_copy, ..
                } => [upper.as_deref(), base_copy.as_deref()],
                _ => [None, None],
            })
            .flatten()
            .filter_map(|p| {
                let dir = p.parent()?.file_name()?.to_string_lossy().into_owned();
                Some((dir, p.file_name()?.to_os_string()))
            })
            .collect();
        for dir in ["upper", "base"] {
            let Ok(rd) = fs::read_dir(self.store.join(dir)) else {
                continue;
            };
            for e in rd.flatten() {
                if !named.contains(&(dir.to_string(), e.file_name())) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// Rebuilds the view from a saved list of changes.
    fn replay(&self, inner: &mut Inner, changes: &[OverlayChange]) {
        // One node per upper file: a hard-linked file is saved once per
        // name, and its names share the file, or unlinking one would
        // trash it under the others.
        let mut by_upper: HashMap<PathBuf, Ino> = HashMap::new();
        for change in changes {
            let path = change.path();
            // A saved path is the chat's own: one that isn't (a list
            // written by hand) never reaches outside the project.
            if !path.split('/').all(|part| check_name(part).is_ok()) {
                continue;
            }
            let (dir_path, name) = match path.rsplit_once('/') {
                Some((d, n)) => (d, n),
                None => ("", path),
            };
            let Ok(parent) = self.resolve_in(inner, dir_path) else {
                // Its folder can't be found now (a branch without it):
                // kept for a later open, its file named to the sweep.
                if let OverlayChange::Write {
                    upper: Some(file),
                    by_product: false,
                    ..
                } = change
                {
                    inner.used += fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                }
                inner.parked.push(change.clone());
                continue;
            };
            let _ = self.load_dir(inner, parent, false);
            match change {
                OverlayChange::Delete { base, .. } => {
                    if let Some(b) = base {
                        inner.deleted.insert(path.to_string(), b.clone());
                    }
                    if let Some(ino) = Self::detach(inner, parent, name) {
                        self.collect(inner, ino);
                    }
                    inner
                        .nodes
                        .get_mut(&parent)
                        .unwrap()
                        .removed
                        .insert(name.to_string());
                }
                OverlayChange::Write {
                    kind,
                    origin,
                    upper,
                    base_copy,
                    perm,
                    perm_changed,
                    target,
                    by_product,
                    ..
                } => {
                    if let Some(old) = Self::detach(inner, parent, name) {
                        self.collect(inner, old);
                    }
                    if let Some(&ino) = upper.as_ref().and_then(|file| by_upper.get(file)) {
                        // Another name of a file replayed already: a link
                        // to it, its first name kept first as `link` does.
                        self.attach(inner, parent, name, ino);
                        let node = inner.nodes.get_mut(&ino).unwrap();
                        let newest = node.links.remove(0);
                        node.links.push(newest);
                        inner.nodes.get_mut(&parent).unwrap().removed.remove(name);
                        continue;
                    }
                    let meta = origin
                        .as_deref()
                        .and_then(|o| fs::symlink_metadata(self.abs(o)).ok())
                        .map(|m| self.meta_of(&m))
                        .unwrap_or(Meta {
                            perm: *perm,
                            uid: self.uid,
                            gid: self.gid,
                            atime: now(),
                            mtime: now(),
                            ctime: now(),
                            crtime: now(),
                            attributes: 0,
                        });
                    let data = match (kind, upper) {
                        (Kind::File, Some(file)) => {
                            let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                            if !by_product {
                                inner.used += size;
                            }
                            Data::Upper {
                                file: file.clone(),
                                size,
                                base_copy: base_copy.clone(),
                            }
                        }
                        (Kind::File, None) => Data::Base,
                        _ => Data::None,
                    };
                    let node = Node {
                        kind: *kind,
                        links: Vec::new(),
                        origin: origin.clone(),
                        data,
                        meta: Meta {
                            perm: *perm,
                            ..meta
                        },
                        perm_changed: *perm_changed,
                        children: (*kind == Kind::Dir).then(BTreeMap::new),
                        removed: BTreeSet::new(),
                        listed_at: None,
                        target: target.clone(),
                        xattrs: BTreeMap::new(),
                        open: 0,
                        by_product: *by_product,
                    };
                    let ino = Self::add_node(inner, node);
                    if let Some(file) = upper {
                        by_upper.insert(file.clone(), ino);
                    }
                    self.attach(inner, parent, name, ino);
                    inner.nodes.get_mut(&parent).unwrap().removed.remove(name);
                }
            }
        }
        // Deletes that preceded a write at the same name mark a replacement.
        for change in changes {
            if let OverlayChange::Delete { path, .. } = change {
                let (dir_path, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                if let Ok(parent) = self.resolve_in(inner, dir_path) {
                    if let Some(dir) = inner.nodes.get_mut(&parent) {
                        dir.removed.insert(name.to_string());
                    }
                }
            }
        }
    }

    fn resolve_in(&self, inner: &mut Inner, path: &str) -> VfsResult<Ino> {
        let mut ino = ROOT;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            ino = self.find(inner, ino, part)?;
        }
        Ok(ino)
    }
}

impl Backend for OverlayFs {
    fn getattr(&self, ino: Ino) -> VfsResult<Attr> {
        let inner = self.lock();
        let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
        let mut attr = self.attr(ino, node);
        // An untouched file shows the project's file as it is now.
        if node.data == Data::Base && !node.perm_changed {
            if let Some(m) = node
                .origin
                .as_deref()
                .and_then(|o| fs::symlink_metadata(self.abs(o)).ok())
            {
                let fresh = self.meta_of(&m);
                attr.mtime = fresh.mtime;
                attr.ctime = fresh.ctime;
                attr.perm = fresh.perm;
            }
        }
        Ok(attr)
    }

    fn lookup(&self, parent: Ino, name: &str) -> VfsResult<Attr> {
        let mut inner = self.lock();
        let ino = self.find(&mut inner, parent, name)?;
        Ok(self.attr(ino, &inner.nodes[&ino]))
    }

    fn parent_of(&self, dir: Ino) -> VfsResult<Ino> {
        let inner = self.lock();
        let node = Self::dir_node(&inner, dir)?;
        Ok(node.links.first().map(|(p, _)| *p).unwrap_or(ROOT))
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
        check_name(name)?;
        let mut inner = self.lock();
        self.load_dir(&mut inner, parent, false)?;
        if self.find(&mut inner, parent, name).is_ok() {
            return Err(VfsError::Exists);
        }
        let path = Self::path_in(&inner, parent)
            .map(|p| join(&p, name))
            .unwrap_or_default();
        let by_product = self.tiers.is_by_product(&path, kind == Kind::Dir);
        // A project file the chat deleted and now writes anew (`rm x && gen
        // > x`): it continues that file, merged against what it was, as
        // long as the project still has it as it was.
        let mut origin = None;
        let mut base_copy = None;
        if kind == Kind::File && !by_product {
            if let Some(DeletedBase::File { hash }) = inner.deleted.get(&path) {
                let abs = self.abs(&path);
                if let Ok(bytes) = fs::read(&abs) {
                    if hex(&crate::file_guard::hash(&bytes)) == *hash {
                        let n = inner.next_file;
                        inner.next_file += 1;
                        let copy = self.store.join("base").join(n.to_string());
                        fs::write(&copy, &bytes).map_err(io_err)?;
                        base_copy = Some(copy);
                        origin = Some(path.clone());
                        inner.deleted.remove(&path);
                    }
                }
            }
        }
        let data = match kind {
            Kind::File => {
                let n = inner.next_file;
                inner.next_file += 1;
                let file = self.store.join("upper").join(n.to_string());
                fs::write(&file, b"").map_err(io_err)?;
                Data::Upper {
                    file,
                    size: 0,
                    base_copy,
                }
            }
            _ => Data::None,
        };
        let t = now();
        let node = Node {
            kind,
            links: Vec::new(),
            origin,
            data,
            meta: Meta {
                perm,
                uid,
                gid,
                atime: t,
                mtime: t,
                ctime: t,
                crtime: t,
                attributes: 0,
            },
            perm_changed: false,
            children: (kind == Kind::Dir).then(BTreeMap::new),
            removed: BTreeSet::new(),
            listed_at: None,
            target: target.map(str::to_string),
            xattrs: BTreeMap::new(),
            open: 0,
            by_product,
        };
        let ino = Self::add_node(&mut inner, node);
        self.attach(&mut inner, parent, name, ino);
        let attr = self.attr(ino, &inner.nodes[&ino]);
        drop(inner);
        self.touched();
        Ok(attr)
    }

    fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()> {
        let mut inner = self.lock();
        let ino = self.find(&mut inner, parent, name)?;
        if inner.nodes[&ino].is_dir() {
            return Err(VfsError::IsDir);
        }
        let real = inner.nodes[&parent]
            .children
            .as_ref()
            .and_then(|c| c.iter().find(|(_, i)| **i == ino))
            .map(|(n, _)| n.clone());
        let real = real.as_deref().unwrap_or(name).to_string();
        self.remember_deleted(&mut inner, parent, &real, ino);
        Self::detach(&mut inner, parent, &real);
        self.collect(&mut inner, ino);
        drop(inner);
        self.touched();
        Ok(())
    }

    fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()> {
        let mut inner = self.lock();
        let ino = self.find(&mut inner, parent, name)?;
        if !inner.nodes[&ino].is_dir() {
            return Err(VfsError::NotDir);
        }
        self.load_dir(&mut inner, ino, true)?;
        if inner.nodes[&ino]
            .children
            .as_ref()
            .is_some_and(|c| !c.is_empty())
        {
            return Err(VfsError::NotEmpty);
        }
        let real = inner.nodes[&parent]
            .children
            .as_ref()
            .and_then(|c| c.iter().find(|(_, i)| **i == ino))
            .map(|(n, _)| n.clone());
        let real = real.as_deref().unwrap_or(name).to_string();
        self.remember_deleted(&mut inner, parent, &real, ino);
        Self::detach(&mut inner, parent, &real);
        self.collect(&mut inner, ino);
        drop(inner);
        self.touched();
        Ok(())
    }

    fn rename(
        &self,
        parent: Ino,
        name: &str,
        new_parent: Ino,
        new_name: &str,
        mode: RenameMode,
    ) -> VfsResult<()> {
        check_name(new_name)?;
        let mut inner = self.lock();
        let src = self.find(&mut inner, parent, name)?;
        Self::dir_node(&inner, new_parent)?;
        self.load_dir(&mut inner, new_parent, false)?;
        let dst = self.find(&mut inner, new_parent, new_name).ok();
        // Names as stored: on macOS and Windows a lookup ignores case, so the
        // name asked for can differ from the entry's.
        let stored = |inner: &Inner, dir: Ino, ino: Ino| -> Option<String> {
            inner
                .nodes
                .get(&dir)?
                .children
                .as_ref()?
                .iter()
                .find(|(_, i)| **i == ino)
                .map(|(n, _)| n.clone())
        };
        let src_name = stored(&inner, parent, src).unwrap_or_else(|| name.to_string());
        if dst == Some(src) {
            // The same entry. Found by a lookup that ignores case, under a
            // name the folder doesn't have, it's a change of case (`a` →
            // `A`), which renames it. Found under the name itself (the
            // same name, or a hard link's), nothing moves, as POSIX says.
            let under_new_name = inner.nodes[&new_parent]
                .children
                .as_ref()
                .is_some_and(|c| c.contains_key(new_name));
            if parent == new_parent && !under_new_name {
                self.remember_deleted(&mut inner, parent, &src_name, src);
                Self::detach(&mut inner, parent, &src_name);
                self.attach(&mut inner, new_parent, new_name, src);
                drop(inner);
                self.touched();
            }
            return Ok(());
        }
        let dst_name = dst
            .and_then(|d| stored(&inner, new_parent, d))
            .unwrap_or_else(|| new_name.to_string());
        // A folder can't move into itself; nor, exchanged, can the other
        // folder move into it (`a/b` for `a` would make `a` its own child).
        let within = |inner: &Inner, dir: Ino, mut at: Ino| loop {
            if at == dir {
                return true;
            }
            if at == ROOT {
                return false;
            }
            at = inner
                .nodes
                .get(&at)
                .and_then(|n| n.links.first())
                .map(|(p, _)| *p)
                .unwrap_or(ROOT);
        };
        if let Some(dst) = dst {
            if matches!(mode, RenameMode::Exchange)
                && inner.nodes[&dst].is_dir()
                && within(&inner, dst, parent)
            {
                return Err(VfsError::Invalid);
            }
        }
        if inner.nodes[&src].is_dir() && within(&inner, src, new_parent) {
            return Err(VfsError::Invalid);
        }
        // Moving a project folder: list what it holds first, so it keeps
        // pointing at the project's files.
        for ino in [Some(src), dst].into_iter().flatten() {
            if inner.nodes[&ino].is_dir() && inner.nodes[&ino].origin.is_some() {
                self.load_tree(&mut inner, ino);
            }
        }
        match (mode, dst) {
            (RenameMode::NoReplace, Some(_)) => return Err(VfsError::Exists),
            (RenameMode::Exchange, None) => return Err(VfsError::NotFound),
            (RenameMode::Exchange, Some(dst)) => {
                self.remember_deleted(&mut inner, parent, &src_name, src);
                self.remember_deleted(&mut inner, new_parent, &dst_name, dst);
                Self::detach(&mut inner, parent, &src_name);
                Self::detach(&mut inner, new_parent, &dst_name);
                self.attach(&mut inner, new_parent, &dst_name, src);
                self.attach(&mut inner, parent, &src_name, dst);
            }
            (_, dst) => {
                if let Some(dst) = dst {
                    let (src_dir, dst_dir) =
                        (inner.nodes[&src].is_dir(), inner.nodes[&dst].is_dir());
                    if dst_dir && !src_dir {
                        return Err(VfsError::IsDir);
                    }
                    if src_dir && !dst_dir {
                        return Err(VfsError::NotDir);
                    }
                    if dst_dir {
                        self.load_dir(&mut inner, dst, true)?;
                        if inner.nodes[&dst]
                            .children
                            .as_ref()
                            .is_some_and(|c| !c.is_empty())
                        {
                            return Err(VfsError::NotEmpty);
                        }
                    }
                    self.inherit(&mut inner, src, dst)?;
                    self.remember_deleted(&mut inner, new_parent, &dst_name, dst);
                    Self::detach(&mut inner, new_parent, &dst_name);
                    self.collect(&mut inner, dst);
                }
                self.remember_deleted(&mut inner, parent, &src_name, src);
                Self::detach(&mut inner, parent, &src_name);
                self.attach(&mut inner, new_parent, new_name, src);
                let n = inner.nodes.get_mut(&src).unwrap();
                n.meta.ctime = now();
            }
        }
        drop(inner);
        self.touched();
        Ok(())
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr> {
        check_name(new_name)?;
        let mut inner = self.lock();
        if inner.nodes.get(&ino).ok_or(VfsError::NotFound)?.is_dir() {
            return Err(VfsError::NotPermitted);
        }
        if self.find(&mut inner, new_parent, new_name).is_ok() {
            return Err(VfsError::Exists);
        }
        // Linked names share content the chat owns: the project's file can
        // change or go without taking the link's content with it.
        if inner.nodes[&ino].kind == Kind::File {
            inner = self.copy_up(inner, ino, true)?;
            // The lock was let go while copying: still a folder there, and
            // the name still free?
            Self::dir_node(&inner, new_parent)?;
            if self.find(&mut inner, new_parent, new_name).is_ok() {
                return Err(VfsError::Exists);
            }
        }
        self.attach(&mut inner, new_parent, new_name, ino);
        // `attach` makes the newest name the first; keep the original first.
        let node = inner.nodes.get_mut(&ino).unwrap();
        let newest = node.links.remove(0);
        node.links.push(newest);
        let attr = self.attr(ino, &inner.nodes[&ino]);
        drop(inner);
        self.touched();
        Ok(attr)
    }

    fn readlink(&self, ino: Ino) -> VfsResult<String> {
        let inner = self.lock();
        let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
        if node.kind != Kind::Symlink {
            return Err(VfsError::Invalid);
        }
        node.target.clone().ok_or(VfsError::NotFound)
    }

    fn read(&self, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let inner = self.lock();
        let node = inner.nodes.get(&ino).ok_or(VfsError::NotFound)?;
        let (path, own) = match &node.data {
            Data::Upper { file, .. } => (file.clone(), true),
            Data::Base => (self.abs(node.origin.as_deref().unwrap_or("")), false),
            Data::None => return Err(VfsError::IsDir),
        };
        drop(inner);
        // The project's files are opened afresh: they may be replaced.
        let file = if own {
            self.handle(&path).map_err(io_err)?
        } else {
            std::sync::Arc::new(fs::File::open(&path).map_err(io_err)?)
        };
        read_at(&file, offset, size).map_err(io_err)
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize> {
        let mut inner = self.copy_up(self.lock(), ino, true)?;
        let node = &inner.nodes[&ino];
        let (file, size) = match &node.data {
            Data::Upper { file, size, .. } => (file.clone(), *size),
            _ => return Err(VfsError::IsDir),
        };
        let end = offset + data.len() as u64;
        let grows = if end > size && !node.by_product {
            end - size
        } else {
            0
        };
        if grows > 0 {
            self.charge(&mut inner, grows)?;
        }
        if let Err(e) = self.handle(&file).and_then(|f| write_at(&f, offset, data)) {
            // Nothing was added: the charge goes back.
            inner.used = inner.used.saturating_sub(grows);
            return Err(io_err(e));
        }
        let node = inner.nodes.get_mut(&ino).unwrap();
        if let Data::Upper { size, .. } = &mut node.data {
            *size = (*size).max(end);
        }
        let t = now();
        node.meta.mtime = t;
        node.meta.ctime = t;
        drop(inner);
        self.bump();
        Ok(data.len())
    }

    fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
        let mut inner = self.lock();
        if let Some(new_size) = set.size {
            inner = self.copy_up(inner, ino, new_size > 0)?;
            let node = &inner.nodes[&ino];
            if let Data::Upper { file, size, .. } = &node.data {
                let (file, size, by_product) = (file.clone(), *size, node.by_product);
                if new_size > size && !by_product {
                    self.charge(&mut inner, new_size - size)?;
                } else if new_size < size && !by_product {
                    inner.used = inner.used.saturating_sub(size - new_size);
                }
                fs::OpenOptions::new()
                    .write(true)
                    .open(&file)
                    .and_then(|f| f.set_len(new_size))
                    .map_err(io_err)?;
                if let Data::Upper { size, .. } = &mut inner.nodes.get_mut(&ino).unwrap().data {
                    *size = new_size;
                }
                inner.nodes.get_mut(&ino).unwrap().meta.mtime = now();
            }
        }
        let node = inner.nodes.get_mut(&ino).ok_or(VfsError::NotFound)?;
        if let Some(perm) = set.perm {
            if perm != node.meta.perm {
                node.meta.perm = perm & 0o7777;
                node.perm_changed = true;
            }
        }
        if let Some(uid) = set.uid {
            node.meta.uid = uid;
        }
        if let Some(gid) = set.gid {
            node.meta.gid = gid;
        }
        if let Some(t) = set.atime {
            node.meta.atime = t;
        }
        if let Some(t) = set.mtime {
            node.meta.mtime = t;
        }
        if let Some(t) = set.crtime {
            node.meta.crtime = t;
        }
        if let Some(a) = set.attributes {
            node.meta.attributes = a;
        }
        node.meta.ctime = now();
        let attr = self.attr(ino, &inner.nodes[&ino]);
        let structural = set.size.is_some() || set.perm.is_some();
        drop(inner);
        if structural {
            self.touched();
        }
        Ok(attr)
    }

    fn dir_is_empty(&self, ino: Ino) -> VfsResult<bool> {
        let mut inner = self.lock();
        let stale = Self::stale(&inner, ino);
        self.load_dir(&mut inner, ino, stale)?;
        Ok(inner.nodes[&ino]
            .children
            .as_ref()
            .is_none_or(|c| c.is_empty()))
    }

    fn readdir_plus(&self, ino: Ino) -> VfsResult<Vec<(DirEntry, Attr)>> {
        let mut inner = self.lock();
        // Listed again from the project only when it may have changed: the
        // project's watcher marks folders it saw change (project_changed),
        // and anything else is re-read after RELIST_AFTER.
        let stale = Self::stale(&inner, ino);
        let changes = self.load_dir(&mut inner, ino, stale)?;
        let children: Vec<(String, Ino)> = inner.nodes[&ino]
            .children
            .as_ref()
            .map(|c| c.iter().map(|(n, i)| (n.clone(), *i)).collect())
            .unwrap_or_default();
        let out = children
            .into_iter()
            .filter_map(|(name, c)| {
                let node = inner.nodes.get(&c)?;
                Some((
                    DirEntry {
                        name,
                        ino: c,
                        kind: node.kind,
                    },
                    self.attr(c, node),
                ))
            })
            .collect();
        drop(inner);
        self.notify(&changes);
        Ok(out)
    }

    fn open(&self, ino: Ino, _write: bool) -> VfsResult<Attr> {
        let mut inner = self.lock();
        let node = inner.nodes.get_mut(&ino).ok_or(VfsError::NotFound)?;
        node.open += 1;
        Ok(self.attr(ino, &inner.nodes[&ino]))
    }

    fn release(&self, ino: Ino) {
        let mut inner = self.lock();
        let Some(node) = inner.nodes.get_mut(&ino) else {
            return;
        };
        node.open = node.open.saturating_sub(1);
        let wrote = matches!(node.data, Data::Upper { .. });
        self.collect(&mut inner, ino);
        drop(inner);
        if wrote {
            self.touched();
        }
    }

    fn getxattr(&self, ino: Ino, name: &str) -> VfsResult<Vec<u8>> {
        let inner = self.lock();
        inner
            .nodes
            .get(&ino)
            .ok_or(VfsError::NotFound)?
            .xattrs
            .get(name)
            .cloned()
            .ok_or(VfsError::NoAttr)
    }

    fn setxattr(
        &self,
        ino: Ino,
        name: &str,
        value: &[u8],
        create_only: bool,
        replace_only: bool,
    ) -> VfsResult<()> {
        let mut inner = self.lock();
        let node = inner.nodes.get_mut(&ino).ok_or(VfsError::NotFound)?;
        let exists = node.xattrs.contains_key(name);
        if create_only && exists {
            return Err(VfsError::Exists);
        }
        if replace_only && !exists {
            return Err(VfsError::NoAttr);
        }
        node.xattrs.insert(name.to_string(), value.to_vec());
        Ok(())
    }

    fn listxattr(&self, ino: Ino) -> VfsResult<Vec<String>> {
        let inner = self.lock();
        Ok(inner
            .nodes
            .get(&ino)
            .ok_or(VfsError::NotFound)?
            .xattrs
            .keys()
            .cloned()
            .collect())
    }

    fn removexattr(&self, ino: Ino, name: &str) -> VfsResult<()> {
        let mut inner = self.lock();
        inner
            .nodes
            .get_mut(&ino)
            .ok_or(VfsError::NotFound)?
            .xattrs
            .remove(name)
            .map(|_| ())
            .ok_or(VfsError::NoAttr)
    }

    fn usage(&self) -> (u64, u64, u64) {
        let inner = self.lock();
        (self.limit, inner.used, inner.nodes.len() as u64)
    }

    fn set_listener(&self, listener: Option<Listener>) {
        *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = listener;
    }

    fn resolve(&self, path: &str) -> VfsResult<Ino> {
        let mut inner = self.lock();
        self.resolve_in(&mut inner, path)
    }

    fn resolve_parent<'a>(&self, path: &'a str) -> VfsResult<(Ino, &'a str)> {
        let path = path.trim_end_matches(['/', '\\']);
        let (dir, name) = match path.rfind(['/', '\\']) {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let mut inner = self.lock();
        Ok((self.resolve_in(&mut inner, dir)?, name))
    }

    fn path_of(&self, ino: Ino) -> VfsResult<String> {
        Self::path_in(&self.lock(), ino).ok_or(VfsError::NotFound)
    }
}

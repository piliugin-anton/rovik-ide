//! The FUSE driver (Linux, macOS): kernel requests → [`VirtualFs`].
//!
//! Linux mounts through the `fusermount3`/`fusermount` helper (fuser's pure
//! Rust mount, no libfuse needed). macOS mounts through macFUSE's library,
//! loaded at run time (see `macfuse.rs`), so the app starts on a Mac that
//! doesn't have macFUSE yet.
//!
//! The kernel enforces permissions (`default_permissions`), so modes and
//! owners behave like on a disk filesystem. Attributes are cached for a
//! second; changes the app makes behind the kernel's back are pushed to it as
//! cache invalidations.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fuser::{
    AccessFlags, BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, FopenFlags, Generation, INodeNo,
    InitFlags, KernelConfig, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request, SessionACL, TimeOrNow, WriteFlags,
};

use super::problem::MountProblem;
use super::store::{Attr, Change, Kind, RenameMode, SetAttr, VfsError, VirtualFs};

const TTL: Duration = Duration::from_secs(1);
const BLOCK: u64 = 4096;

fn errno(e: VfsError) -> Errno {
    match e {
        VfsError::NotFound => Errno::ENOENT,
        VfsError::Exists => Errno::EEXIST,
        VfsError::NotDir => Errno::ENOTDIR,
        VfsError::IsDir => Errno::EISDIR,
        VfsError::NotEmpty => Errno::ENOTEMPTY,
        VfsError::Invalid => Errno::EINVAL,
        VfsError::NameTooLong => Errno::ENAMETOOLONG,
        VfsError::NoSpace => Errno::ENOSPC,
        VfsError::NotPermitted => Errno::EPERM,
        VfsError::Loop => Errno::ELOOP,
        VfsError::NoAttr => Errno::NO_XATTR,
        VfsError::Range => Errno::ERANGE,
    }
}

/// Who a new file belongs to. macFUSE's FSKit backend doesn't tell who
/// made a request (it arrives as root); files are then the user's.
fn owner(req: &Request) -> (u32, u32) {
    #[cfg(target_os = "macos")]
    if req.uid() == 0 && unsafe { libc::geteuid() } != 0 {
        return unsafe { (libc::getuid(), libc::getgid()) };
    }
    (req.uid(), req.gid())
}

fn kind(k: Kind) -> FileType {
    match k {
        Kind::File => FileType::RegularFile,
        Kind::Dir => FileType::Directory,
        Kind::Symlink => FileType::Symlink,
    }
}

fn file_attr(a: &Attr) -> FileAttr {
    FileAttr {
        ino: INodeNo(a.ino),
        size: a.size,
        blocks: a.size.div_ceil(512),
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime,
        kind: kind(a.kind),
        perm: a.perm,
        nlink: a.nlink,
        uid: a.uid,
        gid: a.gid,
        rdev: 0,
        blksize: BLOCK as u32,
        flags: 0,
    }
}

/// Names on the volume are UTF-8; anything else can't exist on it.
fn name_str(name: &OsStr) -> Result<&str, Errno> {
    name.to_str().ok_or(Errno::EINVAL)
}

fn time(t: TimeOrNow) -> SystemTime {
    match t {
        TimeOrNow::SpecificTime(t) => t,
        TimeOrNow::Now => SystemTime::now(),
    }
}

/// Open files keep the kernel's page cache between opens: what programs
/// write goes through it, and what the app writes invalidates it (see
/// `FuseMount::watch`). Linux only — older macFUSE versions don't support
/// invalidation.
#[cfg(target_os = "linux")]
const OPEN_FLAGS: FopenFlags = FopenFlags::FOPEN_KEEP_CACHE;
#[cfg(not(target_os = "linux"))]
const OPEN_FLAGS: FopenFlags = FopenFlags::empty();

/// `renameat2` flags; macOS's `renamex_np` spells them differently.
#[cfg(target_os = "linux")]
const RENAME_NOREPLACE: u32 = 1;
#[cfg(target_os = "linux")]
const RENAME_EXCHANGE: u32 = 2;
#[cfg(target_os = "macos")]
const RENAME_NOREPLACE: u32 = 0x4; // RENAME_EXCL
#[cfg(target_os = "macos")]
const RENAME_EXCHANGE: u32 = 0x2; // RENAME_SWAP

type DirSnapshot = Vec<(String, Attr)>;

pub struct FuseFs {
    vfs: Arc<VirtualFs>,
    next_fh: AtomicU64,
    /// Open directories' listings, taken at `opendir`: `rm -rf` deletes
    /// while it reads, and offsets into a list that shifts would skip names.
    dirs: Mutex<HashMap<u64, DirSnapshot>>,
}

impl FuseFs {
    pub fn new(vfs: Arc<VirtualFs>) -> Self {
        FuseFs { vfs, next_fh: AtomicU64::new(1), dirs: Mutex::new(HashMap::new()) }
    }

    fn fh(&self) -> FileHandle {
        FileHandle(self.next_fh.fetch_add(1, Ordering::Relaxed))
    }

    fn entry(&self, reply: ReplyEntry, result: Result<Attr, VfsError>) {
        match result {
            Ok(attr) => reply.entry(&TTL, &file_attr(&attr), Generation(0)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn empty(reply: ReplyEmpty, result: Result<(), VfsError>) {
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
}

impl fuser::Filesystem for FuseFs {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        // Each is asked for separately: an old kernel lacking one still
        // gets the others.
        for flag in [
            // Listings come with attributes: `ls -l`, `find`, `git status`
            // don't need a lookup per entry.
            InitFlags::FUSE_DO_READDIRPLUS,
            InitFlags::FUSE_READDIRPLUS_AUTO,
            // Lookups in one directory may run in parallel.
            InitFlags::FUSE_PARALLEL_DIROPS,
            InitFlags::FUSE_CACHE_SYMLINKS,
        ] {
            let _ = config.add_capabilities(flag);
        }
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match name.to_str() {
            Some(name) => self.entry(reply, self.vfs.lookup(parent.0, name)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.vfs.getattr(ino.0) {
            Ok(attr) => reply.attr(&TTL, &file_attr(&attr)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let set = SetAttr {
            perm: mode.map(|m| (m & 0o7777) as u16),
            uid,
            gid,
            size,
            atime: atime.map(time),
            mtime: mtime.map(time),
            crtime,
            attributes: None,
        };
        match self.vfs.setattr(ino.0, &set) {
            Ok(attr) => reply.attr(&TTL, &file_attr(&attr)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match self.vfs.readlink(ino.0) {
            Ok(target) => reply.data(target.as_bytes()),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn mknod(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, _rdev: u32, reply: ReplyEntry) {
        // Regular files only: no devices, FIFOs or sockets in memory.
        if mode & libc::S_IFMT as u32 != libc::S_IFREG as u32 && mode & libc::S_IFMT as u32 != 0 {
            return reply.error(Errno::EPERM);
        }
        let name = match name_str(name) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let perm = (mode & !umask & 0o7777) as u16;
        self.entry(reply, self.vfs.create(parent.0, name, Kind::File, perm, owner(req).0, owner(req).1, None));
    }

    fn mkdir(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let name = match name_str(name) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let perm = (mode & !umask & 0o7777) as u16;
        self.entry(reply, self.vfs.create(parent.0, name, Kind::Dir, perm, owner(req).0, owner(req).1, None));
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match name_str(name) {
            Ok(name) => Self::empty(reply, self.vfs.unlink(parent.0, name)),
            Err(_) => reply.error(Errno::ENOENT),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match name_str(name) {
            Ok(name) => Self::empty(reply, self.vfs.rmdir(parent.0, name)),
            Err(_) => reply.error(Errno::ENOENT),
        }
    }

    fn symlink(&self, req: &Request, parent: INodeNo, link_name: &OsStr, target: &Path, reply: ReplyEntry) {
        let (Ok(name), Some(target)) = (name_str(link_name), target.to_str()) else {
            return reply.error(Errno::EINVAL);
        };
        self.entry(reply, self.vfs.create(parent.0, name, Kind::Symlink, 0o777, owner(req).0, owner(req).1, Some(target)));
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let (Ok(name), Ok(newname)) = (name_str(name), name_str(newname)) else {
            return reply.error(Errno::EINVAL);
        };
        let bits = flags.bits();
        let mode = if bits & RENAME_EXCHANGE != 0 {
            RenameMode::Exchange
        } else if bits & RENAME_NOREPLACE != 0 {
            RenameMode::NoReplace
        } else if bits == 0 {
            RenameMode::Replace
        } else {
            return reply.error(Errno::EINVAL);
        };
        Self::empty(reply, self.vfs.rename(parent.0, name, newparent.0, newname, mode));
    }

    fn link(&self, _req: &Request, ino: INodeNo, newparent: INodeNo, newname: &OsStr, reply: ReplyEntry) {
        match name_str(newname) {
            Ok(name) => self.entry(reply, self.vfs.link(ino.0, newparent.0, name)),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let writable = flags.0 & libc::O_ACCMODE != libc::O_RDONLY;
        let attr = match self.vfs.open(ino.0, writable) {
            Ok(attr) => attr,
            Err(e) => return reply.error(errno(e)),
        };
        if attr.kind == Kind::Dir {
            self.vfs.release(ino.0);
            return reply.error(Errno::EISDIR);
        }
        // With atomic O_TRUNC the kernel leaves truncating to us.
        if writable && flags.0 & libc::O_TRUNC != 0 && attr.size > 0 {
            if let Err(e) = self.vfs.setattr(ino.0, &SetAttr { size: Some(0), ..Default::default() }) {
                self.vfs.release(ino.0);
                return reply.error(errno(e));
            }
        }
        reply.opened(self.fh(), OPEN_FLAGS);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.vfs.read(ino.0, offset, size as usize) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.vfs.write(ino.0, offset, data) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn flush(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _lock_owner: LockOwner, reply: ReplyEmpty) {
        // Nothing to flush in memory. ENOSYS makes the kernel stop sending
        // flushes (one round trip less per close); close() still succeeds.
        reply.error(Errno::ENOSYS);
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.vfs.release(ino.0);
        reply.ok();
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        // As for flush: the kernel then answers fsync() itself, with success.
        reply.error(Errno::ENOSYS);
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let entries = match self.vfs.readdir_plus(ino.0) {
            Ok(entries) => entries,
            Err(e) => return reply.error(errno(e)),
        };
        let parent = self.vfs.parent_of(ino.0).unwrap_or(ino.0);
        let (Ok(this), Ok(up)) = (self.vfs.getattr(ino.0), self.vfs.getattr(parent)) else {
            return reply.error(Errno::ENOENT);
        };
        let mut listing: DirSnapshot = Vec::with_capacity(entries.len() + 2);
        listing.push((".".into(), this));
        listing.push(("..".into(), up));
        listing.extend(entries.into_iter().map(|(e, attr)| (e.name, attr)));
        let fh = self.fh();
        self.dirs.lock().unwrap_or_else(|e| e.into_inner()).insert(fh.0, listing);
        reply.opened(fh, FopenFlags::empty());
    }

    fn readdir(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(listing) = dirs.get(&fh.0) else {
            return reply.error(Errno::EBADF);
        };
        for (i, (name, attr)) in listing.iter().enumerate().skip(offset as usize) {
            // The offset handed back is where the next call continues.
            if reply.add(INodeNo(attr.ino), i as u64 + 1, kind(attr.kind), name) {
                break;
            }
        }
        reply.ok();
    }

    fn readdirplus(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, mut reply: ReplyDirectoryPlus) {
        let dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(listing) = dirs.get(&fh.0) else {
            return reply.error(Errno::EBADF);
        };
        for (i, (name, attr)) in listing.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(attr.ino), i as u64 + 1, name, &TTL, &file_attr(attr), Generation(0)) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, reply: ReplyEmpty) {
        self.dirs.lock().unwrap_or_else(|e| e.into_inner()).remove(&fh.0);
        reply.ok();
    }

    fn fsyncdir(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        reply.error(Errno::ENOSYS);
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let (capacity, used, nodes) = self.vfs.usage();
        let blocks = capacity / BLOCK;
        let free = capacity.saturating_sub(used) / BLOCK;
        reply.statfs(blocks, free, free, nodes + 1_000_000, 1_000_000, BLOCK as u32, super::store::MAX_NAME as u32, BLOCK as u32);
    }

    fn setxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, value: &[u8], flags: i32, _position: u32, reply: ReplyEmpty) {
        let Ok(name) = name_str(name) else {
            return reply.error(Errno::EINVAL);
        };
        let create = flags & libc::XATTR_CREATE != 0;
        let replace = flags & libc::XATTR_REPLACE != 0;
        Self::empty(reply, self.vfs.setxattr(ino.0, name, value, create, replace));
    }

    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        let Ok(name) = name_str(name) else {
            return reply.error(Errno::NO_XATTR);
        };
        match self.vfs.getxattr(ino.0, name) {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() > size as usize => reply.error(Errno::ERANGE),
            Ok(value) => reply.data(&value),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        let names = match self.vfs.listxattr(ino.0) {
            Ok(names) => names,
            Err(e) => return reply.error(errno(e)),
        };
        let mut data = Vec::new();
        for name in names {
            data.extend_from_slice(name.as_bytes());
            data.push(0);
        }
        if size == 0 {
            reply.size(data.len() as u32);
        } else if data.len() > size as usize {
            reply.error(Errno::ERANGE);
        } else {
            reply.data(&data);
        }
    }

    fn removexattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match name_str(name) {
            Ok(name) => Self::empty(reply, self.vfs.removexattr(ino.0, name)),
            Err(_) => reply.error(Errno::NO_XATTR),
        }
    }

    fn access(&self, _req: &Request, ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        // The kernel checks permissions itself (default_permissions).
        Self::empty(reply, self.vfs.getattr(ino.0).map(|_| ()));
    }

    fn create(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, _flags: i32, reply: ReplyCreate) {
        let name = match name_str(name) {
            Ok(n) => n,
            Err(e) => return reply.error(e),
        };
        let perm = (mode & !umask & 0o7777) as u16;
        let attr = match self.vfs.create(parent.0, name, Kind::File, perm, owner(req).0, owner(req).1, None) {
            Ok(attr) => attr,
            Err(e) => return reply.error(errno(e)),
        };
        if let Err(e) = self.vfs.open(attr.ino, true) {
            return reply.error(errno(e));
        }
        reply.created(&TTL, &file_attr(&attr), Generation(0), self.fh(), OPEN_FLAGS);
    }

    fn fallocate(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, length: u64, mode: i32, reply: ReplyEmpty) {
        // Plain preallocation only (extends the size); no hole punching.
        if mode != 0 {
            return reply.error(Errno::EOPNOTSUPP);
        }
        let result = self.vfs.getattr(ino.0).and_then(|attr| {
            let end = offset.saturating_add(length);
            if end > attr.size {
                self.vfs.setattr(ino.0, &SetAttr { size: Some(end), ..Default::default() })?;
            }
            Ok(())
        });
        Self::empty(reply, result);
    }
}

fn mount_options() -> Config {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("rovik-vfs".into()),
        MountOption::DefaultPermissions,
        MountOption::RW,
        MountOption::Exec,
        MountOption::NoAtime,
    ];
    #[cfg(target_os = "linux")]
    config.mount_options.push(MountOption::Subtype("rovik".into()));
    #[cfg(target_os = "macos")]
    config.mount_options.extend([
        // No ._* AppleDouble files: xattrs are stored natively.
        MountOption::CUSTOM("noappledouble".into()),
    ]);
    config.acl = SessionACL::Owner;
    // Linux: several threads, each with its own /dev/fuse descriptor, so
    // requests from different programs are served in parallel (the store's
    // read lock lets reads overlap). fuser supports this only on Linux.
    #[cfg(target_os = "linux")]
    {
        config.n_threads = Some(std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4));
        config.clone_fd = true;
    }
    config
}

/// A mounted volume. Dropping it unmounts.
pub struct FuseMount {
    session: Option<BackgroundSession>,
    vfs: Arc<VirtualFs>,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    mountpoint: std::path::PathBuf,
    #[cfg(target_os = "macos")]
    mac: Option<super::macfuse::MacMount>,
}

impl FuseMount {
    /// Tells the kernel what the app changed, so it doesn't serve stale
    /// cached data.
    fn watch(session: &BackgroundSession, vfs: &Arc<VirtualFs>) {
        let notifier = session.notifier();
        vfs.set_listener(Some(Box::new(move |changes| {
            for change in changes {
                // Errors only mean the kernel had nothing cached (or, on an
                // old macFUSE, doesn't support notifications).
                let _ = match change {
                    Change::Data(ino) => notifier.inval_inode(INodeNo(*ino), 0, 0),
                    Change::Entry { parent, name } => notifier.inval_entry(INodeNo(*parent), OsStr::new(name)),
                };
            }
        })));
    }

    pub fn unmount(mut self) -> Result<(), String> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> Result<(), String> {
        self.vfs.set_listener(None);
        let Some(session) = self.session.take() else { return Ok(()) };
        #[cfg(target_os = "linux")]
        {
            session.umount_and_join().map_err(|e| format!("unmounting: {e}"))
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(mac) = self.mac.take() {
                mac.unmount(&self.mountpoint)?;
            }
            session.join().map_err(|e| format!("unmounting: {e}"))
        }
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

/// Clears a mount left by a crashed run ("Transport endpoint is not
/// connected"), which would make the folder unusable.
#[cfg(target_os = "linux")]
fn clear_stale_mount(mountpoint: &Path) {
    let stale = matches!(std::fs::metadata(mountpoint), Err(e) if e.raw_os_error() == Some(libc::ENOTCONN));
    if stale {
        for helper in ["fusermount3", "fusermount"] {
            let ok = std::process::Command::new(helper)
                .args(["-u", "-z"])
                .arg(mountpoint)
                .output()
                .is_ok_and(|o| o.status.success());
            if ok {
                break;
            }
        }
    }
}

/// Mounts at `requested`, or where the platform mounts by default; returns
/// where it mounted.
pub fn mount(vfs: Arc<VirtualFs>, requested: Option<&Path>) -> Result<(FuseMount, std::path::PathBuf), MountProblem> {
    #[cfg(target_os = "linux")]
    {
        let mountpoint = requested.map(Path::to_path_buf).unwrap_or_else(super::default_mountpoint);
        clear_stale_mount(&mountpoint);
        std::fs::create_dir_all(&mountpoint).map_err(|e| MountProblem::failed(format!("{}: {e}", mountpoint.display())))?;
        let fs = FuseFs::new(vfs.clone());
        let session = fuser::Session::new(fs, &mountpoint, &mount_options())
            .map_err(|e| MountProblem::failed(format!("mounting {}: {e}", mountpoint.display())))?
            .spawn()
            .map_err(|e| MountProblem::failed(e.to_string()))?;
        FuseMount::watch(&session, &vfs);
        Ok((FuseMount { session: Some(session), vfs, mountpoint: mountpoint.clone() }, mountpoint))
    }
    #[cfg(target_os = "macos")]
    {
        let (session, mac, mountpoint) = super::macfuse::mount(FuseFs::new(vfs.clone()), requested, &mount_options())?;
        FuseMount::watch(&session, &vfs);
        Ok((FuseMount { session: Some(session), vfs, mountpoint: mountpoint.clone(), mac: Some(mac) }, mountpoint))
    }
}

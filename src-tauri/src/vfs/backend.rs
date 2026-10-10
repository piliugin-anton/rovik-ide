//! What the drivers (FUSE, macFUSE, WinFsp) serve: any filesystem with this
//! inode API, not only the app's in-memory [`VirtualFs`].
//!
//! The virtual-files volume serves a `VirtualFs`. A chat's overlay
//! (src/overlay) serves the project as that chat sees it: the project
//! underneath, the chat's changes on top. Both go through the same drivers,
//! so a command a chat runs sees its files through ordinary system calls.
//!
//! The methods mean what [`VirtualFs`]'s methods of the same names mean.

use super::store::{
    Attr, DirEntry, Ino, Kind, Listener, RenameMode, SetAttr, VfsResult, VirtualFs,
};

// `dir_is_empty`, `resolve`, `resolve_parent` and `path_of`: only the
// path-based WinFsp driver uses them.
#[cfg_attr(not(windows), allow(dead_code))]
pub trait Backend: Send + Sync {
    fn getattr(&self, ino: Ino) -> VfsResult<Attr>;
    fn lookup(&self, parent: Ino, name: &str) -> VfsResult<Attr>;
    fn parent_of(&self, dir: Ino) -> VfsResult<Ino>;
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        parent: Ino,
        name: &str,
        kind: Kind,
        perm: u16,
        uid: u32,
        gid: u32,
        target: Option<&str>,
    ) -> VfsResult<Attr>;
    fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()>;
    fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()>;
    fn rename(
        &self,
        parent: Ino,
        name: &str,
        new_parent: Ino,
        new_name: &str,
        mode: RenameMode,
    ) -> VfsResult<()>;
    fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr>;
    fn readlink(&self, ino: Ino) -> VfsResult<String>;
    fn read(&self, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>>;
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize>;
    fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr>;
    fn dir_is_empty(&self, ino: Ino) -> VfsResult<bool>;
    fn readdir_plus(&self, ino: Ino) -> VfsResult<Vec<(DirEntry, Attr)>>;
    fn open(&self, ino: Ino, write: bool) -> VfsResult<Attr>;
    fn release(&self, ino: Ino);
    fn getxattr(&self, ino: Ino, name: &str) -> VfsResult<Vec<u8>>;
    fn setxattr(
        &self,
        ino: Ino,
        name: &str,
        value: &[u8],
        create_only: bool,
        replace_only: bool,
    ) -> VfsResult<()>;
    fn listxattr(&self, ino: Ino) -> VfsResult<Vec<String>>;
    fn removexattr(&self, ino: Ino, name: &str) -> VfsResult<()>;
    /// (capacity, used, nodes), for `statfs`.
    fn usage(&self) -> (u64, u64, u64);
    /// Changes made behind the kernel's back, so it can drop what it cached.
    fn set_listener(&self, listener: Option<Listener>);
    fn resolve(&self, path: &str) -> VfsResult<Ino>;
    fn resolve_parent<'a>(&self, path: &'a str) -> VfsResult<(Ino, &'a str)>;
    fn path_of(&self, ino: Ino) -> VfsResult<String>;
}

impl Backend for VirtualFs {
    fn getattr(&self, ino: Ino) -> VfsResult<Attr> {
        VirtualFs::getattr(self, ino)
    }
    fn lookup(&self, parent: Ino, name: &str) -> VfsResult<Attr> {
        VirtualFs::lookup(self, parent, name)
    }
    fn parent_of(&self, dir: Ino) -> VfsResult<Ino> {
        VirtualFs::parent_of(self, dir)
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
        VirtualFs::create(self, parent, name, kind, perm, uid, gid, target)
    }
    fn unlink(&self, parent: Ino, name: &str) -> VfsResult<()> {
        VirtualFs::unlink(self, parent, name)
    }
    fn rmdir(&self, parent: Ino, name: &str) -> VfsResult<()> {
        VirtualFs::rmdir(self, parent, name)
    }
    fn rename(
        &self,
        parent: Ino,
        name: &str,
        new_parent: Ino,
        new_name: &str,
        mode: RenameMode,
    ) -> VfsResult<()> {
        VirtualFs::rename(self, parent, name, new_parent, new_name, mode)
    }
    fn link(&self, ino: Ino, new_parent: Ino, new_name: &str) -> VfsResult<Attr> {
        VirtualFs::link(self, ino, new_parent, new_name)
    }
    fn readlink(&self, ino: Ino) -> VfsResult<String> {
        VirtualFs::readlink(self, ino)
    }
    fn read(&self, ino: Ino, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        VirtualFs::read(self, ino, offset, size)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> VfsResult<usize> {
        VirtualFs::write(self, ino, offset, data)
    }
    fn setattr(&self, ino: Ino, set: &SetAttr) -> VfsResult<Attr> {
        VirtualFs::setattr(self, ino, set)
    }
    fn dir_is_empty(&self, ino: Ino) -> VfsResult<bool> {
        VirtualFs::dir_is_empty(self, ino)
    }
    fn readdir_plus(&self, ino: Ino) -> VfsResult<Vec<(DirEntry, Attr)>> {
        VirtualFs::readdir_plus(self, ino)
    }
    fn open(&self, ino: Ino, write: bool) -> VfsResult<Attr> {
        VirtualFs::open(self, ino, write)
    }
    fn release(&self, ino: Ino) {
        VirtualFs::release(self, ino)
    }
    fn getxattr(&self, ino: Ino, name: &str) -> VfsResult<Vec<u8>> {
        VirtualFs::getxattr(self, ino, name)
    }
    fn setxattr(
        &self,
        ino: Ino,
        name: &str,
        value: &[u8],
        create_only: bool,
        replace_only: bool,
    ) -> VfsResult<()> {
        VirtualFs::setxattr(self, ino, name, value, create_only, replace_only)
    }
    fn listxattr(&self, ino: Ino) -> VfsResult<Vec<String>> {
        VirtualFs::listxattr(self, ino)
    }
    fn removexattr(&self, ino: Ino, name: &str) -> VfsResult<()> {
        VirtualFs::removexattr(self, ino, name)
    }
    fn usage(&self) -> (u64, u64, u64) {
        VirtualFs::usage(self)
    }
    fn set_listener(&self, listener: Option<Listener>) {
        VirtualFs::set_listener(self, listener)
    }
    fn resolve(&self, path: &str) -> VfsResult<Ino> {
        VirtualFs::resolve(self, path)
    }
    fn resolve_parent<'a>(&self, path: &'a str) -> VfsResult<(Ino, &'a str)> {
        VirtualFs::resolve_parent(self, path)
    }
    fn path_of(&self, ino: Ino) -> VfsResult<String> {
        VirtualFs::path_of(self, ino)
    }
}

//! The WinFsp driver (Windows): file system requests → a [`Backend`] (the
//! app's [`VirtualFs`](super::store::VirtualFs), or a chat's overlay).
//!
//! WinFsp hands over full paths (`\dir\file`) and Windows semantics: names
//! match regardless of case, deletion is "mark on open, delete on cleanup",
//! and every file needs a security descriptor. The volume is mounted on a
//! folder (not a drive letter), so commands reach it like any other path.
//!
//! winfsp.dll is delay-loaded: the app starts without WinFsp installed, and
//! `driver::load_winfsp` loads the installed DLL before anything here runs.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, U16CStr};
use winfsp_sys::{FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES};

use super::backend::Backend;
use super::store::{Attr, Kind, RenameMode, SetAttr, VfsError, ROOT};

const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_OBJECT_PATH_NOT_FOUND: u32 = 0xC000_003A;
const STATUS_OBJECT_NAME_COLLISION: u32 = 0xC000_0035;
const STATUS_OBJECT_NAME_INVALID: u32 = 0xC000_0033;
const STATUS_NOT_A_DIRECTORY: u32 = 0xC000_0103;
const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;
const STATUS_DIRECTORY_NOT_EMPTY: u32 = 0xC000_0101;
const STATUS_DISK_FULL: u32 = 0xC000_007F;
const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;
const STATUS_NAME_TOO_LONG: u32 = 0xC000_0106;
const STATUS_END_OF_FILE: u32 = 0xC000_0011;
const STATUS_BUFFER_OVERFLOW: u32 = 0x8000_0005;
const STATUS_REPARSE_POINT_NOT_RESOLVED: u32 = 0xC000_0280;
const STATUS_EA_LIST_INCONSISTENT: u32 = 0x8000_0014;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
const FILE_DIRECTORY_FILE: u32 = 0x1;
/// FILE_WRITE_DATA | FILE_APPEND_DATA | GENERIC_WRITE | GENERIC_ALL
const WRITE_ACCESS: u32 = 0x2 | 0x4 | 0x4000_0000 | 0x1000_0000;
const CLEANUP_DELETE: u32 = 0x01;
const CLEANUP_SET_ARCHIVE_BIT: u32 = 0x10;
const CLEANUP_SET_LAST_ACCESS_TIME: u32 = 0x20;
const CLEANUP_SET_LAST_WRITE_TIME: u32 = 0x40;

const ALLOCATION_UNIT: u64 = 4096;
/// 100ns intervals between 1601-01-01 (FILETIME) and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

fn status(code: u32) -> FspError {
    FspError::NTSTATUS(code as i32)
}

fn nt(e: VfsError) -> FspError {
    status(match e {
        VfsError::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        VfsError::Exists => STATUS_OBJECT_NAME_COLLISION,
        VfsError::NotDir => STATUS_NOT_A_DIRECTORY,
        VfsError::IsDir => STATUS_FILE_IS_A_DIRECTORY,
        VfsError::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        VfsError::Invalid => STATUS_OBJECT_NAME_INVALID,
        VfsError::NameTooLong => STATUS_NAME_TOO_LONG,
        VfsError::NoSpace => STATUS_DISK_FULL,
        VfsError::NotPermitted => STATUS_ACCESS_DENIED,
        VfsError::Loop => STATUS_REPARSE_POINT_NOT_RESOLVED,
        VfsError::NoAttr | VfsError::Range => STATUS_EA_LIST_INCONSISTENT,
    })
}

fn filetime(t: SystemTime) -> u64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => FILETIME_UNIX_EPOCH + (d.as_nanos() / 100) as u64,
        Err(_) => 0,
    }
}

fn from_filetime(ft: u64) -> SystemTime {
    if ft >= FILETIME_UNIX_EPOCH {
        UNIX_EPOCH + Duration::from_nanos((ft - FILETIME_UNIX_EPOCH) * 100)
    } else {
        UNIX_EPOCH
    }
}

fn attributes(attr: &Attr) -> u32 {
    let extra = attr.attributes & !FILE_ATTRIBUTE_DIRECTORY & !FILE_ATTRIBUTE_NORMAL;
    match attr.kind {
        Kind::Dir => FILE_ATTRIBUTE_DIRECTORY | extra,
        _ if extra == 0 => FILE_ATTRIBUTE_NORMAL,
        _ => extra,
    }
}

fn fill(attr: &Attr, info: &mut FileInfo) {
    info.file_attributes = attributes(attr);
    info.reparse_tag = 0;
    info.file_size = attr.size;
    info.allocation_size = attr.size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT;
    info.creation_time = filetime(attr.crtime);
    info.last_access_time = filetime(attr.atime);
    info.last_write_time = filetime(attr.mtime);
    info.change_time = filetime(attr.ctime);
    info.index_number = attr.ino;
    info.hard_links = 0;
    info.ea_size = 0;
}

fn path(name: &U16CStr) -> String {
    name.to_string_lossy()
}

/// Full access for SYSTEM, Administrators and Everyone: the volume is the
/// user's own scratch space, and permissions on it aren't persisted anyway.
fn default_security_descriptor() -> Result<Vec<u8>, String> {
    use windows::core::w;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{GetSecurityDescriptorLength, PSECURITY_DESCRIPTOR};
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)"),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(|e| format!("security descriptor: {e}"))?;
        let len = GetSecurityDescriptorLength(sd) as usize;
        let bytes = std::slice::from_raw_parts(sd.0 as *const u8, len).to_vec();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        Ok(bytes)
    }
}

pub struct WinFile {
    ino: u64,
    is_dir: bool,
    dir_buffer: DirBuffer,
    /// Set by `set_delete`; the delete itself happens at cleanup.
    delete_pending: AtomicBool,
}

pub struct WinFs {
    vfs: Arc<dyn Backend>,
    security: Vec<u8>,
}

impl WinFs {
    fn write_security(&self, buffer: Option<&mut [c_void]>) -> winfsp::Result<u64> {
        if let Some(buffer) = buffer {
            if buffer.len() < self.security.len() {
                return Err(status(STATUS_BUFFER_OVERFLOW));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.security.as_ptr(),
                    buffer.as_mut_ptr().cast::<u8>(),
                    self.security.len(),
                );
            }
        }
        Ok(self.security.len() as u64)
    }

    fn resolve(&self, name: &U16CStr) -> winfsp::Result<u64> {
        let p = path(name);
        self.vfs.resolve(&p).map_err(|e| {
            if e == VfsError::NotFound {
                // Distinguish a missing file from a missing folder on the way.
                let parent_missing = self.vfs.resolve_parent(&p).is_err();
                status(if parent_missing {
                    STATUS_OBJECT_PATH_NOT_FOUND
                } else {
                    STATUS_OBJECT_NAME_NOT_FOUND
                })
            } else {
                nt(e)
            }
        })
    }

    fn open_context(
        &self,
        ino: u64,
        write: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<WinFile> {
        let attr = self.vfs.open(ino, write).map_err(nt)?;
        fill(&attr, file_info.as_mut());
        // On a case-insensitive volume, report the name as stored.
        if let Ok(stored) = self.vfs.path_of(ino) {
            let wide: Vec<u16> = std::ffi::OsStr::new(&format!("\\{}", stored.replace('/', "\\")))
                .encode_wide()
                .collect();
            if wide.len() * 2 < file_info.normalized_name_size() as usize {
                file_info.set_normalized_name(&wide, None);
            }
        }
        Ok(WinFile {
            ino,
            is_dir: attr.kind == Kind::Dir,
            dir_buffer: DirBuffer::new(),
            delete_pending: AtomicBool::new(false),
        })
    }
}

impl FileSystemContext for WinFs {
    type FileContext = WinFile;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let ino = self.resolve(file_name)?;
        let attr = self.vfs.getattr(ino).map_err(nt)?;
        let size = self.write_security(security_descriptor)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: size,
            attributes: attributes(&attr),
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        granted_access: FILE_ACCESS_RIGHTS,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let ino = self.resolve(file_name)?;
        self.open_context(ino, granted_access & WRITE_ACCESS != 0, file_info)
    }

    fn close(&self, context: Self::FileContext) {
        self.vfs.release(context.ino);
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let p = path(file_name);
        let (parent, name) = self.vfs.resolve_parent(&p).map_err(|e| match e {
            VfsError::NotFound => status(STATUS_OBJECT_PATH_NOT_FOUND),
            e => nt(e),
        })?;
        let kind = if create_options & FILE_DIRECTORY_FILE != 0 {
            Kind::Dir
        } else {
            Kind::File
        };
        let perm = if kind == Kind::Dir { 0o755 } else { 0o644 };
        let attr = self
            .vfs
            .create(parent, name, kind, perm, 0, 0, None)
            .map_err(nt)?;
        let mut extra = file_attributes & !FILE_ATTRIBUTE_DIRECTORY & !FILE_ATTRIBUTE_NORMAL;
        if kind == Kind::File {
            extra |= FILE_ATTRIBUTE_ARCHIVE;
        }
        let _ = self.vfs.setattr(
            attr.ino,
            &SetAttr {
                attributes: Some(extra),
                ..Default::default()
            },
        );
        self.open_context(attr.ino, true, file_info)
    }

    fn cleanup(&self, context: &Self::FileContext, file_name: Option<&U16CStr>, flags: u32) {
        if flags & CLEANUP_DELETE != 0 {
            if let Some(name) = file_name {
                let p = path(name);
                if let Ok((parent, leaf)) = self.vfs.resolve_parent(&p) {
                    let _ = if context.is_dir {
                        self.vfs.rmdir(parent, leaf)
                    } else {
                        self.vfs.unlink(parent, leaf)
                    };
                }
            }
            return;
        }
        let now = SystemTime::now();
        let mut set = SetAttr::default();
        if flags & CLEANUP_SET_LAST_ACCESS_TIME != 0 {
            set.atime = Some(now);
        }
        if flags & CLEANUP_SET_LAST_WRITE_TIME != 0 {
            set.mtime = Some(now);
        }
        if flags & CLEANUP_SET_ARCHIVE_BIT != 0 && !context.is_dir {
            if let Ok(attr) = self.vfs.getattr(context.ino) {
                set.attributes = Some(attr.attributes | FILE_ATTRIBUTE_ARCHIVE);
            }
        }
        let _ = self.vfs.setattr(context.ino, &set);
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if let Some(context) = context {
            fill(&self.vfs.getattr(context.ino).map_err(nt)?, file_info);
        }
        Ok(())
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        fill(&self.vfs.getattr(context.ino).map_err(nt)?, file_info);
        Ok(())
    }

    fn get_security(
        &self,
        _context: &Self::FileContext,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        self.write_security(security_descriptor)
    }

    fn set_security(
        &self,
        _context: &Self::FileContext,
        _security_information: u32,
        _modification_descriptor: winfsp::filesystem::ModificationDescriptor,
    ) -> winfsp::Result<()> {
        // Accepted and ignored: every file has the same, permissive ACL.
        Ok(())
    }

    fn overwrite(
        &self,
        context: &Self::FileContext,
        file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
        replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let current = self.vfs.getattr(context.ino).map_err(nt)?;
        let requested = file_attributes & !FILE_ATTRIBUTE_NORMAL;
        let attributes = if replace_file_attributes {
            requested
        } else {
            current.attributes | requested
        } | FILE_ATTRIBUTE_ARCHIVE;
        let attr = self
            .vfs
            .setattr(
                context.ino,
                &SetAttr {
                    size: Some(0),
                    attributes: Some(attributes),
                    ..Default::default()
                },
            )
            .map_err(nt)?;
        fill(&attr, file_info);
        Ok(())
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if let Ok(lock) = context.dir_buffer.acquire(marker.is_none(), None) {
            let mut info: DirInfo = DirInfo::new();
            let mut add = |name: &str, attr: &Attr| -> winfsp::Result<()> {
                info.reset();
                let wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
                info.set_name_raw(wide.as_slice())?;
                fill(attr, info.file_info_mut());
                lock.write(&mut info)
            };
            if context.ino != ROOT {
                add(".", &self.vfs.getattr(context.ino).map_err(nt)?)?;
                let parent = self.vfs.parent_of(context.ino).map_err(nt)?;
                add("..", &self.vfs.getattr(parent).map_err(nt)?)?;
            }
            for (entry, attr) in self.vfs.readdir_plus(context.ino).map_err(nt)? {
                add(&entry.name, &attr)?;
            }
        }
        Ok(context.dir_buffer.read(marker, buffer))
    }

    fn rename(
        &self,
        _context: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        let from = path(file_name);
        let to = path(new_file_name);
        let (parent, name) = self.vfs.resolve_parent(&from).map_err(nt)?;
        let (new_parent, new_name) = self.vfs.resolve_parent(&to).map_err(|e| match e {
            VfsError::NotFound => status(STATUS_OBJECT_PATH_NOT_FOUND),
            e => nt(e),
        })?;
        // Windows never replaces a directory by rename.
        if let Ok(target) = self.vfs.lookup(new_parent, new_name) {
            let same = self
                .vfs
                .lookup(parent, name)
                .is_ok_and(|src| src.ino == target.ino);
            if !same && target.kind == Kind::Dir {
                return Err(status(STATUS_ACCESS_DENIED));
            }
        }
        let mode = if replace_if_exists {
            RenameMode::Replace
        } else {
            RenameMode::NoReplace
        };
        self.vfs
            .rename(parent, name, new_parent, new_name, mode)
            .map_err(nt)
    }

    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        file_attributes: u32,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let mut set = SetAttr::default();
        if file_attributes != INVALID_FILE_ATTRIBUTES {
            set.attributes =
                Some(file_attributes & !FILE_ATTRIBUTE_DIRECTORY & !FILE_ATTRIBUTE_NORMAL);
        }
        if creation_time != 0 {
            set.crtime = Some(from_filetime(creation_time));
        }
        if last_access_time != 0 {
            set.atime = Some(from_filetime(last_access_time));
        }
        if last_write_time != 0 {
            set.mtime = Some(from_filetime(last_write_time));
        }
        fill(&self.vfs.setattr(context.ino, &set).map_err(nt)?, file_info);
        Ok(())
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        if delete_file && context.is_dir && !self.vfs.dir_is_empty(context.ino).map_err(nt)? {
            return Err(status(STATUS_DIRECTORY_NOT_EMPTY));
        }
        context.delete_pending.store(delete_file, Ordering::Relaxed);
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let current = self.vfs.getattr(context.ino).map_err(nt)?;
        // Allocation size isn't tracked; shrinking it below the file size
        // truncates, as on NTFS.
        let size = if set_allocation_size {
            current.size.min(new_size)
        } else {
            new_size
        };
        let attr = if size != current.size {
            self.vfs
                .setattr(
                    context.ino,
                    &SetAttr {
                        size: Some(size),
                        ..Default::default()
                    },
                )
                .map_err(nt)?
        } else {
            current
        };
        fill(&attr, file_info);
        Ok(())
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        let data = self
            .vfs
            .read(context.ino, offset, buffer.len())
            .map_err(nt)?;
        if data.is_empty() && !buffer.is_empty() {
            return Err(status(STATUS_END_OF_FILE));
        }
        buffer[..data.len()].copy_from_slice(&data);
        Ok(data.len() as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        let size = self.vfs.getattr(context.ino).map_err(nt)?.size;
        let offset = if write_to_eof { size } else { offset };
        let mut data = buffer;
        if constrained_io {
            // Paging I/O never extends the file.
            if offset >= size {
                fill(&self.vfs.getattr(context.ino).map_err(nt)?, file_info);
                return Ok(0);
            }
            data = &buffer[..buffer.len().min((size - offset) as usize)];
        }
        let written = self.vfs.write(context.ino, offset, data).map_err(nt)?;
        fill(&self.vfs.getattr(context.ino).map_err(nt)?, file_info);
        Ok(written as u32)
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let (capacity, used, _) = self.vfs.usage();
        out_volume_info.total_size = capacity;
        out_volume_info.free_size = capacity.saturating_sub(used);
        out_volume_info.set_volume_label("Rovik");
        Ok(())
    }

    fn set_volume_label(
        &self,
        _volume_label: &U16CStr,
        volume_info: &mut VolumeInfo,
    ) -> winfsp::Result<()> {
        self.get_volume_info(volume_info)
    }
}

/// A mounted volume. Dropping it unmounts.
pub struct WinFspMount {
    host: Option<FileSystemHost<WinFs>>,
}

impl WinFspMount {
    pub fn unmount(mut self) -> Result<(), String> {
        self.stop();
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(mut host) = self.host.take() {
            host.stop();
            host.unmount();
        }
    }
}

impl Drop for WinFspMount {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Mounts on `mountpoint`, a folder that must not exist yet (WinFsp creates
/// it as a reparse point) inside one that does.
pub fn mount(vfs: Arc<dyn Backend>, mountpoint: &Path) -> Result<WinFspMount, String> {
    super::driver::load_winfsp()?;
    if let Some(parent) = mountpoint.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // A leftover empty folder from an earlier run would block the mount.
    let _ = std::fs::remove_dir(mountpoint);

    let mut params = VolumeParams::new();
    params
        .sector_size(ALLOCATION_UNIT as u16)
        .sectors_per_allocation_unit(1)
        .max_component_length(super::store::MAX_NAME as u16)
        .volume_creation_time(filetime(SystemTime::now()))
        .volume_serial_number((filetime(SystemTime::now()) / 10_000_000) as u32)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(true)
        .post_cleanup_when_modified_only(true)
        .flush_and_purge_on_cleanup(true)
        .filesystem_name("rovik-vfs");

    let fs = WinFs {
        vfs,
        security: default_security_descriptor()?,
    };
    let mut host: FileSystemHost<WinFs> =
        FileSystemHost::new(params, fs).map_err(|e| format!("starting WinFsp: {e:?}"))?;
    host.mount(mountpoint.as_os_str())
        .map_err(|e| format!("mounting {}: {e:?}", mountpoint.display()))?;
    if let Err(e) = host.start() {
        host.unmount();
        return Err(format!("starting WinFsp: {e:?}"));
    }
    Ok(WinFspMount { host: Some(host) })
}

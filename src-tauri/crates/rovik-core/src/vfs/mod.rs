//! Virtual files: files that live in the app's memory but are real files to
//! every other program.
//!
//! The app writes a file into [`VirtualFs`]; the volume is mounted through
//! the OS's user-space filesystem driver (FUSE on Linux, macFUSE on macOS),
//! so a command the AI agent runs can `cat`, `grep`,
//! edit, `chmod +x` and execute it through ordinary system calls. Changes
//! those commands make are visible to the app the same way.
//!
//! The driver is optional software: `vfs_status` says whether it's there and
//! `vfs_install_driver` installs it (see `driver.rs`). The volume is mounted
//! on first use and unmounted when the app exits.
//!
//! Rust code uses [`VfsState`] directly:
//!
//! ```ignore
//! let vfs = app.state::<VfsState>();
//! let path = vfs.create_file("plans/step-1.md", b"...", false)?; // a real path
//!
//! // Callbacks for what programs (and the app) do to the files:
//! let id = vfs.subscribe(EventMask::CLOSED, |e| {
//!     if let Op::Closed { modified: true } = e.op { /* e.path was written */ }
//! });
//! vfs.unsubscribe(id);
//! ```
//!
//! See `events.rs` for the event kinds and what a callback may do.

pub mod backend;
pub mod driver;
pub mod events;
pub mod history;
pub mod problem;
pub mod store;

#[cfg(unix)]
mod fuse;
#[cfg(target_os = "macos")]
mod macfuse;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use serde::Serialize;

#[allow(unused_imports)] // re-exported for app code
pub use events::{EventBatch, EventMask, FsEvent, Op, Source, SubscriptionId};

use history::{HistoryLimits, RevisionInfo};
use problem::{MountProblem, SettingsPane};
use store::{Kind, Options, VfsError, VirtualFs};

/// Total bytes the virtual files may hold.
const CAPACITY: u64 = 2 << 30;
/// Batched subscriptions: events waiting at most, per batch, and how long a
/// batch collects.
const EVENT_QUEUE: usize = 16 * 1024;
const EVENT_BATCH: usize = 1024;
const EVENT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// A mounted volume; unmounted when dropped (or by `unmount`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type Mount = fuse::FuseMount;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub struct Mount;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Mount {
    pub fn unmount(self) -> Result<(), String> {
        Ok(())
    }
}

/// Mounts `fs` at `requested`, or the platform's default place; returns
/// where. The virtual-files volume and chats' overlays both mount this way.
/// Tests: whether a FUSE mount works here (the driver, the device and the
/// right to mount), tried once. Tests that need one are skipped without it.
#[cfg(any(test, feature = "testing"))]
pub fn mount_works() -> bool {
    static WORKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WORKS.get_or_init(|| {
        #[cfg(not(unix))]
        return false;
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir().join(format!("mount-probe-{}", std::process::id()));
            let (base, store, at) = (dir.join("base"), dir.join("store"), dir.join("at"));
            for d in [&base, &store] {
                let _ = std::fs::create_dir_all(d);
            }
            let works = crate::overlay::fs::OverlayFs::open(&base, &store, 1 << 20)
                .ok()
                .and_then(|fs| platform_mount(Arc::new(fs), Some(&at)).ok())
                .map(|(mount, _)| {
                    let _ = mount.unmount();
                })
                .is_some();
            let _ = std::fs::remove_dir_all(&dir);
            if !works {
                eprintln!("FUSE mounts don't work here: the tests that need one are skipped");
            }
            works
        }
    })
}

pub fn platform_mount(
    fs: Arc<dyn backend::Backend>,
    requested: Option<&Path>,
) -> Result<(Mount, PathBuf), MountProblem> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        fuse::mount(fs, requested)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (fs, requested);
        Err(MountProblem::failed(driver::UNSUPPORTED))
    }
}

/// Whether a mount here can only go directly under `/Volumes`: macOS with
/// macFUSE's FSKit backend ([`problem::fskit_mountpoint_ok`]). False
/// everywhere else.
pub fn mounts_only_under_volumes() -> bool {
    #[cfg(target_os = "macos")]
    {
        macfuse::plan(None).backend == problem::Backend::FsKit
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Where the volume is mounted unless told otherwise: a per-user folder
/// without spaces, so paths can be pasted into commands unquoted. Each of
/// Rovik's processes has its own (`vfs`, `vfs-2`…: os_lock.rs's slots), or
/// one would mount over another's.
pub fn default_mountpoint() -> PathBuf {
    let vfs = crate::os_lock::slotted("vfs");
    #[cfg(target_os = "linux")]
    {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_dir()) {
            Some(dir) => PathBuf::from(dir).join("rovik-ide").join(vfs),
            None => std::env::temp_dir()
                .join(format!("rovik-ide-{}", unsafe { libc::getuid() }))
                .join(vfs),
        }
    }
    #[cfg(target_os = "macos")]
    {
        // /Volumes/RovikVirtualFiles with FSKit, else under $TMPDIR (per user).
        let _ = vfs;
        macfuse::plan(None).mountpoint
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Windows: %TEMP% is per user.
        std::env::temp_dir().join(format!("rovik-ide-{vfs}"))
    }
}

/// `path` canonicalized: the one spelling the agent's tools compare paths
/// by.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize()
}

struct Mounted {
    at: PathBuf,
    mount: Mount,
}

/// The virtual volume and its mount. Cheap to clone: the clones share it.
#[derive(Clone, Default)]
pub struct VfsState(Arc<Volume>);

impl std::ops::Deref for VfsState {
    type Target = Volume;

    fn deref(&self) -> &Volume {
        &self.0
    }
}

pub struct Volume {
    fs: Arc<VirtualFs>,
    mounted: Mutex<Option<Mounted>>,
    /// Held while a mount is in progress.
    mounting: Mutex<()>,
    /// Why the last mount attempt failed, until one succeeds.
    problem: Mutex<Option<MountProblem>>,
    /// Tests: where `mountpoint` says the volume is, without a mount
    /// (`pretend_mounted`), for code that only needs to know where its
    /// files would be (CLM requests read and write them in memory).
    #[cfg(test)]
    pretend: Mutex<Option<PathBuf>>,
}

impl Default for Volume {
    fn default() -> Self {
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0, 0);
        Volume {
            fs: VirtualFs::new(Options {
                capacity: CAPACITY,
                uid,
                gid,
                ..Options::default()
            }),
            mounted: Mutex::new(None),
            mounting: Mutex::new(()),
            problem: Mutex::new(None),
            #[cfg(test)]
            pretend: Mutex::new(None),
        }
    }
}

/// Paths inside the volume are relative; a leading separator is allowed,
/// `..` that would leave the volume isn't.
fn clean(path: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts
                    .pop()
                    .ok_or_else(|| format!("{path}: outside the virtual volume"))?;
            }
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

fn err(path: &str, e: VfsError) -> String {
    format!("{path}: {e}")
}

impl VfsState {
    fn lock(&self) -> MutexGuard<'_, Option<Mounted>> {
        self.mounted.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The in-memory filesystem, mounted or not.
    #[allow(dead_code)] // for app code that needs more than files
    pub fn fs(&self) -> &Arc<VirtualFs> {
        &self.fs
    }

    pub fn mountpoint(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(at) = self
            .pretend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Some(at);
        }
        self.lock().as_ref().map(|m| m.at.clone())
    }

    /// Tests: `mountpoint` says `at` from now on, though nothing is mounted.
    #[cfg(test)]
    pub fn pretend_mounted(&self, at: PathBuf) {
        *self.pretend.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Mounts the volume (at the default place unless `at` is given) and
    /// returns where. Already mounted: returns the current place.
    pub fn mount(&self, at: Option<PathBuf>) -> Result<PathBuf, String> {
        self.try_mount(at).map_err(|p| p.message())
    }

    /// Like `mount`, with what went wrong and how to fix it. The problem is
    /// also kept for `vfs_status` until a mount succeeds.
    pub fn try_mount(&self, at: Option<PathBuf>) -> Result<PathBuf, MountProblem> {
        let result = self.mount_inner(at);
        *self.problem.lock().unwrap_or_else(|e| e.into_inner()) = result.as_ref().err().cloned();
        result
    }

    fn mount_inner(&self, at: Option<PathBuf>) -> Result<PathBuf, MountProblem> {
        // One mount at a time, without holding `mounted` while it runs (it
        // can wait on macFUSE for a while; status reads mustn't).
        let _one_at_a_time = self.mounting.lock().unwrap_or_else(|e| e.into_inner());
        let previous = {
            let mut mounted = self.lock();
            match mounted.as_ref() {
                Some(m) if at.as_ref().is_none_or(|a| a == &m.at) => return Ok(m.at.clone()),
                Some(_) => mounted.take(),
                None => None,
            }
        };
        if let Some(previous) = previous {
            previous.mount.unmount().map_err(MountProblem::failed)?;
        }
        let status = driver::status();
        if !status.installed {
            return Err(MountProblem::not_installed(status.driver, status.detail));
        }
        let (mount, at) = platform_mount(self.fs.clone(), at.as_deref())?;
        *self.lock() = Some(Mounted {
            at: at.clone(),
            mount,
        });
        Ok(at)
    }

    pub fn problem(&self) -> Option<MountProblem> {
        self.problem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn unmount(&self) -> Result<(), String> {
        match self.lock().take() {
            Some(m) => m.mount.unmount(),
            None => Ok(()),
        }
    }

    /// The real path of a file in the volume, mounting it if needed.
    pub fn real_path(&self, path: &str) -> Result<PathBuf, String> {
        let at = self.mount(None)?;
        let rel = clean(path)?;
        Ok(if rel.is_empty() {
            at
        } else {
            rel.split('/').fold(at, |p, part| p.join(part))
        })
    }

    /// Creates or replaces a virtual file (and its folders) and returns the
    /// real path programs can open it by.
    pub fn create_file(
        &self,
        path: &str,
        data: &[u8],
        executable: bool,
    ) -> Result<PathBuf, String> {
        let rel = clean(path)?;
        if rel.is_empty() {
            return Err("a file path is needed".into());
        }
        let real = self.real_path(&rel)?;
        let perm = if executable { Some(0o755) } else { None };
        self.fs
            .write_file(&rel, data, perm)
            .map_err(|e| err(&rel, e))?;
        Ok(real)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let rel = clean(path)?;
        self.fs.read_file(&rel).map_err(|e| err(&rel, e))
    }

    /// Calls `callback` for every event whose kind is in `mask`. It runs on
    /// the thread serving the file operation: keep it quick, or use
    /// [`events::batched`] / [`VfsState::subscribe_batched`].
    #[allow(dead_code)] // for app code
    pub fn subscribe(
        &self,
        mask: EventMask,
        callback: impl Fn(&FsEvent) + Send + Sync + 'static,
    ) -> SubscriptionId {
        self.fs.subscribe(mask, callback)
    }

    /// Like `subscribe`, but `sink` gets batches on a background thread, so
    /// file operations never wait for it. Events beyond the queue's room are
    /// dropped and counted in the next batch.
    pub fn subscribe_batched(
        &self,
        mask: EventMask,
        sink: impl FnMut(EventBatch) + Send + 'static,
    ) -> SubscriptionId {
        let callback = events::batched(EVENT_QUEUE, EVENT_BATCH, EVENT_INTERVAL, sink);
        self.fs.subscribe_shared(mask, callback)
    }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.fs.unsubscribe(id)
    }

    /// Revisions of the file at `path` (or of the deleted file that was
    /// there), oldest first.
    pub fn revisions(&self, path: &str) -> Result<Vec<RevisionInfo>, String> {
        let rel = clean(path)?;
        self.fs.revisions(&rel).map_err(|e| err(&rel, e))
    }

    /// The content of revision `number` of the file at `path`.
    pub fn read_revision(&self, path: &str, number: u64) -> Result<Arc<[u8]>, String> {
        let rel = clean(path)?;
        self.fs
            .read_revision(&rel, number)
            .map_err(|e| err(&rel, e))
    }

    /// Makes revision `number` the file's content again (recreating a
    /// deleted file); returns its real path.
    pub fn restore_revision(&self, path: &str, number: u64) -> Result<PathBuf, String> {
        let rel = clean(path)?;
        let real = self.real_path(&rel)?;
        self.fs
            .restore_revision(&rel, number)
            .map_err(|e| err(&rel, e))?;
        Ok(real)
    }

    /// Replaces the history of the file at `path` with saved revisions
    /// (number, content, time in ms, source), oldest first.
    pub fn import_history(
        &self,
        path: &str,
        revisions: Vec<(u64, Vec<u8>, u64, Source)>,
    ) -> Result<(), String> {
        let rel = clean(path)?;
        self.fs
            .import_history(&rel, revisions)
            .map_err(|e| err(&rel, e))
    }

    /// Writes a file without mounting (the volume may be mounted or not).
    pub fn write_unmounted(&self, path: &str, data: &[u8]) -> Result<(), String> {
        let rel = clean(path)?;
        self.fs
            .write_file(&rel, data, None)
            .map(|_| ())
            .map_err(|e| err(&rel, e))
    }

    /// A folder's entries: (name, is a folder, size in bytes), by name.
    pub fn list(&self, path: &str) -> Result<Vec<(String, bool, u64)>, String> {
        let rel = clean(path)?;
        let mut entries: Vec<(String, bool, u64)> = self
            .fs
            .list(&rel)
            .map_err(|e| err(&rel, e))?
            .into_iter()
            .map(|(entry, attr)| (entry.name, entry.kind == Kind::Dir, attr.size))
            .collect();
        entries.sort();
        Ok(entries)
    }

    pub fn remove(&self, path: &str) -> Result<(), String> {
        let rel = clean(path)?;
        if rel.is_empty() {
            return Err("can't remove the volume's root".into());
        }
        self.fs.remove(&rel).map_err(|e| err(&rel, e))
    }
}

// --- Commands ------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsStatus {
    pub driver: driver::DriverStatus,
    /// Where the volume is mounted, if it is.
    pub mountpoint: Option<String>,
    pub default_mountpoint: String,
    /// Why the last mount failed and what to do (until one succeeds).
    pub problem: Option<MountProblem>,
    pub capacity: u64,
    pub used: u64,
    pub history: HistoryStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStatus {
    /// Revisions held, over all files.
    pub revisions: usize,
    pub bytes: u64,
    pub limits: HistoryLimits,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsFile {
    /// Path inside the volume.
    pub path: String,
    /// Path programs open it by.
    pub real_path: String,
    pub size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfsEntry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
}

pub async fn vfs_status(state: &VfsState) -> Result<VfsStatus, String> {
    let driver = crate::rt::spawn_blocking(driver::status)
        .await
        .map_err(|e| e.to_string())?;
    let (capacity, used, _) = state.fs.usage();
    let (revisions, history_bytes) = state.fs.history_usage();
    Ok(VfsStatus {
        driver,
        mountpoint: state.mountpoint().map(|p| p.to_string_lossy().into_owned()),
        default_mountpoint: crate::rt::spawn_blocking(default_mountpoint)
            .await
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
        problem: state.problem(),
        capacity,
        used,
        history: HistoryStatus {
            revisions,
            bytes: history_bytes,
            limits: state.fs.history_limits(),
        },
    })
}

pub async fn vfs_install_driver() -> Result<driver::InstallOutcome, String> {
    driver::install().await
}

pub async fn vfs_mount(state: &VfsState, mountpoint: Option<String>) -> Result<String, String> {
    let at = mountpoint
        .filter(|m| !m.trim().is_empty())
        .map(PathBuf::from);
    // Can wait on macFUSE for a while; not on the async runtime.
    let state = state.clone();
    crate::rt::spawn_blocking(move || state.mount(at).map(|p| p.to_string_lossy().into_owned()))
        .await
        .map_err(|e| e.to_string())?
}

/// Opens the System Settings page a mount problem points to (macOS).
pub async fn vfs_open_settings(pane: SettingsPane) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(pane.url())
            .status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("only on macOS".into())
    }
}

pub async fn vfs_unmount(state: &VfsState) -> Result<(), String> {
    state.unmount()
}

/// Writes a virtual file. `encoding: "base64"` for binary content.
pub async fn vfs_write_file(
    state: &VfsState,
    path: String,
    content: String,
    encoding: Option<String>,
    executable: Option<bool>,
) -> Result<VfsFile, String> {
    let data = match encoding.as_deref() {
        Some("base64") => base64::engine::general_purpose::STANDARD
            .decode(content.as_bytes())
            .map_err(|e| e.to_string())?,
        None | Some("utf8") | Some("utf-8") => content.into_bytes(),
        Some(other) => return Err(format!("unknown encoding {other}")),
    };
    let size = data.len() as u64;
    let real = state.create_file(&path, &data, executable.unwrap_or(false))?;
    Ok(VfsFile {
        path: clean(&path)?,
        real_path: real.to_string_lossy().into_owned(),
        size,
    })
}

pub async fn vfs_read_file(state: &VfsState, path: String) -> Result<String, String> {
    state
        .read_file(&path)
        .map(|d| String::from_utf8_lossy(&d).into_owned())
}

pub async fn vfs_list(state: &VfsState, path: Option<String>) -> Result<Vec<VfsEntry>, String> {
    let rel = clean(path.as_deref().unwrap_or(""))?;
    let entries = state.fs.list(&rel).map_err(|e| err(&rel, e))?;
    Ok(entries
        .into_iter()
        .map(|(e, attr)| VfsEntry {
            size: attr.size,
            name: e.name,
            kind: e.kind,
        })
        .collect())
}

/// Streams events in batches to `send` until `vfs_unsubscribe`, or until
/// `send` says its receiver is gone. `kinds`: "created", "opened", "read",
/// "written", "truncated", "closed", "removed", "renamed", "changes" (the
/// default: everything but the per-request reads/writes and opens) or "all".
pub async fn vfs_subscribe(
    state: &VfsState,
    kinds: Option<Vec<String>>,
    send: impl Fn(EventBatch) -> bool + Send + 'static,
) -> Result<u64, String> {
    let mask = match kinds {
        Some(kinds) if !kinds.is_empty() => EventMask::from_names(&kinds)?,
        _ => EventMask::CHANGES,
    };
    // Unsubscribes itself once the receiver is gone (the page reloaded).
    let fs = Arc::downgrade(&state.fs);
    let me = Arc::new(std::sync::OnceLock::<SubscriptionId>::new());
    let me_in_sink = me.clone();
    let id = state.subscribe_batched(mask, move |batch| {
        if !send(batch) {
            if let (Some(fs), Some(id)) = (fs.upgrade(), me_in_sink.get()) {
                fs.unsubscribe(*id);
            }
        }
    });
    let _ = me.set(id);
    Ok(id.0)
}

pub async fn vfs_unsubscribe(state: &VfsState, id: u64) -> Result<bool, String> {
    Ok(state.unsubscribe(SubscriptionId(id)))
}

/// A file's revisions, oldest first.
pub async fn vfs_revisions(state: &VfsState, path: String) -> Result<Vec<RevisionInfo>, String> {
    state.revisions(&path)
}

/// A revision's content: text (lossy UTF-8), or `encoding: "base64"`.
pub async fn vfs_read_revision(
    state: &VfsState,
    path: String,
    revision: u64,
    encoding: Option<String>,
) -> Result<String, String> {
    let data = state.read_revision(&path, revision)?;
    match encoding.as_deref() {
        Some("base64") => Ok(base64::engine::general_purpose::STANDARD.encode(&data)),
        None | Some("utf8") | Some("utf-8") => Ok(String::from_utf8_lossy(&data).into_owned()),
        Some(other) => Err(format!("unknown encoding {other}")),
    }
}

/// Makes a revision the file's content again (itself a new revision).
pub async fn vfs_restore_revision(
    state: &VfsState,
    path: String,
    revision: u64,
) -> Result<VfsFile, String> {
    let real = state.restore_revision(&path, revision)?;
    let size = state
        .fs
        .metadata(&clean(&path)?)
        .map(|a| a.size)
        .unwrap_or(0);
    Ok(VfsFile {
        path: clean(&path)?,
        real_path: real.to_string_lossy().into_owned(),
        size,
    })
}

/// Changes how much history is kept (`maxBytes: 0` turns it off).
pub async fn vfs_set_history_limits(state: &VfsState, limits: HistoryLimits) -> Result<(), String> {
    state.fs.set_history_limits(limits);
    Ok(())
}

pub async fn vfs_remove(state: &VfsState, path: String) -> Result<(), String> {
    state.remove(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_keeps_paths_inside_the_volume() {
        assert_eq!(clean("/a//b/./c").unwrap(), "a/b/c");
        assert_eq!(clean("a\\b\\..\\c").unwrap(), "a/c");
        assert_eq!(clean("").unwrap(), "");
        assert!(clean("../etc/passwd").is_err());
        assert!(clean("a/../../b").is_err());
    }
}

/// Mounts for real and drives the volume with ordinary tools. Needs FUSE
/// (`/dev/fuse` and `fusermount3`): skipped where mounts don't work.
#[cfg(all(test, target_os = "linux"))]
mod mount_tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::process::Command;

    fn sh(dir: &Path, script: &str) -> (bool, String) {
        let out = Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(dir)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }

    /// A volume a program still uses (a dev server left running in a
    /// chat's view) doesn't keep its unmount waiting: past the wait the
    /// connection is cut, or the unmount says it's still in use.
    #[cfg(target_os = "linux")]
    #[test]
    fn unmounting_a_volume_in_use_doesnt_wait_for_its_user() {
        if !crate::vfs::mount_works() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("busy-mount-{}", std::process::id()));
        let (base, store, at) = (dir.join("base"), dir.join("store"), dir.join("at"));
        for d in [&base, &store] {
            std::fs::create_dir_all(d).unwrap();
        }
        let fs = crate::overlay::fs::OverlayFs::open(&base, &store, 1 << 20).unwrap();
        let (mount, view) = platform_mount(Arc::new(fs), Some(&at)).unwrap();
        let mut user = Command::new("sleep")
            .arg("60")
            .current_dir(&view)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let result = mount.unmount();
        let took = started.elapsed();
        let _ = user.kill();
        let _ = user.wait();
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
        if let Err(e) = result {
            assert!(e.contains("still in use"), "{e}");
        }
        // Detached at least: the folder no longer shows the volume.
        let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap();
        assert!(!mounts.contains(&*view.to_string_lossy()), "{mounts}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn common_tools_work_on_virtual_files() {
        if !crate::vfs::mount_works() {
            return;
        }
        let state = VfsState::default();
        let at = std::env::temp_dir().join(format!("rovik-vfs-test-{}", std::process::id()));
        // A failed attempt is kept, with what went wrong, until one works.
        let failed = state
            .try_mount(Some(PathBuf::from("/proc/rovik-vfs-test")))
            .unwrap_err();
        assert_eq!(failed.kind, problem::ProblemKind::Failed);
        assert!(failed.detail.is_some() && state.problem() == Some(failed));
        state
            .mount(Some(at.clone()))
            .expect("mount (is FUSE installed?)");
        assert_eq!(state.problem(), None);

        // The app creates files; commands see them.
        let path = state
            .create_file("notes/hello.txt", b"hello\nworld\n", false)
            .unwrap();
        assert_eq!(path, at.join("notes/hello.txt"));
        let script = state
            .create_file("bin/run.sh", b"#!/bin/sh\necho ran \"$@\"\n", true)
            .unwrap();
        let (ok, out) = sh(
            &at,
            "cat notes/hello.txt && grep -n world notes/hello.txt && wc -l < notes/hello.txt",
        );
        assert!(ok, "{out}");
        assert_eq!(out, "hello\nworld\n2:world\n2\n");
        let (ok, out) = sh(&at, &format!("{} x", script.display()));
        assert!(ok && out == "ran x\n", "{out}");

        // Commands change files; the app sees it.
        let (ok, out) = sh(
            &at,
            "sed -i 's/world/there/' notes/hello.txt && echo more >> notes/hello.txt \
             && mkdir -p a/b && cp notes/hello.txt a/b/copy && mv a/b/copy a/moved \
             && ln -s ../notes/hello.txt a/link && ln a/moved a/hard && truncate -s 3 a/hard && touch -d 2020-01-01T00:00:00Z a/moved \
             && chmod 600 a/moved && ls -la a && stat -c '%a %s %Y' a/moved \
             && cat a/link && find . -type f | sort && tar cf - notes | tar tf - \
             && dd if=/dev/zero of=big bs=64k count=16 2>/dev/null && sha256sum big | cut -c1-8 \
             && rm -rf a big && ls",
        );
        assert!(ok, "{out}");
        assert!(out.contains("600 3 1577836800"), "{out}");
        assert!(out.contains("hello\nthere\nmore\n"), "{out}");
        assert!(
            out.contains("./a/hard\n./a/moved\n./bin/run.sh\n./notes/hello.txt\n"),
            "{out}"
        );
        assert_eq!(
            state.read_file("notes/hello.txt").unwrap(),
            b"hello\nthere\nmore\n"
        );
        assert!(state.read_file("a").is_err());

        // App-side changes reach processes that cached the old contents.
        let (ok, _) = sh(&at, "cat notes/hello.txt > /dev/null");
        assert!(ok);
        state
            .create_file("notes/hello.txt", b"replaced\n", false)
            .unwrap();
        let (_, out) = sh(&at, "cat notes/hello.txt");
        assert_eq!(out, "replaced\n");
        state.remove("notes").unwrap();
        let (ok, _) = sh(&at, "test ! -e notes");
        assert!(ok);

        // Callbacks see what commands do, with the paths they used.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let id = state.subscribe(EventMask::CHANGES | EventMask::OPENED, move |e| {
            sink.lock()
                .unwrap()
                .push((e.source, e.path.clone(), e.op.clone()));
        });
        let (ok, out) = sh(
            &at,
            "echo hi > ev.txt && cat ev.txt >/dev/null && mv ev.txt ev2.txt && rm ev2.txt",
        );
        assert!(ok, "{out}");
        state.unsubscribe(id);
        let seen = seen.lock().unwrap().clone();
        let has = |path: &str, op: &Op| {
            seen.iter()
                .any(|(s, p, o)| *s == Source::Process && p == path && o == op)
        };
        assert!(has("ev.txt", &Op::Created { kind: Kind::File }), "{seen:?}");
        assert!(has("ev.txt", &Op::Closed { modified: true }), "{seen:?}");
        assert!(has("ev.txt", &Op::Opened { write: false }), "{seen:?}");
        assert!(
            has(
                "ev2.txt",
                &Op::Renamed {
                    from: "ev.txt".into()
                }
            ),
            "{seen:?}"
        );
        assert!(
            has("ev2.txt", &Op::Removed { kind: Kind::File }),
            "{seen:?}"
        );

        // Revisions of what commands did, safe saves (sed -i) included.
        let (ok, out) = sh(&at, "printf 'a\\n' > rev.txt && printf 'b\\n' >> rev.txt && sed -i 's/b/c/' rev.txt && rm rev.txt");
        assert!(ok, "{out}");
        let revs: Vec<String> = state
            .revisions("rev.txt")
            .unwrap()
            .iter()
            .map(|r| {
                String::from_utf8_lossy(&state.read_revision("rev.txt", r.number).unwrap())
                    .into_owned()
            })
            .collect();
        assert_eq!(revs, ["a\n", "a\nb\n", "a\nc\n"]);
        let path = state.restore_revision("rev.txt", 2).unwrap();
        let (_, out) = sh(&at, &format!("cat {}", path.display()));
        assert_eq!(out, "a\nb\n");
        state.remove("rev.txt").unwrap();

        // Many files, read in parallel by several processes.
        for i in 0..2000 {
            state
                .create_file(
                    &format!("many/{}/f{i}.txt", i % 20),
                    format!("{i}\n").as_bytes(),
                    false,
                )
                .unwrap();
        }
        let (ok, out) = sh(&at, "for d in many/*; do (cat $d/* | wc -l > $d.count) & done; wait; awk '{s+=$1} END {print s}' many/*.count");
        assert!(ok, "{out}");
        assert_eq!(out.trim(), "2000");
        state.remove("many").unwrap();

        // git, which exercises locking, renames and many small files.
        if Command::new("git").arg("--version").output().is_ok() {
            let (ok, out) = sh(
                &at,
                "mkdir repo && cd repo && git init -q && git config user.email t@t && git config user.name t \
                 && echo one > f && git add f && git commit -qm one && echo two >> f && git commit -qam two \
                 && git log --oneline | wc -l && git status --porcelain && git gc -q && git fsck --no-progress && echo ok",
            );
            assert!(ok, "{out}");
            assert!(out.ends_with("2\nok\n"), "{out}");
        }

        state.unmount().unwrap();
        assert!(
            std::fs::read_dir(&at).unwrap().next().is_none(),
            "unmounted"
        );
        let _ = std::fs::remove_dir(&at);
    }

    /// fuser's "[Not Implemented]" warnings, from every mount in the tests.
    static UNIMPLEMENTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Unimplemented;

    impl log::Log for Unimplemented {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }

        fn log(&self, record: &log::Record) {
            let message = record.args().to_string();
            if message.contains("[Not Implemented]") {
                UNIMPLEMENTED.lock().unwrap().push(message);
            }
        }

        fn flush(&self) {}
    }

    /// What programs ask of any file, and the volume leaves to the kernel,
    /// gets a disk's answers, without a warning in the log.
    #[test]
    fn requests_left_to_the_kernel_log_nothing() {
        if !crate::vfs::mount_works() {
            return;
        }
        log::set_logger(&Unimplemented).expect("the only logger in the tests");
        log::set_max_level(log::LevelFilter::Warn);
        let state = VfsState::default();
        let at = std::env::temp_dir().join(format!("rovik-vfs-quiet-{}", std::process::id()));
        state.mount(Some(at.clone())).unwrap();
        let path = state.create_file("f.txt", b"hello\n", false).unwrap();
        let file = std::fs::File::open(path).unwrap();
        let copy = std::fs::File::create(at.join("copy.txt")).unwrap();
        let fd = file.as_raw_fd();

        // isatty(), which Python's open() asks: an ioctl.
        let (tty, error) = unsafe { (libc::isatty(fd), std::io::Error::last_os_error()) };
        assert_eq!((tty, error.raw_os_error()), (0, Some(libc::ENOTTY)));
        // An event loop's poll.
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLOUT,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut ready, 1, 0) }, 1);
        assert_eq!(ready.revents, libc::POLLIN | libc::POLLOUT);
        // Looking for holes (tar, cp).
        assert_eq!(unsafe { libc::lseek(fd, 0, libc::SEEK_HOLE) }, 6);
        assert_eq!(unsafe { libc::lseek(fd, 0, libc::SEEK_DATA) }, 0);
        // A copy in the kernel (cp, std::fs::copy).
        let mut from = 0;
        let copied = unsafe {
            libc::copy_file_range(fd, &mut from, copy.as_raw_fd(), std::ptr::null_mut(), 6, 0)
        };
        assert_eq!(copied, 6);
        drop((file, copy));
        assert_eq!(state.read_file("copy.txt").unwrap(), b"hello\n");

        state.unmount().unwrap();
        let _ = std::fs::remove_dir(&at);
        assert_eq!(*UNIMPLEMENTED.lock().unwrap(), Vec::<String>::new());
    }
}

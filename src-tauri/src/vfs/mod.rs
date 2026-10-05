//! Virtual files: files that live in the app's memory but are real files to
//! every other program.
//!
//! The app writes a file into [`VirtualFs`]; the volume is mounted through
//! the OS's user-space filesystem driver (FUSE on Linux, macFUSE on macOS,
//! WinFsp on Windows), so a command the AI agent runs can `cat`, `grep`,
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
//! ```

pub mod driver;
pub mod store;

#[cfg(unix)]
mod fuse;
#[cfg(target_os = "macos")]
mod macfuse;
#[cfg(windows)]
mod winfsp_fs;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use serde::Serialize;
use tauri::State;

use store::{Kind, Options, VfsError, VirtualFs};

/// Total bytes the virtual files may hold.
const CAPACITY: u64 = 2 << 30;

#[cfg(any(target_os = "linux", target_os = "macos"))]
type Mount = fuse::FuseMount;
#[cfg(windows)]
type Mount = winfsp_fs::WinFspMount;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
struct Mount;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Mount {
    fn unmount(self) -> Result<(), String> {
        Ok(())
    }
}

fn platform_mount(fs: Arc<VirtualFs>, at: &Path) -> Result<Mount, String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        fuse::mount(fs, at)
    }
    #[cfg(windows)]
    {
        winfsp_fs::mount(fs, at)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (fs, at);
        Err("virtual files aren't supported on this system".into())
    }
}

/// Where the volume is mounted unless told otherwise: a per-user folder
/// without spaces, so paths can be pasted into commands unquoted.
pub fn default_mountpoint() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_dir()) {
            Some(dir) => PathBuf::from(dir).join("rovik-ide").join("vfs"),
            None => std::env::temp_dir().join(format!("rovik-ide-{}", unsafe { libc::getuid() })).join("vfs"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS: $TMPDIR is per user. Windows: %TEMP% is too.
        std::env::temp_dir().join("rovik-ide-vfs")
    }
}

struct Mounted {
    at: PathBuf,
    mount: Mount,
}

pub struct VfsState {
    fs: Arc<VirtualFs>,
    mounted: Mutex<Option<Mounted>>,
}

impl Default for VfsState {
    fn default() -> Self {
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0, 0);
        VfsState {
            fs: VirtualFs::new(Options { capacity: CAPACITY, uid, gid, ..Options::default() }),
            mounted: Mutex::new(None),
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
                parts.pop().ok_or_else(|| format!("{path}: outside the virtual volume"))?;
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
        self.lock().as_ref().map(|m| m.at.clone())
    }

    /// Mounts the volume (at the default place unless `at` is given) and
    /// returns where. Already mounted: returns the current place.
    pub fn mount(&self, at: Option<PathBuf>) -> Result<PathBuf, String> {
        let mut mounted = self.lock();
        if let Some(m) = mounted.as_ref() {
            if at.as_ref().is_none_or(|a| a == &m.at) {
                return Ok(m.at.clone());
            }
            let previous = mounted.take().expect("checked above");
            previous.mount.unmount()?;
        }
        let status = driver::status();
        if !status.installed {
            return Err(format!(
                "{} {} is needed for virtual files; install it first.",
                status.detail.unwrap_or_default(),
                status.driver
            ));
        }
        let at = at.unwrap_or_else(default_mountpoint);
        let mount = platform_mount(self.fs.clone(), &at)?;
        *mounted = Some(Mounted { at: at.clone(), mount });
        Ok(at)
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
        Ok(if rel.is_empty() { at } else { rel.split('/').fold(at, |p, part| p.join(part)) })
    }

    /// Creates or replaces a virtual file (and its folders) and returns the
    /// real path programs can open it by.
    pub fn create_file(&self, path: &str, data: &[u8], executable: bool) -> Result<PathBuf, String> {
        let rel = clean(path)?;
        if rel.is_empty() {
            return Err("a file path is needed".into());
        }
        let real = self.real_path(&rel)?;
        let perm = if executable { Some(0o755) } else { None };
        self.fs.write_file(&rel, data, perm).map_err(|e| err(&rel, e))?;
        Ok(real)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let rel = clean(path)?;
        self.fs.read_file(&rel).map_err(|e| err(&rel, e))
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
    pub capacity: u64,
    pub used: u64,
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

#[tauri::command]
pub async fn vfs_status(state: State<'_, VfsState>) -> Result<VfsStatus, String> {
    let driver = tauri::async_runtime::spawn_blocking(driver::status).await.map_err(|e| e.to_string())?;
    let (capacity, used, _) = state.fs.usage();
    Ok(VfsStatus {
        driver,
        mountpoint: state.mountpoint().map(|p| p.to_string_lossy().into_owned()),
        default_mountpoint: default_mountpoint().to_string_lossy().into_owned(),
        capacity,
        used,
    })
}

#[tauri::command]
pub async fn vfs_install_driver() -> Result<driver::InstallOutcome, String> {
    driver::install().await
}

#[tauri::command]
pub async fn vfs_mount(state: State<'_, VfsState>, mountpoint: Option<String>) -> Result<String, String> {
    let at = mountpoint.filter(|m| !m.trim().is_empty()).map(PathBuf::from);
    state.mount(at).map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn vfs_unmount(state: State<'_, VfsState>) -> Result<(), String> {
    state.unmount()
}

/// Writes a virtual file. `encoding: "base64"` for binary content.
#[tauri::command]
pub async fn vfs_write_file(
    state: State<'_, VfsState>,
    path: String,
    content: String,
    encoding: Option<String>,
    executable: Option<bool>,
) -> Result<VfsFile, String> {
    let data = match encoding.as_deref() {
        Some("base64") => base64::engine::general_purpose::STANDARD.decode(content.as_bytes()).map_err(|e| e.to_string())?,
        None | Some("utf8") | Some("utf-8") => content.into_bytes(),
        Some(other) => return Err(format!("unknown encoding {other}")),
    };
    let size = data.len() as u64;
    let real = state.create_file(&path, &data, executable.unwrap_or(false))?;
    Ok(VfsFile { path: clean(&path)?, real_path: real.to_string_lossy().into_owned(), size })
}

#[tauri::command]
pub async fn vfs_read_file(state: State<'_, VfsState>, path: String) -> Result<String, String> {
    state.read_file(&path).map(|d| String::from_utf8_lossy(&d).into_owned())
}

#[tauri::command]
pub async fn vfs_list(state: State<'_, VfsState>, path: Option<String>) -> Result<Vec<VfsEntry>, String> {
    let rel = clean(path.as_deref().unwrap_or(""))?;
    let entries = state.fs.list(&rel).map_err(|e| err(&rel, e))?;
    Ok(entries
        .into_iter()
        .map(|e| VfsEntry { size: state.fs.getattr(e.ino).map(|a| a.size).unwrap_or(0), name: e.name, kind: e.kind })
        .collect())
}

#[tauri::command]
pub async fn vfs_remove(state: State<'_, VfsState>, path: String) -> Result<(), String> {
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
/// (`/dev/fuse` and `fusermount3`), so it's opt-in:
/// `cargo test vfs -- --ignored`.
#[cfg(all(test, target_os = "linux"))]
mod mount_tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, script: &str) -> (bool, String) {
        let out = Command::new("sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
    }

    #[test]
    #[ignore]
    fn common_tools_work_on_virtual_files() {
        let state = VfsState::default();
        let at = std::env::temp_dir().join(format!("rovik-vfs-test-{}", std::process::id()));
        state.mount(Some(at.clone())).expect("mount (is FUSE installed?)");

        // The app creates files; commands see them.
        let path = state.create_file("notes/hello.txt", b"hello\nworld\n", false).unwrap();
        assert_eq!(path, at.join("notes/hello.txt"));
        let script = state.create_file("bin/run.sh", b"#!/bin/sh\necho ran \"$@\"\n", true).unwrap();
        let (ok, out) = sh(&at, "cat notes/hello.txt && grep -n world notes/hello.txt && wc -l < notes/hello.txt");
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
        assert!(out.contains("./a/hard\n./a/moved\n./bin/run.sh\n./notes/hello.txt\n"), "{out}");
        assert_eq!(state.read_file("notes/hello.txt").unwrap(), b"hello\nthere\nmore\n");
        assert!(state.read_file("a").is_err());

        // App-side changes reach processes that cached the old contents.
        let (ok, _) = sh(&at, "cat notes/hello.txt > /dev/null");
        assert!(ok);
        state.create_file("notes/hello.txt", b"replaced\n", false).unwrap();
        let (_, out) = sh(&at, "cat notes/hello.txt");
        assert_eq!(out, "replaced\n");
        state.remove("notes").unwrap();
        let (ok, _) = sh(&at, "test ! -e notes");
        assert!(ok);

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
        assert!(std::fs::read_dir(&at).unwrap().next().is_none(), "unmounted");
        let _ = std::fs::remove_dir(&at);
    }
}

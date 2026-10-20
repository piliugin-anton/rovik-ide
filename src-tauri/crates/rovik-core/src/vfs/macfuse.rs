//! Mounting through macFUSE.
//!
//! macFUSE's own library (`libfuse.2.dylib`) does the mounting: since
//! macFUSE 5.3 its internal mount protocol changed, and the FSKit backend
//! mounts through a framework, so imitating `mount_macfuse` directly no
//! longer works. Its public `fuse_mount` / `fuse_chan_fd` / `fuse_unmount`
//! do, with either backend; fuser then serves the descriptor
//! `fuse_chan_fd` returns (as fuser itself does for macFUSE 5.3.3+, see
//! cberner/fuser#752). The library is loaded at run time (`dlopen`), not
//! linked: the app has to start on a Mac without macFUSE, to install it.
//!
//! Backends (see `problem.rs`):
//! - FSKit (macOS 15.4+, macFUSE 5.1+): no kernel extension; macFUSE's file
//!   system extension must be switched on once in System Settings. Mounts
//!   only under /Volumes.
//! - Kernel extension otherwise: must be allowed in Privacy & Security (and
//!   on Apple silicon, enabled in Recovery first).

use std::ffi::{c_char, c_int, c_void, CString};
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use fuser::{BackgroundSession, Config, MountOption, Session};

use super::fuse::FuseFs;
use super::problem::{self, Backend, MountProblem};

pub const BUNDLE: &str = "/Library/Filesystems/macfuse.fs";
const LIBRARIES: [&str; 2] = [
    "/usr/local/lib/libfuse.2.dylib",
    "/usr/local/lib/libfuse.dylib",
];
/// FSKit volumes must live under /Volumes; no spaces (macFUSE #1198).
const FSKIT_MOUNTPOINT: &str = "/Volumes/RovikVirtualFiles";
const VOLUME_NAME: &str = "RovikVirtualFiles";
/// Past this, the mount (or its first request) isn't coming.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);

#[repr(C)]
struct FuseArgs {
    argc: c_int,
    argv: *mut *mut c_char,
    allocated: c_int,
}

type FuseMountFn = unsafe extern "C" fn(*const c_char, *mut FuseArgs) -> *mut c_void;
type FuseChanFdFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type FuseUnmountFn = unsafe extern "C" fn(*const c_char, *mut c_void);

struct Library {
    mount: FuseMountFn,
    chan_fd: FuseChanFdFn,
    unmount: FuseUnmountFn,
}

/// Loaded once it's there (never unloaded); a failed load is retried, since
/// macFUSE may be installed while the app runs.
static LIBRARY: Mutex<Option<&'static Library>> = Mutex::new(None);

fn library() -> Result<&'static Library, String> {
    let mut loaded = LIBRARY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lib) = *loaded {
        return Ok(lib);
    }
    let mut errors = Vec::new();
    for path in LIBRARIES {
        let c_path = CString::new(path).expect("no NUL in a constant");
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            errors.push(format!("{path}: {}", dl_error()));
            continue;
        }
        let symbol = |name: &str| {
            let c_name = CString::new(name).expect("no NUL in a constant");
            let ptr = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
            (!ptr.is_null())
                .then_some(ptr)
                .ok_or_else(|| format!("{path} has no {name}"))
        };
        match (
            symbol("fuse_mount"),
            symbol("fuse_chan_fd"),
            symbol("fuse_unmount"),
        ) {
            (Ok(mount), Ok(chan_fd), Ok(unmount)) => {
                // SAFETY: these are the libfuse 2 functions with these signatures.
                let lib = unsafe {
                    Library {
                        mount: std::mem::transmute::<*mut c_void, FuseMountFn>(mount),
                        chan_fd: std::mem::transmute::<*mut c_void, FuseChanFdFn>(chan_fd),
                        unmount: std::mem::transmute::<*mut c_void, FuseUnmountFn>(unmount),
                    }
                };
                let lib: &'static Library = Box::leak(Box::new(lib));
                *loaded = Some(lib);
                return Ok(lib);
            }
            (a, b, c) => errors.extend([a.err(), b.err(), c.err()].into_iter().flatten()),
        }
    }
    Err(format!(
        "macFUSE's library couldn't be loaded ({})",
        errors.join("; ")
    ))
}

fn dl_error() -> String {
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        "unknown error".into()
    } else {
        unsafe { std::ffi::CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned()
    }
}

fn sysctl_string(name: &str) -> Option<String> {
    let c_name = CString::new(name).ok()?;
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    let rc = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let text = &buf[..len.min(buf.len())];
    Some(
        String::from_utf8_lossy(text)
            .trim_end_matches('\0')
            .trim()
            .to_string(),
    )
}

pub fn macos_version() -> Option<(u32, u32, u32)> {
    problem::parse_version(&sysctl_string("kern.osproductversion")?)
}

/// Apple silicon, also when the app runs under Rosetta.
pub fn apple_silicon() -> bool {
    let c_name = CString::new("hw.optional.arm64").expect("constant");
    let mut value: c_int = 0;
    let mut len = std::mem::size_of::<c_int>();
    let rc = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            (&mut value as *mut c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    rc == 0 && value == 1
}

/// The backend and mount point a mount would use.
pub struct Plan {
    pub backend: Backend,
    pub mountpoint: PathBuf,
}

pub fn plan(requested: Option<&Path>) -> Plan {
    let macfuse = super::driver::macfuse_version()
        .as_deref()
        .and_then(problem::parse_version);
    let backend = problem::choose_backend(macos_version(), macfuse);
    // Each of Rovik's processes its own (vfs/mod.rs `default_mountpoint`).
    let mountpoint = requested
        .map(Path::to_path_buf)
        .unwrap_or_else(|| match backend {
            Backend::FsKit => PathBuf::from(crate::os_lock::slotted(FSKIT_MOUNTPOINT)),
            Backend::Kext => std::env::temp_dir().join(crate::os_lock::slotted("rovik-ide-vfs")),
        });
    Plan {
        backend,
        mountpoint,
    }
}

/// Whether macFUSE's FSKit module is switched on (None: can't tell).
fn fskit_module_enabled() -> Option<bool> {
    let out = Command::new("/usr/bin/pluginkit")
        .args(["-m", "-A", "-p", "com.apple.fskit.fsmodule"])
        .output()
        .ok()?;
    problem::fskit_module_enabled(&String::from_utf8_lossy(&out.stdout))
}

fn option_string(option: &MountOption) -> String {
    match option {
        MountOption::FSName(name) => format!("fsname={name}"),
        MountOption::Subtype(subtype) => format!("subtype={subtype}"),
        MountOption::CUSTOM(value) => value.clone(),
        MountOption::AutoUnmount => "auto_unmount".into(),
        MountOption::DefaultPermissions => "default_permissions".into(),
        MountOption::Dev => "dev".into(),
        MountOption::NoDev => "nodev".into(),
        MountOption::Suid => "suid".into(),
        MountOption::NoSuid => "nosuid".into(),
        MountOption::RO => "ro".into(),
        MountOption::RW => "rw".into(),
        MountOption::Exec => "exec".into(),
        MountOption::NoExec => "noexec".into(),
        MountOption::Atime => "atime".into(),
        MountOption::NoAtime => "noatime".into(),
        MountOption::DirSync => "dirsync".into(),
        MountOption::Sync => "sync".into(),
        MountOption::Async => "async".into(),
    }
}

/// The channel `fuse_mount` returned; only passed back to `fuse_unmount`.
struct Channel(*mut c_void);
// SAFETY: an opaque handle libfuse doesn't tie to a thread.
unsafe impl Send for Channel {}

/// Runs `f` with this process's stderr going to a pipe, returning what was
/// written there: libfuse reports why a mount failed only on stderr. The
/// pipe is read as it's written, on a thread of its own, so a writer never
/// waits on it (nor fails on it, as on a full pipe that doesn't block), and
/// stderr is restored after `timeout` even if `f` is still running.
fn capturing_stderr<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> (Option<T>, String) {
    static STDERR: Mutex<()> = Mutex::new(());
    let _one_at_a_time = STDERR.lock().unwrap_or_else(|e| e.into_inner());
    let mut fds = [0 as c_int; 2];
    let piped = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    let saved = if piped { unsafe { libc::dup(2) } } else { -1 };
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reading = match (piped, saved >= 0) {
        (true, true) => {
            unsafe {
                libc::dup2(fds[1], 2);
                libc::close(fds[1]);
            }
            let mut reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
            let sink = captured.clone();
            Some(std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => sink
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extend_from_slice(&buf[..n]),
                    }
                }
            }))
        }
        (true, false) => {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            None
        }
        (false, _) => None,
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let result = rx.recv_timeout(timeout).ok();
    if piped && saved >= 0 {
        unsafe {
            libc::dup2(saved, 2);
            libc::close(saved);
        }
    }
    // With stderr restored the pipe has no writer left, but for a process
    // `f` started that holds it still: the reader is waited for a moment.
    if let Some(reading) = reading {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !reading.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if reading.is_finished() {
            let _ = reading.join();
        }
    }
    let captured = captured.lock().unwrap_or_else(|e| e.into_inner());
    let captured = String::from_utf8_lossy(&captured).trim().to_string();
    (result, captured)
}

/// A mounted macFUSE volume's handle, for unmounting.
pub struct MacMount {
    channel: Channel,
    mountpoint: CString,
}

impl MacMount {
    pub fn unmount(self, path: &Path) -> Result<(), String> {
        if let Ok(lib) = library() {
            unsafe { (lib.unmount)(self.mountpoint.as_ptr(), self.channel.0) };
        }
        // Whatever fuse_unmount left mounted.
        if unsafe { libc::unmount(self.mountpoint.as_ptr(), libc::MNT_FORCE) } != 0 {
            let _ = Command::new("/usr/sbin/diskutil")
                .args(["unmount", "force"])
                .arg(path)
                .output();
        }
        Ok(())
    }
}

pub fn mount(
    fs: FuseFs,
    requested: Option<&Path>,
    config: &Config,
) -> Result<(BackgroundSession, MacMount, PathBuf), MountProblem> {
    let version = super::driver::macfuse_version()
        .ok_or_else(|| MountProblem::not_installed("macFUSE", None))?;
    if problem::parse_version(&version).is_some_and(problem::macfuse_version_unusable) {
        return Err(problem::update_macfuse(&version));
    }
    let Plan {
        backend,
        mountpoint,
    } = plan(requested);
    let mut options: Vec<String> = config.mount_options.iter().map(option_string).collect();
    options.push(format!("volname={VOLUME_NAME}"));
    match backend {
        Backend::FsKit => {
            if !problem::fskit_mountpoint_ok(&mountpoint.to_string_lossy()) {
                return Err(MountProblem::failed(format!(
                    "{}: with macFUSE's FSKit backend the folder must be under /Volumes, with only letters, digits, - _ . in its path",
                    mountpoint.display()
                )));
            }
            // Switched off: no point trying (and no confusing error).
            if fskit_module_enabled() == Some(false) {
                return Err(problem::enable_extension(None));
            }
            options.push("backend=fskit".into());
        }
        Backend::Kext => {
            std::fs::create_dir_all(&mountpoint)
                .map_err(|e| MountProblem::failed(format!("{}: {e}", mountpoint.display())))?;
        }
    }
    let lib = library().map_err(MountProblem::failed)?;
    let c_mountpoint = CString::new(mountpoint.as_os_str().as_bytes())
        .map_err(|_| MountProblem::failed("mount point contains a NUL"))?;

    let mount_path = c_mountpoint.clone();
    let opts = options.join(",");
    let (channel, stderr) = capturing_stderr(MOUNT_TIMEOUT, move || {
        let mut argv_owned: Vec<CString> = ["rovik-ide", "-o", &opts]
            .iter()
            .map(|a| CString::new(*a).unwrap_or_default())
            .collect();
        let mut argv: Vec<*mut c_char> = argv_owned
            .iter_mut()
            .map(|a| a.as_ptr() as *mut c_char)
            .collect();
        let mut args = FuseArgs {
            argc: argv.len() as c_int,
            argv: argv.as_mut_ptr(),
            allocated: 0,
        };
        let ch = unsafe { (lib.mount)(mount_path.as_ptr(), &mut args) };
        drop(argv_owned);
        Channel(ch)
    });
    let channel = match channel {
        Some(ch) if !ch.0.is_null() => ch,
        Some(_) => {
            return Err(problem::classify_macos_failure(
                backend,
                apple_silicon(),
                &stderr,
            ))
        }
        None => {
            return Err(problem::classify_macos_failure(
                backend,
                apple_silicon(),
                &format!(
                    "macFUSE didn't finish mounting within {}s. {stderr}",
                    MOUNT_TIMEOUT.as_secs()
                ),
            ))
        }
    };
    let unmount_now = |channel: Channel| {
        unsafe { (lib.unmount)(c_mountpoint.as_ptr(), channel.0) };
    };

    // fuse_chan_fd lends the descriptor; the session gets its own copy.
    let borrowed = unsafe { (lib.chan_fd)(channel.0) };
    let fd = if borrowed >= 0 {
        unsafe { libc::dup(borrowed) }
    } else {
        -1
    };
    if fd < 0 {
        unmount_now(channel);
        return Err(problem::update_macfuse(&version));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    // The INIT handshake waits for the kernel's (or FSKit's) first request.
    let (tx, rx) = mpsc::channel();
    let (acl, session_config) = (config.acl, config.clone());
    std::thread::Builder::new()
        .name("macfuse-handshake".into())
        .spawn(move || {
            let _ = tx.send(Session::from_fd(fs, fd, acl, session_config).and_then(|s| s.spawn()));
        })
        .map_err(|e| MountProblem::failed(e.to_string()))?;
    match rx.recv_timeout(MOUNT_TIMEOUT) {
        Ok(Ok(session)) => Ok((
            session,
            MacMount {
                channel,
                mountpoint: c_mountpoint.clone(),
            },
            mountpoint,
        )),
        Ok(Err(e)) => {
            unmount_now(channel);
            Err(problem::classify_macos_failure(
                backend,
                apple_silicon(),
                &e.to_string(),
            ))
        }
        Err(_) => {
            unmount_now(channel);
            Err(problem::classify_macos_failure(
                backend,
                apple_silicon(),
                "the file system extension didn't start serving the volume",
            ))
        }
    }
}

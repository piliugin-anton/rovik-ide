//! Mounting through macFUSE without linking its library.
//!
//! This does what macFUSE's libfuse does in `mount_darwin.c`: start the
//! `mount_macfuse` helper with one end of a socket pair in `_FUSE_COMMFD`;
//! the helper opens a `/dev/macfuse*` device, sends its descriptor back over
//! the socket (`SCM_RIGHTS`) and then mounts, which completes once we answer
//! the kernel's INIT on that descriptor. Linking libfuse instead would make
//! the app refuse to start on a Mac where macFUSE isn't installed yet.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fuser::{BackgroundSession, Config, MountOption, Session};

use super::fuse::FuseFs;

pub const BUNDLE: &str = "/Library/Filesystems/macfuse.fs";
const MOUNT_HELPER: &str = "/Library/Filesystems/macfuse.fs/Contents/Resources/mount_macfuse";
/// The user may first have to allow the system extension; past this the
/// mount has failed.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Receives the device descriptor the helper sends.
fn receive_fd(socket: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    loop {
        let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, 0) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "mount_macfuse closed the socket without a device"));
        }
        break;
    }
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() || (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::other("mount_macfuse sent no device descriptor"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const libc::c_int);
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

pub fn mount(fs: FuseFs, mountpoint: &Path, config: &Config) -> Result<BackgroundSession, String> {
    if !Path::new(MOUNT_HELPER).exists() {
        return Err("macFUSE isn't installed".into());
    }
    std::fs::create_dir_all(mountpoint).map_err(|e| format!("{}: {e}", mountpoint.display()))?;

    let (ours, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    let their_fd = theirs.as_raw_fd();
    let options: Vec<String> = config.mount_options.iter().map(option_string).collect();
    let mut command = Command::new(MOUNT_HELPER);
    command
        .arg("-o")
        .arg(options.join(","))
        .arg(mountpoint)
        .env("_FUSE_COMMFD", their_fd.to_string())
        .env("_FUSE_COMMVERS", "2")
        .env("_FUSE_CALL_BY_LIB", "1")
        .env("_FUSE_DAEMON_PATH", std::env::current_exe().unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // The helper must inherit its end of the socket.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(their_fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| format!("starting mount_macfuse: {e}"))?;
    drop(theirs);

    let fd = match receive_fd(&ours) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = child.kill();
            let stderr = child.wait_with_output().map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string()).unwrap_or_default();
            return Err(if stderr.is_empty() {
                format!("mounting with macFUSE: {e}")
            } else {
                format!("mounting with macFUSE: {stderr}")
            });
        }
    };

    // The INIT handshake blocks until the helper's mount reaches the kernel,
    // so it runs on its own thread while the helper is watched for failure.
    let (tx, rx) = mpsc::channel();
    let acl = config.acl;
    let session_config = config.clone();
    std::thread::Builder::new()
        .name("macfuse-handshake".into())
        .spawn(move || {
            let result = Session::from_fd(fs, fd, acl, session_config).and_then(|s| s.spawn());
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;

    let started = Instant::now();
    let session = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(session)) => break session,
            Ok(Err(e)) => return Err(format!("mounting with macFUSE: {e}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("mounting with macFUSE: handshake thread ended".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = io::Read::read_to_string(&mut pipe, &mut stderr);
                }
                return Err(format!(
                    "mount_macfuse failed ({status}){}{}",
                    if stderr.trim().is_empty() { "" } else { ": " },
                    stderr.trim()
                ));
            }
        }
        if started.elapsed() > MOUNT_TIMEOUT {
            let _ = child.kill();
            return Err("mounting with macFUSE timed out. If macOS asked to allow a system extension, allow it in \
                 System Settings → Privacy & Security and try again."
                .into());
        }
    };
    // Reap the helper whenever it exits; keep our end of the socket open
    // until then, as libfuse does.
    std::thread::spawn(move || {
        let _ = child.wait();
        drop(ours);
    });
    Ok(session)
}

pub fn unmount(mountpoint: &Path) -> Result<(), String> {
    let path = CString::new(mountpoint.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if unsafe { libc::unmount(path.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    if unsafe { libc::unmount(path.as_ptr(), libc::MNT_FORCE) } == 0 {
        return Ok(());
    }
    let out = Command::new("/usr/sbin/diskutil")
        .args(["unmount", "force"])
        .arg(mountpoint)
        .output()
        .map_err(|e| format!("unmounting: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("unmounting: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

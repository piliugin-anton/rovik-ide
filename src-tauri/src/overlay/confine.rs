//! Keeping an isolated chat's commands out of the project.
//!
//! A chat's commands run in its view of the project, but a command can
//! still name the project's own folder by its full path (`cd /home/me/app`,
//! a build script with an absolute path) and write there. On Linux, each
//! command runs in a mount namespace of its own in which the project's path
//! is the chat's view: whatever path a command uses, it reaches the chat's
//! files. The namespace needs no privileges (a user namespace that maps the
//! user to itself), so it works wherever the kernel allows unprivileged
//! user namespaces; where it doesn't, [`available`] is false and commands
//! naming the project's path are refused instead (chats.rs).
//!
//! On macOS and Windows there's no such namespace: the refusal applies.

use std::path::PathBuf;
use std::process::Command;

/// Where a chat's command runs: its view, which the project's path shows.
#[derive(Clone, Debug)]
pub struct Confine {
    pub view: PathBuf,
    pub project: PathBuf,
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    fn cstring(p: &std::path::Path) -> io::Result<CString> {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    /// Writes a whole small file; for `/proc/self/*_map`.
    fn write_proc(path: &CString, data: &[u8]) -> io::Result<()> {
        // SAFETY: plain system calls on a valid path and buffer; nothing is
        // allocated, which matters between fork and exec.
        unsafe {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let n = libc::write(fd, data.as_ptr().cast(), data.len());
            let err = io::Error::last_os_error();
            libc::close(fd);
            if n != data.len() as isize {
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn apply(cmd: &mut Command, c: &super::Confine) -> io::Result<()> {
        let view = cstring(&c.view)?;
        let project = cstring(&c.project)?;
        // Everything the child needs, made before the fork.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let uid_map = format!("{uid} {uid} 1").into_bytes();
        let gid_map = format!("{gid} {gid} 1").into_bytes();
        let setgroups = CString::new("/proc/self/setgroups").unwrap();
        let uid_file = CString::new("/proc/self/uid_map").unwrap();
        let gid_file = CString::new("/proc/self/gid_map").unwrap();
        let root = CString::new("/").unwrap();
        // SAFETY: the closure runs in the child between fork and exec and
        // only makes system calls on data prepared above.
        unsafe {
            cmd.pre_exec(move || {
                // A user namespace mapping the user to itself, so the mount
                // namespace needs no privileges; root needs only the latter.
                let flags = if uid == 0 {
                    libc::CLONE_NEWNS
                } else {
                    libc::CLONE_NEWUSER | libc::CLONE_NEWNS
                };
                if libc::unshare(flags) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if uid != 0 {
                    write_proc(&setgroups, b"deny")?;
                    write_proc(&uid_file, &uid_map)?;
                    write_proc(&gid_file, &gid_map)?;
                }
                // Mounts made here stay here.
                if libc::mount(
                    std::ptr::null(),
                    root.as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                if libc::mount(
                    view.as_ptr(),
                    project.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

/// Runs `cmd` confined: the project's path shows the chat's view.
pub fn apply(cmd: &mut Command, c: &Confine) {
    #[cfg(target_os = "linux")]
    let _ = linux::apply(cmd, c);
    #[cfg(not(target_os = "linux"))]
    let _ = (cmd, c);
}

/// Whether commands can be confined here: tried once, on a real view, since
/// kernels and security modules (AppArmor on Ubuntu 24.04) may forbid
/// unprivileged user namespaces.
pub fn available(c: &Confine) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        static WORKS: OnceLock<bool> = OnceLock::new();
        *WORKS.get_or_init(|| {
            if std::env::var("ROVIK_CONFINE_COMMANDS").is_ok_and(|v| v == "0") {
                return false;
            }
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "true"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            apply(&mut cmd, c);
            cmd.status().is_ok_and(|s| s.success())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = c;
        false
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A command that writes to the project's path writes to the view.
    #[test]
    fn the_projects_path_shows_the_view() {
        let root = std::env::temp_dir().join(format!("confine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (view, project) = (root.join("view"), root.join("project"));
        std::fs::create_dir_all(&view).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "project\n").unwrap();
        std::fs::write(view.join("a.txt"), "view\n").unwrap();
        let c = Confine {
            view: view.clone(),
            project: project.clone(),
        };
        if !available(&c) {
            eprintln!("user namespaces aren't allowed here: skipped");
            return;
        }
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            &format!(
                "cat {0}/a.txt && echo written > {0}/b.txt",
                project.display()
            ),
        ]);
        apply(&mut cmd, &c);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "view\n");
        assert_eq!(
            std::fs::read_to_string(view.join("b.txt")).unwrap(),
            "written\n"
        );
        assert!(
            !project.join("b.txt").exists(),
            "the project wasn't touched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

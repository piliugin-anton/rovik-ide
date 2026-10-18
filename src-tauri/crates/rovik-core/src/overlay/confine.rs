//! Keeping an isolated chat's commands out of the project.
//!
//! A chat's commands run in its view of the project, but a command can
//! still name the project's own folder by its full path (`cd /home/me/app`,
//! a build script with an absolute path) and write there. On Linux, each
//! command runs in a mount namespace of its own in which the project's path
//! is the chat's view: whatever path a command uses, it reaches the chat's
//! files. Two ways make it, tried once in this order (see [`Way`]):
//!
//! - bubblewrap (`bwrap`), where it's installed and may: Ubuntu 24.04's
//!   AppArmor forbids unprivileged user namespaces to other programs, but
//!   gives bwrap a profile of its own that allows them, and an older
//!   system's bwrap may be setuid;
//! - the app's own: a user namespace that maps the user to itself, so the
//!   mount namespace needs no privileges, wherever the kernel allows that.
//!
//! With neither, [`available`] is false and commands naming the project's
//! path are refused instead (chats.rs). `ROVIK_CONFINE_COMMANDS` picks one
//! way (`bwrap`, `unshare`) or none (`0`).
//!
//! On macOS and Windows there's no such namespace: the refusal applies.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Where a chat's command runs: its view, which the project's path shows.
#[derive(Clone, Debug)]
pub struct Confine {
    pub view: PathBuf,
    pub project: PathBuf,
}

/// How a command gets its namespace. Only Linux has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Way {
    /// `bwrap` makes it and runs the command in it.
    Bwrap,
    /// The command's own process makes it, before it runs the command.
    Unshare,
}

/// The ways to try, in order: bwrap's first, the app's own if bwrap isn't
/// there or can't.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ways() -> &'static [Way] {
    match std::env::var("ROVIK_CONFINE_COMMANDS").as_deref() {
        Ok("0") => &[],
        Ok("bwrap") => &[Way::Bwrap],
        Ok("unshare") => &[Way::Unshare],
        _ => &[Way::Bwrap, Way::Unshare],
    }
}

/// `program` with `args`, run `way`.
fn command_with(way: Way, c: &Confine, program: &str, args: &[&str]) -> Command {
    match way {
        Way::Bwrap => {
            // The whole file system as it is, devices too, but for the
            // project's path: the chat's view. The user stays itself, and
            // the working folder is kept (found again in the namespace).
            let mut cmd = Command::new("bwrap");
            cmd.arg("--dev-bind").args(["/", "/"]);
            cmd.arg("--bind").arg(&c.view).arg(&c.project);
            cmd.arg("--").arg(program).args(args);
            cmd
        }
        Way::Unshare => {
            let mut cmd = Command::new(program);
            cmd.args(args);
            #[cfg(target_os = "linux")]
            let _ = linux::apply(&mut cmd, c);
            cmd
        }
    }
}

/// Whether `way` confines a command here: one that does nothing, run.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn works(way: Way, c: &Confine) -> bool {
    let mut cmd = command_with(way, c, "/bin/sh", &["-c", "true"]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// The way commands are confined here, found once, on a real view: kernels
/// and security modules (AppArmor on Ubuntu 24.04) may forbid unprivileged
/// user namespaces, and bwrap may not be installed.
fn way(c: &Confine) -> Option<Way> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        static WAY: OnceLock<Option<Way>> = OnceLock::new();
        *WAY.get_or_init(|| {
            let found = ways().iter().copied().find(|w| works(*w, c));
            log::info!("an isolated chat's commands are confined: {found:?}");
            found
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = c;
        None
    }
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

/// `program` with `args`, confined: the project's path shows the chat's
/// view. As it is where commands can't be confined, which callers ask
/// first ([`available`]).
pub fn command(c: &Confine, program: &str, args: &[&str]) -> Command {
    match way(c) {
        Some(way) => command_with(way, c, program, args),
        None => {
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd
        }
    }
}

/// Whether commands can be confined here.
pub fn available(c: &Confine) -> bool {
    way(c).is_some()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A command that writes to the project's path writes to the view,
    /// either way, in the folder it was started in, its exit code its own.
    #[test]
    fn the_projects_path_shows_the_view_either_way() {
        for way in [Way::Bwrap, Way::Unshare] {
            let root = std::env::temp_dir().join(format!("confine-{}-{way:?}", std::process::id()));
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
            if !works(way, &c) {
                eprintln!("{way:?} can't confine commands here: skipped");
                continue;
            }
            let script = format!(
                "cat {0}/a.txt && echo written > {0}/b.txt && pwd && exit 3",
                project.display()
            );
            let mut cmd = command_with(way, &c, "/bin/sh", &["-c", &script]);
            let out = cmd.current_dir(&view).output().unwrap();
            assert_eq!(
                out.status.code(),
                Some(3),
                "{way:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = format!("view\n{}\n", view.display());
            assert_eq!(String::from_utf8_lossy(&out.stdout), stdout, "{way:?}");
            assert_eq!(
                std::fs::read_to_string(view.join("b.txt")).unwrap(),
                "written\n",
                "{way:?}"
            );
            assert!(
                !project.join("b.txt").exists(),
                "{way:?}: the project wasn't touched"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// bwrap first, the app's own way after it; one way, or none, by name.
    #[test]
    fn the_ways_tried() {
        // Read as the process started: tests don't set the variable.
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_none() {
            assert_eq!(ways(), [Way::Bwrap, Way::Unshare]);
        }
    }
}

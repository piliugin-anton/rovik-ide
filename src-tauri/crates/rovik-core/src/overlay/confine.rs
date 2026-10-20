//! Keeping a chat's commands in their place: in the chat's view of the
//! project, and out of the credential stores.
//!
//! A command the agent runs is the user's own process: whatever the
//! permission rules let through runs with every file the user can open.
//! Two things are worth more than a text check against that:
//!
//! - **The redirect.** An isolated chat's commands run in its view of the
//!   project, but a command can still name the project's own folder by its
//!   full path (`cd /home/me/app`, a build script with an absolute path)
//!   and write there. Confined, the project's path *is* the chat's view:
//!   whatever path a command uses, it reaches the chat's files.
//! - **The hiding.** The provider keys (`~/.rovik`, keys.json on a server)
//!   and the user's SSH, cloud and signing credentials are made invisible
//!   to the command ([`secret_dirs`]). A chat's command has no business
//!   reading them, and a misled model shouldn't be able to.
//!
//! The [`Confine`] *policy* says what a command may see; it is the same on
//! every operating system. A [`Mechanism`] is how one OS enforces it, and
//! each OS module ([`linux`], [`macos`], [`windows`]) lists its mechanisms
//! in order of preference. The first that works on this machine, found
//! once, confines every command. Where none does, [`available`] is false
//! and the callers fall back to their text checks (chats.rs, server.rs).
//!
//! Linux has two mechanisms (mount namespaces: bubblewrap, then the app's
//! own user namespace); macOS and Windows have none yet, and say in their
//! modules what the one to add would be. `ROVIK_CONFINE_COMMANDS` picks one
//! mechanism by name, or none (`0`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, OnceLock};

/*
 * ---------------------------------------------------------------------------
 * The policy: what a command may see
 * ---------------------------------------------------------------------------
 */

/// What a confined command may see. The project's path shows the chat's
/// `view` (no redirect when the two are the same: the command already runs
/// in its own copy, as on a server), and the directories in `hide` are
/// empty to it.
#[derive(Clone, Debug)]
pub struct Confine {
    pub view: PathBuf,
    pub project: PathBuf,
    /// Directories made inaccessible to the command. Best-effort: only
    /// those that exist, and only where a mechanism is available.
    pub hide: Vec<PathBuf>,
}

impl Confine {
    /// An isolated chat's command: the project's path shows its view, and
    /// the credential stores ([`secret_dirs`]) are hidden.
    pub fn redirecting(view: PathBuf, project: PathBuf) -> Confine {
        Confine {
            view,
            project,
            hide: secret_dirs(),
        }
    }

    /// A command that already runs in its own copy (a server worktree, or a
    /// chat that works in the project directly): nothing is redirected, the
    /// credential stores are still hidden.
    pub fn hiding(root: PathBuf) -> Confine {
        Confine {
            view: root.clone(),
            project: root,
            hide: secret_dirs(),
        }
    }

    /// Whether the project's path is pointed at the view.
    pub fn redirects(&self) -> bool {
        self.view != self.project
    }
}

/// Credential directories the app registers on top of the built-in list
/// (its own secret stores that aren't under `~/.rovik`).
static PROTECTED: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Default::default);

/// Registers directories to hide from every chat's commands, in addition to
/// the built-in list. The app calls it at start with its own secret stores;
/// idempotent.
pub fn protect(paths: impl IntoIterator<Item = PathBuf>) {
    let mut protected = PROTECTED.lock().unwrap_or_else(|e| e.into_inner());
    for path in paths {
        if !protected.contains(&path) {
            protected.push(path);
        }
    }
}

/// The credential directories a chat's commands are kept out of: the app's
/// own provider keys (`~/.rovik`, keys.json on a server) and the user's
/// SSH, cloud, container and signing credentials, plus whatever the app
/// registered with [`protect`]. Only those that exist right now. A command
/// that legitimately needs one of these (network git over SSH, a cloud
/// deploy) is one an isolated or server chat refuses anyway, and the user's
/// own terminal isn't confined; `ROVIK_CONFINE_COMMANDS=0` turns all of
/// this off.
pub fn secret_dirs() -> Vec<PathBuf> {
    const UNDER_HOME: &[&str] = &[
        ".rovik",
        ".ssh",
        ".aws",
        ".gnupg",
        ".config/gcloud",
        ".config/gh",
        ".kube",
        ".docker",
    ];
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty());
    let mut dirs: Vec<PathBuf> = home
        .iter()
        .flat_map(|home| UNDER_HOME.iter().map(move |rel| home.join(rel)))
        .collect();
    dirs.extend(
        PROTECTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned(),
    );
    dirs.retain(|p| p.is_dir());
    dirs.sort();
    dirs.dedup();
    dirs
}

/*
 * ---------------------------------------------------------------------------
 * The mechanisms: how an operating system enforces the policy
 * ---------------------------------------------------------------------------
 */

/// One way this operating system enforces a [`Confine`] on a command. To
/// add one: implement it in the OS's module and list it in that module's
/// `mechanisms()`, in order of preference. Nothing else changes.
trait Mechanism: Send + Sync {
    /// Its name: what `ROVIK_CONFINE_COMMANDS` picks it by, and logs say.
    fn name(&self) -> &'static str;

    /// `program` with `args`, confined by `c`. A mechanism that can't
    /// redirect ignores `c.view`; [`available`] then says so for a
    /// redirecting policy, and its callers keep their text check.
    fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command;

    /// Whether it can point the project's path at the view. A mechanism
    /// that only hides (macOS's Seatbelt would be one) says false.
    fn can_redirect(&self) -> bool;
}

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use other as os;
#[cfg(windows)]
use windows as os;

/// This OS's mechanisms, in order of preference, as `ROVIK_CONFINE_COMMANDS`
/// leaves them: all of them, one by name, or none (`0`).
fn candidates() -> Vec<Box<dyn Mechanism>> {
    let all = os::mechanisms();
    match std::env::var("ROVIK_CONFINE_COMMANDS").as_deref() {
        Ok("0") => Vec::new(),
        Ok(name) if all.iter().any(|m| m.name() == name) => {
            all.into_iter().filter(|m| m.name() == name).collect()
        }
        _ => all,
    }
}

/// Whether `mechanism` confines a command here: one that does nothing, run.
/// Kernels and security modules may forbid what it needs (AppArmor on
/// Ubuntu 24.04 forbids unprivileged user namespaces), or its program may
/// not be installed.
fn works(mechanism: &dyn Mechanism, c: &Confine) -> bool {
    let (program, args) = os::PROBE;
    let mut cmd = mechanism.command(c, program, args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// The mechanism that confines commands here: the first candidate that
/// works, found once, on the first policy asked for.
fn chosen(c: &Confine) -> Option<&'static dyn Mechanism> {
    static CHOSEN: OnceLock<Option<Box<dyn Mechanism>>> = OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            let found = candidates().into_iter().find(|m| works(m.as_ref(), c));
            log::info!(
                "chats' commands are confined by: {:?}",
                found.as_ref().map(|m| m.name())
            );
            found
        })
        .as_deref()
}

/*
 * ---------------------------------------------------------------------------
 * What the callers use
 * ---------------------------------------------------------------------------
 */

/// Whether `c` can be enforced here: a mechanism works, and it redirects
/// if `c` asks for that.
pub fn available(c: &Confine) -> bool {
    chosen(c).is_some_and(|m| !c.redirects() || m.can_redirect())
}

/// `program` with `args`, confined by `c`. As it is where `c` can't be
/// enforced, which callers ask first ([`available`]).
pub fn command(c: &Confine, program: &str, args: &[&str]) -> Command {
    match chosen(c).filter(|_| available(c)) {
        Some(m) => m.command(c, program, args),
        None => {
            let mut cmd = crate::process::command(program);
            cmd.args(args);
            cmd
        }
    }
}

/// The policy for a command with no overlay (a server worktree, a chat that
/// works in the project directly): nothing redirected, the credential
/// stores hidden — where that can be done here.
pub fn hiding_if_available(root: &Path) -> Option<Confine> {
    let c = Confine::hiding(root.to_path_buf());
    available(&c).then_some(c)
}

/*
 * ---------------------------------------------------------------------------
 * Linux: mount namespaces
 * ---------------------------------------------------------------------------
 */

/// Each command runs in a mount namespace of its own: the chat's view
/// bind-mounted over the project's path, and an empty tmpfs over each
/// hidden directory. Two mechanisms make it:
///
/// - [`Bwrap`], bubblewrap, where it's installed and may: Ubuntu 24.04's
///   AppArmor forbids unprivileged user namespaces to other programs but
///   gives bwrap a profile that allows them, and an older system's bwrap
///   may be setuid. It also drops the command's capabilities, so the
///   command can't undo the mounts: the strong one.
/// - [`Unshare`], the app's own: a user namespace mapping the user to
///   itself, so the mount namespace needs no privileges, wherever the
///   kernel allows that. It drops the capabilities that namespace granted
///   before the command runs, for the same reason.
#[cfg(target_os = "linux")]
mod linux {
    use super::{Confine, Mechanism};
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);

    pub(super) fn mechanisms() -> Vec<Box<dyn Mechanism>> {
        vec![Box::new(Bwrap), Box::new(Unshare)]
    }

    /// `bwrap` makes the namespace and runs the command in it.
    pub(super) struct Bwrap;

    impl Mechanism for Bwrap {
        fn name(&self) -> &'static str {
            "bwrap"
        }

        fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command {
            // The whole file system as it is, devices too, but for the
            // project's path (the chat's view) and the hidden directories
            // (a fresh tmpfs each). bwrap applies these in order, so the
            // overmounts win over `--dev-bind / /`. The user stays itself,
            // and the working folder is kept (found again in the namespace).
            // Unprivileged, bwrap drops the command's capabilities on its
            // own; run by root it keeps them unless told, and root's
            // command could then unmount what hides its secrets.
            let mut cmd = crate::process::command("bwrap");
            cmd.arg("--dev-bind").args(["/", "/"]);
            if c.redirects() {
                cmd.arg("--bind").arg(&c.view).arg(&c.project);
            }
            for dir in &c.hide {
                cmd.arg("--tmpfs").arg(dir);
            }
            cmd.args(["--cap-drop", "ALL"]);
            cmd.arg("--").arg(program).args(args);
            cmd
        }

        fn can_redirect(&self) -> bool {
            true
        }
    }

    /// The command's own process makes the namespace, between fork and
    /// exec.
    pub(super) struct Unshare;

    impl Mechanism for Unshare {
        fn name(&self) -> &'static str {
            "unshare"
        }

        fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command {
            let mut cmd = crate::process::command(program);
            cmd.args(args);
            // A policy whose paths can't be C strings (a NUL in one) isn't
            // enforced; the probe then fails and the mechanism isn't chosen.
            let _ = apply(&mut cmd, c);
            cmd
        }

        fn can_redirect(&self) -> bool {
            true
        }
    }

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

    /// Makes `cmd` enter a namespace of its own and set up `c`'s mounts
    /// before it runs.
    fn apply(cmd: &mut Command, c: &Confine) -> io::Result<()> {
        let view = cstring(&c.view)?;
        let project = cstring(&c.project)?;
        let redirects = c.redirects();
        let hide: Vec<CString> = c
            .hide
            .iter()
            .map(|p| cstring(p))
            .collect::<io::Result<_>>()?;
        // Everything the child needs, made before the fork.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let uid_map = format!("{uid} {uid} 1").into_bytes();
        let gid_map = format!("{gid} {gid} 1").into_bytes();
        let setgroups = CString::new("/proc/self/setgroups").unwrap();
        let uid_file = CString::new("/proc/self/uid_map").unwrap();
        let gid_file = CString::new("/proc/self/gid_map").unwrap();
        let root = CString::new("/").unwrap();
        let tmpfs = CString::new("tmpfs").unwrap();
        // SAFETY: the closure runs in the child between fork and exec and
        // only makes system calls on data prepared above; it allocates
        // nothing.
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
                if redirects
                    && libc::mount(
                        view.as_ptr(),
                        project.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REC,
                        std::ptr::null(),
                    ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                // A fresh empty tmpfs over each hidden directory. One that
                // can't be hidden (it went away since the policy was made)
                // isn't worth failing the command over.
                for dir in &hide {
                    let _ = libc::mount(
                        tmpfs.as_ptr(),
                        dir.as_ptr(),
                        tmpfs.as_ptr(),
                        0,
                        std::ptr::null(),
                    );
                }
                // Give up the privileges this user namespace granted, so the
                // command can't unmount what was just mounted (that needs
                // CAP_SYS_ADMIN) or regain anything through a setuid exec.
                drop_privileges();
                Ok(())
            });
        }
        Ok(())
    }

    /// Drops every capability and sets no-new-privileges for the command
    /// about to run. Between fork and exec: only plain syscalls, no
    /// allocation. Best-effort: the mounts are already in place, so the
    /// command stays confined even if a step here is refused.
    unsafe fn drop_privileges() {
        let _ = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        // Clear the effective, permitted and inheritable sets (capability
        // version 3: two 32-bit words each).
        #[repr(C)]
        struct Header {
            version: u32,
            pid: libc::c_int,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Data {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        const VERSION_3: u32 = 0x2008_0522;
        let header = Header {
            version: VERSION_3,
            pid: 0,
        };
        let data = [Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }; 2];
        let _ = libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr());
        // Empty the bounding set too, so no capability survives the exec.
        for cap in 0..64 {
            let _ = libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0);
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * macOS and Windows: nothing yet
 * ---------------------------------------------------------------------------
 */

/// No mechanism yet: the callers' text checks apply. The one to add is
/// Seatbelt, `sandbox-exec -p <profile> -- program args`, with a profile
/// that allows everything but `file-read*` and `file-write*` under each
/// hidden directory (`(deny file-read* (subpath "/Users/me/.ssh"))`). macOS
/// can't bind-mount, so that mechanism would say `can_redirect() == false`
/// and the project's full path would stay refused by text. Implement
/// [`Mechanism`] for it and list it in `mechanisms()`.
#[cfg(target_os = "macos")]
mod macos {
    use super::Mechanism;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);

    pub(super) fn mechanisms() -> Vec<Box<dyn Mechanism>> {
        Vec::new()
    }
}

/// No mechanism yet: the callers' text checks apply. The one to add is a
/// restricted token or an AppContainer profile for the command's process,
/// with the hidden directories denied by ACL (and the job object
/// command_tool.rs already puts the process in). Windows can't bind-mount
/// either, so it would say `can_redirect() == false`. Note that
/// command_tool.rs builds the Windows shell line with `raw_arg` for cmd.exe
/// (`/S /C "…"`); a mechanism here gets that line as `args` and must pass
/// it on the same way.
#[cfg(windows)]
mod windows {
    use super::Mechanism;

    pub(super) const PROBE: (&str, &[&str]) = ("cmd.exe", &["/C", "exit", "0"]);

    pub(super) fn mechanisms() -> Vec<Box<dyn Mechanism>> {
        Vec::new()
    }
}

/// Any other Unix: no mechanism.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod other {
    use super::Mechanism;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);

    pub(super) fn mechanisms() -> Vec<Box<dyn Mechanism>> {
        Vec::new()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A fresh root for one test and one mechanism, under the temp folder.
    fn fresh(test: &str, m: &dyn Mechanism) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{test}-{}-{}", std::process::id(), m.name()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// A command that writes to the project's path writes to the view,
    /// with either mechanism, in the folder it was started in, its exit
    /// code its own.
    #[test]
    fn the_projects_path_shows_the_view_either_way() {
        for m in linux::mechanisms() {
            let root = fresh("confine", m.as_ref());
            let (view, project) = (root.join("view"), root.join("project"));
            std::fs::create_dir_all(&view).unwrap();
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(project.join("a.txt"), "project\n").unwrap();
            std::fs::write(view.join("a.txt"), "view\n").unwrap();
            let c = Confine {
                view: view.clone(),
                project: project.clone(),
                hide: Vec::new(),
            };
            if !works(m.as_ref(), &c) {
                eprintln!("{} can't confine commands here: skipped", m.name());
                continue;
            }
            let script = format!(
                "cat {0}/a.txt && echo written > {0}/b.txt && pwd && exit 3",
                project.display()
            );
            let out = m
                .command(&c, "/bin/sh", &["-c", &script])
                .current_dir(&view)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(3),
                "{}: {}",
                m.name(),
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = format!("view\n{}\n", view.display());
            assert_eq!(String::from_utf8_lossy(&out.stdout), stdout, "{}", m.name());
            assert_eq!(
                std::fs::read_to_string(view.join("b.txt")).unwrap(),
                "written\n",
                "{}",
                m.name()
            );
            assert!(
                !project.join("b.txt").exists(),
                "{}: the project wasn't touched",
                m.name()
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// A hidden directory is empty to the command, with either mechanism,
    /// and the command can't undo that: unmounting takes a capability it
    /// no longer has. The real directory is left as it was (the tmpfs is
    /// only in the command's own mount namespace).
    #[test]
    fn a_hidden_directory_is_empty_to_the_command() {
        for m in linux::mechanisms() {
            let root = fresh("hide", m.as_ref());
            let (view, project, secret) =
                (root.join("view"), root.join("project"), root.join("secret"));
            for dir in [&view, &project, &secret] {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(secret.join("keys.json"), "TOPSECRET\n").unwrap();
            let c = Confine {
                view: view.clone(),
                project: project.clone(),
                hide: vec![secret.clone()],
            };
            if !works(m.as_ref(), &c) {
                eprintln!("{} can't confine commands here: skipped", m.name());
                continue;
            }
            // Read it, list it, write into it, and try to unmount the
            // tmpfs over it (both the program and the system call).
            let script = format!(
                "cat {0}/keys.json 2>/dev/null; umount {0} 2>/dev/null; \
                 ls {0} | wc -l; echo planted > {0}/planted 2>/dev/null; true",
                secret.display()
            );
            let out = m
                .command(&c, "/bin/sh", &["-c", &script])
                .current_dir(&view)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", m.name());
            // Nothing read (no TOPSECRET), nothing listed (0 entries).
            assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n", "{}", m.name());
            assert_eq!(
                std::fs::read_to_string(secret.join("keys.json")).unwrap(),
                "TOPSECRET\n",
                "{}",
                m.name()
            );
            assert!(!secret.join("planted").exists(), "{}", m.name());
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// The hiding-only policy (a server worktree, a chat in the project
    /// directly) redirects nothing and still hides; the command runs where
    /// it was started.
    #[test]
    fn hiding_only_needs_no_redirect() {
        for m in linux::mechanisms() {
            let root = fresh("hideonly", m.as_ref());
            let (worktree, secret) = (root.join("worktree"), root.join("secret"));
            std::fs::create_dir_all(&worktree).unwrap();
            std::fs::create_dir_all(&secret).unwrap();
            std::fs::write(secret.join("keys.json"), "TOPSECRET\n").unwrap();
            let c = Confine {
                hide: vec![secret.clone()],
                ..Confine::hiding(worktree.clone())
            };
            assert!(!c.redirects());
            if !works(m.as_ref(), &c) {
                eprintln!("{} can't confine commands here: skipped", m.name());
                continue;
            }
            let script = format!("cat {}/keys.json 2>/dev/null; pwd", secret.display());
            let out = m
                .command(&c, "/bin/sh", &["-c", &script])
                .current_dir(&worktree)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", m.name());
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("{}\n", worktree.display()),
                "{}",
                m.name()
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// The built-in list names the provider keys' folder and the user's
    /// credential stores, only those that exist, and what the app
    /// registers on top.
    #[test]
    fn the_secret_dirs_are_the_existing_credential_stores() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        for dir in secret_dirs() {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        let registered = std::env::temp_dir().join(format!("protected-{}", std::process::id()));
        std::fs::create_dir_all(&registered).unwrap();
        protect([registered.clone(), registered.clone()]);
        assert!(secret_dirs().contains(&registered));
        if home.join(".rovik").is_dir() {
            assert!(secret_dirs().contains(&home.join(".rovik")));
        }
        let _ = std::fs::remove_dir_all(&registered);
    }

    /// bwrap first, the app's own way after it; one, or none, by name.
    #[test]
    fn the_mechanisms_tried() {
        let names = |ms: Vec<Box<dyn Mechanism>>| -> Vec<&'static str> {
            ms.iter().map(|m| m.name()).collect()
        };
        // Read as the process started: tests don't set the variable.
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_none() {
            assert_eq!(names(candidates()), ["bwrap", "unshare"]);
        }
        assert_eq!(names(linux::mechanisms()), ["bwrap", "unshare"]);
    }
}

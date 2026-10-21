//! Landlock: the Linux kernel's own sandbox, which any process may put
//! itself in, no package, user namespace or administrator needed. Linux's
//! fallback where bubblewrap can't make its sandbox (not installed, the
//! install refused, user namespaces forbidden, as in many containers).
//!
//! Landlock only allows: a process in a ruleset reaches the folders a rule
//! names, and what's beneath them, and nothing else. So the policy is the
//! rules that cover everything but the hidden folders ([`grants`]): the
//! whole system to read, but where a hidden folder is inside (the home
//! folder for `~/.ssh`), each entry beside it instead, down to it; and the
//! writable folders to write, split the same way around a hidden folder
//! inside one. Entries made beside a hidden folder after the command
//! started aren't covered, so they're out of reach to it: a new file in the
//! home folder itself, for example (bubblewrap makes the home read-only
//! anyway), or in a writable folder that holds a hidden one (which the
//! app's policies don't have: the hidden folders are credentials, and the
//! writable folders inside them a daemon's worktrees, not the other way
//! round). Listing a folder that holds a hidden one fails the same way
//! (`ls ~`), though what's in it can be read.
//!
//! A shown folder inside a hidden one (`Confine::show`) is a rule of its
//! own, to read. Offline (`Confine::offline`), from version 4 (Linux 6.7),
//! the ruleset handles TCP and allows none of it: no connection and no
//! listening, on the loopback too, where bubblewrap keeps one. UDP isn't
//! covered (a name still resolves; nothing connects).
//!
//! What it can't do that bubblewrap does: point the project's path at a
//! chat's view (so an isolated chat's commands get the project read-only,
//! as under Seatbelt, and the callers keep their text check), hide the
//! other processes, or end what a command leaves running. It does keep a
//! command from tracing the app or reading its memory (a sandboxed process
//! can't trace one outside its sandbox), and from changing mounts; the
//! capabilities a command run by root would have are dropped too.
//!
//! Used from Landlock's version 3 (Linux 6.2) on: before it, truncating a
//! file wasn't something it could forbid, so the read-only system wouldn't
//! be, quite.

use super::{Confine, Mechanism};
use std::collections::HashSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock};

/// The kernel's interface (include/uapi/linux/landlock.h). Its system calls
/// have the same numbers on every architecture the app is built for.
mod sys {
    pub const CREATE_RULESET: libc::c_long = 444;
    pub const ADD_RULE: libc::c_long = 445;
    pub const RESTRICT_SELF: libc::c_long = 446;

    pub const CREATE_RULESET_VERSION: libc::c_uint = 1;
    pub const RULE_PATH_BENEATH: libc::c_int = 1;

    pub const EXECUTE: u64 = 1 << 0;
    pub const WRITE_FILE: u64 = 1 << 1;
    pub const READ_FILE: u64 = 1 << 2;
    pub const READ_DIR: u64 = 1 << 3;
    pub const REMOVE_DIR: u64 = 1 << 4;
    pub const REMOVE_FILE: u64 = 1 << 5;
    pub const MAKE_CHAR: u64 = 1 << 6;
    pub const MAKE_DIR: u64 = 1 << 7;
    pub const MAKE_REG: u64 = 1 << 8;
    pub const MAKE_SOCK: u64 = 1 << 9;
    pub const MAKE_FIFO: u64 = 1 << 10;
    pub const MAKE_BLOCK: u64 = 1 << 11;
    pub const MAKE_SYM: u64 = 1 << 12;
    /// Version 2: moving and linking across folders.
    pub const REFER: u64 = 1 << 13;
    /// Version 3.
    pub const TRUNCATE: u64 = 1 << 14;

    /// Version 4: TCP.
    pub const NET_BIND_TCP: u64 = 1 << 0;
    pub const NET_CONNECT_TCP: u64 = 1 << 1;

    /// The ruleset's attribute; a ruleset without network rules passes
    /// only its first field's size, which version 3 takes.
    #[repr(C)]
    pub struct RulesetAttr {
        pub handled_access_fs: u64,
        pub handled_access_net: u64,
    }

    #[repr(C, packed)]
    pub struct PathBeneathAttr {
        pub allowed_access: u64,
        pub parent_fd: i32,
    }
}

/// The version it needs: the first that can forbid truncating.
const MIN_ABI: i64 = 3;
/// The version that can cut the network (TCP, Linux 6.7).
const NET_ABI: i64 = 4;

/// What a rule lets a command do beneath its folder.
const READ: u64 = sys::EXECUTE | sys::READ_FILE | sys::READ_DIR;
const WRITE: u64 = sys::WRITE_FILE
    | sys::REMOVE_DIR
    | sys::REMOVE_FILE
    | sys::MAKE_DIR
    | sys::MAKE_REG
    | sys::MAKE_SOCK
    | sys::MAKE_FIFO
    | sys::MAKE_SYM
    | sys::REFER
    | sys::TRUNCATE;
/// What the ruleset forbids unless a rule allows it: all of version 3's,
/// devices made included. Not version 5's device ioctls: a terminal needs
/// them, and they reach nothing a file's rule would keep out.
const HANDLED: u64 = READ | WRITE | sys::MAKE_CHAR | sys::MAKE_BLOCK;
/// What a rule on a file (not a folder) may name.
const FILE_ONLY: u64 = sys::EXECUTE | sys::READ_FILE | sys::WRITE_FILE | sys::TRUNCATE;

/// The devices a command writes to as a terminal program does: its output
/// thrown away, its terminal. `/dev/shm` (POSIX shared memory, which
/// Python's multiprocessing makes files in) is written as a folder.
const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
];
const SHARED_MEMORY: &str = "/dev/shm";

/// What a rule grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Grant {
    /// Reading and running.
    Read,
    /// Reading, running and writing: a writable folder.
    Write,
    /// Reading and writing what's there: a device.
    Device,
}

impl Grant {
    fn access(self) -> u64 {
        match self {
            Grant::Read => READ,
            Grant::Write => READ | WRITE,
            Grant::Device => sys::READ_FILE | sys::WRITE_FILE,
        }
    }
}

/// The rules that enforce `c`: everything but the hidden folders to read,
/// the writable ones (and a few devices) to write. A folder with a hidden
/// one inside is covered entry by entry instead, down to it; a writable
/// folder inside a hidden one is covered all the same, being a rule of its
/// own. Symbolic links get no rule: Landlock checks what a link leads to,
/// where that is. Folders gone since the policy was made are left out.
pub(super) fn grants(c: &Confine) -> Vec<(PathBuf, Grant)> {
    let hidden: Vec<PathBuf> = c.hide.iter().filter(|d| d.is_dir()).cloned().collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    cover(Path::new("/"), Grant::Read, &hidden, &mut seen, &mut out);
    for dir in c.writable.iter().filter(|d| d.is_dir()) {
        cover(dir, Grant::Write, &hidden, &mut seen, &mut out);
    }
    // A shown folder inside a hidden one: a rule of its own, to read.
    for dir in c.show.iter().filter(|d| d.is_dir()) {
        cover(dir, Grant::Read, &hidden, &mut seen, &mut out);
    }
    for device in DEVICES {
        let device = Path::new(device);
        if device.exists() {
            out.push((device.to_path_buf(), Grant::Device));
        }
    }
    if Path::new(SHARED_MEMORY).is_dir() {
        cover(
            Path::new(SHARED_MEMORY),
            Grant::Write,
            &hidden,
            &mut seen,
            &mut out,
        );
    }
    out
}

/// `path` with `grant`: a rule for it, or where a hidden folder is beneath
/// it, a rule for each of its entries but that one, and so on down.
fn cover(
    path: &Path,
    grant: Grant,
    hidden: &[PathBuf],
    seen: &mut HashSet<(PathBuf, Grant)>,
    out: &mut Vec<(PathBuf, Grant)>,
) {
    if hidden.iter().any(|h| h == path) || !seen.insert((path.to_path_buf(), grant)) {
        return;
    }
    if !hidden.iter().any(|h| h.starts_with(path)) {
        out.push((path.to_path_buf(), grant));
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_symlink()) {
            continue;
        }
        cover(&entry.path(), grant, hidden, seen, out);
    }
}

/// The kernel's Landlock version; 0 without it (too old, or not enabled).
static ABI: LazyLock<i64> = LazyLock::new(|| {
    // SAFETY: the version query takes no attribute.
    let abi = unsafe {
        libc::syscall(
            sys::CREATE_RULESET,
            std::ptr::null::<sys::RulesetAttr>(),
            0usize,
            sys::CREATE_RULESET_VERSION,
        )
    };
    abi.max(0)
});

/// The ruleset for `c`, made here (opening its folders, which the command,
/// once forked, mustn't: the app's other threads may hold locks it would
/// wait on forever). The command only applies it. An error: the kernel
/// can't, or not well enough (`MIN_ABI`).
fn ruleset(c: &Confine) -> std::io::Result<OwnedFd> {
    if *ABI < MIN_ABI {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Landlock version {MIN_ABI} (Linux 6.2) is needed; this kernel has {}",
                *ABI
            ),
        ));
    }
    if c.offline && *ABI < NET_ABI {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Landlock version {NET_ABI} (Linux 6.7) is needed to cut the network; this \
                 kernel has {}",
                *ABI
            ),
        ));
    }
    // Offline: every TCP bind and connection handled, and none allowed.
    let attr = sys::RulesetAttr {
        handled_access_fs: HANDLED,
        handled_access_net: sys::NET_BIND_TCP | sys::NET_CONNECT_TCP,
    };
    let size = match c.offline {
        true => std::mem::size_of::<sys::RulesetAttr>(),
        false => std::mem::size_of::<u64>(),
    };
    // SAFETY: `attr` is a valid ruleset attribute at least the size passed.
    let fd = unsafe {
        libc::syscall(
            sys::CREATE_RULESET,
            &attr as *const sys::RulesetAttr,
            size,
            0u32,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the kernel just gave this descriptor (close-on-exec), ours alone.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    for (path, grant) in grants(c) {
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: a valid C string; the descriptor is closed below.
        let raw = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if raw < 0 {
            continue; // gone since: nothing to allow
        }
        // SAFETY: just opened, ours alone.
        let parent = unsafe { OwnedFd::from_raw_fd(raw) };
        let is_dir = std::fs::metadata(&path).is_ok_and(|m| m.is_dir());
        let access = grant.access() & if is_dir { HANDLED } else { FILE_ONLY };
        if access == 0 {
            continue;
        }
        let rule = sys::PathBeneathAttr {
            allowed_access: access,
            parent_fd: parent.as_raw_fd(),
        };
        // SAFETY: both descriptors are open; `rule` is the attribute the
        // rule type takes.
        let added = unsafe {
            libc::syscall(
                sys::ADD_RULE,
                ruleset.as_raw_fd(),
                sys::RULE_PATH_BENEATH,
                &rule as *const sys::PathBeneathAttr,
                0u32,
            )
        };
        if added != 0 {
            log::debug!(
                "landlock: no rule for {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(ruleset)
}

/// Each command applies the policy's ruleset to itself as it starts,
/// before it runs anything: no other program involved.
pub(super) struct Landlock;

pub(super) const NAME: &str = "landlock";

impl Mechanism for Landlock {
    fn name(&self) -> &'static str {
        NAME
    }

    fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command {
        let mut cmd = crate::process::command(program);
        cmd.args(args);
        // The score's file is in /proc, which the ruleset makes read-only:
        // set it first (callers that set it again after are then refused,
        // harmlessly).
        crate::process::ordinary_oom_score(&mut cmd);
        // An error the command then fails to start with, as `works` expects.
        let ruleset = ruleset(c).map(Arc::new).map_err(|e| {
            log::debug!("landlock: {e}");
            e.raw_os_error().unwrap_or(libc::ENOSYS)
        });
        // SAFETY: between fork and exec it makes only system calls (prctl,
        // landlock_restrict_self) on a descriptor opened before the fork.
        unsafe {
            cmd.pre_exec(move || {
                let ruleset = ruleset
                    .as_ref()
                    .map_err(|&errno| std::io::Error::from_raw_os_error(errno))?;
                // Run by root, what it runs would get every capability back:
                // none left to give (refused, harmlessly, to anyone else).
                for cap in 0..64 {
                    libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0);
                }
                libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_CLEAR_ALL,
                    0,
                    0,
                    0,
                );
                // What Landlock asks of an unprivileged process: nothing it
                // runs gains privileges (no set-user-ID program, no sudo).
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::syscall(sys::RESTRICT_SELF, ruleset.as_raw_fd(), 0u32) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd
    }

    fn can_redirect(&self) -> bool {
        false
    }

    fn hides_processes(&self) -> bool {
        false
    }

    fn cuts_network(&self) -> bool {
        *ABI >= NET_ABI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test, as the file system has it.
    fn fresh(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    /// The rules leave the hidden folder out by covering what's beside it,
    /// and each folder above it, entry by entry; the writable folder inside
    /// it gets its own; a link next to it gets none.
    #[test]
    fn the_rules_go_around_a_hidden_folder() {
        let root = fresh("llrules");
        let (home, secret, cache) = (
            root.join("home"),
            root.join("home/.ssh"),
            root.join("home/.cache"),
        );
        let work = secret.join("work");
        for d in [&work, &cache] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".bashrc"), "").unwrap();
        std::os::unix::fs::symlink(&secret, home.join("keys")).unwrap();
        let c = Confine {
            view: work.clone(),
            project: work.clone(),
            hide: vec![secret.clone(), root.join("gone")],
            writable: vec![cache.clone(), work.clone()],
            ..Confine::default()
        };
        let rules = grants(&c);
        let has = |p: &Path, g: Grant| rules.iter().any(|(q, h)| q == p && *h == g);
        let names = |p: &Path| rules.iter().any(|(q, _)| q == p);
        // No rule on a folder above the hidden one, nor on it or the link.
        for above in [Path::new("/"), root.as_path(), home.as_path()] {
            assert!(!names(above), "{}", above.display());
        }
        assert!(!names(&secret) && !names(&home.join("keys")));
        // What's beside it is read; the system beside the test's folders too.
        assert!(has(&home.join(".bashrc"), Grant::Read));
        assert!(has(&cache, Grant::Read) && has(&cache, Grant::Write));
        assert!(has(Path::new("/usr"), Grant::Read));
        // The writable folder inside the hidden one is the command's.
        assert!(has(&work, Grant::Write));
        assert!(has(Path::new("/dev/null"), Grant::Device));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Runs `script` under `c` in `cwd`: its output, or None where this
    /// kernel has no Landlock to use (the test is then skipped).
    fn run(c: &Confine, cwd: &Path, script: &str) -> Option<std::process::Output> {
        if !super::super::works(&Landlock, c) {
            eprintln!("Landlock can't confine commands here: skipped");
            return None;
        }
        Some(
            Landlock
                .command(c, "/bin/sh", &["-c", script])
                .current_dir(cwd)
                .output()
                .unwrap(),
        )
    }

    fn policy(root: &Path, hide: &[&Path], writable: &[&Path]) -> Confine {
        Confine {
            view: root.to_path_buf(),
            project: root.to_path_buf(),
            hide: hide.iter().map(|p| p.to_path_buf()).collect(),
            writable: writable.iter().map(|p| p.to_path_buf()).collect(),
            ..Confine::default()
        }
    }

    /// A hidden folder can't be read, listed or written; a writable folder
    /// inside it is the command's (a daemon's worktrees in its own folder),
    /// and what's hidden inside another writable one stays hidden. The rest
    /// of the system reads as before.
    #[test]
    fn a_hidden_folder_is_out_of_reach_but_for_a_writable_one_inside() {
        let root = fresh("llhide");
        let daemon = root.join("daemon");
        let work = daemon.join("work");
        let cache = root.join("cache");
        let secret = cache.join(".secret");
        for d in [&work, &secret] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(daemon.join("keys.json"), "TOPSECRET\n").unwrap();
        std::fs::write(secret.join("token"), "TOKEN\n").unwrap();
        std::fs::write(cache.join("entry"), "cached\n").unwrap();
        std::fs::write(work.join("old.rs"), "fn old() {}\n").unwrap();
        let c = policy(&work, &[&daemon, &secret], &[&work, &cache]);
        let script = format!(
            "cat {d}/keys.json 2>/dev/null || echo no-keys; \
             ls {d} >/dev/null 2>&1 || echo no-list; \
             echo x > {d}/planted 2>/dev/null || echo no-plant; \
             cat {s}/token 2>/dev/null || echo no-token; \
             echo more >> {c}/entry && echo cache-written; \
             mv {w}/old.rs {w}/new.rs && echo moved; \
             head -c 4 /etc/passwd >/dev/null && echo system-read",
            d = daemon.display(),
            s = secret.display(),
            c = cache.display(),
            w = work.display()
        );
        let Some(out) = run(&c, &work, &script) else {
            return;
        };
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "no-keys\nno-list\nno-plant\nno-token\ncache-written\nmoved\nsystem-read\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!daemon.join("planted").exists());
        assert!(work.join("new.rs").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The system is read-only but for the writable folders and the
    /// devices: nothing written, truncated or made elsewhere; a file moves
    /// from one writable folder to another.
    #[test]
    fn the_system_is_read_only_but_for_the_writable_folders() {
        let root = fresh("llwrite");
        let (a, b, other) = (root.join("a"), root.join("b"), root.join("other"));
        for d in [&a, &b, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(other.join("kept"), "kept\n").unwrap();
        let c = policy(&a, &[], &[&a, &b]);
        let script = format!(
            "echo x > {a}/f && echo written; \
             mv {a}/f {b}/f && echo moved-across; \
             echo x > {o}/no 2>/dev/null || echo other-read-only; \
             truncate -s 0 {o}/kept 2>/dev/null || echo no-truncate; \
             mkdir {o}/d 2>/dev/null || echo no-mkdir; \
             echo gone > /dev/null && echo devnull",
            a = a.display(),
            b = b.display(),
            o = other.display()
        );
        let Some(out) = run(&c, &a, &script) else {
            return;
        };
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "written\nmoved-across\nother-read-only\nno-truncate\nno-mkdir\ndevnull\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(b.join("f").exists());
        assert_eq!(
            std::fs::read_to_string(other.join("kept")).unwrap(),
            "kept\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The command can't read the app's memory or environment (where the
    /// provider keys are), has no capabilities even run by root, and its
    /// exit code is its own.
    #[test]
    fn the_app_is_out_of_reach_and_no_capability_is_left() {
        let root = fresh("llapp");
        let c = policy(&root, &[], &[&root]);
        let script = format!(
            "cat /proc/{pid}/environ >/dev/null 2>&1 || echo no-environ; \
             grep ^CapEff /proc/self/status | tr -s '\\t' ' '; exit 3",
            pid = std::process::id()
        );
        let Some(out) = run(&c, &root, &script) else {
            return;
        };
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "no-environ\nCapEff: 0000000000000000\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P01 (evolve-plan): a shown folder inside a hidden one gets a rule
    /// to read; the rest of the hidden folder stays out of reach, and the
    /// shown one isn't written.
    #[test]
    fn a_shown_folder_inside_a_hidden_one_reads_but_isnt_written() {
        let root = fresh("llshown");
        let cache = root.join("cache");
        let (venv, repos, work) = (cache.join("venv"), cache.join("repos"), root.join("work"));
        for d in [&venv, &repos, &work] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(venv.join("pytest"), "tool\n").unwrap();
        std::fs::write(repos.join("answer"), "ANSWER\n").unwrap();
        let c = Confine {
            show: vec![venv.clone()],
            ..policy(&work, &[&cache], &[&work])
        };
        assert!(grants(&c).contains(&(venv.clone(), Grant::Read)));
        let script = format!(
            "cat {v}/pytest; cat {r}/answer 2>/dev/null || echo no-answer; \
             echo x > {v}/planted 2>/dev/null || echo read-only",
            v = venv.display(),
            r = repos.display()
        );
        let Some(out) = run(&c, &work, &script) else {
            return;
        };
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "tool\nno-answer\nread-only\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!venv.join("planted").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P01: offline (version 4 on), no TCP connection is made, to this
    /// machine's loopback either; with the network, it is.
    #[test]
    fn an_offline_command_makes_no_connection() {
        let root = fresh("lloffline");
        let online = policy(&root, &[], &[&root]);
        if !Path::new("/bin/bash").exists() || !super::super::works(&Landlock, &online) {
            eprintln!("Landlock or bash missing here: skipped");
            return;
        }
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let connects = |c: &Confine| {
            let script = format!(
                "(exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && echo connected || echo refused"
            );
            let out = Landlock
                .command(c, "/bin/bash", &["-c", &script])
                .current_dir(&root)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert_eq!(connects(&online), "connected\n");
        let offline = Confine {
            offline: true,
            ..online.clone()
        };
        if Landlock.cuts_network() {
            assert_eq!(connects(&offline), "refused\n");
        } else {
            // Too old to cut it: the command doesn't run at all.
            assert!(!super::super::works(&Landlock, &offline));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing hidden: one rule reads the whole system.
    #[test]
    fn with_nothing_hidden_one_rule_reads_everything() {
        let root = fresh("llplain");
        let c = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: Vec::new(),
            writable: vec![root.clone()],
            ..Confine::default()
        };
        let rules = grants(&c);
        assert_eq!(rules[0], (PathBuf::from("/"), Grant::Read));
        assert!(rules.contains(&(root.clone(), Grant::Write)));
        let _ = std::fs::remove_dir_all(&root);
    }
}

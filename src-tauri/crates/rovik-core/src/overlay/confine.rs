//! The sandbox the agent's commands run in.
//!
//! A command the agent runs is the user's own process: whatever the
//! permission rules let through would run with every file the user can
//! open, and a text check is no match for a misled model. So each command
//! runs under a [`Confine`] *policy*:
//!
//! - **The system is read-only** but for the command's own folders
//!   (`writable`): the chat's view or worktree, its build folder, `/tmp`
//!   and the toolchains' caches under the home folder.
//! - **The credential stores are empty** (`hide`): the provider keys
//!   (`~/.rovik`, keys.json on a server) and the user's SSH, cloud and
//!   signing credentials ([`secret_dirs`]).
//! - **The redirect**: an isolated chat's commands see its view at the
//!   project's own path too, so one that names the project by its full
//!   path (`cd /home/me/app`, a build script) still stays in the chat.
//!
//! The policy is the same on every operating system. A [`Mechanism`] is how
//! one OS enforces it, and each OS module ([`linux`], [`macos`],
//! [`windows`]) lists its mechanisms in order of preference; the first that
//! works here, found once, confines every command. Linux has bubblewrap
//! (`bwrap`), which also hides the other processes, ends whatever a
//! command leaves behind when it exits, and drops every capability, so a
//! command can't undo its sandbox; the app offers to install it
//! ([`status`], [`install`]). macOS has Seatbelt (`sandbox-exec`), part of
//! the system, which can't redirect a path: an isolated chat's commands
//! there see the project read-only instead of the chat's view at its path,
//! and the callers keep their text check for that (chats.rs, server.rs). On
//! both the sandbox is required: without it commands are refused
//! ([`refusal`]). Windows has none: its chats' commands run in WSL, under
//! bubblewrap. `ROVIK_CONFINE_COMMANDS=0` turns it all off, for
//! troubleshooting.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};

pub use crate::vfs::driver::InstallOutcome;

/*
 * ---------------------------------------------------------------------------
 * The policy: what a command may see
 * ---------------------------------------------------------------------------
 */

/// What a confined command may see. The project's path shows the chat's
/// `view` (no redirect when the two are the same: the command already runs
/// in its own copy, as on a server), the directories in `hide` are empty to
/// it, and it may write only under those in `writable`.
#[derive(Clone, Debug)]
pub struct Confine {
    pub view: PathBuf,
    pub project: PathBuf,
    /// Directories made inaccessible to the command.
    pub hide: Vec<PathBuf>,
    /// Directories the command may write to; the rest is read-only.
    pub writable: Vec<PathBuf>,
}

impl Confine {
    /// An isolated chat's command: the project's path shows its view, and
    /// it may write to the view and the usual folders ([`writable_dirs`]).
    pub fn redirecting(view: PathBuf, project: PathBuf) -> Confine {
        Confine {
            writable: writable_dirs(&view),
            hide: secret_dirs(),
            view,
            project,
        }
    }

    /// A command that already runs in its own copy (a server worktree, or a
    /// chat that works in the project directly): nothing is redirected.
    pub fn hiding(root: PathBuf) -> Confine {
        Confine {
            writable: writable_dirs(&root),
            hide: secret_dirs(),
            view: root.clone(),
            project: root,
        }
    }

    /// With `dir` writable too (a chat's build folder).
    pub fn also_writable(mut self, dir: PathBuf) -> Confine {
        if !self.writable.contains(&dir) {
            self.writable.push(dir);
        }
        self
    }

    /// Whether the project's path is pointed at the view.
    pub fn redirects(&self) -> bool {
        self.view != self.project
    }
}

/// Credential directories the app registers on top of the built-in list
/// (its own secret stores that aren't under `~/.rovik`).
static PROTECTED: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Default::default);

/// Whether the app's commands may run only sandboxed: set by the app at its
/// start (`require`). On Linux they may only ever run so; on Windows, which
/// has no sandbox of its own, this is what keeps them in WSL.
static REQUIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The app's commands run sandboxed or not at all, on every system: where
/// there's no sandbox here (Windows), none of them runs here. The desktop
/// app calls it at start; the library's own callers (tests, tools) run
/// commands as the system allows.
pub fn require() {
    REQUIRED.store(true, std::sync::atomic::Ordering::Relaxed);
}

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

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
}

/// Those of `dirs` that exist, each as the file system has it, once.
fn existing(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = dirs
        .into_iter()
        .filter_map(|d| d.canonicalize().ok())
        .filter(|d| d.is_dir())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The credential directories a chat's commands are kept out of: the app's
/// own provider keys (`~/.rovik`, keys.json on a server) and the user's
/// SSH, cloud, container and signing credentials, plus whatever the app
/// registered with [`protect`]. Only those that exist right now. A command
/// that legitimately needs one of these (network git over SSH, a cloud
/// deploy) is one an isolated or server chat refuses anyway, and the user's
/// own terminal isn't confined.
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
        // macOS: the keychains' files.
        "Library/Keychains",
    ];
    let under_home = home()
        .into_iter()
        .flat_map(|home| UNDER_HOME.iter().map(move |rel| home.join(rel)));
    let registered = PROTECTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    existing(under_home.chain(registered))
}

/// Where a command run in `root` may write: `root` itself, the temporary
/// folders, and the toolchains' caches and user installs under the home
/// folder (what a build or a test run writes besides the project). Only
/// those that exist right now. Everything else is read-only to it.
pub fn writable_dirs(root: &Path) -> Vec<PathBuf> {
    const UNDER_HOME: &[&str] = &[
        ".cache",
        ".cargo",
        ".rustup",
        ".npm",
        ".yarn",
        ".bun",
        ".deno",
        ".m2",
        ".gradle",
        ".nuget",
        ".local/bin",
        ".local/lib",
        ".local/share/pnpm",
        ".local/share/uv",
        ".local/state",
        "go",
        // macOS: the caches (Homebrew, pip, clang's modules) and Xcode's
        // build products.
        "Library/Caches",
        "Library/Developer/Xcode/DerivedData",
    ];
    let mut dirs = vec![root.to_path_buf(), std::env::temp_dir()];
    dirs.extend(std::env::var_os("TMPDIR").map(PathBuf::from));
    // The system's too where `TMPDIR` is elsewhere (a daemon's own, K13):
    // commands write to `/tmp` by name as often as through `TMPDIR`.
    if cfg!(unix) {
        dirs.push(PathBuf::from("/tmp"));
    }
    dirs.extend(
        home()
            .into_iter()
            .flat_map(|home| UNDER_HOME.iter().map(move |rel| home.join(rel))),
    );
    // macOS: the user's cache folder beside `TMPDIR` (/var/folders/…/C),
    // where the developer tools keep theirs.
    if cfg!(target_os = "macos") {
        dirs.extend(std::env::temp_dir().parent().map(|d| d.join("C")));
    }
    existing(dirs)
}

/*
 * ---------------------------------------------------------------------------
 * The mechanisms: how an operating system enforces the policy
 * ---------------------------------------------------------------------------
 */

/// One way this operating system enforces a [`Confine`] on a command. To
/// add one: implement it in the OS's module and list it in that module's
/// `MECHANISMS`, in order of preference. Nothing else changes.
trait Mechanism: Sync {
    /// Its name: what `ROVIK_CONFINE_COMMANDS` picks it by, and logs say.
    fn name(&self) -> &'static str;

    /// `program` with `args`, confined by `c`. A mechanism that can't
    /// redirect ignores `c.view`; [`available`] then says so for a
    /// redirecting policy, and its callers keep their text check.
    fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command;

    /// Whether it can point the project's path at the view. A mechanism
    /// that only hides (macOS's Seatbelt) says false.
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

/// Whether confinement is turned off (`ROVIK_CONFINE_COMMANDS=0`).
fn switched_off() -> bool {
    std::env::var("ROVIK_CONFINE_COMMANDS").as_deref() == Ok("0")
}

/// This OS's mechanisms, in order of preference, as `ROVIK_CONFINE_COMMANDS`
/// leaves them: all of them, one by name, or none (`0`).
fn candidates() -> Vec<&'static dyn Mechanism> {
    if switched_off() {
        return Vec::new();
    }
    let all: Vec<&'static dyn Mechanism> = os::MECHANISMS.to_vec();
    match std::env::var("ROVIK_CONFINE_COMMANDS").as_deref() {
        Ok(name) if all.iter().any(|m| m.name() == name) => {
            all.into_iter().filter(|m| m.name() == name).collect()
        }
        _ => all,
    }
}

/// Whether `mechanism` confines a command here: one that does nothing, run
/// under `c`. Kernels and security modules may forbid what it needs, or its
/// program may not be installed.
fn works(mechanism: &dyn Mechanism, c: &Confine) -> bool {
    let (program, args) = os::PROBE;
    let mut cmd = mechanism.command(c, program, args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// What the probe runs under: the process's temporary folder as the root.
fn probe_policy() -> Confine {
    Confine::hiding(std::env::temp_dir())
}

/// What was found, once (None: not looked yet).
static CHOSEN: Mutex<Option<Option<&'static dyn Mechanism>>> = Mutex::new(None);

/// The mechanism that confines commands here: the first candidate that
/// works, found once (again after [`forget`]).
fn chosen() -> Option<&'static dyn Mechanism> {
    let mut cache = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    *cache.get_or_insert_with(|| {
        let policy = probe_policy();
        let found = candidates().into_iter().find(|m| works(*m, &policy));
        log::info!(
            "the agent's commands are confined by: {:?}",
            found.map(|m| m.name())
        );
        found
    })
}

/// Forgets what was found, so the next command looks again: after the
/// sandbox's program was installed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn forget() {
    *CHOSEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/*
 * ---------------------------------------------------------------------------
 * What the callers use
 * ---------------------------------------------------------------------------
 */

/// Whether `c` can be enforced here: a mechanism works, and it redirects
/// if `c` asks for that.
pub fn available(c: &Confine) -> bool {
    chosen().is_some_and(|m| !c.redirects() || m.can_redirect())
}

/// `program` with `args`, confined by `c`. As it is where `c` can't be
/// enforced, which callers ask first ([`available`], [`refusal`]).
pub fn command(c: &Confine, program: &str, args: &[&str]) -> Command {
    match chosen().filter(|_| available(c)) {
        Some(m) => m.command(c, program, args),
        None => {
            let mut cmd = crate::process::command(program);
            cmd.args(args);
            cmd
        }
    }
}

/// The policy for a command with no overlay (a server worktree, a chat that
/// works in the project directly) — where it can be enforced here.
pub fn hiding_if_available(root: &Path) -> Option<Confine> {
    let c = Confine::hiding(root.to_path_buf());
    available(&c).then_some(c)
}

/// Why no command of the agent's may run here, if none may: on Linux and
/// macOS the sandbox is required, and its program is missing or can't make
/// one; in the Windows app (`require`) commands run in the computer's Linux
/// side (WSL), never here. None where the sandbox works, where it's turned
/// off, and on systems without a mechanism (their text checks apply).
pub fn refusal() -> Option<String> {
    refusal_if(REQUIRED.load(std::sync::atomic::Ordering::Relaxed))
}

/// `refusal`, the app having required the sandbox (`required`) or not.
#[cfg_attr(not(windows), allow(unused_variables))]
fn refusal_if(required: bool) -> Option<String> {
    if switched_off() {
        return None;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if chosen().is_none() {
        return Some(os::missing());
    }
    #[cfg(target_os = "windows")]
    if required {
        return Some(windows::MISSING.into());
    }
    None
}

/// The sandbox on this machine, for the app's settings and its offer to
/// install it at start.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatus {
    /// Commands are refused without it (Linux, macOS).
    pub required: bool,
    /// What makes it: "bubblewrap" on Linux, "Seatbelt" on macOS; "" where
    /// there's none.
    pub program: &'static str,
    /// It's there and confines commands.
    pub working: bool,
    /// What's missing or wrong, for the user.
    pub detail: Option<String>,
    /// The app can install it (otherwise `detail` says how).
    pub can_install: bool,
}

pub fn status() -> SandboxStatus {
    os::status()
}

/// Installs the sandbox's program, asking the way the OS does (polkit's
/// dialog on Linux), and looks again.
pub async fn install() -> Result<InstallOutcome, String> {
    os::install().await
}

/*
 * ---------------------------------------------------------------------------
 * Linux: bubblewrap
 * ---------------------------------------------------------------------------
 */

/// Each command runs under `bwrap` in namespaces of its own: the system
/// read-only, the writable folders bound over it, the chat's view over the
/// project's path, an empty tmpfs over each hidden directory, a pid
/// namespace (it sees no other processes), fresh `/dev` and `/proc`, every
/// capability dropped (so it can't undo any of this), and it dies with the
/// app. The distro's package, which the .deb and .rpm depend on and the
/// host installer adds: on Ubuntu 24.04 it's the package's AppArmor profile
/// that lets anything make a user namespace at all.
#[cfg(target_os = "linux")]
mod linux {
    use super::{Confine, InstallOutcome, Mechanism, SandboxStatus};
    use crate::distro::{self, Package, RootRun};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);
    pub(super) static MECHANISMS: &[&dyn Mechanism] = &[&Bwrap];

    const PACKAGE: Package = Package {
        name: "bubblewrap",
        gentoo: "sys-apps/bubblewrap",
    };
    const PROGRAM: &str = "bwrap";

    pub(super) struct Bwrap;

    impl Mechanism for Bwrap {
        fn name(&self) -> &'static str {
            PROGRAM
        }

        fn command(&self, c: &Confine, program: &str, args: &[&str]) -> Command {
            // bwrap applies the mounts in order, each over the ones before:
            // the writable folders over the read-only system, the view over
            // the project's path, the hidden directories over all. A folder
            // gone since the policy was made is skipped: bwrap would refuse
            // to start otherwise. The working folder is kept (found again
            // in the namespace).
            let mut cmd = crate::process::command(PROGRAM);
            cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
            for dir in c.writable.iter().filter(|d| d.is_dir()) {
                cmd.arg("--bind").arg(dir).arg(dir);
            }
            if c.redirects() {
                cmd.arg("--bind").arg(&c.view).arg(&c.project);
            }
            for dir in c.hide.iter().filter(|d| d.is_dir()) {
                cmd.arg("--tmpfs").arg(dir);
            }
            // A writable folder inside a hidden one stays the command's
            // (`--data` puts a daemon's worktrees in its own folder), and
            // what's hidden inside it stays hidden.
            let inside = |d: &Path, of: &[PathBuf]| of.iter().any(|o| d != o && d.starts_with(o));
            let back: Vec<PathBuf> = c
                .writable
                .iter()
                .filter(|d| d.is_dir() && inside(d, &c.hide))
                .cloned()
                .collect();
            for dir in &back {
                cmd.arg("--bind").arg(dir).arg(dir);
            }
            for dir in c.hide.iter().filter(|d| d.is_dir() && inside(d, &back)) {
                cmd.arg("--tmpfs").arg(dir);
            }
            // Run by root, bwrap keeps the capabilities unless told; run by
            // a user it drops them anyway.
            cmd.args(["--unshare-pid", "--die-with-parent", "--cap-drop", "ALL"]);
            cmd.arg("--").arg(program).args(args);
            cmd
        }

        fn can_redirect(&self) -> bool {
            true
        }
    }

    /// `bwrap --version`'s first line.
    fn version() -> Option<String> {
        let out = crate::process::command(PROGRAM)
            .arg("--version")
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
    }

    pub(super) fn status() -> SandboxStatus {
        let working = super::chosen().is_some();
        let installed = distro::which(PROGRAM).is_some();
        let detail = match (working, installed) {
            (true, _) => None,
            (false, true) => Some(
                "bubblewrap is installed but can't make a sandbox here (the kernel or a \
                 security module may forbid user namespaces)."
                    .to_string(),
            ),
            (false, false) => Some("bubblewrap isn't installed.".to_string()),
        };
        SandboxStatus {
            required: true,
            program: PACKAGE.name,
            working,
            detail,
            can_install: !installed && distro::install_script(&PACKAGE).is_some(),
        }
    }

    pub(super) async fn install() -> Result<InstallOutcome, String> {
        let Some(script) = distro::install_script(&PACKAGE) else {
            return Ok(InstallOutcome::Manual {
                message: "No supported package manager was found. Install bubblewrap (the \
                          package is called bubblewrap nearly everywhere)."
                    .into(),
                command: None,
            });
        };
        if let RootRun::Manual { message, command } =
            distro::run_as_root(&format!("set -e; {script}"), "Installing bubblewrap").await?
        {
            return Ok(InstallOutcome::Manual {
                message,
                command: Some(command),
            });
        }
        super::forget();
        let now = status();
        match now.working {
            true => Ok(InstallOutcome::Installed { version: version() }),
            false => Ok(InstallOutcome::Manual {
                message: now.detail.unwrap_or_default(),
                command: None,
            }),
        }
    }

    /// What a command is refused with while there's no sandbox.
    pub(super) fn missing() -> String {
        let how = match distro::which(PROGRAM) {
            Some(_) => {
                "it's installed but can't make a sandbox here (the kernel or a security \
                        module may forbid user namespaces)"
            }
            None => {
                "it isn't installed: the app offers to install it at start, or install the \
                     bubblewrap package yourself"
            }
        };
        format!(
            "Error: on Linux the agent's commands run in a bubblewrap sandbox, and {how}. \
             Then try again."
        )
    }
}

/*
 * ---------------------------------------------------------------------------
 * macOS and Windows: nothing yet
 * ---------------------------------------------------------------------------
 */

/// A system with no mechanism yet: the callers' text checks apply, nothing
/// is required, nothing can be installed.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn no_sandbox_yet() -> SandboxStatus {
    SandboxStatus {
        required: false,
        program: "",
        working: false,
        detail: Some("There's no sandbox for the agent's commands on this system yet.".into()),
        can_install: false,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn nothing_to_install() -> Result<InstallOutcome, String> {
    Ok(InstallOutcome::Manual {
        message: "There's no sandbox to install on this system yet.".into(),
        command: None,
    })
}

/*
 * ---------------------------------------------------------------------------
 * macOS: Seatbelt
 * ---------------------------------------------------------------------------
 */

/// The devices a command writes to as a terminal program does: its output
/// thrown away, its terminal, its own descriptors, DTrace's helper (which
/// node and the JVM open).
const SEATBELT_DEVICES: &[&str] = &[
    r#"(literal "/dev/null")"#,
    r#"(literal "/dev/zero")"#,
    r#"(literal "/dev/dtracehelper")"#,
    r#"(literal "/dev/ptmx")"#,
    r#"(regex #"^/dev/tty")"#,
    r#"(regex #"^/dev/fd/")"#,
];

/// The Seatbelt profile (macOS's sandbox language) that enforces `c`, and
/// the paths it names, as `sandbox-exec -D` parameters: a path never goes
/// into the profile's text, so none can break it. Everything is allowed
/// but what the policy forbids: writes outside the writable folders and
/// those devices, and anything under a hidden folder, but for the writable
/// folders inside one (a daemon's worktrees in its own folder, `--data`).
/// The folders are as the file system has them: Seatbelt sees `/tmp` as
/// `/private/tmp`. Nothing is redirected: Seatbelt can't.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(c: &Confine) -> (String, Vec<(String, PathBuf)>) {
    let writable: Vec<&PathBuf> = c.writable.iter().filter(|d| d.is_dir()).collect();
    let hidden: Vec<&PathBuf> = c.hide.iter().filter(|d| d.is_dir()).collect();
    let mut params: Vec<(String, PathBuf)> = Vec::new();
    for (i, dir) in writable.iter().enumerate() {
        params.push((format!("W{i}"), dir.to_path_buf()));
    }
    for (i, dir) in hidden.iter().enumerate() {
        params.push((format!("H{i}"), dir.to_path_buf()));
    }
    let under = |name: String| format!(r#"(subpath (param "{name}"))"#);
    let any = |filters: Vec<String>| format!("(require-any {})", filters.join(" "));

    let mut profile = String::from("(version 1)\n(allow default)\n");
    let mut writes: Vec<String> = (0..writable.len())
        .map(|i| under(format!("W{i}")))
        .collect();
    writes.extend(SEATBELT_DEVICES.iter().map(|d| d.to_string()));
    profile += &format!("(deny file-write* (require-not {}))\n", any(writes));
    for (i, dir) in hidden.iter().enumerate() {
        let inside: Vec<String> = writable
            .iter()
            .enumerate()
            .filter(|(_, w)| *w != dir && w.starts_with(dir))
            .map(|(j, _)| under(format!("W{j}")))
            .collect();
        let filter = if inside.is_empty() {
            under(format!("H{i}"))
        } else {
            format!(
                "(require-all {} (require-not {}))",
                under(format!("H{i}")),
                any(inside)
            )
        };
        profile += &format!("(deny file-read* file-write* {filter})\n");
    }
    (profile, params)
}

/// Each command runs under `sandbox-exec` with the policy's profile
/// ([`seatbelt_profile`]): the system read-only but for the writable
/// folders, the hidden ones out of reach. It's part of macOS, so there's
/// nothing to install; it's deprecated as a command, but the sandbox it
/// applies is the one macOS's own services run in. It can't redirect a
/// path, so an isolated chat's commands get the project read-only, not its
/// view there. Unlike bubblewrap it doesn't hide the other processes.
#[cfg(target_os = "macos")]
mod macos {
    use super::{InstallOutcome, Mechanism, SandboxStatus};
    use std::ffi::OsString;
    use std::process::Command;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);
    pub(super) static MECHANISMS: &[&dyn Mechanism] = &[&Seatbelt];

    const PROGRAM: &str = "/usr/bin/sandbox-exec";

    pub(super) struct Seatbelt;

    impl Mechanism for Seatbelt {
        fn name(&self) -> &'static str {
            "seatbelt"
        }

        fn command(&self, c: &super::Confine, program: &str, args: &[&str]) -> Command {
            let (profile, params) = super::seatbelt_profile(c);
            let mut cmd = crate::process::command(PROGRAM);
            cmd.arg("-p").arg(profile);
            for (name, path) in params {
                let mut define = OsString::from(format!("{name}="));
                define.push(path);
                cmd.arg("-D").arg(define);
            }
            cmd.arg(program).args(args);
            cmd
        }

        fn can_redirect(&self) -> bool {
            false
        }
    }

    pub(super) fn missing() -> String {
        "macOS's sandbox (sandbox-exec) couldn't confine a command here, so the agent's \
         commands are refused. It's part of macOS: if this Mac's management policy blocks \
         it, ask whoever manages this Mac."
            .into()
    }

    pub(super) fn status() -> SandboxStatus {
        let working = super::chosen().is_some();
        SandboxStatus {
            required: true,
            program: "Seatbelt",
            working,
            detail: (!working).then(missing),
            can_install: false,
        }
    }

    pub(super) async fn install() -> Result<InstallOutcome, String> {
        Ok(InstallOutcome::Manual {
            message: "Seatbelt is part of macOS: there's nothing to install.".into(),
            command: None,
        })
    }
}

/// No mechanism yet. The one to add is a restricted token or an
/// AppContainer profile for the command's process, with the hidden
/// directories denied and the writable folders allowed by ACL (and the job
/// object command_tool.rs already puts the process in). Windows can't
/// bind-mount either, so it would say `can_redirect() == false`. Note that
/// command_tool.rs builds the Windows shell line with `raw_arg` for cmd.exe
/// (`/S /C "…"`); a mechanism here gets that line as `args` and must pass
/// it on the same way.
#[cfg(windows)]
/// Windows has no sandbox of its own: a chat of a project runs in the WSL
/// distribution the app sets up (the client's `Remote::go_home`), where
/// Rovik's daemon confines its commands with bubblewrap, and a chat that
/// runs here, without a project, is refused its commands.
mod windows {
    use super::{InstallOutcome, Mechanism, SandboxStatus};

    pub(super) const PROBE: (&str, &[&str]) = ("cmd.exe", &["/C", "exit", "0"]);
    pub(super) static MECHANISMS: &[&dyn Mechanism] = &[];

    pub(super) const MISSING: &str = "On Windows the agent's commands run sandboxed in this \
        computer's Linux side (WSL), which a chat of a project moves to before it runs: open the \
        chat's project, and set WSL up in Settings → This computer → WSL if it isn't.";

    pub(super) fn status() -> SandboxStatus {
        SandboxStatus {
            required: true,
            program: "WSL",
            working: false,
            detail: Some(MISSING.into()),
            can_install: false,
        }
    }

    pub(super) async fn install() -> Result<InstallOutcome, String> {
        super::nothing_to_install().await
    }
}

/// Any other Unix: no mechanism.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod other {
    use super::{InstallOutcome, Mechanism, SandboxStatus};

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);
    pub(super) static MECHANISMS: &[&dyn Mechanism] = &[];

    pub(super) fn status() -> SandboxStatus {
        super::no_sandbox_yet()
    }

    pub(super) async fn install() -> Result<InstallOutcome, String> {
        super::nothing_to_install().await
    }
}

/// The Seatbelt profile's text, on every system; macOS runs it below.
#[cfg(test)]
mod seatbelt_profile_tests {
    use super::*;

    #[test]
    fn the_profile_names_its_folders_by_parameter_and_lets_the_nested_one_through() {
        let root = std::env::temp_dir().join(format!("sbpl-{}", std::process::id()));
        let (daemon, work, secret) = (root.join("d"), root.join("d/work"), root.join("s"));
        for d in [&work, &secret] {
            std::fs::create_dir_all(d).unwrap();
        }
        let c = Confine {
            view: work.clone(),
            project: work.clone(),
            hide: vec![daemon.clone(), secret.clone(), root.join("gone")],
            writable: vec![work.clone(), root.join("gone-too")],
        };
        let (profile, params) = seatbelt_profile(&c);
        let names: Vec<(&str, &Path)> = params
            .iter()
            .map(|(n, p)| (n.as_str(), p.as_path()))
            .collect();
        // Folders gone since the policy was made are left out.
        assert_eq!(
            names,
            [
                ("W0", work.as_path()),
                ("H0", daemon.as_path()),
                ("H1", secret.as_path())
            ]
        );
        assert!(
            !profile.contains(&*root.to_string_lossy()),
            "no path in the text"
        );
        assert!(profile.starts_with("(version 1)\n(allow default)\n"));
        assert!(profile.contains(
            r#"(deny file-write* (require-not (require-any (subpath (param "W0")) (literal "/dev/null")"#
        ));
        assert!(profile.contains(
            r#"(deny file-read* file-write* (require-all (subpath (param "H0")) (require-not (require-any (subpath (param "W0"))))))"#
        ));
        assert!(profile.contains(r#"(deny file-read* file-write* (subpath (param "H1")))"#));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Seatbelt enforcing the policy, on macOS.
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;

    const SEATBELT: &dyn Mechanism = &macos::Seatbelt;

    /// A fresh folder for one test, as the file system has it (Seatbelt
    /// sees /var/folders as /private/var/folders).
    fn fresh(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    fn run(c: &Confine, cwd: &Path, script: &str) -> std::process::Output {
        SEATBELT
            .command(c, "/bin/sh", &["-c", script])
            .current_dir(cwd)
            .output()
            .unwrap()
    }

    fn stdout(out: &std::process::Output) -> String {
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// macOS always has it, and it confines commands here.
    #[test]
    fn seatbelt_is_the_mechanism_and_works() {
        assert!(
            works(SEATBELT, &probe_policy()),
            "sandbox-exec refused the probe"
        );
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_none() {
            assert_eq!(chosen().map(|m| m.name()), Some("seatbelt"));
            assert_eq!(refusal(), None);
            assert!(status().required && status().working);
        }
    }

    /// The command writes in its folder and to /dev/null, and nowhere
    /// else; its exit code is its own.
    #[test]
    fn only_the_writable_folders_are_written() {
        let root = fresh("sbwrite");
        let (mine, other) = (root.join("mine"), root.join("other"));
        for d in [&mine, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let c = Confine {
            view: mine.clone(),
            project: mine.clone(),
            hide: Vec::new(),
            writable: vec![mine.clone()],
        };
        let script = format!(
            "echo ok > {m}/a.txt && echo gone > /dev/null && \
             (echo no > {o}/b.txt) 2>/dev/null; cat {m}/a.txt; exit 3",
            m = mine.display(),
            o = other.display()
        );
        let out = run(&c, &mine, &script);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout(&out), "ok\n");
        assert!(!other.join("b.txt").exists(), "wrote outside its folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A hidden folder can't be read, listed or written, but for a
    /// writable folder inside it; one hidden inside a writable folder is.
    #[test]
    fn hidden_folders_are_out_of_reach_but_for_writable_ones_inside() {
        let root = fresh("sbhide");
        let (daemon, secret) = (root.join("daemon"), root.join("secret"));
        let work = daemon.join("work");
        let deep = work.join(".ssh");
        for d in [&work, &secret, &deep] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(secret.join("key"), "TOPSECRET\n").unwrap();
        std::fs::write(daemon.join("keys.json"), "TOPSECRET\n").unwrap();
        std::fs::write(deep.join("id"), "TOPSECRET\n").unwrap();
        let c = Confine {
            view: work.clone(),
            project: work.clone(),
            hide: vec![secret.clone(), daemon.clone(), deep.clone()],
            writable: vec![work.clone(), secret.clone()],
        };
        let script = format!(
            "cat {s}/key {d}/keys.json {w}/.ssh/id 2>/dev/null; \
             ls {s} 2>/dev/null | wc -l | tr -d ' '; \
             echo planted > {s}/planted 2>/dev/null; \
             echo mine > {w}/mine.txt && cat {w}/mine.txt",
            s = secret.display(),
            d = daemon.display(),
            w = work.display()
        );
        let out = run(&c, &work, &script);
        assert_eq!(
            stdout(&out),
            "0\nmine\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!secret.join("planted").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const BWRAP: &dyn Mechanism = &linux::Bwrap;

    /// A fresh root for one test, under the temp folder, and the policy on
    /// it: nothing hidden, only `writable` writable.
    fn fresh(test: &str, writable: &[&Path]) -> (PathBuf, Confine) {
        let root = std::env::temp_dir().join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let c = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: Vec::new(),
            writable: writable.iter().map(|p| p.to_path_buf()).collect(),
        };
        (root, c)
    }

    /// Runs `script` under `c` in `cwd`: its output, or None where bwrap
    /// can't make a sandbox here (the test is then skipped).
    fn run(c: &Confine, cwd: &Path, script: &str) -> Option<std::process::Output> {
        if !works(BWRAP, c) {
            eprintln!("bwrap can't confine commands here: skipped");
            return None;
        }
        Some(
            BWRAP
                .command(c, "/bin/sh", &["-c", script])
                .current_dir(cwd)
                .output()
                .unwrap(),
        )
    }

    fn stdout(out: &std::process::Output) -> String {
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A command that writes to the project's path writes to the view, in
    /// the folder it was started in, its exit code its own.
    #[test]
    fn the_projects_path_shows_the_view() {
        let (root, mut c) = fresh("confine", &[]);
        let (view, project) = (root.join("view"), root.join("project"));
        std::fs::create_dir_all(&view).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "project\n").unwrap();
        std::fs::write(view.join("a.txt"), "view\n").unwrap();
        (c.view, c.project, c.writable) = (view.clone(), project.clone(), vec![view.clone()]);
        let script = format!(
            "cat {0}/a.txt && echo written > {0}/b.txt && pwd && exit 3",
            project.display()
        );
        let Some(out) = run(&c, &view, &script) else {
            return;
        };
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout(&out), format!("view\n{}\n", view.display()));
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

    /// A hidden directory is empty to the command, which can't undo that:
    /// unmounting takes a capability it no longer has. The real directory
    /// is left as it was (the tmpfs is only in the command's namespace).
    #[test]
    fn a_hidden_directory_is_empty_to_the_command() {
        let (root, mut c) = fresh("hide", &[]);
        let secret = root.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("keys.json"), "TOPSECRET\n").unwrap();
        c.hide = vec![secret.clone()];
        c.writable = vec![root.clone()];
        // Read it, unmount it, list it, write into it.
        let script = format!(
            "cat {0}/keys.json 2>/dev/null; umount {0} 2>/dev/null; \
             ls {0} | wc -l; echo planted > {0}/planted 2>/dev/null; true",
            secret.display()
        );
        let Some(out) = run(&c, &root, &script) else {
            return;
        };
        assert!(out.status.success());
        // Nothing read (no TOPSECRET), nothing listed (0 entries).
        assert_eq!(stdout(&out), "0\n");
        assert_eq!(
            std::fs::read_to_string(secret.join("keys.json")).unwrap(),
            "TOPSECRET\n"
        );
        assert!(!secret.join("planted").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A writable folder inside a hidden one is the command's still (a
    /// daemon started with `--data` keeps its worktrees in its own
    /// folder), while the rest of the hidden folder, and what's hidden
    /// inside the writable one, stay hidden.
    #[test]
    fn a_writable_folder_inside_a_hidden_one_stays_writable() {
        let (root, mut c) = fresh("nested", &[]);
        let daemon = root.join("daemon");
        let work = daemon.join("work").join("chat");
        let secret = work.join(".secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(daemon.join("keys.json"), "TOPSECRET\n").unwrap();
        std::fs::write(work.join("old.rs"), "fn old() {}\n").unwrap();
        std::fs::write(secret.join("token"), "TOKEN\n").unwrap();
        c.hide = vec![daemon.clone(), secret.clone()];
        c.writable = vec![work.clone()];
        let script = format!(
            "cat {d}/keys.json 2>/dev/null; cat {s}/token 2>/dev/null; \
             mv {w}/old.rs {w}/new.rs && echo moved",
            d = daemon.display(),
            s = secret.display(),
            w = work.display()
        );
        let Some(out) = run(&c, &work, &script) else {
            return;
        };
        assert_eq!(stdout(&out), "moved\n");
        assert!(work.join("new.rs").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The system is read-only but for the writable folders, the command
    /// sees no other processes, and it has no capabilities.
    #[test]
    fn the_system_is_read_only_but_for_the_allowlist() {
        let (root, mut c) = fresh("readonly", &[]);
        let allowed = root.join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        c.writable = vec![allowed.clone()];
        let script = format!(
            "echo x > {0}/ok 2>/dev/null && echo allowed-written; \
             echo x > {1}/no 2>/dev/null || echo root-read-only; \
             echo x > /usr/rovik-test 2>/dev/null || echo usr-read-only; \
             echo x > /tmp/rovik-test-$$ 2>/dev/null || echo tmp-read-only; \
             echo processes=$(ls /proc | grep -c '^[0-9]'); \
             grep ^CapEff /proc/self/status | tr -s '\\t' ' '",
            allowed.display(),
            root.display()
        );
        let Some(out) = run(&c, &allowed, &script) else {
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            &lines[..4],
            &[
                "allowed-written",
                "root-read-only",
                "usr-read-only",
                "tmp-read-only"
            ],
            "{text}"
        );
        // Its own shell and pipeline, and bwrap's pid 1: no one else's.
        let processes: u32 = lines[4].trim_start_matches("processes=").parse().unwrap();
        assert!(processes <= 6, "{text}");
        assert_eq!(lines[5], "CapEff: 0000000000000000", "{text}");
        assert!(allowed.join("ok").exists());
        assert!(!root.join("no").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The hiding-only policy (a server worktree, a chat in the project
    /// directly) redirects nothing: the command runs where it was started
    /// and can write there.
    #[test]
    fn hiding_only_needs_no_redirect() {
        let (root, _) = fresh("hideonly", &[]);
        let c = Confine::hiding(root.clone());
        assert!(!c.redirects());
        assert!(c.writable.contains(&root.canonicalize().unwrap()));
        let Some(out) = run(&c, &root, "pwd && echo x > here && cat here") else {
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout(&out), format!("{}\nx\n", root.display()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The built-in lists name only folders that exist, as the file system
    /// has them; what the app registers is hidden too.
    #[test]
    fn the_secret_and_writable_dirs_are_the_existing_ones() {
        for dir in secret_dirs()
            .iter()
            .chain(&writable_dirs(&std::env::temp_dir()))
        {
            assert!(dir.is_dir(), "{}", dir.display());
            assert_eq!(dir, &dir.canonicalize().unwrap(), "{}", dir.display());
        }
        let registered = std::env::temp_dir().join(format!("protected-{}", std::process::id()));
        std::fs::create_dir_all(&registered).unwrap();
        protect([registered.clone(), registered.clone()]);
        assert!(secret_dirs().contains(&registered.canonicalize().unwrap()));
        let _ = std::fs::remove_dir_all(&registered);
        assert!(writable_dirs(&std::env::temp_dir())
            .contains(&std::env::temp_dir().canonicalize().unwrap()));
    }

    /// Windows has no sandbox: the app, which requires one, runs no command
    /// here (its chats' commands run in WSL); a caller that doesn't require
    /// it runs them as before.
    #[cfg(windows)]
    #[test]
    fn the_windows_app_runs_no_command_here() {
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_some() {
            return;
        }
        let why = refusal_if(true).unwrap();
        assert!(why.contains("Settings → This computer → WSL"), "{why}");
        assert_eq!(refusal_if(false), None);
        assert!(status().required);
        assert!(!status().working);
    }

    /// Linux has one mechanism, bubblewrap, and the status says whether it
    /// works here; a working sandbox refuses nothing.
    #[test]
    fn bubblewrap_is_the_mechanism() {
        // Read as the process started: tests don't set the variable.
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_none() {
            let names: Vec<&str> = candidates().iter().map(|m| m.name()).collect();
            assert_eq!(names, ["bwrap"]);
        }
        let status = status();
        assert!(status.required);
        assert_eq!(status.program, "bubblewrap");
        assert_eq!(status.working, chosen().is_some());
        assert_eq!(refusal().is_none(), status.working || switched_off());
        if !status.working {
            assert!(status.detail.is_some());
        }
    }
}

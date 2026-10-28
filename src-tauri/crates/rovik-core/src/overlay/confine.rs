//! The sandbox the agent's commands run in.
//!
//! A command the agent runs is the user's own process: whatever the
//! permission rules let through would run with every file the user can
//! open, and a text check is no match for a misled model. So each command
//! runs under a [`Confine`] *policy*:
//!
//! - **The system is read-only** but for the command's own folders
//!   (`writable`): the chat's view or worktree, its build folder, `/tmp`
//!   and the toolchains' caches under the home folder; inside those, what
//!   runs outside the sandbox later stays read-only (`read_only`: the
//!   PATH's bin folders, cargo's config, [`read_only_paths`]).
//! - **The credential stores are empty** (`hide`): the provider keys
//!   (`~/.rovik`, keys.json on a server) and the user's SSH, cloud and
//!   signing credentials, folders and single files (`~/.netrc`, cargo's
//!   token) ([`secret_paths`]).
//! - **The local services are closed** (`sockets`): the agents that sign
//!   with the user's keys (ssh-agent, gpg-agent), the desktop session (X11,
//!   Wayland, D-Bus, which can start programs outside the sandbox) and the
//!   container daemons ([`local_sockets`]). A read-only folder doesn't stop
//!   a connection to a socket in it, so each mechanism closes them its own
//!   way; the variables that name them are left out of the command's
//!   environment ([`SESSION_VARIABLES`]).
//! - **The redirect**: an isolated chat's commands see its view at the
//!   project's own path too, so one that names the project by its full
//!   path (`cd /home/me/app`, a build script) still stays in the chat.
//! - **Tool homes**: a tool that keeps a login in the folder it writes its
//!   other state to (Expo's CLI) is pointed at a folder of its own, by the
//!   variable it reads ([`TOOL_HOMES`]), so it runs without the login.
//! - **The network**: on a team server, the user's private network
//!   ([`network`], Linux), which the command joins before its sandbox's own
//!   steps: its servers' ports are this user's alone. While the user's
//!   allowed hosts are on, the agent's network is sealed but for their
//!   filter, on a desktop too, and on macOS Seatbelt keeps a command's
//!   connections to the loopback (`loopback_only`): allowed_hosts.rs,
//!   docs/design/allowed-hosts-plan.md.
//!
//! The policy is the same on every operating system. A [`Mechanism`] is how
//! one OS enforces it, and each OS module ([`linux`], [`macos`],
//! [`windows`]) lists its mechanisms in order of preference; the first that
//! works here, found once, confines every command. Linux has bubblewrap
//! (`bwrap`), which also hides the other processes, ends whatever a
//! command leaves behind when it exits, and drops every capability, so a
//! command can't undo its sandbox; the app offers to install it
//! ([`status`], [`install`]). Where bubblewrap can't run (not installed,
//! user namespaces forbidden), Linux falls back on Landlock, the kernel's
//! own sandbox (used from Linux 6.2), which needs nothing installed: it enforces the
//! policy but for the redirect, and doesn't hide the other processes
//! (confine/landlock.rs). macOS has Seatbelt (`sandbox-exec`), part of
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
/// in its own copy, as on a server), the folders and files in `hide` are
/// empty to it, and it may write only under the folders in `writable`.
#[derive(Clone, Debug)]
pub struct Confine {
    pub view: PathBuf,
    pub project: PathBuf,
    /// Folders and files made inaccessible to the command.
    pub hide: Vec<PathBuf>,
    /// Directories the command may write to; the rest is read-only.
    pub writable: Vec<PathBuf>,
    /// The user's local services' sockets, and folders of them, closed to
    /// the command ([`local_sockets`]).
    pub sockets: Vec<PathBuf>,
    /// Folders and files inside writable ones that stay read-only: what
    /// runs outside the sandbox later ([`read_only_paths`]).
    pub read_only: Vec<PathBuf>,
    /// The private network the command starts in: the agent's
    /// ([`network::agents`]), a team server's, or on a desktop and in WSL
    /// the sealed one made while the allowed hosts are on; none otherwise.
    pub network: Option<std::sync::Arc<network::Network>>,
    /// Its connections out reach the loopback alone, but not these of its
    /// ports (the proxies' there): where the allowed hosts hold it on
    /// macOS, with Seatbelt ([`Confine::to_loopback`]). On Linux the
    /// agent's sealed network does.
    pub loopback_only: Option<Vec<u16>>,
}

impl Confine {
    /// An isolated chat's command: the project's path shows its view, and
    /// it may write to the view and the usual folders ([`writable_dirs`]).
    pub fn redirecting(view: PathBuf, project: PathBuf) -> Confine {
        let writable = writable_dirs(&view);
        Confine {
            sockets: local_sockets(&writable),
            writable,
            hide: secret_paths(),
            read_only: read_only_paths(),
            view,
            project,
            network: None,
            loopback_only: None,
        }
        .in_agents_network()
    }

    /// A command that already runs in its own copy (a server worktree, or a
    /// chat that works in the project directly): nothing is redirected.
    pub fn hiding(root: PathBuf) -> Confine {
        let writable = writable_dirs(&root);
        Confine {
            sockets: local_sockets(&writable),
            writable,
            hide: secret_paths(),
            read_only: read_only_paths(),
            view: root.clone(),
            project: root,
            network: None,
            loopback_only: None,
        }
        .in_agents_network()
    }

    /// In the agent's network, where there's one ([`network::agents`]):
    /// the command joins it.
    fn in_agents_network(self) -> Confine {
        match network::agents() {
            Some(network) => self.with_network(network),
            None => self,
        }
    }

    /// In `network`, its folder (the pages' shims, its resolv.conf) read
    /// by the command but not changed: even inside a folder the command
    /// writes, or one hidden from it, as a daemon's own folder is under
    /// `--data`.
    pub fn with_network(mut self, network: std::sync::Arc<network::Network>) -> Confine {
        let dir = network.dir().to_path_buf();
        if !self.read_only.contains(&dir) {
            self.read_only.push(dir);
        }
        self.network = Some(network);
        self
    }

    /// `program`, to be confined by this policy: made the way every
    /// program the app starts is, then made to join the policy's network
    /// first, before a mechanism adds its own steps.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn base_command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = crate::process::command(program);
        if let Some(network) = &self.network {
            network.join(&mut cmd);
        }
        cmd
    }

    /// With its connections out reaching the loopback alone, but not
    /// `closed`'s ports there (the proxies'): the allowed hosts' hold on
    /// macOS (allowed-hosts-plan.md, J07).
    pub fn to_loopback(mut self, closed: Vec<u16>) -> Confine {
        self.loopback_only = Some(closed);
        self
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

/// Those of `paths` that exist, each as the file system has it, once.
fn existing(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = paths
        .into_iter()
        .filter_map(|d| d.canonicalize().ok())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The credentials a chat's commands are kept out of: the app's own
/// provider keys (`~/.rovik`, keys.json on a server) and the user's SSH,
/// cloud, container, package registry and signing credentials, folders and
/// single files, plus whatever the app registered with [`protect`]. Only
/// those that exist right now. A command that legitimately needs one of
/// these (network git over SSH, a cloud deploy, a publish) is one an
/// isolated or server chat refuses anyway, and the user's own terminal
/// isn't confined.
pub fn secret_paths() -> Vec<PathBuf> {
    const UNDER_HOME: &[&str] = &[
        ".rovik",
        ".ssh",
        ".aws",
        ".azure",
        ".gnupg",
        ".config/gcloud",
        ".config/gh",
        ".config/op",
        ".config/doctl",
        ".config/rclone",
        ".oci",
        ".kube",
        ".docker",
        ".terraform.d",
        ".password-store",
        // GNOME's keyring files.
        ".local/share/keyrings",
        // Expo's and EAS's login, beside the state Expo's CLI writes on
        // every run: the CLI gets a folder of its own ([`TOOL_HOMES`]).
        ".expo",
        // macOS: the keychains' files.
        "Library/Keychains",
        // Single files: logins for git, package registries, databases
        // and the X server.
        ".netrc",
        ".git-credentials",
        ".config/git/credentials",
        ".config/hub",
        ".npmrc",
        ".yarnrc",
        ".pypirc",
        ".gem/credentials",
        ".pgpass",
        ".my.cnf",
        ".Xauthority",
        // Cargo's registry token, in `~/.cargo`, which commands may write
        // ([`TOOL_DIRS`]).
        ".cargo/credentials.toml",
        ".cargo/credentials",
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

/// What a confined command may read but not change though it's inside a
/// folder it writes ([`TOOL_DIRS`]): the folders of programs on the user's
/// PATH and the files their shells and cargo read. A program put there, a
/// `build.rustc-wrapper` in cargo's config, or a line in `~/.cargo/env`
/// would run outside the sandbox, in the user's own terminal, later.
/// Installing a tool for the user (`cargo install`, `pip install --user`)
/// is theirs to do. Only those that exist right now.
pub fn read_only_paths() -> Vec<PathBuf> {
    const UNDER_HOME: &[&str] = &[
        ".cargo/bin",
        ".cargo/config.toml",
        ".cargo/config",
        ".cargo/env",
        ".local/bin",
        ".bun/bin",
        ".deno/bin",
        ".yarn/bin",
        "go/bin",
    ];
    let paths = home()
        .into_iter()
        .flat_map(|home| UNDER_HOME.iter().map(move |rel| home.join(rel)));
    existing(paths)
}

/// The variables that tell a program where the session's services are:
/// left out of a confined command's environment, whose sockets are closed
/// to it anyway ([`local_sockets`]), so a tool doesn't try them.
const SESSION_VARIABLES: &[&str] = &[
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
];

/// Sockets the system's services listen on, whatever the session says.
const SYSTEM_SOCKETS: &[&str] = &[
    // X11's sockets and the session manager's.
    "/tmp/.X11-unix",
    "/tmp/.ICE-unix",
    // The system bus: polkit lets a desktop user mount disks and change
    // the network through it.
    "/run/dbus/system_bus_socket",
    // Container daemons, which run what they're asked as root. On macOS
    // Docker's is a link into the home folder, followed.
    "/run/docker.sock",
    "/var/run/docker.sock",
    "/run/podman",
    "/run/containerd/containerd.sock",
];

/// The sockets of the user's local services a confined command is kept
/// from: the ssh-agent's (`SSH_AUTH_SOCK`), the user's runtime folder
/// (`XDG_RUNTIME_DIR`, `/run/user/<uid>`: the session bus, Wayland, audio,
/// gpg-agent, the keyring and systemd's user manager, which runs anything
/// it's asked to outside the sandbox), an XQuartz display, and
/// [`SYSTEM_SOCKETS`]. Each as the file system has it, once; only those
/// that exist and aren't one of the command's `writable` folders (a
/// runtime folder that is the temporary folder stays the command's).
pub fn local_sockets(writable: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = ["SSH_AUTH_SOCK", "XDG_RUNTIME_DIR", "DISPLAY"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect();
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getuid can't fail.
        let uid = unsafe { libc::getuid() };
        paths.push(PathBuf::from(format!("/run/user/{uid}")));
    }
    if cfg!(unix) {
        paths.extend(SYSTEM_SOCKETS.iter().map(PathBuf::from));
    }
    let mut paths: Vec<PathBuf> = paths
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| !writable.contains(p))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// When a folder of [`TOOL_DIRS`] that isn't there yet is made before a
/// command runs: the sandbox can only open a folder that exists, so a tool's
/// first run there couldn't make its cache (a fresh machine's `go test`
/// fails on `~/.cache/go-build`).
enum Make {
    /// Always: the folders every tool writes to (XDG's).
    Always,
    /// When this program is installed, so homes without it get no folder;
    /// or when a cloud host's shared toolchain runs it
    /// ([`crate::toolchains`]: cargo keeps `~/.cargo` even there).
    With(&'static str),
    /// Never: the installer makes it, or the system has it.
    Never,
}

/// The toolchains' caches and user installs under the home folder.
const TOOL_DIRS: &[(&str, Make)] = &[
    (".cache", Make::Always),
    (".cargo", Make::With("cargo")),
    (".rustup", Make::Never),
    (".npm", Make::With("npm")),
    (".yarn", Make::With("yarn")),
    (".bun", Make::Never),
    (".deno", Make::With("deno")),
    (".m2", Make::With("mvn")),
    (".gradle", Make::With("gradle")),
    // CocoaPods' copies of the specs repositories (`pod install` in a
    // React Native app's ios/).
    (".cocoapods", Make::With("pod")),
    (".nuget", Make::With("dotnet")),
    (".local/bin", Make::Never),
    (".local/lib", Make::Never),
    (".local/share/pnpm", Make::With("pnpm")),
    (".local/share/uv", Make::With("uv")),
    (".local/state", Make::Always),
    ("go", Make::With("go")),
    // macOS: the caches (Homebrew, pip, clang's modules) and Xcode's
    // build products.
    ("Library/Caches", Make::Never),
    ("Library/Developer/Xcode/DerivedData", Make::Never),
];

/// Makes the folders of [`TOOL_DIRS`] under `home` that should be there
/// and aren't, `installed` saying whether a program is: where the
/// project's commands find it, a cloud host's shared Rust and Node and
/// this user's Yarn and pnpm included ([`crate::toolchains::finder`]),
/// though they aren't on this account's PATH.
fn make_missing(home: &Path, installed: impl Fn(&str) -> bool) {
    if !home.is_dir() {
        return;
    }
    for (rel, make) in TOOL_DIRS {
        let dir = home.join(rel);
        let wanted = match make {
            Make::Always => true,
            Make::With(program) => installed(program),
            Make::Never => false,
        };
        if wanted && !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
    }
    // Landlock writes a folder with a read-only entry entry by entry
    // (`~/.cargo`, around `bin`), so it takes nothing new at its top: what
    // cargo makes there on first use is made now (its locks are empty).
    let cargo = home.join(".cargo");
    if cargo.is_dir() {
        for dir in ["registry", "git"] {
            let _ = std::fs::create_dir_all(cargo.join(dir));
        }
        for lock in [".package-cache", ".package-cache-mutate"] {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(cargo.join(lock));
        }
    }
}

/// Where a command run in `root` may write: `root` itself, the temporary
/// folders, the toolchains' caches and user installs under the home folder
/// (what a build or a test run writes besides the project), made first
/// where a tool would make them ([`Make`]), and a cloud host's shared
/// caches ([`crate::toolchains::writable`]). Only those that exist.
/// Everything else is read-only to it.
pub fn writable_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf(), std::env::temp_dir()];
    dirs.extend(std::env::var_os("TMPDIR").map(PathBuf::from));
    // The system's too where `TMPDIR` is elsewhere (a daemon's own, K13):
    // commands write to `/tmp` by name as often as through `TMPDIR`.
    if cfg!(unix) {
        dirs.push(PathBuf::from("/tmp"));
    }
    if let Some(home) = home() {
        if cfg!(unix) {
            make_missing(&home, crate::toolchains::finder(root));
        }
        dirs.extend(TOOL_DIRS.iter().map(|(rel, _)| home.join(rel)));
    }
    // A cloud host's: the npm cache and the crates the daemons share,
    // which the `rovik` group writes ([`crate::toolchains`]).
    dirs.extend(crate::toolchains::writable());
    // macOS: the user's cache folder beside `TMPDIR` (/var/folders/…/C),
    // where the developer tools keep theirs.
    if cfg!(target_os = "macos") {
        dirs.extend(std::env::temp_dir().parent().map(|d| d.join("C")));
    }
    existing(dirs).into_iter().filter(|d| d.is_dir()).collect()
}

/// Tools that keep a login in the folder they write their other state to,
/// a folder [`secret_paths`] hides: the variable each reads its folder from,
/// and the folder of its own a confined command gets instead, under
/// [`TOOL_HOMES_DIR`]. Expo's CLI (`expo lint`, `expo install`, …) writes
/// its state file on every run and fails where it can't; pointed there it
/// works, without the user's Expo login.
const TOOL_HOMES: &[(&str, &str)] = &[("__UNSAFE_EXPO_HOME_DIRECTORY", "expo")];

/// Where [`TOOL_HOMES`] go, under the home folder: inside `.cache`, which
/// the commands may write to ([`TOOL_DIRS`]).
const TOOL_HOMES_DIR: &str = ".cache/rovik-ide";

/// The variables that point [`TOOL_HOMES`] at their folders under `home`,
/// each folder made if it can be.
fn tool_home_env(home: &Path) -> Vec<(&'static str, PathBuf)> {
    TOOL_HOMES
        .iter()
        .map(|(var, name)| {
            let dir = home.join(TOOL_HOMES_DIR).join(name);
            let _ = std::fs::create_dir_all(&dir);
            (*var, dir)
        })
        .collect()
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
    /// that only hides (macOS's Seatbelt, Landlock) says false.
    fn can_redirect(&self) -> bool;

    /// Whether a command it confined that ended so (`code`, `stdout`,
    /// `stderr`) never started: the sandbox couldn't be made, so nothing
    /// ran and it may be tried again ([`until_made`]). Only bubblewrap
    /// fails so.
    fn never_started(&self, _code: Option<i32>, _stdout: &str, _stderr: &str) -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
mod landlock;
#[cfg(target_os = "linux")]
pub mod network;
pub mod page_url;
#[cfg(unix)]
pub mod pages;

/// Off Linux there's no private network: a team server runs Linux (D16),
/// and none is ever made elsewhere. The type keeps the callers the same.
#[cfg(not(target_os = "linux"))]
pub mod network {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// Why there's none.
    const NONE: &str = "a private network for the agent's commands is made on Linux only";

    /// A private network, which never exists here.
    #[derive(Debug)]
    pub enum Network {}

    impl Network {
        pub fn start(_dir: &Path, _opener: &Path) -> Result<Network, String> {
            Err(NONE.into())
        }

        pub fn join(&self, _cmd: &mut std::process::Command) {
            match *self {}
        }

        pub fn give_browser(&self, _cmd: &mut std::process::Command, _chat: &str) {
            match *self {}
        }

        pub fn listen(&self) -> std::io::Result<std::net::TcpListener> {
            match *self {}
        }

        pub fn listen_on(&self, _port: u16) -> std::io::Result<Vec<std::net::TcpListener>> {
            match *self {}
        }

        pub fn connect(&self, _port: u16) -> std::io::Result<std::net::TcpStream> {
            match *self {}
        }

        pub fn listening_ports(&self) -> Vec<u16> {
            match *self {}
        }

        pub fn listening_on(&self, _port: u16) -> Vec<(u32, Option<String>)> {
            match *self {}
        }

        pub fn dir(&self) -> &Path {
            match *self {}
        }

        pub fn pages_socket(&self) -> PathBuf {
            match *self {}
        }
    }

    /// Never made here: why not.
    pub fn enable(_dir: &Path, _opener: &Path) -> Result<(), String> {
        Err(NONE.into())
    }

    /// None: there's no private network here.
    pub fn current() -> Option<Arc<Network>> {
        None
    }

    /// None: the agent's processes start in the computer's network here.
    pub fn agents() -> Option<Arc<Network>> {
        None
    }

    /// Nothing to hold or let go here: the agent's network is sealed on
    /// Linux alone.
    pub fn seal_agents(_sealed: bool, _closed: Vec<u16>) -> Result<(), String> {
        Ok(())
    }

    /// A container's namespaces: Linux's alone.
    pub enum Namespaces {}

    impl Namespaces {
        pub fn of(_pid: u32) -> std::io::Result<Namespaces> {
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, NONE))
        }

        pub fn connect(&self, _port: u16) -> std::io::Result<std::net::TcpStream> {
            match *self {}
        }
    }
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
/// program may not be installed. A sandbox that couldn't be made this once
/// (a mount went away meanwhile) is made again ([`until_made`]): the
/// session isn't left on the fallback for it.
fn works(mechanism: &dyn Mechanism, c: &Confine) -> bool {
    let (program, args) = os::PROBE;
    let probe = || {
        let mut cmd = mechanism.command(c, program, args);
        cmd.stdin(Stdio::null());
        cmd.output().map_err(|e| e.to_string())
    };
    until_made(probe, |out| unmade(mechanism, out)).is_ok_and(|out| out.status.success())
}

/// Whether `out`, what a command confined by `mechanism` ended with, says
/// its sandbox couldn't be made, so nothing ran ([`until_made`]).
fn unmade(mechanism: &dyn Mechanism, out: &std::process::Output) -> bool {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    !out.status.success()
        && mechanism.never_started(out.status.code(), &text(&out.stdout), &text(&out.stderr))
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

/// `program` with `args`, confined by `c`, the tools that keep a login
/// where the policy hides it pointed at folders of their own
/// ([`TOOL_HOMES`]). As it is where `c` can't be enforced, which callers
/// ask first ([`available`], [`refusal`]).
pub fn command(c: &Confine, program: &str, args: &[&str]) -> Command {
    match chosen().filter(|_| available(c)) {
        Some(m) => {
            let mut cmd = m.command(c, program, args);
            for (var, dir) in home().map(|h| tool_home_env(&h)).unwrap_or_default() {
                cmd.env(var, dir);
            }
            for var in SESSION_VARIABLES {
                cmd.env_remove(var);
            }
            cmd
        }
        None => {
            let mut cmd = crate::process::command(program);
            cmd.args(args);
            cmd
        }
    }
}

/// Whether a command confined by `c` that ended so (`code`, `stdout`,
/// `stderr`) never started: its sandbox couldn't be made ([`until_made`]).
pub fn never_started(c: &Confine, code: Option<i32>, stdout: &str, stderr: &str) -> bool {
    chosen()
        .filter(|_| available(c))
        .is_some_and(|m| m.never_started(code, stdout, stderr))
}

/// The most times a command is started while its sandbox can't be made.
const SANDBOX_TRIES: u32 = 5;

/// The pause before a command starts again, times the tries so far.
const SANDBOX_PAUSE: std::time::Duration = std::time::Duration::from_millis(25);

/// `attempt` (a confined command, built and run), tried again while what
/// it ended with says its sandbox couldn't be made (`unmade`), at most
/// [`SANDBOX_TRIES`] times in all: 25 ms later, then 50, 75 and 100 ms
/// after the try before. bubblewrap copies the system's mounts and makes
/// them read-only one by one; one that goes away meanwhile (another chat's
/// view unmounted) makes it give up, and a try once it's gone works.
/// Nothing ran in such a try, so nothing runs twice. The last try's result
/// is the one returned.
pub fn until_made<T>(
    mut attempt: impl FnMut() -> Result<T, String>,
    unmade: impl Fn(&T) -> bool,
) -> Result<T, String> {
    let mut tries = 1;
    loop {
        let ended = attempt()?;
        if tries == SANDBOX_TRIES || !unmade(&ended) {
            return Ok(ended);
        }
        log::info!(
            "a command's sandbox couldn't be made: trying again ({tries} of {SANDBOX_TRIES} tried)"
        );
        std::thread::sleep(SANDBOX_PAUSE * tries);
        tries += 1;
    }
}

/// Whether the kernel's Landlock confines commands here: Linux's fallback
/// where bubblewrap can't (confine/landlock.rs). False off Linux.
pub fn landlock_works() -> bool {
    #[cfg(target_os = "linux")]
    {
        works(&landlock::Landlock, &probe_policy())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
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
    /// What confines them instead, while `program` can't: "Landlock" on
    /// Linux, a lighter sandbox (`detail` says what it doesn't do).
    pub fallback: Option<&'static str>,
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
/// host installer adds. Where AppArmor restricts unprivileged user
/// namespaces (Ubuntu 23.10 and later) bwrap can't make its sandbox without
/// a profile that lets it: the packages, the host installer and `install`
/// here add `rovik-bwrap` (`distro::BWRAP_APPARMOR`). Where it can't run,
/// Landlock confines commands instead (confine/landlock.rs), without the
/// redirect or the pid namespace; the status says so, and the app still
/// offers bubblewrap.
#[cfg(target_os = "linux")]
mod linux {
    use super::{Confine, InstallOutcome, Mechanism, SandboxStatus};
    use crate::distro::{self, Package, RootRun};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(super) const PROBE: (&str, &[&str]) = ("/bin/sh", &["-c", "true"]);
    pub(super) static MECHANISMS: &[&dyn Mechanism] = &[&Bwrap, &super::landlock::Landlock];

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
            let mut cmd = c.base_command(PROGRAM);
            cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
            for dir in c.writable.iter().filter(|d| d.is_dir()) {
                cmd.arg("--bind").arg(dir).arg(dir);
            }
            if c.redirects() {
                cmd.arg("--bind").arg(&c.view).arg(&c.project);
            }
            // What runs outside the sandbox later, back to read-only over
            // the writable folder it's in.
            for path in c.read_only.iter().filter(|p| p.exists()) {
                cmd.arg("--ro-bind").arg(path).arg(path);
            }
            for dir in c.hide.iter().filter(|d| d.is_dir()) {
                cmd.arg("--tmpfs").arg(dir);
            }
            // A writable folder inside a hidden one stays the command's
            // (`--data` puts a daemon's worktrees in its own folder), and
            // what stays read-only inside one is still the command's to
            // read; what's hidden inside either stays hidden.
            let inside = |d: &Path, of: &[PathBuf]| of.iter().any(|o| d != o && d.starts_with(o));
            let mut back: Vec<PathBuf> = Vec::new();
            for dir in c
                .writable
                .iter()
                .filter(|d| d.is_dir() && inside(d, &c.hide))
            {
                cmd.arg("--bind").arg(dir).arg(dir);
                back.push(dir.clone());
            }
            for path in c
                .read_only
                .iter()
                .filter(|p| p.exists() && inside(p, &c.hide))
            {
                cmd.arg("--ro-bind").arg(path).arg(path);
                back.push(path.clone());
            }
            for dir in c.hide.iter().filter(|d| d.is_dir() && inside(d, &back)) {
                cmd.arg("--tmpfs").arg(dir);
            }
            // Hidden files and the local services: a file or a socket gets
            // `/dev/null` over it, read-only (empty to read, refusing a
            // connection, and as a mount point it can't be replaced or
            // deleted), a folder of sockets is emptied. Last, so a writable
            // folder bound above (`~/.cargo`, `/tmp`) doesn't bring them
            // back; one inside a hidden folder is gone already.
            let closed = c.hide.iter().filter(|p| !p.is_dir()).chain(&c.sockets);
            for path in closed.filter(|p| !inside(p, &c.hide) || inside(p, &back)) {
                match std::fs::symlink_metadata(path) {
                    Ok(m) if m.is_dir() => {
                        cmd.arg("--tmpfs").arg(path);
                    }
                    Ok(_) => {
                        cmd.arg("--ro-bind").arg("/dev/null").arg(path);
                    }
                    Err(_) => {}
                }
            }
            // A writable folder inside a folder of sockets (a temporary
            // folder in the runtime folder) stays the command's.
            for dir in c
                .writable
                .iter()
                .filter(|d| d.is_dir() && inside(d, &c.sockets))
            {
                cmd.arg("--bind").arg(dir).arg(dir);
            }
            // Run by root, bwrap keeps the capabilities unless told; run by
            // a user it drops them anyway. Its own IPC namespace keeps the
            // command from the session's shared memory and message queues.
            cmd.args([
                "--unshare-pid",
                "--unshare-ipc",
                "--die-with-parent",
                "--cap-drop",
                "ALL",
            ]);
            cmd.arg("--").arg(program).args(args);
            // Abstract sockets have no file to mount over: X11's and older
            // systems' session bus listen on one too. Landlock closes them
            // to the command (Linux 6.12 on). Not to a set-user-ID bwrap,
            // which a process that can't gain privileges couldn't start.
            if !setuid() {
                super::landlock::close_abstract_sockets(&mut cmd);
            }
            cmd
        }

        fn can_redirect(&self) -> bool {
            true
        }

        /// bwrap that can't make the sandbox exits with 1 before the
        /// command starts, with one line of its own on stderr that says
        /// why: "bwrap: Can't bind mount /oldroot/ on /newroot/: Unable to
        /// apply mount flags: remount …: Invalid argument" when a mount
        /// went away while it made them read-only, "Can't find source
        /// path" for a folder removed meanwhile. A command that ran and
        /// failed has its own output; one that printed only such a line (a
        /// bwrap of its own that couldn't make its sandbox) is tried again
        /// too, as the user would.
        fn never_started(&self, code: Option<i32>, stdout: &str, stderr: &str) -> bool {
            code == Some(1)
                && stdout.is_empty()
                && stderr.starts_with("bwrap: ")
                && stderr.trim_end().lines().count() == 1
        }
    }

    /// Whether the installed bwrap is set-user-ID (older distributions'),
    /// found once.
    fn setuid() -> bool {
        use std::os::unix::fs::PermissionsExt;
        static SETUID: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
            distro::which(PROGRAM)
                .and_then(|p| std::fs::metadata(p).ok())
                .is_some_and(|m| m.permissions().mode() & 0o4000 != 0)
        });
        *SETUID
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

    /// Why bubblewrap can't make a sandbox though it's installed.
    fn blocked() -> &'static str {
        if distro::userns_restricted() {
            "AppArmor stops it from making a sandbox here (Ubuntu 23.10 and later restrict user \
             namespaces): a profile that lets it, added with administrator permission, fixes that"
        } else {
            "it can't make a sandbox here (the kernel or a security module may forbid user \
             namespaces)"
        }
    }

    pub(super) fn status() -> SandboxStatus {
        let chosen = super::chosen().map(|m| m.name());
        let full = chosen == Some(PROGRAM);
        let installed = distro::which(PROGRAM).is_some();
        let problem = match installed {
            true => format!("bubblewrap is installed, but {}.", blocked()),
            false => "bubblewrap isn't installed.".to_string(),
        };
        let fallback = (chosen == Some(super::landlock::NAME)).then_some("Landlock");
        let detail = match (full, fallback) {
            (true, _) => None,
            (false, Some(_)) => Some(format!("{problem} {LANDLOCK_INSTEAD}")),
            (false, None) => Some(problem),
        };
        SandboxStatus {
            required: true,
            program: PACKAGE.name,
            working: chosen.is_some(),
            fallback,
            detail,
            can_install: (!installed && distro::install_script(&PACKAGE).is_some())
                || (installed && !full && distro::userns_restricted()),
        }
    }

    /// What the fallback does and doesn't, for the user.
    const LANDLOCK_INSTEAD: &str = "The agent's commands run under Landlock instead, which \
        keeps them out of your credentials (but cargo's token) and writing only in their own \
        folders, but doesn't hide your other processes from them, can't close your SSH and GPG \
        agents' and desktop session's sockets to them, and gives an isolated chat's commands the \
        project read-only rather than the chat's own copy at its path.";

    /// Installs bubblewrap where it's missing, and where AppArmor restricts
    /// user namespaces adds the profile that lets it make its sandbox
    /// (`distro::BWRAP_APPARMOR`): one administrator prompt for both.
    pub(super) async fn install() -> Result<InstallOutcome, String> {
        let package = match distro::which(PROGRAM) {
            Some(_) => String::new(),
            None => match distro::install_script(&PACKAGE) {
                Some(script) => format!("{script}\n"),
                None => {
                    return Ok(InstallOutcome::Manual {
                        message: "No supported package manager was found. Install bubblewrap \
                                  (the package is called bubblewrap nearly everywhere)."
                            .into(),
                        command: None,
                    })
                }
            },
        };
        // The profile's script checks for the restriction itself.
        let script = format!("set -e\n{package}{}", distro::BWRAP_APPARMOR);
        if let RootRun::Manual { message, command } =
            distro::run_as_root(&script, "Setting up bubblewrap").await?
        {
            return Ok(InstallOutcome::Manual {
                message,
                command: Some(command),
            });
        }
        super::forget();
        let now = status();
        match now.working && now.fallback.is_none() {
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
            Some(_) if distro::userns_restricted() => format!(
                "it's installed, but {}: the app offers to at start",
                blocked()
            ),
            Some(_) => format!("it's installed, but {}", blocked()),
            None => "it isn't installed: the app offers to install it at start, or install the \
                     bubblewrap package yourself"
                .to_string(),
        };
        format!(
            "Error: on Linux the agent's commands run in a bubblewrap sandbox, and {how}. \
             (This kernel has no Landlock, version 3 or later (Linux 6.2), to fall back on.) \
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
        fallback: None,
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
    let hidden: Vec<&PathBuf> = c.hide.iter().filter(|d| d.exists()).collect();
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
    for (i, path) in c.read_only.iter().filter(|p| p.exists()).enumerate() {
        let name = format!("R{i}");
        let filter = if path.is_dir() {
            under(name.clone())
        } else {
            format!(r#"(literal (param "{name}"))"#)
        };
        profile += &format!("(deny file-write* {filter})\n");
        params.push((name, path.to_path_buf()));
    }
    for (i, dir) in hidden.iter().enumerate() {
        let inside: Vec<String> = writable
            .iter()
            .enumerate()
            .filter(|(_, w)| *w != dir && w.starts_with(dir))
            .map(|(j, _)| under(format!("W{j}")))
            .collect();
        let filter = if !dir.is_dir() {
            format!(r#"(literal (param "H{i}"))"#)
        } else if inside.is_empty() {
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
    // The local services: no connection to a socket in a folder of them,
    // or to one of them.
    let mut closed = Vec::new();
    for (i, path) in c.sockets.iter().filter(|p| p.exists()).enumerate() {
        let name = format!("S{i}");
        closed.push(if path.is_dir() {
            under(name.clone())
        } else {
            format!(r#"(literal (param "{name}"))"#)
        });
        params.push((name, path.to_path_buf()));
    }
    if !closed.is_empty() {
        profile += &format!("(deny network-outbound {})\n", any(closed));
    }
    if let Some(ports) = &c.loopback_only {
        profile += &loopback_rules(ports);
    }
    (profile, params)
}

/// Seatbelt's rules for connections out that reach the loopback alone, but
/// not its `closed` ports (the proxies' there): the allowed hosts' on macOS
/// (allowed-hosts-plan.md, J07). Each rule says all it means, so nothing
/// rests on which of two rules Seatbelt prefers.
fn loopback_rules(closed: &[u16]) -> String {
    let mut rules = String::from(
        "(deny network-outbound (require-all (remote ip \"*:*\") \
         (require-not (remote ip \"localhost:*\"))))\n",
    );
    for port in closed {
        rules += &format!("(deny network-outbound (remote ip \"localhost:{port}\"))\n");
    }
    rules
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
            fallback: None,
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
            fallback: None,
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

#[cfg(test)]
mod tool_home_tests {
    use super::*;

    /// Expo's CLI gets a folder of its own under the commands' cache, made
    /// for it, whatever the home holds.
    #[test]
    fn a_tool_with_a_login_gets_a_folder_of_its_own_in_the_cache() {
        let home = std::env::temp_dir().join(format!("tool-homes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".expo")).unwrap();
        let env = tool_home_env(&home);
        let expo = home.join(".cache/rovik-ide/expo");
        assert_eq!(env, [("__UNSAFE_EXPO_HOME_DIRECTORY", expo.clone())]);
        assert!(expo.is_dir());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The variables that name the session's services (the ssh-agent's
    /// socket, the display, the session bus) are left out of a confined
    /// command's environment.
    #[test]
    fn a_confined_command_doesnt_get_the_sessions_socket_variables() {
        let root = std::env::temp_dir().join(format!("session-vars-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let c = Confine::hiding(root.clone());
        if !available(&c) {
            eprintln!("no sandbox here: skipped");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let cmd = command(&c, "/bin/sh", &["-c", "true"]);
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for var in [
            "SSH_AUTH_SOCK",
            "DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XAUTHORITY",
        ] {
            assert!(removed.iter().any(|r| r == var), "{var}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A confined command has the variable, and may write where it points:
    /// Expo's CLI writes its state file there instead of `~/.expo`, which
    /// the sandbox hides (it holds the user's Expo login).
    #[cfg(unix)]
    #[test]
    fn a_confined_command_writes_the_tools_state_in_its_own_folder() {
        let root = std::env::temp_dir().join(format!("tool-home-run-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let c = Confine::hiding(root.clone());
        if !available(&c) || home().is_none() {
            eprintln!("no sandbox here: skipped");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let probe = format!("probe-{}.json", std::process::id());
        let script = format!(
            "echo '{{}}' > \"$__UNSAFE_EXPO_HOME_DIRECTORY/{probe}\" && cat \"$__UNSAFE_EXPO_HOME_DIRECTORY/{probe}\""
        );
        let out = command(&c, "/bin/sh", &["-c", &script])
            .current_dir(&root)
            .output()
            .unwrap();
        let dir = home().unwrap().join(TOOL_HOMES_DIR).join("expo");
        let _ = std::fs::remove_file(dir.join(&probe));
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
    }
}

/// The Seatbelt profile's text, on every system; macOS runs it below.
#[cfg(test)]
mod seatbelt_profile_tests {
    use super::*;

    /// What stays read-only inside a writable folder gets a write deny of
    /// its own, by parameter: a folder by subpath, a file by literal.
    #[test]
    fn the_profile_keeps_the_read_only_paths_from_writes() {
        let root = std::env::temp_dir().join(format!("sbro-{}", std::process::id()));
        let (cargo, bin, config) = (
            root.join(".cargo"),
            root.join(".cargo/bin"),
            root.join(".cargo/config.toml"),
        );
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(&config, "").unwrap();
        let c = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: Vec::new(),
            writable: vec![cargo.clone()],
            sockets: Vec::new(),
            read_only: vec![bin.clone(), config.clone(), root.join("gone")],
            network: None,
            loopback_only: None,
        };
        let (profile, params) = seatbelt_profile(&c);
        assert!(params.contains(&("R0".into(), bin.clone())));
        assert!(params.contains(&("R1".into(), config.clone())));
        assert!(!params.iter().any(|(n, _)| n == "R2"), "gone: left out");
        assert!(profile.contains(r#"(deny file-write* (subpath (param "R0")))"#));
        assert!(profile.contains(r#"(deny file-write* (literal (param "R1")))"#));
        let _ = std::fs::remove_dir_all(&root);
    }

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
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
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

/// The Seatbelt profile closes the local services' sockets and hides
/// single files.
#[cfg(test)]
mod seatbelt_socket_tests {
    use super::*;

    #[test]
    fn the_profile_denies_connecting_to_the_local_sockets_and_reading_a_hidden_file() {
        let root = std::env::temp_dir().join(format!("sbsock-{}", std::process::id()));
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let agent = root.join("agent");
        std::fs::write(&agent, "").unwrap();
        let netrc = root.join(".netrc");
        std::fs::write(&netrc, "").unwrap();
        let c = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: vec![netrc.clone()],
            writable: vec![root.clone()],
            sockets: vec![runtime.clone(), agent.clone(), root.join("gone")],
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
        };
        let (profile, params) = seatbelt_profile(&c);
        assert!(params.contains(&("S0".to_string(), runtime.clone())));
        assert!(params.contains(&("S1".to_string(), agent.clone())));
        assert!(!params.iter().any(|(n, _)| n == "S2"), "gone: left out");
        // A hidden file is out of reach by its exact path.
        assert!(params.contains(&("H0".to_string(), netrc.clone())));
        assert!(profile.contains(r#"(deny file-read* file-write* (literal (param "H0")))"#));
        assert!(profile.contains(
            r#"(deny network-outbound (require-any (subpath (param "S0")) (literal (param "S1"))))"#
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Held to the loopback (the allowed hosts on macOS), a command's
    /// connections out reach it alone, and not a proxy's port there.
    #[test]
    fn held_to_the_loopback_the_profile_denies_other_addresses_and_the_proxies() {
        let root = std::env::temp_dir();
        let c = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: Vec::new(),
            writable: Vec::new(),
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
        };
        assert!(!seatbelt_profile(&c).0.contains("remote ip"));
        let (profile, _) = seatbelt_profile(&c.to_loopback(vec![3128, 1080]));
        assert!(
            profile.contains(
                "(deny network-outbound (require-all (remote ip \"*:*\") \
                 (require-not (remote ip \"localhost:*\"))))\n"
            ),
            "{profile}"
        );
        for port in [3128, 1080] {
            let closed = format!("(deny network-outbound (remote ip \"localhost:{port}\"))\n");
            assert!(profile.contains(&closed), "{profile}");
        }
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
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
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
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
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

    /// Held to the loopback (the allowed hosts, allowed-hosts-plan.md J07),
    /// a command reaches a server there, but not the same server at this
    /// computer's own address, nor a proxy's port on the loopback.
    #[test]
    fn held_to_the_loopback_a_command_reaches_it_alone() {
        let root = fresh("sbloopback");
        let server = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        // This computer's address on its way out: connecting a UDP socket
        // sends nothing.
        let address = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|socket| {
                socket.connect("192.0.2.1:80")?;
                socket.local_addr()
            })
            .map(|local| local.ip())
            .ok()
            .filter(|ip| !ip.is_loopback());
        let Some(address) = address else {
            eprintln!("no address of this computer's own: skipped");
            return;
        };
        let reach = |host: &str, port: u16| format!("nc -z -G 2 {host} {port}; echo $?");
        let script = [
            reach("127.0.0.1", port),
            reach(&address.to_string(), port),
            reach("127.0.0.1", proxy_port),
        ]
        .join("; ");
        let free = Confine {
            view: root.clone(),
            project: root.clone(),
            hide: Vec::new(),
            writable: vec![root.clone()],
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
        };
        // Not held, it reaches all three.
        let out = run(&free, &root, &script);
        assert_eq!(
            stdout(&out),
            "0\n0\n0\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = run(&free.to_loopback(vec![proxy_port]), &root, &script);
        assert_eq!(
            stdout(&out),
            "0\n1\n1\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        drop((server, proxy));
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
            sockets: Vec::new(),
            read_only: Vec::new(),
            network: None,
            loopback_only: None,
        };
        (root, c)
    }

    /// Runs `script` under `c` in `cwd`: its output, or None where bwrap
    /// can't make a sandbox here (the test is then skipped). A sandbox
    /// bwrap couldn't make this once, as another test's chat view was
    /// unmounted while it made the mounts read-only, is made again, as for
    /// the agent's commands.
    fn run(c: &Confine, cwd: &Path, script: &str) -> Option<std::process::Output> {
        if !works(BWRAP, c) {
            eprintln!("bwrap can't confine commands here: skipped");
            return None;
        }
        let start = || {
            BWRAP
                .command(c, "/bin/sh", &["-c", script])
                .current_dir(cwd)
                .output()
                .map_err(|e| e.to_string())
        };
        Some(until_made(start, |out| unmade(BWRAP, out)).unwrap())
    }

    fn stdout(out: &std::process::Output) -> String {
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Inside a writable folder, what stays read-only can't be changed: no
    /// program put in a bin folder, no config file rewritten; the folder's
    /// other entries are written as before.
    #[test]
    fn read_only_paths_inside_a_writable_folder_stay_read_only() {
        let (root, mut c) = fresh("confinero", &[]);
        let (cargo, bin, config) = (
            root.join(".cargo"),
            root.join(".cargo/bin"),
            root.join(".cargo/config.toml"),
        );
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(&config, "old\n").unwrap();
        c.writable = vec![cargo.clone()];
        c.read_only = vec![bin.clone(), config.clone()];
        let script = format!(
            "(echo planted > {b}/git) 2>/dev/null; (echo new > {f}) 2>/dev/null; \
             echo cached > {c}/cache.txt && cat {c}/cache.txt",
            b = bin.display(),
            f = config.display(),
            c = cargo.display()
        );
        let Some(out) = run(&c, &root, &script) else {
            return;
        };
        assert_eq!(
            stdout(&out),
            "cached\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!bin.join("git").exists(), "a program planted on the PATH");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "old\n");
        let _ = std::fs::remove_dir_all(&root);
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

    /// What stays read-only inside a hidden folder is the command's to read
    /// and run still (a daemon started with `--data` keeps what its
    /// commands run in its own folder), and not to change; the rest of the
    /// hidden folder, and what's hidden inside the read-only one, stay
    /// hidden.
    #[test]
    fn a_read_only_folder_inside_a_hidden_one_stays_readable_and_read_only() {
        let (root, mut c) = fresh("nestedro", &[]);
        let daemon = root.join("daemon");
        let net = daemon.join("net");
        let secret = net.join(".secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(daemon.join("keys.json"), "TOPSECRET\n").unwrap();
        std::fs::write(secret.join("token"), "TOKEN\n").unwrap();
        let shim = net.join("shim");
        std::fs::write(&shim, "#!/bin/sh\necho shim\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        c.hide = vec![daemon.clone(), secret.clone()];
        c.writable = vec![daemon.clone()];
        c.read_only = vec![net.clone()];
        let script = format!(
            "cat {d}/keys.json 2>/dev/null; cat {s}/token 2>/dev/null; {n}/shim; \
             echo planted > {n}/planted 2>/dev/null || echo read-only",
            d = daemon.display(),
            s = secret.display(),
            n = net.display()
        );
        let Some(out) = run(&c, &root, &script) else {
            return;
        };
        assert_eq!(
            stdout(&out),
            "shim\nread-only\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!net.join("planted").exists());
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

    /// A hidden file is empty to the command, inside a folder it may write
    /// to too (cargo's token in `~/.cargo`), and it can't be written,
    /// replaced or deleted; the folder's other files are the command's.
    #[test]
    fn a_hidden_file_is_empty_and_kept_even_in_a_writable_folder() {
        let (root, mut c) = fresh("hidefile", &[]);
        let cargo = root.join("cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        let (token, netrc) = (cargo.join("credentials.toml"), root.join(".netrc"));
        std::fs::write(&token, "token = \"SECRET\"\n").unwrap();
        std::fs::write(&netrc, "password SECRET\n").unwrap();
        c.writable = vec![cargo.clone()];
        c.hide = vec![token.clone(), netrc.clone()];
        let script = format!(
            "cat {t} {n}; echo; echo x > {t} 2>/dev/null || echo no-write; \
             echo y > {k}/new && mv {k}/new {t} 2>/dev/null || echo no-replace; \
             rm -f {t} 2>/dev/null || echo no-delete; \
             echo z > {k}/config.toml && echo other-written",
            t = token.display(),
            n = netrc.display(),
            k = cargo.display()
        );
        let Some(out) = run(&c, &cargo, &script) else {
            return;
        };
        assert_eq!(
            stdout(&out),
            "\nno-write\nno-replace\nno-delete\nother-written\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&token).unwrap(),
            "token = \"SECRET\"\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Whether a client inside `c` connects to `addr` (a path, or an
    /// abstract name after `@`): None where bwrap or python3 can't run here.
    fn connects(c: &Confine, cwd: &Path, addr: &str) -> Option<bool> {
        if crate::distro::which("python3").is_none() {
            eprintln!("no python3 to connect with: skipped");
            return None;
        }
        let script = format!(
            "python3 -c \"import socket,sys; a=sys.argv[1]; s=socket.socket(socket.AF_UNIX); \
             s.connect(chr(0)+a[1:] if a.startswith('@') else a); print('connected')\" '{addr}' \
             2>/dev/null || echo refused"
        );
        let out = run(c, cwd, &script)?;
        Some(stdout(&out) == "connected\n")
    }

    /// A socket of the user's (an agent's) and a folder of them (the
    /// runtime folder) are closed to the command, though they're in a
    /// folder it may write to; another socket there stays open.
    #[test]
    fn the_local_services_sockets_are_closed_to_the_command() {
        use std::os::unix::net::UnixListener;
        let (root, mut c) = fresh("sockets", &[]);
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (agent, bus, other) = (root.join("agent"), run_dir.join("bus"), root.join("other"));
        let _listeners: Vec<UnixListener> = [&agent, &bus, &other]
            .iter()
            .map(|p| UnixListener::bind(p).unwrap())
            .collect();
        c.writable = vec![root.clone()];
        c.sockets = vec![agent.clone(), run_dir.clone()];
        let script = format!(
            "[ -S {a} ] || echo agent-closed; ls -A {r} | wc -l; [ -S {o} ] && echo other-open; \
             echo x > {root}/written && echo written",
            a = agent.display(),
            r = run_dir.display(),
            o = other.display(),
            root = root.display(),
        );
        let Some(out) = run(&c, &root, &script) else {
            return;
        };
        assert_eq!(
            stdout(&out).replace(' ', ""),
            "agent-closed\n0\nother-open\nwritten\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for (path, open) in [(&agent, false), (&bus, false), (&other, true)] {
            if let Some(connected) = connects(&c, &root, &path.to_string_lossy()) {
                assert_eq!(connected, open, "{}", path.display());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A writable folder inside a folder of sockets (a temporary folder in
    /// the runtime folder) stays the command's.
    #[test]
    fn a_writable_folder_inside_a_folder_of_sockets_stays_writable() {
        let (root, mut c) = fresh("socketdir", &[]);
        let runtime = root.join("runtime");
        let tmp = runtime.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(runtime.join("keyring"), "").unwrap();
        c.writable = vec![tmp.clone()];
        c.sockets = vec![runtime.clone()];
        let script = format!(
            "[ -e {r}/keyring ] || echo closed; echo x > {t}/f && echo written",
            r = runtime.display(),
            t = tmp.display()
        );
        let Some(out) = run(&c, &tmp, &script) else {
            return;
        };
        assert_eq!(stdout(&out), "closed\nwritten\n");
        assert!(tmp.join("f").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An abstract socket made outside the sandbox (X11's) is closed to the
    /// command where the kernel can (Linux 6.12 on); one the command makes
    /// itself stays its own.
    #[test]
    fn an_abstract_socket_made_outside_is_closed_to_the_command() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixListener};
        if landlock::abstract_socket_scope() == 0 {
            eprintln!("this kernel has no Landlock scopes: skipped");
            return;
        }
        let (root, mut c) = fresh("abstract", &[]);
        c.writable = vec![root.clone()];
        let name = format!("rovik-test-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _listener = UnixListener::bind_addr(&addr).unwrap();
        if let Some(connected) = connects(&c, &root, &format!("@{name}")) {
            assert!(!connected);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The runtime folder and the system's service sockets are closed, as
    /// the file system has them, once; one that's the command's own
    /// writable folder isn't.
    #[test]
    fn the_local_sockets_are_the_existing_ones_but_the_writable() {
        let all = local_sockets(&[]);
        for path in &all {
            assert!(path.exists(), "{}", path.display());
            assert_eq!(path, &path.canonicalize().unwrap(), "{}", path.display());
        }
        let mut sorted = all.clone();
        sorted.dedup();
        assert_eq!(sorted, all);
        if let Some(first) = all.first() {
            assert!(!local_sockets(std::slice::from_ref(first)).contains(first));
        }
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
    fn the_secret_paths_and_writable_dirs_are_the_existing_ones() {
        for dir in &writable_dirs(&std::env::temp_dir()) {
            assert!(dir.is_dir(), "{}", dir.display());
            assert_eq!(dir, &dir.canonicalize().unwrap(), "{}", dir.display());
        }
        for path in &secret_paths() {
            assert!(path.exists(), "{}", path.display());
            assert_eq!(path, &path.canonicalize().unwrap(), "{}", path.display());
        }
        let registered = std::env::temp_dir().join(format!("protected-{}", std::process::id()));
        std::fs::create_dir_all(&registered).unwrap();
        protect([registered.clone(), registered.clone()]);
        assert!(secret_paths().contains(&registered.canonicalize().unwrap()));
        let _ = std::fs::remove_dir_all(&registered);
        assert!(writable_dirs(&std::env::temp_dir())
            .contains(&std::env::temp_dir().canonicalize().unwrap()));
    }

    /// A tool's folder is made before its first sandboxed run: the XDG ones
    /// always, a toolchain's only where it's installed; nothing the
    /// installer makes, and nothing in a home that isn't there.
    #[test]
    fn missing_tool_folders_are_made_where_a_tool_would_make_them() {
        let home = std::env::temp_dir().join(format!("confine-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        make_missing(&home, |_| true);
        assert!(!home.exists());
        std::fs::create_dir_all(&home).unwrap();
        make_missing(&home, |program| program == "go");
        assert!(home.join(".cache").is_dir());
        assert!(home.join(".local/state").is_dir());
        assert!(home.join("go").is_dir());
        assert!(!home.join(".npm").exists());
        assert!(!home.join(".rustup").exists());
        assert!(!home.join(".local/bin").exists());
        let _ = std::fs::remove_dir_all(&home);
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

    /// Linux prefers bubblewrap, then Landlock, and the status says which
    /// works here; a working sandbox refuses nothing.
    #[test]
    fn bubblewrap_is_the_mechanism_and_landlock_its_fallback() {
        // Read as the process started: tests don't set the variable.
        if std::env::var_os("ROVIK_CONFINE_COMMANDS").is_none() {
            let names: Vec<&str> = candidates().iter().map(|m| m.name()).collect();
            assert_eq!(names, ["bwrap", "landlock"]);
        }
        let status = status();
        assert!(status.required);
        assert_eq!(status.program, "bubblewrap");
        assert_eq!(status.working, chosen().is_some());
        assert_eq!(refusal().is_none(), status.working || switched_off());
        let name = chosen().map(|m| m.name());
        assert_eq!(status.fallback.is_some(), name == Some("landlock"));
        // Anything but bubblewrap at work says what's missing.
        assert_eq!(status.detail.is_none(), name == Some("bwrap"));
    }

    /// A command bwrap couldn't make the sandbox for never started; one
    /// that ran, failed or not, did.
    #[test]
    fn a_sandbox_bwrap_couldnt_make_is_told_from_a_command_that_failed() {
        let (root, mut c) = fresh("confineunmade", &[]);
        let never_started = |out: &std::process::Output| {
            let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
            BWRAP.never_started(out.status.code(), &text(&out.stdout), &text(&out.stderr))
        };
        for script in [
            "true",
            "exit 1",
            "echo failed >&2; exit 1",
            "echo out; exit 1",
        ] {
            let Some(out) = run(&c, &root, script) else {
                return;
            };
            assert!(!never_started(&out), "{script}");
        }
        // The view to show at the project's path is gone: bwrap can't bind
        // it, and the command doesn't run.
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        c.view = root.join("gone");
        c.project = project;
        let out = BWRAP
            .command(&c, "/bin/sh", &["-c", "echo ran"])
            .output()
            .unwrap();
        assert!(
            never_started(&out),
            "{:?} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod until_made_tests {
    use super::*;

    /// `until_made` over `ends`, one per try (`Ok(true)`: its sandbox
    /// couldn't be made; `Ok(false)`: the command ran): what it returned,
    /// and how many tries it took.
    fn tried(ends: &[Result<bool, &str>]) -> (Result<bool, String>, usize) {
        let mut tries = 0;
        let ended = until_made(
            || {
                tries += 1;
                ends[tries - 1].map_err(String::from)
            },
            |unmade: &bool| *unmade,
        );
        (ended, tries)
    }

    /// A command whose sandbox couldn't be made starts again, a moment
    /// later, until it's made.
    #[test]
    fn a_command_whose_sandbox_couldnt_be_made_starts_again_until_it_is() {
        assert_eq!(tried(&[Ok(true), Ok(false)]), (Ok(false), 2));
        assert_eq!(tried(&[Ok(true), Ok(true), Ok(false)]), (Ok(false), 3));
    }

    /// A command that ran, failed or not, never runs again, nor one that
    /// couldn't be started at all.
    #[test]
    fn a_command_that_ran_or_couldnt_be_started_isnt_tried_again() {
        assert_eq!(tried(&[Ok(false), Ok(false)]), (Ok(false), 1));
        assert_eq!(
            tried(&[Err("no shell"), Ok(false)]),
            (Err("no shell".into()), 1)
        );
    }

    /// A sandbox that can't be made is tried a few times in all; the last
    /// try's result is what the caller gets.
    #[test]
    fn a_sandbox_that_cant_be_made_is_tried_a_few_times_then_reported() {
        let mut ends = vec![Ok(true); SANDBOX_TRIES as usize];
        ends.push(Ok(false));
        assert_eq!(tried(&ends), (Ok(true), SANDBOX_TRIES as usize));
    }
}

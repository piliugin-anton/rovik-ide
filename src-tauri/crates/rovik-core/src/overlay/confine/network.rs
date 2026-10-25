//! The private network a team server's daemon gives its user's commands
//! (D20).
//!
//! On a server several people share, its ports are shared too: two users'
//! dev servers clash on 3000, and one user's agent can reach, and test,
//! another user's server by mistake. So the daemon gives its user one
//! network of their own, as their own computer would have: every command
//! of theirs, and every run of a connector's program, starts in it. A
//! server there can listen on any port and address without clashing with
//! anyone else's, and no one else's process can reach it. Inside it the
//! ports are this user's to share among their own servers: one that a
//! server holds is busy for the others, as on a computer.
//!
//! - **The namespaces.** A holder (`cat`, reading a pipe only the daemon
//!   writes) starts in a new user and network namespace, and the daemon
//!   writes its maps from outside, mapping only this user, to itself: what
//!   Ubuntu's AppArmor restriction on unprivileged user namespaces still
//!   lets a program without a profile do. A program joins both as it
//!   starts ([`Network::join`]), before its sandbox's own steps (bubblewrap
//!   then makes its namespaces inside).
//! - **The way out.** pasta (the `passt` package) attaches to the holder,
//!   gives the namespace the host's addresses and routes, and carries its
//!   connections out through ordinary sockets of this user's. Nothing comes
//!   in through it (`-t none -u none`): a server bound to 0.0.0.0 there is
//!   reachable only from inside. From the namespace's loopback it forwards
//!   only the ports of the proxies the daemon's environment names on this
//!   machine's loopback, so installs through a local proxy work. The
//!   server's own services are reached at the gateway's address, which
//!   pasta maps to the server's loopback.
//! - **DNS.** Where the host's resolver is on its loopback (systemd-resolved's
//!   127.0.0.53), which the namespace can't see, a joined program sees a
//!   resolv.conf naming pasta's DNS forwarder in its place, bound over
//!   /etc/resolv.conf in a mount namespace of its own.
//! - **Reaching in.** The daemon stays outside. It reaches a server in the
//!   network with a socket a short-lived process makes inside and hands
//!   back ([`Network::connect`], [`Network::listen`]); a service's port
//!   listens inside the same way ([`Network::listen_on`]), and a container
//!   of this user's is reached in its own namespaces ([`Namespaces`]).
//! - **Kept going.** A watcher starts the holder (in the same namespaces)
//!   or pasta again if either ends, until the network is dropped.
//!
//! A desktop, and WSL, are one person's computer: they keep the host's
//! network. Where pasta is missing or user namespaces are forbidden, a team
//! server's commands share the server's network, as before, and
//! `rovik-agentd doctor` says why.

use super::pages;
use std::ffi::{CString, OsString};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// pasta: the passt package's program for network namespaces.
const PASTA: &str = "pasta";

/// What a joined program's resolv.conf names where the host's resolver is
/// on its loopback: pasta forwards DNS queries sent there to the host's
/// resolver. Link-local, so it shadows no real server.
const DNS_FORWARDER: &str = "169.254.1.1";

/// The host's resolv.conf.
const HOST_RESOLV: &str = "/etc/resolv.conf";

/// The programs a command opens a page in a browser with: each, in the
/// network, asks Rovik to open it instead (pages.rs).
const BROWSERS: &[&str] = &[
    "xdg-open",
    "sensible-browser",
    "x-www-browser",
    "www-browser",
];

/// How long pasta may take to set the namespace up.
const READY_WITHIN: Duration = Duration::from_secs(10);

/// How long a connection into the network may take to be accepted.
const CONNECT_WITHIN: libc::timeval = libc::timeval {
    tv_sec: 5,
    tv_usec: 0,
};

/// How often the watcher looks at the holder and pasta.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// The longest the watcher waits before starting pasta again, when it
/// keeps ending.
const MOST_BACKOFF: Duration = Duration::from_secs(30);

/// This user's network, once a team server's daemon made it ([`enable`]).
static CURRENT: Mutex<Option<Arc<Network>>> = Mutex::new(None);

/// This user's private network from now on: what a team server's daemon
/// calls once as it starts, with `dir` its folder (made again, empty) and
/// `opener` the program its pages' shims run as `<opener> open URL` (the
/// daemon's own binary). Says why there's none when it can't be made: the
/// commands share the server's network then.
pub fn enable(dir: &Path, opener: &Path) -> Result<(), String> {
    let network = Network::start(dir, opener)?;
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(network));
    Ok(())
}

/// This user's private network, where a team server's daemon made one.
pub fn current() -> Option<Arc<Network>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A network namespace of this user's, with its way out (module docs).
pub struct Network {
    /// Its user and network namespaces, held open: joined programs enter
    /// them, and they live as long as these do.
    user: OwnedFd,
    net: OwnedFd,
    /// The holder's pid: where /proc shows the namespace's sockets. It
    /// changes when the watcher starts the holder again.
    holder: Arc<AtomicU32>,
    /// The file a joined program sees at the host's resolv.conf, and that
    /// path: C strings, made before any fork.
    resolv: Option<(CString, CString)>,
    /// Its folder: pasta's log, the resolv.conf, the pages' shims and
    /// socket.
    dir: PathBuf,
    /// Told to stop, the watcher ends pasta and the holder.
    stop: Arc<(Mutex<bool>, Condvar)>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

/// A port something listens on in the network, and what holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    pub pid: u32,
    /// Its command line, where it can be read.
    pub command: Option<String>,
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Network")
            .field("holder", &self.holder.load(Ordering::Relaxed))
            .field("dir", &self.dir)
            .finish()
    }
}

impl Network {
    /// A new network, its folder `dir` (made again, empty), its pages'
    /// shims running `opener`.
    pub fn start(dir: &Path, opener: &Path) -> Result<Network, String> {
        let ports = loopback_proxy_ports(std::env::vars_os());
        Network::start_with(dir, opener, Path::new(HOST_RESOLV), &ports)
    }

    /// `start`, reading the host's resolv.conf at `host_resolv`, the
    /// network reaching `ports` of the host's loopback at the same address.
    fn start_with(
        dir: &Path,
        opener: &Path,
        host_resolv: &Path,
        ports: &[u16],
    ) -> Result<Network, String> {
        let pasta = crate::distro::which(PASTA).ok_or_else(|| {
            "pasta isn't installed (the passt package), which carries a private network's \
             connections out"
                .to_string()
        })?;
        let _ = std::fs::remove_dir_all(dir);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        // What's made from here on goes if a later step fails.
        let mut made = Made::new(dir);
        let holder = spawn_holder(None)
            .map_err(|e| format!("couldn't make the network's namespaces: {e}"))?;
        let pid = holder.id();
        made.holder = Some(holder);
        map_to_self(pid).map_err(|e| {
            format!("couldn't map this user into the network's user namespace: {e}")
        })?;
        let open = |kind: &str| {
            std::fs::File::open(format!("/proc/{pid}/ns/{kind}"))
                .map(OwnedFd::from)
                .map_err(|e| format!("couldn't open the network's {kind} namespace: {e}"))
        };
        let (user, net) = (open("user")?, open("net")?);
        let resolv = resolv_for(host_resolv, dir)?;
        write_shims(dir, opener).map_err(|e| format!("{}: {e}", dir.join("bin").display()))?;
        let ports = ports.to_vec();
        let mut pasta_child = spawn_pasta(&pasta, pid, &ports, dir)
            .map_err(|e| format!("couldn't start {}: {e}", pasta.display()))?;
        if let Err(why) = wait_configured(&mut pasta_child, pid, &ports, dir) {
            let _ = pasta_child.kill();
            let _ = pasta_child.wait();
            return Err(why);
        }
        let holder = made.holder.take().expect("the holder was started above");
        made.keep();
        let pid_shared = Arc::new(AtomicU32::new(pid));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let watched = Watched {
            holder,
            pasta: pasta_child,
            program: pasta,
            ports,
            dir: dir.to_path_buf(),
            namespaces: (user.as_raw_fd(), net.as_raw_fd()),
            pid: pid_shared.clone(),
        };
        let stopping = stop.clone();
        let watcher = std::thread::Builder::new()
            .name("private network".into())
            .spawn(move || watch(watched, &stopping))
            .map_err(|e| format!("couldn't watch the private network: {e}"))?;
        Ok(Network {
            user,
            net,
            holder: pid_shared,
            resolv,
            dir: dir.to_path_buf(),
            stop,
            watcher: Some(watcher),
        })
    }

    /// Makes `cmd` start in the network, before anything else it does as
    /// it starts: it joins the namespaces and, where the host's resolver is
    /// on its loopback, sees the network's resolv.conf. A sandbox's own
    /// steps, added after, come after.
    pub fn join(&self, cmd: &mut Command) {
        let (user, net) = (self.user.as_raw_fd(), self.net.as_raw_fd());
        let resolv = self.resolv.clone();
        // SAFETY: between fork and exec it makes only system calls (setns,
        // unshare, mount) with descriptors and strings made before the
        // fork, which `self` keeps open while the command starts.
        unsafe {
            cmd.pre_exec(move || enter(user, net, resolv.as_ref()));
        }
    }

    /// Gives `cmd`, a program that runs in the network for `chat`, the
    /// network's browser in its environment ([`Network::env`]), on top of
    /// the PATH it's given or would inherit.
    pub fn give_browser(&self, cmd: &mut Command, chat: &str) {
        let path = cmd
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .and_then(|(_, value)| value.map(OsString::from))
            .or_else(|| crate::process::child_var("PATH"));
        for (key, value) in self.env(chat, path) {
            cmd.env(key, value);
        }
    }

    /// What a program in the network gets in its environment, for `chat`:
    /// the pages' shims first on its PATH (`path`, the PATH it would get
    /// otherwise) and as its BROWSER, and where they ask (pages.rs).
    pub fn env(&self, chat: &str, path: Option<OsString>) -> Vec<(&'static str, OsString)> {
        let bin = self.dir.join("bin");
        let mut dirs = vec![bin.clone()];
        dirs.extend(std::env::split_paths(&path.unwrap_or_default()));
        vec![
            (
                "PATH",
                std::env::join_paths(dirs).unwrap_or_else(|_| bin.clone().into_os_string()),
            ),
            ("BROWSER", bin.join(BROWSERS[0]).into_os_string()),
            (pages::SOCKET_VAR, self.pages_socket().into_os_string()),
            (pages::CHAT_VAR, OsString::from(chat)),
        ]
    }

    /// Its folder, which the commands may read but not change.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where its pages' shims ask (pages.rs).
    pub fn pages_socket(&self) -> PathBuf {
        self.dir.join("pages.sock")
    }

    /// A connection to `port` on the network's loopback, made inside it:
    /// to 127.0.0.1, then to ::1, where a server for "localhost" may listen
    /// alone.
    pub fn connect(&self, port: u16) -> io::Result<std::net::TcpStream> {
        let to = [
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ];
        self.socket_inside(Inside::Connect(to.map(raw_address)))
            .map(std::net::TcpStream::from)
    }

    /// A socket listening on 127.0.0.1 in the network, on a port of the
    /// system's choosing: where a program in it reaches Rovik (a
    /// connector's MCP server).
    pub fn listen(&self) -> io::Result<std::net::TcpListener> {
        self.socket_inside(Inside::Listen(raw_address(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        )))))
        .map(std::net::TcpListener::from)
    }

    /// Sockets listening on `port` of the network's loopback, where a
    /// program in it reaches a project's service (D21): on 127.0.0.1, and
    /// on ::1 as well where the network has IPv6, since a client may look
    /// `localhost` up as either. An error when 127.0.0.1's can't be made,
    /// the port busy in the network among them.
    pub fn listen_on(&self, port: u16) -> io::Result<Vec<std::net::TcpListener>> {
        let v4 = self.socket_inside(Inside::Listen(raw_address(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            port,
        )))))?;
        let mut listening = vec![std::net::TcpListener::from(v4)];
        let v6 = self.socket_inside(Inside::Listen(raw_address(SocketAddr::from((
            Ipv6Addr::LOCALHOST,
            port,
        )))));
        match v6 {
            Ok(v6) => listening.push(std::net::TcpListener::from(v6)),
            Err(e) => log::debug!("no IPv6 listener on port {port} of the network: {e}"),
        }
        Ok(listening)
    }

    /// The processes listening on `port` in the network, each with its
    /// command line where it can be read: what keeps the port busy.
    pub fn listening_on(&self, port: u16) -> Vec<(u32, Option<String>)> {
        self.listening()
            .into_iter()
            .filter(|listener| listener.port == port)
            .map(|listener| (listener.pid, listener.command))
            .collect()
    }

    /// Every port something listens on in the network, once each.
    pub fn listening_ports(&self) -> Vec<u16> {
        let net = PathBuf::from(format!("/proc/{}/net", self.holder.load(Ordering::Relaxed)));
        let mut ports: Vec<u16> = crate::listening::sockets(&net)
            .map(|sockets| sockets.into_values().collect())
            .unwrap_or_default();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Every port something listens on in the network, with the process
    /// holding it, as this user's processes show them.
    pub fn listening(&self) -> Vec<Listener> {
        let net = PathBuf::from(format!("/proc/{}/net", self.holder.load(Ordering::Relaxed)));
        let Ok(sockets) = crate::listening::sockets(&net) else {
            return Vec::new();
        };
        let processes = crate::listening::processes().unwrap_or_default();
        crate::listening::holders(&sockets, processes)
            .into_iter()
            .map(|(pid, port)| Listener {
                port,
                pid,
                command: crate::listening::command_line(pid),
            })
            .collect()
    }

    /// The socket `inside` makes, made by a short-lived process that joins
    /// the network and hands it back: the daemon itself stays outside.
    fn socket_inside(&self, inside: Inside) -> io::Result<OwnedFd> {
        socket_in(Some(self.user.as_raw_fd()), self.net.as_raw_fd(), inside)
    }
}

/// The namespaces of a process in one of this user's rootless containers,
/// held open: how the daemon reaches a service in it (D21), whose
/// namespaces the user owns. Held, they stay that container's even once
/// its process is gone and its pid another's.
pub struct Namespaces {
    /// Its user namespace; None when it's this process's own (a root
    /// test's container), which can't be entered again.
    user: Option<OwnedFd>,
    net: OwnedFd,
}

impl Namespaces {
    /// Process `pid`'s; an error when its network is this process's own,
    /// which no container's is: a connection made there would reach the
    /// server's loopback, every user's.
    pub fn of(pid: u32) -> io::Result<Namespaces> {
        let open =
            |kind: &str| std::fs::File::open(format!("/proc/{pid}/ns/{kind}")).map(OwnedFd::from);
        let (user, net) = (open("user")?, open("net")?);
        if same_namespace(&net, "net")? {
            return Err(io::Error::other(format!(
                "process {pid} is in the daemon's own network, not a container's"
            )));
        }
        let user = (!same_namespace(&user, "user")?).then_some(user);
        Ok(Namespaces { user, net })
    }

    /// A connection to `port` on its network's loopback, made inside it
    /// (its user namespace joined first): to 127.0.0.1, then ::1.
    pub fn connect(&self, port: u16) -> io::Result<std::net::TcpStream> {
        let to = [
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ];
        socket_in(
            self.user.as_ref().map(|fd| fd.as_raw_fd()),
            self.net.as_raw_fd(),
            Inside::Connect(to.map(raw_address)),
        )
        .map(std::net::TcpStream::from)
    }
}

/// Whether the namespace open as `fd` is this process's own of `kind`.
fn same_namespace(fd: &OwnedFd, kind: &str) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let own = std::fs::metadata(format!("/proc/self/ns/{kind}"))?;
    let theirs = std::fs::File::from(fd.try_clone()?).metadata()?;
    Ok(own.ino() == theirs.ino() && own.dev() == theirs.dev())
}

/// The socket `inside` makes, made by a short-lived process that joins the
/// user namespace `user` (where given) and the network namespace `net` and
/// hands it back: the caller itself stays outside.
fn socket_in(user: Option<RawFd>, net: RawFd, inside: Inside) -> io::Result<OwnedFd> {
    let (ours, theirs) = UnixStream::pair()?;
    let back = theirs.as_raw_fd();
    // Any program will do: the work is done as it starts.
    let mut cmd = crate::process::command("true");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: between fork and exec it makes only system calls (setns,
    // socket, setsockopt, connect, bind, listen, sendmsg, close) with
    // descriptors and addresses made before the fork.
    unsafe {
        cmd.pre_exec(move || {
            if let Some(user) = user {
                if libc::setns(user, libc::CLONE_NEWUSER) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setns(net, libc::CLONE_NEWNET) != 0 {
                return Err(io::Error::last_os_error());
            }
            let socket = inside.make()?;
            let sent = send_descriptor(back, socket);
            libc::close(socket);
            sent
        });
    }
    // An error of the steps above is the spawn's.
    let status = cmd.status()?;
    drop(theirs);
    if !status.success() {
        return Err(io::Error::other(format!(
            "the process that joins the network ended with {status}"
        )));
    }
    receive_descriptor(&ours)
}

impl Drop for Network {
    fn drop(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped.lock().unwrap_or_else(|e| e.into_inner()) = true;
        wake.notify_all();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What the watcher keeps going.
struct Watched {
    holder: Child,
    pasta: Child,
    program: PathBuf,
    ports: Vec<u16>,
    dir: PathBuf,
    /// The namespaces a new holder joins: the network's descriptors, open
    /// until the watcher has ended.
    namespaces: (RawFd, RawFd),
    pid: Arc<AtomicU32>,
}

/// Starts the holder or pasta again when either ends, until told to stop;
/// then ends both.
fn watch(mut w: Watched, stop: &(Mutex<bool>, Condvar)) {
    let mut backoff = WATCH_EVERY;
    let mut next_pasta: Option<Instant> = None;
    let mut pasta_since = Instant::now();
    loop {
        let (stopped, wake) = stop;
        let guard = stopped.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = wake
            .wait_timeout_while(guard, WATCH_EVERY, |stopped| !*stopped)
            .unwrap_or_else(|e| e.into_inner());
        if *guard {
            break;
        }
        drop(guard);
        if let Ok(Some(status)) = w.holder.try_wait() {
            log::warn!("the private network's holder ended ({status}): starting it again");
            match spawn_holder(Some(w.namespaces)) {
                Ok(holder) => {
                    w.pid.store(holder.id(), Ordering::Relaxed);
                    w.holder = holder;
                    // pasta follows the holder it attached to: again, now.
                    let _ = w.pasta.kill();
                }
                Err(e) => log::error!("the private network's holder couldn't start again: {e}"),
            }
        }
        match (w.pasta.try_wait(), next_pasta) {
            (Ok(Some(status)), None) => {
                // A pasta that ran a while ended by itself, not at once.
                if pasta_since.elapsed() > MOST_BACKOFF {
                    backoff = WATCH_EVERY;
                }
                log::warn!(
                    "the private network's pasta ended ({status}), starting it again in {backoff:?}: {}",
                    log_tail(&w.dir)
                );
                next_pasta = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(MOST_BACKOFF);
            }
            (_, Some(at)) if Instant::now() >= at => {
                next_pasta = None;
                match spawn_pasta(&w.program, w.pid.load(Ordering::Relaxed), &w.ports, &w.dir) {
                    Ok(pasta) => {
                        w.pasta = pasta;
                        pasta_since = Instant::now();
                    }
                    Err(e) => {
                        log::error!("the private network's pasta couldn't start again: {e}");
                        next_pasta = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(MOST_BACKOFF);
                    }
                }
            }
            _ => {}
        }
    }
    let _ = w.pasta.kill();
    let _ = w.pasta.wait();
    let _ = w.holder.kill();
    let _ = w.holder.wait();
}

/// What `start` made so far, undone unless kept: the holder and the folder.
struct Made {
    dir: PathBuf,
    holder: Option<Child>,
    kept: bool,
}

impl Made {
    fn new(dir: &Path) -> Made {
        Made {
            dir: dir.to_path_buf(),
            holder: None,
            kept: false,
        }
    }

    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        if let Some(mut holder) = self.holder.take() {
            let _ = holder.kill();
            let _ = holder.wait();
        }
        if !self.kept {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The holder: `cat`, reading a pipe whose other end the watcher holds, so
/// it ends with the network, or with the daemon. In new namespaces, or in
/// `rejoin`'s (user, network) when it's started again.
fn spawn_holder(rejoin: Option<(RawFd, RawFd)>) -> io::Result<Child> {
    let mut cmd = crate::process::command("cat");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: between fork and exec it makes only system calls (unshare, or
    // setns on descriptors the network keeps open).
    unsafe {
        cmd.pre_exec(move || match rejoin {
            Some((user, net)) => enter(user, net, None),
            None => {
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
        });
    }
    // Spawned once it has exec'd: its namespaces are made by now.
    cmd.spawn()
}

/// Maps this user, alone and to itself, into the user namespace of process
/// `pid`, from outside it: the way an unprivileged process may, even where
/// AppArmor keeps the namespace's own process from writing its maps.
fn map_to_self(pid: u32) -> io::Result<()> {
    // SAFETY: neither call can fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    std::fs::write(format!("/proc/{pid}/setgroups"), "deny")?;
    std::fs::write(format!("/proc/{pid}/uid_map"), format!("{uid} {uid} 1\n"))?;
    std::fs::write(format!("/proc/{pid}/gid_map"), format!("{gid} {gid} 1\n"))
}

/// Joins the user namespace `user` and the network namespace `net`, then
/// binds `resolv`'s file over its path in a mount namespace of its own:
/// between fork and exec, making only system calls.
fn enter(user: RawFd, net: RawFd, resolv: Option<&(CString, CString)>) -> io::Result<()> {
    // SAFETY: system calls on descriptors and C strings that outlive them.
    unsafe {
        if libc::setns(user, libc::CLONE_NEWUSER) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setns(net, libc::CLONE_NEWNET) != 0 {
            return Err(io::Error::last_os_error());
        }
        if let Some((file, over)) = resolv {
            // Owned by the joined user namespace, it doesn't pass its
            // mounts back to the host's.
            if libc::unshare(libc::CLONE_NEWNS) != 0 {
                return Err(io::Error::last_os_error());
            }
            let bound = libc::mount(
                file.as_ptr(),
                over.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            );
            if bound != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// pasta attached to the holder `holder`: the host's addresses and routes
/// in the namespace, nothing forwarded in, the server's loopback `ports`
/// reached from the namespace's, DNS queries to [`DNS_FORWARDER`] answered
/// by the host's resolver; what it says in its log.
fn spawn_pasta(program: &Path, holder: u32, ports: &[u16], dir: &Path) -> io::Result<Child> {
    let log = std::fs::File::create(dir.join("pasta.log"))?;
    let mut cmd = crate::process::command(program);
    cmd.args(pasta_args(holder, ports))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    cmd.spawn()
}

/// pasta's arguments (`spawn_pasta`): in the foreground, the daemon's child.
fn pasta_args(holder: u32, ports: &[u16]) -> Vec<String> {
    let reached = match ports {
        [] => "none".to_string(),
        ports => ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(","),
    };
    let mut args = [
        "--foreground",
        "--quiet",
        "--config-net",
        "--tcp-ports",
        "none",
        "--udp-ports",
        "none",
        "--tcp-ns",
        &reached,
        "--udp-ns",
        "none",
        "--dns-forward",
        DNS_FORWARDER,
        &holder.to_string(),
    ]
    .map(String::from)
    .to_vec();
    // Started by root (a test; the daemon never runs as root), pasta would
    // become nobody, who can't open root's namespaces.
    // SAFETY: geteuid can't fail.
    if unsafe { libc::geteuid() } == 0 {
        args.splice(0..0, ["--runas".to_string(), "0".to_string()]);
    }
    args
}

/// Waits until pasta has set the namespace of holder `holder` up: it has a
/// default route, which pasta adds once the addresses are in place, and
/// pasta listens on each of the server's loopback `ports` it reaches, which
/// comes a little after. Why not, when pasta ends first or takes too long.
fn wait_configured(
    pasta: &mut Child,
    holder: u32,
    ports: &[u16],
    dir: &Path,
) -> Result<(), String> {
    let deadline = Instant::now() + READY_WITHIN;
    loop {
        if let Ok(Some(status)) = pasta.try_wait() {
            return Err(format!(
                "pasta couldn't set the network up ({status}): {}",
                log_tail(dir)
            ));
        }
        if has_default_route(holder) && listens_on(holder, ports) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pasta didn't set the network up within {} s: {}",
                READY_WITHIN.as_secs(),
                log_tail(dir)
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Whether the network namespace of process `pid` has a default route, as
/// /proc shows it from outside.
fn has_default_route(pid: u32) -> bool {
    let read = |table: &str| std::fs::read_to_string(format!("/proc/{pid}/net/{table}"));
    let v4 = read("route").unwrap_or_default();
    // Iface, Destination (hex, 00000000 for the default), ...
    let ipv4 = v4
        .lines()
        .skip(1)
        .any(|row| row.split_whitespace().nth(1) == Some("00000000"));
    let v6 = read("ipv6_route").unwrap_or_default();
    // Destination, prefix length, ..., the device last: ::/0 on a device
    // other than the loopback.
    let ipv6 = v6.lines().any(|row| {
        let columns: Vec<&str> = row.split_whitespace().collect();
        columns.first() == Some(&"00000000000000000000000000000000")
            && columns.get(1) == Some(&"00")
            && columns.last() != Some(&"lo")
    });
    ipv4 || ipv6
}

/// Whether something listens on each of `ports` in the network namespace
/// of process `pid`, as /proc shows it from outside.
fn listens_on(pid: u32, ports: &[u16]) -> bool {
    if ports.is_empty() {
        return true;
    }
    let net = PathBuf::from(format!("/proc/{pid}/net"));
    let listening = crate::listening::sockets(&net).unwrap_or_default();
    ports
        .iter()
        .all(|port| listening.values().any(|listened| listened == port))
}

/// The end of pasta's log, for a message.
fn log_tail(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join("pasta.log")).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match &lines[lines.len().saturating_sub(4)..] {
        [] => "it said nothing".to_string(),
        tail => tail.join(" / "),
    }
}

/// Where the host's resolv.conf at `host` names a resolver on its loopback,
/// which the namespace can't reach: the same file with pasta's DNS
/// forwarder in its place, written in `dir`, and the path it goes over.
/// None where nothing needs replacing, or there's no resolv.conf.
fn resolv_for(host: &Path, dir: &Path) -> Result<Option<(CString, CString)>, String> {
    let Ok(text) = std::fs::read_to_string(host) else {
        return Ok(None);
    };
    let Some(text) = through_forwarder(&text) else {
        return Ok(None);
    };
    let file = dir.join("resolv.conf");
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644));
    let c = |path: &Path| {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("{} has a NUL in its path", path.display()))
    };
    Ok(Some((c(&file)?, c(host)?)))
}

/// `text`, a resolv.conf, with its nameservers on the loopback replaced by
/// pasta's DNS forwarder (once, where the first was): None when none is on
/// the loopback.
fn through_forwarder(text: &str) -> Option<String> {
    let mut replaced = false;
    let mut out = String::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let on_loopback = words.next() == Some("nameserver")
            && words
                .next()
                .and_then(|address| address.split('%').next())
                .and_then(|address| address.parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
        if on_loopback {
            if !replaced {
                out.push_str(&format!("nameserver {DNS_FORWARDER}\n"));
                replaced = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    replaced.then_some(out)
}

/// The ports of the proxies `vars` (an environment) name on this machine's
/// loopback (`HTTPS_PROXY=http://127.0.0.1:3128`): the network reaches
/// them at the same address, so a command's installs go through them.
fn loopback_proxy_ports(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<u16> {
    const PROXIES: &[&str] = &["http_proxy", "https_proxy", "all_proxy", "ftp_proxy"];
    let mut ports: Vec<u16> = vars
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.to_str()?.to_ascii_lowercase();
            PROXIES
                .contains(&name.as_str())
                .then(|| loopback_port(value.to_str()?))?
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The port of the proxy at `address` (`http://127.0.0.1:3128`,
/// `localhost:8080`, `socks5h://[::1]:1080`) when it's on the loopback.
fn loopback_port(address: &str) -> Option<u16> {
    let address = address.trim();
    let url = match address.contains("://") {
        true => reqwest::Url::parse(address),
        false => reqwest::Url::parse(&format!("http://{address}")),
    }
    .ok()?;
    let host = url.host_str()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let local = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !local {
        return None;
    }
    url.port_or_known_default()
        .or_else(|| url.scheme().starts_with("socks").then_some(1080))
}

/// Writes the pages' shims in `dir`/bin: each program a command opens a
/// page with, as a script that runs `<opener> open URL` (pages.rs).
fn write_shims(dir: &Path, opener: &Path) -> io::Result<()> {
    let bin = dir.join("bin");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&bin)?;
    let opener = crate::distro::sh_quote(&opener.to_string_lossy());
    let script = format!(
        "#!/bin/sh\n\
         # Rovik: asks Rovik to open the page in the user's browser, on their\n\
         # computer (docs/cloud-hosts.md).\n\
         exec {opener} open \"$@\"\n"
    );
    for name in BROWSERS {
        let path = bin.join(name);
        std::fs::write(&path, &script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// A socket address as the system calls take it.
fn raw_address(address: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: an all-zero sockaddr_storage is valid.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match address {
        SocketAddr::V4(v4) => {
            let raw = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: sockaddr_storage is larger than, and aligned for,
            // sockaddr_in.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), raw) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(v6) => {
            let raw = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: 0,
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: 0,
            };
            // SAFETY: as above, for sockaddr_in6.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), raw) };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, length as libc::socklen_t)
}

/// A socket the short-lived process makes in the network.
enum Inside {
    /// Connected to the first of these that accepts.
    Connect([(libc::sockaddr_storage, libc::socklen_t); 2]),
    /// Listening at this address.
    Listen((libc::sockaddr_storage, libc::socklen_t)),
}

impl Inside {
    /// Makes it: between fork and exec, making only system calls.
    fn make(&self) -> io::Result<RawFd> {
        match self {
            Inside::Connect(to) => {
                // A refusal says more than an address family the system
                // lacks (IPv6 turned off): that's kept only without one.
                let mut refused = None;
                let mut unmade = None;
                for (address, length) in to {
                    // SAFETY: system calls on a socket made here, with an
                    // address of the length given.
                    unsafe {
                        let socket = libc::socket(
                            address.ss_family as libc::c_int,
                            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                            0,
                        );
                        if socket < 0 {
                            unmade = Some(io::Error::last_os_error());
                            continue;
                        }
                        // A full backlog would hold a connect for minutes.
                        libc::setsockopt(
                            socket,
                            libc::SOL_SOCKET,
                            libc::SO_SNDTIMEO,
                            (&CONNECT_WITHIN as *const libc::timeval).cast(),
                            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                        );
                        if libc::connect(
                            socket,
                            (address as *const libc::sockaddr_storage).cast(),
                            *length,
                        ) == 0
                        {
                            return Ok(socket);
                        }
                        refused = Some(io::Error::last_os_error());
                        libc::close(socket);
                    }
                }
                Err(refused
                    .or(unmade)
                    .unwrap_or_else(|| io::Error::from_raw_os_error(libc::ECONNREFUSED)))
            }
            Inside::Listen((address, length)) => {
                // SAFETY: as above.
                unsafe {
                    let socket = libc::socket(
                        address.ss_family as libc::c_int,
                        libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                        0,
                    );
                    if socket < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // A port listened on again soon after (a service's,
                    // started again) isn't held up by the connections
                    // the last listener's clients left closing.
                    let on: libc::c_int = 1;
                    libc::setsockopt(
                        socket,
                        libc::SOL_SOCKET,
                        libc::SO_REUSEADDR,
                        (&on as *const libc::c_int).cast(),
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    );
                    if address.ss_family as libc::c_int == libc::AF_INET6 {
                        // ::1 alone, not 127.0.0.1's port too.
                        libc::setsockopt(
                            socket,
                            libc::IPPROTO_IPV6,
                            libc::IPV6_V6ONLY,
                            (&on as *const libc::c_int).cast(),
                            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                        );
                    }
                    let address = (address as *const libc::sockaddr_storage).cast();
                    if libc::bind(socket, address, *length) != 0 || libc::listen(socket, 128) != 0 {
                        let error = io::Error::last_os_error();
                        libc::close(socket);
                        return Err(error);
                    }
                    Ok(socket)
                }
            }
        }
    }
}

/// Room for one descriptor's control message, aligned as cmsghdr needs.
type ControlRoom = [u64; 4];

/// Sends descriptor `fd` over the Unix socket `over`: between fork and
/// exec, making only a system call.
fn send_descriptor(over: RawFd, fd: RawFd) -> io::Result<()> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut room: ControlRoom = [0; 4];
    // SAFETY: a message on the stack whose control part has room for one
    // descriptor, sent on a socket that's open.
    unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = room.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        if libc::sendmsg(over, &message, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The descriptor that came over `over` (`send_descriptor`).
fn receive_descriptor(over: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut room: ControlRoom = [0; 4];
    // SAFETY: a message on the stack with room for one descriptor; the one
    // received is new, close-on-exec, and ours alone.
    unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = room.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of::<ControlRoom>() as _;
        if libc::recvmsg(over.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null()
            || (*header).cmsg_level != libc::SOL_SOCKET
            || (*header).cmsg_type != libc::SCM_RIGHTS
        {
            return Err(io::Error::other(
                "no socket came back from the network's process",
            ));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// A network for a test, its shims running `opener`, reaching `ports` of
/// the host's loopback, the host's resolv.conf read at `resolv` (the real
/// one when None): None where pasta isn't installed, and the test is then
/// skipped. Anywhere pasta is, a network must be made.
#[cfg(test)]
pub(crate) fn for_test(
    opener: &Path,
    ports: &[u16],
    resolv: Option<&Path>,
) -> Option<(crate::testing::TempDir, Network)> {
    if crate::distro::which(PASTA).is_none() {
        eprintln!("pasta isn't installed: skipped");
        return None;
    }
    let dir = crate::testing::temp_dir();
    let resolv = resolv.unwrap_or(Path::new(HOST_RESOLV));
    let network = Network::start_with(&dir.0.join("net"), opener, resolv, ports)
        .expect("pasta is installed, so a private network is made");
    Some((dir, network))
}

#[cfg(test)]
mod tests;

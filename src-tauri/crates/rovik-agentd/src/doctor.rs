//! `rovik-agentd doctor`: what this account and server have and lack for
//! the daemon (P03, R03). Its checks, a line each: problems stop the
//! daemon from starting; warnings name what won't work (the browser tools
//! without Chrome, CLM without FUSE). Then what the connection test reads
//! too once the daemon is deployed (H04): the versions, the file systems
//! Rovik's folders are on, Rovik's install and its limits, the heavy-work
//! slots, and what the folders and the logs take. `--json` prints all of
//! it as `host::Doctor`.

use crate::auth::Tokens;
use crate::cli::{Options, VERSION};
use crate::config::Config;
use crate::daemon::MIN_PROTOCOL;
use crate::home::{self, Home};
use crate::host;
use crate::limits::{size_of, AdminFile, ADMIN_FILE};
use crate::network;
use rovik_protocol::connectors::ConnectorKind;
use rovik_protocol::host::{Disk, Doctor, Finding, InstallLimits, Level, Storage};
use rovik_protocol::PROTOCOL;
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn doctor(options: Options) -> Result<(), String> {
    let json = options.json;
    let report = gather(&Server::this(options)?);
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render(&report));
    }
    match report
        .findings
        .iter()
        .filter(|f| f.level == Level::Problem)
        .count()
    {
        0 => Ok(()),
        n => Err(format!("{n} problem(s) to fix before the daemon can start")),
    }
}

/// Where doctor looks: this account and server, or a test's stand-ins.
pub struct Server {
    pub home: Home,
    pub socket: PathBuf,
    pub admin_file: PathBuf,
    /// /proc, for other users' daemons, and /proc/sys, for user namespaces.
    pub proc: PathBuf,
    pub sys: PathBuf,
    /// /sys/fs/cgroup, where systemd keeps the daemons' units.
    pub cgroups: PathBuf,
    pub uid: u32,
    /// This account's name.
    pub user: String,
    /// The virtual files' driver and version, or why there are none.
    pub virtual_files: Result<String, String>,
    pub chrome: Option<PathBuf>,
    /// `bwrap`, which confines the chats' commands (overlay/confine.rs).
    pub bwrap: Option<PathBuf>,
    /// The kernel's Landlock confines commands where bubblewrap isn't.
    pub landlock: bool,
    /// Whether a private network can be made for this user's commands
    /// (network.rs): None where none is wanted (no install for everyone,
    /// WSL).
    pub private_network: Option<Result<(), String>>,
    /// What runs this user's project services (installer/containers.rs):
    /// None where Rovik isn't installed for everyone, or in WSL.
    pub containers: Option<Containers>,
    /// The shared Rust and Node versions (installer/toolchains.rs).
    pub toolchains: PathBuf,
    /// The connectors whose program is installed here
    /// (`rovik_core::connectors::installed`).
    pub connectors: Vec<ConnectorKind>,
    pub git: Option<String>,
    /// A folder's file system: its free and total bytes.
    pub disk: fn(&Path) -> Option<(u64, u64)>,
}

/// What runs a user's project services on a team server (D21,
/// installer/containers.rs), as doctor finds it.
pub struct Containers {
    /// What `podman --version` says, where Podman is installed.
    pub podman: Option<String>,
    /// Whether the pinned Docker Compose is installed.
    pub compose: bool,
    /// The subordinate uids and gids this user has (/etc/subuid, /etc/subgid).
    pub ids: (u64, u64),
}

impl Containers {
    /// This server's, for the user `user` with uid `uid`.
    fn here(user: &str, uid: u32) -> Containers {
        let podman = Command::new("podman")
            .arg("--version")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
        let ids = |file: &str| {
            std::fs::read_to_string(file)
                .map(|text| crate::installer::subids::ids_of(&text, user, uid))
                .unwrap_or(0)
        };
        Containers {
            podman,
            compose: rovik_core::pinned_compose::installed().is_some(),
            ids: (ids("/etc/subuid"), ids("/etc/subgid")),
        }
    }
}

impl Server {
    fn this(options: Options) -> Result<Server, String> {
        let home = Home::locate(options.data)?;
        let fuse = rovik_core::vfs::driver::status();
        let installed = Path::new(ADMIN_FILE).is_file();
        let private_network = network::wanted(installed).then(|| network::check(&home));
        let user = home::passwd().map(|p| p.name).unwrap_or_default();
        let containers =
            (installed && !network::in_wsl()).then(|| Containers::here(&user, home::uid()));
        Ok(Server {
            socket: options.socket.unwrap_or_else(|| home.socket()),
            home,
            admin_file: PathBuf::from(ADMIN_FILE),
            proc: PathBuf::from("/proc"),
            sys: PathBuf::from("/proc/sys"),
            cgroups: PathBuf::from("/sys/fs/cgroup"),
            uid: home::uid(),
            user,
            virtual_files: match fuse.detail {
                None => Ok(
                    format!("{} {}", fuse.driver, fuse.version.unwrap_or_default())
                        .trim()
                        .to_string(),
                ),
                Some(why) => Err(why),
            },
            chrome: rovik_core::browser_tools::chrome_path(),
            bwrap: crate::installer::find_on_path("bwrap"),
            landlock: rovik_core::overlay::confine::landlock_works(),
            private_network,
            containers,
            toolchains: PathBuf::from(crate::installer::TOOLCHAINS),
            connectors: rovik_core::connectors::installed(),
            git: git_version(),
            disk: free_space,
        })
    }
}

/// What doctor finds on `server`.
pub fn gather(server: &Server) -> Doctor {
    let mut findings = Findings::default();
    let home = &server.home;

    let folders = home.problems();
    if folders.is_empty() {
        let names: Vec<String> = home
            .folders()
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        findings.ok(format!(
            "{}: this user's alone (made on the first start if missing)",
            names.join(" and ")
        ));
    }
    for problem in folders {
        findings.problem(problem);
    }

    let config = Config::read(&home.config_file());
    let user_slots = config.as_ref().ok().and_then(|c| c.max_slots);
    match config.and_then(|c| c.level().and(Tokens::new(&c.tokens))) {
        Ok(tokens) if tokens.len() == 0 => {
            findings.warning("config.toml names no computer: none can connect yet".into())
        }
        Ok(tokens) => findings.ok(format!("config.toml names {} computer(s)", tokens.len())),
        Err(e) => findings.problem(e),
    }

    if answers(&server.socket) {
        findings.ok(format!("a daemon answers at {}", server.socket.display()));
    } else {
        findings.warning(format!("no daemon answers at {}", server.socket.display()));
    }

    match &server.virtual_files {
        Ok(driver) => findings.ok(format!("virtual files: {driver}")),
        Err(why) => findings.warning(format!(
            "virtual files: {why} CLM chats fall back to trimming."
        )),
    }
    match &server.chrome {
        Some(chrome) => findings.ok(format!("Chrome: {}", chrome.display())),
        None => findings.warning("no Chrome or Chromium: the browser tools won't work".into()),
    }
    if host::user_namespaces(&server.sys) {
        findings.ok("unprivileged user namespaces are allowed".into());
    } else {
        findings.warning(
            "unprivileged user namespaces are off: Chrome runs without its sandbox".into(),
        );
    }
    match &server.bwrap {
        Some(bwrap) => findings.ok(format!(
            "bubblewrap: {} sandboxes the agent's commands",
            bwrap.display()
        )),
        None if server.landlock => findings.warning(format!(
            "no bubblewrap: the agent's commands run under the kernel's Landlock, a lighter \
             sandbox (it doesn't hide the other processes), until it's installed; sudo {} admin \
             bubblewrap installs it",
            crate::installer::INSTALLED
        )),
        None => findings.warning(format!(
            "no bubblewrap: the agent's commands are refused until it's installed; sudo {} \
             admin bubblewrap installs it",
            crate::installer::INSTALLED
        )),
    }
    match &server.private_network {
        None => {}
        Some(Ok(())) => findings.ok(
            "a private network for this user's commands: their servers' ports are theirs alone"
                .into(),
        ),
        Some(Err(why)) => findings.warning(format!(
            "no private network: the agent's commands share the server's ports with other \
             users' ({why}); sudo {} admin pasta installs pasta",
            crate::installer::INSTALLED
        )),
    }
    if let Some(containers) = &server.containers {
        findings.containers(containers, &server.user);
    }
    if server.git.is_none() {
        findings.warning("no git: the projects' repositories aren't garbage-collected".into());
    }

    let (install, admin) = match AdminFile::read(&server.admin_file) {
        Ok(Some(file)) => {
            let limits = file.limits();
            let install = InstallLimits {
                admins: file.admins.clone(),
                limits: limits.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            };
            (Some(install), file)
        }
        Ok(None) => (None, AdminFile::default()),
        Err(e) => {
            findings.problem(e);
            (None, AdminFile::default())
        }
    };
    findings.toolchains(&admin, install.is_some(), &server.toolchains);
    let connectors: Vec<&str> = server.connectors.iter().map(|kind| kind.title()).collect();
    findings.ok(format!(
        "connectors installed: {}",
        match connectors.is_empty() {
            true => "none".to_string(),
            false => connectors.join(", "),
        }
    ));
    let other_daemons =
        host::other_daemons(&server.cgroups, &server.proc, server.uid, &server.user);
    let slots = admin.slots(user_slots, other_daemons);
    let disks = home
        .folders()
        .into_iter()
        .filter_map(|folder| {
            let (free, total) = (server.disk)(folder)?;
            Some(Disk {
                folder: folder.display().to_string(),
                free,
                total,
            })
        })
        .collect();

    Doctor {
        version: VERSION.into(),
        protocol: PROTOCOL,
        min_protocol: MIN_PROTOCOL,
        git: server.git.clone(),
        findings: findings.0,
        disks,
        install,
        slots: slots.manual,
        slot_cap: slots.cap,
        other_daemons,
        storage: Storage {
            projects: size_of(&home.root.join("projects")),
            work: size_of(&home.work()),
            data: size_of(&home.data()),
            tmp: size_of(&home.tmp()),
        },
        logs: size_of(&home.logs()),
    }
}

/// The report as `doctor` prints it.
pub fn render(report: &Doctor) -> String {
    let mut text = format!(
        "rovik-agentd {} (protocol {}, serves apps from protocol {})\n",
        report.version, report.protocol, report.min_protocol
    );
    let mut line = |label: &str, what: String| text.push_str(&format!("  {label:<8} {what}\n"));
    for finding in &report.findings {
        let label = match finding.level {
            Level::Ok => "ok",
            Level::Warning => "warning",
            Level::Problem => "problem",
        };
        line(label, finding.text.clone());
    }
    if let Some(git) = &report.git {
        line("git", git.clone());
    }
    let s = &report.storage;
    line(
        "storage",
        format!(
            "projects {}, working copies {}, chats and attachments {}, temporary files {}",
            size(s.projects),
            size(s.work),
            size(s.data),
            size(s.tmp)
        ),
    );
    line(
        "logs",
        format!("{}, rotated at 5 MB with 3 kept", size(report.logs)),
    );
    for disk in &report.disks {
        line(
            "disk",
            format!(
                "{} free of {} where {} is",
                size(disk.free),
                size(disk.total),
                disk.folder
            ),
        );
    }
    line("slots", slots(report));
    match &report.install {
        Some(install) => {
            line(
                "install",
                format!(
                    "for everyone, administrators: {}",
                    install.admins.join(", ")
                ),
            );
            let set: Vec<String> = install
                .limits
                .iter()
                .filter(|(_, value)| value != "none")
                .map(|(name, value)| format!("{name} {value}"))
                .collect();
            line(
                "limits",
                match set.is_empty() {
                    true => "none set".into(),
                    false => set.join(", "),
                },
            );
        }
        None => line(
            "install",
            "none for everyone: this daemon's own limits apply".into(),
        ),
    }
    text
}

/// The heavy-work slots, in words.
fn slots(report: &Doctor) -> String {
    let others = match report.other_daemons {
        1 => "1 other user's daemon runs".to_string(),
        n => format!("{n} other users' daemons run"),
    };
    let at_once = match report.slots {
        Some(1) if report.other_daemons > 0 => {
            format!("1 heavy command at a time, while {others} here")
        }
        Some(n) => format!("{n} heavy command(s) at a time, as config.toml says"),
        None => "1 heavy command at a time, or 2 on a big machine with room".into(),
    };
    match report.slot_cap {
        Some(cap) => format!("{at_once}; {cap} at most, the administrators say"),
        None => at_once,
    }
}

/// Bytes for people: B, then KB to TB, a decimal under 10.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    match value < 10.0 {
        true => format!("{value:.1} {}", UNITS[unit]),
        false => format!("{value:.0} {}", UNITS[unit]),
    }
}

#[derive(Default)]
struct Findings(Vec<Finding>);

impl Findings {
    fn ok(&mut self, text: String) {
        self.push(Level::Ok, text);
    }

    fn warning(&mut self, text: String) {
        self.push(Level::Warning, text);
    }

    fn problem(&mut self, text: String) {
        self.push(Level::Problem, text);
    }

    fn push(&mut self, level: Level, text: String) {
        self.0.push(Finding { level, text });
    }

    /// What runs this user's project services, `user`'s: Podman new enough,
    /// the pinned Docker Compose, and subordinate ids for their containers.
    /// A warning names everything missing and how it's fixed.
    fn containers(&mut self, containers: &Containers, user: &str) {
        use crate::installer::containers::{podman_version, MIN_PODMAN};
        use crate::installer::subids::COUNT;
        use rovik_core::pinned_compose::{NAME, VERSION};
        let installer = crate::installer::INSTALLED;
        let version = containers.podman.as_deref().and_then(podman_version);
        let mut missing = Vec::new();
        match version {
            None => missing.push(format!(
                "Podman isn't installed (sudo {installer} admin containers installs it)"
            )),
            Some((major, minor)) if (major, minor) < MIN_PODMAN => missing.push(format!(
                "Podman {major}.{minor} is older than the {}.{} Rovik needs (a newer release \
                 of the distribution has it)",
                MIN_PODMAN.0, MIN_PODMAN.1
            )),
            Some(_) => {}
        }
        if !containers.compose {
            missing.push(format!(
                "{NAME} {VERSION} isn't installed (sudo {installer} admin containers installs it)"
            ));
        }
        let (uids, gids) = containers.ids;
        if uids < COUNT || gids < COUNT {
            missing.push(format!(
                "{user} has {uids} subordinate uids and {gids} gids, not the {COUNT} of each \
                 their containers' users map to (sudo {installer} user start adds them)"
            ));
        }
        match (missing.is_empty(), version) {
            (true, Some((major, minor))) => self.ok(format!(
                "containers: Podman {major}.{minor} and {NAME} {VERSION} run this user's \
                 project services"
            )),
            _ => self.warning(format!(
                "the projects' services (their compose files) don't run for this user here: {}",
                missing.join("; ")
            )),
        }
    }

    /// The shared Rust and Node versions every user's chats run, and
    /// whether a chat may install one and the daemons share the caches
    /// (installer/toolchains.rs). `installed`: Rovik is installed for
    /// everyone here, so the folders should be there.
    fn toolchains(&mut self, admin: &AdminFile, installed: bool, root: &Path) {
        let (rust, node) = crate::installer::toolchains::installed_here(root);
        let mut found = Vec::new();
        if !rust.is_empty() {
            found.push(format!("Rust {}", rust.join(", ")));
        }
        if !node.is_empty() {
            found.push(format!("Node {}", node.join(", ")));
        }
        let found = match found.is_empty() {
            true => "none installed yet".to_string(),
            false => format!("installed: {}", found.join(", ")),
        };
        let switches = format!(
            "chats {} install a version, the package caches are {}",
            match admin.toolchains.user_install {
                true => "may",
                false => "may not",
            },
            match admin.toolchains.shared_caches {
                true => "shared",
                false => "each user's own",
            },
        );
        let text = format!(
            "shared toolchains in {}: {found}; {switches}",
            root.display()
        );
        match (installed, root.is_dir()) {
            // Installed for everyone before they existed: they wait for
            // an administrator, since the upgrade runs in the old build.
            (true, false) => self.warning(format!(
                "{text}. To set them up: sudo {} admin toolchains setup",
                crate::installer::INSTALLED
            )),
            _ => self.ok(text),
        }
    }
}

/// Whether a daemon at `socket` answers `/health`.
fn answers(socket: &Path) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = "GET /health HTTP/1.1\r\nHost: rovik-agentd\r\nConnection: close\r\n\r\n";
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer.starts_with("HTTP/1.1 200")
}

/// git's version, where it's installed.
fn git_version() -> Option<String> {
    let out = Command::new("git").arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.trim().strip_prefix("git version ")?;
    out.status.success().then(|| version.to_string())
}

/// The free and total bytes of the file system `folder` is on, or would
/// be on: a folder that isn't made yet goes on its parent's.
fn free_space(folder: &Path) -> Option<(u64, u64)> {
    let existing = folder.ancestors().find(|p| p.exists())?;
    let path = CString::new(existing.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it's given, from a valid C string.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some((stats.f_bavail as u64 * block, stats.f_blocks as u64 * block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// What `doctor` says on a fixture: an account whose folders are its
    /// own, a config.toml naming two computers, no daemon answering, FUSE
    /// but no Chrome, user namespaces allowed, Claude Code, git, a team
    /// server whose administrators set limits, another user's daemon
    /// running, and some files in Rovik's folders.
    #[test]
    fn the_report_on_a_fixture() {
        let dir = rovik_core::testing::temp_dir();
        let home = Home {
            root: dir.0.join(".rovik"),
            share: dir.0.join(".local/share/rovik"),
        };
        for folder in home.folders() {
            fs::create_dir_all(folder).unwrap();
            fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let file = |path: PathBuf, bytes: usize| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![b'x'; bytes]).unwrap();
        };
        file(home.root.join("projects/p.git/objects/pack"), 3 << 20);
        file(home.work().join("chat-1/src/main.rs"), 1 << 20);
        file(home.data().join("chats.db"), 20 << 10);
        file(home.logs().join("agentd.log"), 2 << 10);
        fs::write(
            home.config_file(),
            format!(
                "[tokens]\nlaptop = \"{}\"\ndesk = \"{}\"\n",
                "a".repeat(64),
                "b".repeat(64)
            ),
        )
        .unwrap();
        // The shared Rust and Node versions, with their `.used` files.
        let toolchains = dir.0.join("toolchains");
        for path in [
            toolchains.join(format!(
                "rust/rustup/toolchains/1.82.0-{}-unknown-linux-gnu",
                std::env::consts::ARCH
            )),
            toolchains.join("node/20.11.0"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        for path in [
            toolchains.join("rust/1.82.0.used"),
            toolchains.join("node/20.11.0.used"),
        ] {
            fs::write(path, "").unwrap();
        }
        let admin_file = dir.0.join("agentd.toml");
        let mut admin = AdminFile::sized_for("alice", 8 << 30);
        admin.admins.push("bob".into());
        admin.set("max_slots", "2").unwrap();
        fs::write(&admin_file, admin.to_toml()).unwrap();
        let proc = dir.0.join("proc");
        let theirs = proc.join("4242");
        fs::create_dir_all(&theirs).unwrap();
        fs::write(theirs.join("comm"), "rovik-agentd\n").unwrap();
        fs::write(
            theirs.join("status"),
            "Name:\trovik-agentd\nUid:\t1001\t1001\t1001\t1001\n",
        )
        .unwrap();
        let sys = dir.0.join("sys");
        fs::create_dir_all(sys.join("user")).unwrap();
        fs::write(sys.join("user/max_user_namespaces"), "63000\n").unwrap();
        let mut server = Server {
            socket: home.socket(),
            home,
            admin_file,
            proc,
            sys,
            cgroups: dir.0.join("cgroup"),
            uid: 1000,
            user: "ann".into(),
            virtual_files: Ok("fuse3 3.14.0".into()),
            chrome: None,
            bwrap: Some("/usr/bin/bwrap".into()),
            landlock: true,
            private_network: Some(Ok(())),
            containers: Some(Containers {
                podman: Some("podman version 4.9.3".into()),
                compose: true,
                ids: (65536, 65536),
            }),
            toolchains: dir.0.join("toolchains"),
            connectors: vec![ConnectorKind::ClaudeCode],
            git: Some("2.43.0".into()),
            disk: |_| Some((12 << 30, 40 << 30)),
        };

        let report = gather(&server);
        let shown = render(&report).replace(&dir.0.display().to_string(), "/home/ann");
        assert_eq!(
            shown,
            format!(
                "\
rovik-agentd {VERSION} (protocol {PROTOCOL}, serves apps from protocol {MIN_PROTOCOL})
  ok       /home/ann/.rovik and /home/ann/.local/share/rovik: this user's alone (made on the first start if missing)
  ok       config.toml names 2 computer(s)
  warning  no daemon answers at /home/ann/.rovik/agentd.sock
  ok       virtual files: fuse3 3.14.0
  warning  no Chrome or Chromium: the browser tools won't work
  ok       unprivileged user namespaces are allowed
  ok       bubblewrap: /usr/bin/bwrap sandboxes the agent's commands
  ok       a private network for this user's commands: their servers' ports are theirs alone
  ok       containers: Podman 4.9 and Docker Compose 5.6.1 run this user's project services
  ok       shared toolchains in /home/ann/toolchains: installed: Rust 1.82.0, Node 20.11.0; chats may install a version, the package caches are shared
  ok       connectors installed: Claude Code
  git      2.43.0
  storage  projects 3.0 MB, working copies 1.0 MB, chats and attachments 20 KB, temporary files 0 B
  logs     2.0 KB, rotated at 5 MB with 3 kept
  disk     12 GB free of 40 GB where /home/ann/.rovik is
  disk     12 GB free of 40 GB where /home/ann/.local/share/rovik is
  slots    1 heavy command at a time, while 1 other user's daemon runs here; 2 at most, the administrators say
  install  for everyone, administrators: alice, bob
  limits   max_slots 2, memory_max 4096M, tasks_max 4096, total_memory_max 6553M
"
            )
        );
        // The same as JSON, for the connection test.
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["slots"], 1);
        assert_eq!(json["slotCap"], 2);
        assert_eq!(json["otherDaemons"], 1);
        assert_eq!(json["storage"]["projects"], 3 << 20);
        assert_eq!(json["findings"][2]["level"], "warning");
        let back: Doctor = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);

        // Where no private network can be made, the commands share the
        // server's ports: a warning that says why, and how to fix it.
        server.private_network = Some(Err("pasta isn't installed".into()));
        let shown = render(&gather(&server));
        assert!(
            shown.contains(
                "warning  no private network: the agent's commands share the server's ports \
                 with other users' (pasta isn't installed); sudo /opt/rovik/bin/rovik-agentd \
                 admin pasta installs pasta"
            ),
            "{shown}"
        );
        // And where none is wanted (WSL, a daemon in development), nothing.
        server.private_network = None;
        assert!(!render(&gather(&server)).contains("private network"));

        // Without Podman, the pinned Compose or subordinate ids, a user's
        // project services don't run: one warning names all of it.
        server.containers = Some(Containers {
            podman: None,
            compose: false,
            ids: (0, 65536),
        });
        let shown = render(&gather(&server));
        assert!(
            shown.contains(
                "warning  the projects' services (their compose files) don't run for this user \
                 here: Podman isn't installed (sudo /opt/rovik/bin/rovik-agentd admin containers \
                 installs it); Docker Compose 5.6.1 isn't installed (sudo \
                 /opt/rovik/bin/rovik-agentd admin containers installs it); ann has 0 \
                 subordinate uids and 65536 gids, not the 65536 of each their containers' users \
                 map to (sudo /opt/rovik/bin/rovik-agentd user start adds them)\n"
            ),
            "{shown}"
        );
        server.containers = Some(Containers {
            podman: Some("podman version 3.4.4".into()),
            compose: true,
            ids: (65536, 65536),
        });
        let shown = render(&gather(&server));
        assert!(
            shown.contains("Podman 3.4 is older than the 4.0 Rovik needs"),
            "{shown}"
        );
        // And where Rovik isn't installed for everyone, nothing.
        server.containers = None;
        assert!(!render(&gather(&server)).contains("compose"));

        // A server without any connector's program says so.
        server.connectors = Vec::new();
        let shown = render(&gather(&server));
        assert!(
            shown.contains("ok       connectors installed: none\n"),
            "{shown}"
        );
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(20 << 10), "20 KB");
        assert_eq!(size(12 << 30), "12 GB");
        assert_eq!(size(3 << 40), "3.0 TB");
    }
}

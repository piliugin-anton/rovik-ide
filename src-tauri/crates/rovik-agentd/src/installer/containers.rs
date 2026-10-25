//! The container engine on a cloud host (D21): what runs each user's
//! project services, the databases, caches and queues a project's compose
//! file names, as that user.
//!
//! - **Podman**, from the distro's own packages, with what runs it rootless:
//!   `newuidmap` and `newgidmap` (subordinate ids, subids.rs), and the
//!   network a project's services share, through which they find each
//!   other by name: netavark, `aardvark-dns`, the firewall netavark drives
//!   (iptables for Podman 4's, nftables for 5's) and, for Podman 4,
//!   slirp4netns, which carries that network out. Podman's packages only
//!   recommend some of these on Debian and Ubuntu. Each user's containers
//!   run as that user, under their daemon: there's no daemon of root's, and
//!   no socket that would give anyone root, as Docker's does.
//! - **Docker Compose**, the release `rovik_core::pinned_compose` pins, in
//!   `/opt/rovik/compose/<version>`, checked against its SHA-256 before
//!   anything runs it: the daemons read compose files with it and start
//!   their services through each user's Podman.
//! - **Subordinate ids** for each user with a daemon here (subids.rs).
//! - **skopeo**, from the distro's packages, with which root fills the
//!   server's image cache (images.rs).
//!
//! `install` puts them in place, and so does every upgrade: an older build's
//! `admin upgrade` runs the new build's `admin bubblewrap`, which runs this
//! too. `admin containers` tries again. None of it stops an install: where
//! it's missing, a project's services don't run on the server, and the
//! agent is told so when a command finds them missing. A WSL distribution,
//! one person's computer, has its own way to run containers (Docker
//! Desktop's), and gets none of this.

use super::packages::PackageManager;
use super::{daemon_users, subids, System, INSTALLED};
use crate::home::{self, Passwd};
use rovik_core::pinned_compose::{self, Build, NAME, SERVER_ROOT, VERSION};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

/// The engine's program.
const PODMAN: &str = "podman";

/// The oldest Podman Rovik runs: 4.0 brought netavark, whose networks give
/// a project's services each other's names, and a Docker API Compose works
/// with.
pub(crate) const MIN_PODMAN: (u32, u32) = (4, 0);

/// Where Podman's packages put the helpers it runs (netavark,
/// aardvark-dns): Debian's and Ubuntu's in `/usr/lib/podman`, the others' in
/// `/usr/libexec/podman`.
const HELPER_DIRS: &[&str] = &[
    "/usr/libexec/podman",
    "/usr/lib/podman",
    "/usr/local/libexec/podman",
    "/usr/local/lib/podman",
];

/// The packages that bring Podman and what it needs rootless, for each
/// package manager, installed without their optional extras. Debian's and
/// Ubuntu's Podman only recommends the subordinate id helpers, netavark's
/// firewall and slirp4netns, and its `init: true` (catatonit); netavark is
/// built for iptables up to 1.4 (Ubuntu 24.04, Debian 12) and nftables
/// after. Elsewhere Podman requires all of it, and the names say so.
fn packages_for(manager: PackageManager) -> &'static [&'static str] {
    match manager {
        PackageManager::Apt => &[
            "podman",
            "uidmap",
            "netavark",
            "aardvark-dns",
            "passt",
            "slirp4netns",
            "iptables",
            "nftables",
            "catatonit",
            "skopeo",
        ],
        PackageManager::Dnf => &[
            "podman",
            "netavark",
            "aardvark-dns",
            "passt",
            "shadow-utils",
            "skopeo",
        ],
        PackageManager::Zypper => &[
            "podman",
            "shadow",
            "netavark",
            "aardvark-dns",
            "passt",
            "nftables",
            "skopeo",
        ],
        PackageManager::Apk => &["podman", "iptables", "nftables", "skopeo"],
        PackageManager::Pacman => &[
            "podman",
            "netavark",
            "aardvark-dns",
            "passt",
            "nftables",
            "skopeo",
        ],
    }
}

/// What Podman needs rootless that isn't there, with skopeo, which fills
/// the image cache, by name: empty when it all is. Podman 4 (`version`)
/// carries a project's network out with slirp4netns; 5 and later with
/// pasta, which Rovik installs anyway.
fn lacking(sys: &System, version: Option<(u32, u32)>) -> Vec<&'static str> {
    let helper = |name: &str| {
        HELPER_DIRS
            .iter()
            .any(|dir| Path::new(dir).join(name).is_file())
    };
    let mut lacking = Vec::new();
    for program in [PODMAN, "newuidmap", "newgidmap", "skopeo"] {
        if !sys.has(program) {
            lacking.push(program);
        }
    }
    for name in ["netavark", "aardvark-dns"] {
        if !sys.live() || !helper(name) {
            lacking.push(name);
        }
    }
    if !sys.has("iptables") && !sys.has("nft") {
        lacking.push("a firewall for netavark (iptables or nftables)");
    }
    if version.is_some_and(|v| v < (5, 0)) && !sys.has("slirp4netns") {
        lacking.push("slirp4netns");
    }
    lacking
}

/// `admin containers`: Podman and what it needs, the pinned Docker Compose,
/// and subordinate ids for each user with a daemon here, each put in place
/// where it's missing. Every part is tried; the failures are said together.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    if sys.live() && crate::network::in_wsl() {
        println!(
            "This is WSL, one person's computer: its projects' services run with its own \
             container engine, such as Docker Desktop's."
        );
        return Ok(());
    }
    let mut failures = Vec::new();
    if let Err(why) = engine(sys) {
        failures.push(why);
    }
    if let Err(why) = compose(sys) {
        failures.push(why);
    }
    for user in daemon_users(sys) {
        let Some(account) = home::passwd_named(&user) else {
            continue;
        };
        if let Err(why) = subids::ensure(sys, &account) {
            failures.push(format!("{user}'s subordinate ids: {why}"));
        }
    }
    match failures.is_empty() {
        true => Ok(()),
        false => Err(failures.join("\n")),
    }
}

/// `ensure`, for `install` and `admin bubblewrap`: what couldn't be put in
/// place is said, and stops nothing else.
pub(super) fn ensure_or_say(sys: &System) {
    if let Err(why) = ensure(sys) {
        println!("{}", not_installed(&why));
    }
}

/// `user`'s subordinate ids, for `install` and `user start`: a user
/// without them is told why their containers won't run, and their daemon
/// starts all the same.
pub(super) fn ids_or_say(sys: &System, user: &Passwd) {
    if let Err(why) = subids::ensure(sys, user) {
        println!(
            "{}'s subordinate ids couldn't be added: {why}\nTheir projects' services won't \
             run until they are. An administrator can add them: sudo {INSTALLED} admin \
             containers",
            user.name
        );
    }
}

/// What's said when the containers' parts couldn't be put in place.
pub(super) fn not_installed(why: &str) -> String {
    format!(
        "The container engine isn't set up: {why}\nThe projects' services (their compose \
         files' databases and caches) won't run on this server until it is. To try again: \
         sudo {INSTALLED} admin containers"
    )
}

/// Podman and the programs it needs rootless, installed when any is
/// missing, and then checked to be new enough.
fn engine(sys: &System) -> Result<(), String> {
    let installed = match sys.has(PODMAN) {
        true => sys.run(PODMAN, &["--version"]).ok(),
        false => None,
    };
    let lacking = lacking(sys, installed.as_deref().and_then(podman_version));
    if lacking.is_empty() {
        println!("Podman is installed.");
    } else {
        let manager = PackageManager::find(sys).ok_or_else(|| {
            format!(
                "there's no {} here to install Podman with.",
                PackageManager::names()
            )
        })?;
        manager.install(sys, packages_for(manager))?;
    }
    if !sys.live() {
        return Ok(());
    }
    let said = sys.run(PODMAN, &["--version"])?;
    match podman_version(&said) {
        Some(version) if version >= MIN_PODMAN => {
            println!(
                "Podman {}.{} runs each user's project services as that user.",
                version.0, version.1
            );
            Ok(())
        }
        Some((major, minor)) => Err(format!(
            "this server's Podman is {major}.{minor}, and Rovik needs {}.{} or later: a newer \
             release of the distribution has it",
            MIN_PODMAN.0, MIN_PODMAN.1
        )),
        None => Err(format!(
            "`podman --version` said {:?}, which names no version",
            said.trim()
        )),
    }
}

/// The major and minor version `podman --version` says (`podman version
/// 4.9.3`), if it names one.
pub(crate) fn podman_version(said: &str) -> Option<(u32, u32)> {
    let version = said.split_whitespace().find(|word| {
        word.chars().next().is_some_and(|c| c.is_ascii_digit()) && word.contains('.')
    })?;
    let mut numbers = version
        .split(['.', '-', '+'])
        .map(|n| n.parse::<u32>().ok());
    Some((numbers.next()??, numbers.next()??))
}

/// The pinned Docker Compose in `SERVER_ROOT/<version>`, downloaded and
/// checked when it isn't there, and the other versions gone. The file is
/// root's alone until it has passed, and then everyone's to run.
fn compose(sys: &System) -> Result<(), String> {
    let cpu = std::env::consts::ARCH;
    let build = Build::for_cpu(cpu).ok_or_else(|| format!("{NAME} has no build for {cpu}."))?;
    let root = sys.path(SERVER_ROOT);
    let program = pinned_compose::program_in(&root);
    if program.is_file() {
        println!("{NAME} {VERSION} is installed.");
        return prune(&root);
    }
    if !sys.live() {
        println!(
            "would download {} (SHA-256 {}) to {}",
            build.url(),
            build.sha256,
            pinned_compose::program_in(Path::new(SERVER_ROOT)).display()
        );
        return Ok(());
    }
    let folder = |dir: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(dir)
            .and_then(|()| std::fs::set_permissions(dir, PermissionsExt::from_mode(0o755)))
            .map_err(|e| format!("{}: {e}", dir.display()))
    };
    folder(&root)?;
    let download = rovik_core::pinned_browser::Leftover(
        root.join(format!(".download-{}", std::process::id())),
    );
    super::toolchains::download(&build.url(), &download.0, build.sha256)?;
    std::fs::set_permissions(&download.0, PermissionsExt::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", download.0.display()))?;
    let dir = root.join(VERSION);
    folder(&dir)?;
    std::fs::rename(&download.0, &program).map_err(|e| format!("{}: {e}", program.display()))?;
    prune(&root)?;
    println!(
        "{NAME} {VERSION} is installed for the projects' services: {}",
        pinned_compose::program_in(Path::new(SERVER_ROOT)).display()
    );
    Ok(())
}

/// The other versions in `root`, and what an interrupted download left,
/// gone: only the pinned one runs.
fn prune(root: &Path) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if entry.file_name() == VERSION {
            continue;
        }
        let path = entry.path();
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        removed.map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podmans_version_is_read_from_what_it_says() {
        assert_eq!(podman_version("podman version 4.9.3\n"), Some((4, 9)));
        assert_eq!(podman_version("podman version 5.4.0-dev"), Some((5, 4)));
        assert_eq!(podman_version("podman version 3.4.4"), Some((3, 4)));
        assert_eq!(podman_version("podman: command not found"), None);
        assert!(podman_version("podman version 3.4.4").unwrap() < MIN_PODMAN);
        assert!(podman_version("podman version 4.0.0").unwrap() >= MIN_PODMAN);
    }

    #[test]
    fn every_package_manager_installs_podman_and_its_rootless_helpers() {
        for manager in [
            PackageManager::Apt,
            PackageManager::Dnf,
            PackageManager::Zypper,
            PackageManager::Apk,
            PackageManager::Pacman,
        ] {
            let packages = packages_for(manager);
            assert_eq!(packages.first(), Some(&"podman"), "{manager:?}");
            assert!(
                packages.contains(&"skopeo"),
                "{manager:?}: the image cache's"
            );
            // Alpine's podman depends on netavark, aardvark-dns and passt.
            if manager != PackageManager::Apk {
                assert!(packages.contains(&"aardvark-dns"), "{manager:?}");
            }
        }
        // Debian's and Ubuntu's Podman only recommends newuidmap's package,
        // netavark's firewall and Podman 4's slirp4netns: without them a
        // project's network can't start.
        for wanted in ["uidmap", "netavark", "slirp4netns", "iptables", "nftables"] {
            assert!(
                packages_for(PackageManager::Apt).contains(&wanted),
                "{wanted}"
            );
        }
    }

    /// Under another root nothing is installed: everything is lacking, and
    /// slirp4netns only for a Podman before 5.
    #[test]
    fn what_podman_lacks_is_named() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        let all = lacking(&sys, Some((4, 9)));
        assert_eq!(
            all,
            [
                "podman",
                "newuidmap",
                "newgidmap",
                "skopeo",
                "netavark",
                "aardvark-dns",
                "a firewall for netavark (iptables or nftables)",
                "slirp4netns"
            ]
        );
        assert!(!lacking(&sys, Some((5, 4))).contains(&"slirp4netns"));
        assert!(!lacking(&sys, None).contains(&"slirp4netns"));
    }

    /// A dry run (another root) says what it would install and download,
    /// and changes nothing.
    #[test]
    fn a_dry_run_says_what_it_would_install_and_download() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        assert_eq!(ensure(&sys), Ok(()));
        assert!(!dir.0.join("opt").exists());
    }

    #[test]
    fn only_the_pinned_compose_is_kept() {
        let dir = rovik_core::testing::temp_dir();
        let root = dir.0.join("compose");
        for old in ["2.29.1", VERSION] {
            std::fs::create_dir_all(root.join(old)).unwrap();
            std::fs::write(root.join(old).join("docker-compose"), "x").unwrap();
        }
        std::fs::write(root.join(".download-42"), "half").unwrap();
        prune(&root).unwrap();
        let left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [VERSION]);
    }
}

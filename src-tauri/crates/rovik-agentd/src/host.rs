//! What the server is and has (`HostInfo`), for the hosts page, deploy and
//! upgrade. Each check is best effort: an answer that can't be had reads as
//! "no".

use crate::cli::VERSION;
use crate::daemon::{Daemon, MIN_PROTOCOL};
use crate::home;
use crate::limits::{size_of, AdminFile, ADMIN_FILE};
use rovik_protocol::host::{HostInfo, Install, Storage};
use rovik_protocol::PROTOCOL;
use std::path::Path;

pub async fn info(daemon: &Daemon) -> HostInfo {
    let mounted = daemon.core.vfs.mountpoint().is_some();
    let problem = (!mounted).then(|| {
        daemon
            .virtual_files_problem
            .clone()
            .unwrap_or_else(|| "The virtual files aren't mounted.".into())
    });
    rovik_core::rt::spawn_blocking(move || HostInfo {
        version: VERSION.into(),
        protocol: PROTOCOL,
        min_protocol: MIN_PROTOCOL,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        other_daemons: {
            let user = home::passwd().map(|p| p.name).unwrap_or_default();
            let (cgroups, proc) = (Path::new("/sys/fs/cgroup"), Path::new("/proc"));
            other_daemons(cgroups, proc, home::uid(), &user)
        },
        user: home::passwd().map(|p| p.name).unwrap_or_default(),
        virtual_files: mounted,
        virtual_files_problem: problem,
        chrome: rovik_core::browser_tools::chrome_path().map(|p| p.to_string_lossy().into_owned()),
        connectors: Some(
            rovik_core::connectors::installed()
                .into_iter()
                .map(|kind| kind.id().to_string())
                .collect(),
        ),
        user_namespaces: user_namespaces(Path::new("/proc/sys")),
        install: install(Path::new(ADMIN_FILE)),
    })
    .await
    .unwrap_or_default()
}

/// Whether an unprivileged process may make a user namespace: the kernel
/// allows them, and neither Debian's switch nor Ubuntu's AppArmor rule
/// turns them off.
pub fn user_namespaces(sys: &Path) -> bool {
    let read = |name: &str| {
        std::fs::read_to_string(sys.join(name))
            .ok()
            .map(|v| v.trim().to_string())
    };
    let max = read("user/max_user_namespaces").and_then(|v| v.parse::<u64>().ok());
    max.unwrap_or(0) > 0
        && read("kernel/unprivileged_userns_clone").as_deref() != Some("0")
        && read("kernel/apparmor_restrict_unprivileged_userns").as_deref() != Some("1")
}

/// What Rovik's folders take here (`Storage`), measured now: links not
/// followed, other file systems (the virtual files' mount) not entered.
pub async fn storage(daemon: &Daemon) -> Storage {
    let folders = (
        daemon.projects.dir().to_path_buf(),
        daemon.worktrees.clone(),
        daemon.core.paths.data_dir.clone(),
        daemon.tmp.clone(),
    );
    rovik_core::rt::spawn_blocking(move || Storage {
        projects: size_of(&folders.0),
        work: size_of(&folders.1),
        data: size_of(&folders.2),
        tmp: size_of(&folders.3),
    })
    .await
    .unwrap_or_default()
}

/// Rovik's install for the whole server (D17), when there is one.
pub fn install(config: &Path) -> Option<Install> {
    let file = AdminFile::read(config).ok()??;
    Some(Install {
        admins: file.admins,
    })
}

/// Daemons of other users (5.5). With systemd, the units of `rovik.slice`
/// under `cgroups` that hold a process, but `user`'s own: only root puts a
/// process there, and a `proc` mounted with `hidepid` hides none of them.
/// Without, processes named rovik-agentd whose user isn't `uid`, found
/// through `proc`, which any user can name a process (a few slots less, at
/// worst) and `hidepid` hides.
pub fn other_daemons(cgroups: &Path, proc: &Path, uid: u32, user: &str) -> u32 {
    match units_running(cgroups, user) {
        Some(units) => units,
        None => processes_named(proc, uid),
    }
}

/// The populated `rovik-agentd@*.service` units in `rovik.slice`, but
/// `user`'s; None where there's no such slice (no systemd, or Rovik isn't
/// installed for everyone).
fn units_running(cgroups: &Path, user: &str) -> Option<u32> {
    // cgroup v2, then v1's unified and systemd hierarchies.
    let slice = ["rovik.slice", "unified/rovik.slice", "systemd/rovik.slice"]
        .iter()
        .map(|at| cgroups.join(at))
        .find(|at| at.is_dir())?;
    let mine = crate::installer::instance(user);
    let units = std::fs::read_dir(slice).ok()?;
    let others = units.flatten().filter(|unit| {
        let name = unit.file_name().to_string_lossy().into_owned();
        name.starts_with("rovik-agentd@")
            && name.ends_with(".service")
            && name != mine
            && populated(&unit.path())
    });
    Some(others.count() as u32)
}

/// Whether a unit's cgroup holds a process: `populated 1` in v2's events,
/// else a process in it.
fn populated(cgroup: &Path) -> bool {
    if let Ok(events) = std::fs::read_to_string(cgroup.join("cgroup.events")) {
        return events.lines().any(|l| l.trim() == "populated 1");
    }
    std::fs::read_to_string(cgroup.join("cgroup.procs")).is_ok_and(|p| !p.trim().is_empty())
}

/// Processes named rovik-agentd whose user isn't `uid`, by their users.
fn processes_named(proc: &Path, uid: u32) -> u32 {
    let Ok(entries) = std::fs::read_dir(proc) else {
        return 0;
    };
    let mut users = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let field = |key: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(str::trim)
        };
        let user = field("Uid:")
            .and_then(|ids| ids.split_whitespace().next())
            .and_then(|id| id.parse::<u32>().ok());
        if field("Name:") == Some("rovik-agentd") {
            if let Some(user) = user.filter(|u| *u != uid) {
                users.insert(user);
            }
        }
    }
    users.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn user_namespaces_follow_the_kernels_switches() {
        let dir = rovik_core::testing::temp_dir();
        let sys = &dir.0;
        fs::create_dir_all(sys.join("user")).unwrap();
        fs::create_dir_all(sys.join("kernel")).unwrap();
        assert!(!user_namespaces(sys), "nothing known");
        fs::write(sys.join("user/max_user_namespaces"), "63000\n").unwrap();
        assert!(user_namespaces(sys));
        fs::write(sys.join("kernel/unprivileged_userns_clone"), "0\n").unwrap();
        assert!(!user_namespaces(sys), "Debian's switch off");
        fs::write(sys.join("kernel/unprivileged_userns_clone"), "1\n").unwrap();
        fs::write(
            sys.join("kernel/apparmor_restrict_unprivileged_userns"),
            "1\n",
        )
        .unwrap();
        assert!(!user_namespaces(sys), "Ubuntu's AppArmor rule");
    }

    #[test]
    fn the_install_names_its_administrators() {
        let dir = rovik_core::testing::temp_dir();
        let config = dir.0.join("agentd.toml");
        assert_eq!(install(&config), None);
        fs::write(&config, "admins = [\"ann\", \"bo\"]\nmax_slots = 2\n").unwrap();
        assert_eq!(
            install(&config),
            Some(Install {
                admins: vec!["ann".into(), "bo".into()],
            })
        );
    }

    #[test]
    fn other_users_daemons_are_counted_once_each() {
        let dir = rovik_core::testing::temp_dir();
        let process = |pid: &str, name: &str, uid: u32| {
            let p = dir.0.join(pid);
            fs::create_dir_all(&p).unwrap();
            fs::write(
                p.join("status"),
                format!("Name:\t{name}\nState:\tS\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
            )
            .unwrap();
        };
        process("10", "rovik-agentd", 1000);
        process("11", "rovik-agentd", 1001);
        process("12", "rovik-agentd", 1001);
        process("13", "bash", 1002);
        process("14", "rovik-agentd", 500);
        fs::create_dir_all(dir.0.join("self")).unwrap();
        // No rovik.slice: the processes, by name.
        let cgroups = dir.0.join("cgroup");
        assert_eq!(other_daemons(&cgroups, &dir.0, 500, "ann"), 2);
    }

    /// 5.5: with systemd, the slice's units are counted, which only root
    /// can put a process in: not a process anyone named rovik-agentd, nor
    /// a unit that holds none, nor the user's own.
    #[test]
    fn with_systemd_the_slices_units_are_counted() {
        let dir = rovik_core::testing::temp_dir();
        let proc = dir.0.join("proc");
        let spoof = proc.join("20");
        fs::create_dir_all(&spoof).unwrap();
        fs::write(spoof.join("status"), "Name:\trovik-agentd\nUid:\t1003\n").unwrap();
        let slice = dir.0.join("cgroup/rovik.slice");
        let unit = |name: &str, events: &str| {
            let at = slice.join(name);
            fs::create_dir_all(&at).unwrap();
            fs::write(at.join("cgroup.events"), events).unwrap();
        };
        unit("rovik-agentd@ann.service", "populated 1\nfrozen 0\n");
        unit("rovik-agentd@bo.service", "populated 1\nfrozen 0\n");
        unit("rovik-agentd@cy.service", "populated 0\nfrozen 0\n");
        let cgroups = dir.0.join("cgroup");
        assert_eq!(other_daemons(&cgroups, &proc, 500, "ann"), 1, "bo's");
        // v1: the processes the unit holds.
        let v1 = dir
            .0
            .join("v1/systemd/rovik.slice/rovik-agentd@dee.service");
        fs::create_dir_all(&v1).unwrap();
        fs::write(v1.join("cgroup.procs"), "4242\n").unwrap();
        assert_eq!(other_daemons(&dir.0.join("v1"), &proc, 500, "ann"), 1);
    }
}

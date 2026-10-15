//! What the server is and has (`HostInfo`), for the hosts page, deploy and
//! upgrade. Each check is best effort: an answer that can't be had reads as
//! "no".

use crate::cli::VERSION;
use crate::daemon::{Daemon, MIN_PROTOCOL};
use crate::home;
use rovik_protocol::host::{HostInfo, Install};
use rovik_protocol::PROTOCOL;
use serde::Deserialize;
use std::path::Path;

/// Rovik's install for the whole server (D17), when there is one.
const INSTALL_CONFIG: &str = "/etc/rovik/agentd.toml";

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
        user: home::passwd().map(|p| p.name).unwrap_or_default(),
        virtual_files: mounted,
        virtual_files_problem: problem,
        chrome: rovik_core::browser_tools::chrome_path().map(|p| p.to_string_lossy().into_owned()),
        user_namespaces: user_namespaces(Path::new("/proc/sys")),
        install: install(Path::new(INSTALL_CONFIG)),
        other_daemons: other_daemons(Path::new("/proc"), home::uid()),
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

pub fn install(config: &Path) -> Option<Install> {
    #[derive(Deserialize)]
    struct Agentd {
        #[serde(default)]
        admins: Vec<String>,
    }
    let text = std::fs::read_to_string(config).ok()?;
    let agentd: Agentd = toml::from_str(&text).ok()?;
    Some(Install {
        admins: agentd.admins,
    })
}

/// Daemons of other users: processes named rovik-agentd whose user isn't
/// `uid`, found through `proc` (5.5).
pub fn other_daemons(proc: &Path, uid: u32) -> u32 {
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
        assert_eq!(other_daemons(&dir.0, 500), 2);
    }
}

//! This user's private network on a team server (D20,
//! rovik_core::overlay::confine::network): whether one is wanted here, and
//! where it lives. The daemon makes it as it starts (serve.rs), `doctor`
//! checks that one can be made.

use crate::home::Home;
use crate::installer::{self, INSTALLED};
use std::path::PathBuf;

/// Whether this daemon's commands get a private network: on a server Rovik
/// is installed on for everyone (`installed`, agentd.toml there), several
/// people's, but not in WSL, one person's computer, where they keep its
/// network. `ROVIK_PRIVATE_NETWORK=0` turns it off, for troubleshooting, and
/// `=1` on, for a daemon in development.
pub fn wanted(installed: bool) -> bool {
    wanted_with(
        installed,
        std::env::var("ROVIK_PRIVATE_NETWORK").ok().as_deref(),
    )
}

/// `wanted`, with `ROVIK_PRIVATE_NETWORK` set to `switch`.
fn wanted_with(installed: bool, switch: Option<&str>) -> bool {
    match switch {
        Some("0") => false,
        Some("1") => true,
        _ => installed && !in_wsl(),
    }
}

/// Whether this is a WSL distribution: one person's computer, whose
/// commands keep its network.
#[cfg(target_os = "linux")]
pub fn in_wsl() -> bool {
    rovik_core::distro::in_wsl()
}

/// The daemon serves Linux only; elsewhere it builds for development, and
/// isn't in WSL.
#[cfg(not(target_os = "linux"))]
pub fn in_wsl() -> bool {
    false
}

/// The network's folder: in the daemon's share folder, which commands read
/// but don't write, outside the temporary files the cleanup takes.
pub fn dir(home: &Home) -> PathBuf {
    home.share.join("net")
}

/// The program the network's shims run as `<opener> open URL`: the
/// installed link, which an upgrade keeps pointing at the daemon's build,
/// or this binary itself in development.
pub fn opener() -> PathBuf {
    if installer::runs_installed() {
        return PathBuf::from(INSTALLED);
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from(INSTALLED))
}

/// Whether a private network can be made for this user, as the daemon makes
/// one: why not, when it can't. The network made to find out is gone again.
pub fn check(home: &Home) -> Result<(), String> {
    let dir = home.share.join("net-check");
    let made = rovik_core::overlay::confine::network::Network::start(&dir, &opener()).map(drop);
    let _ = std::fs::remove_dir_all(&dir);
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_server_rovik_is_installed_on_wants_a_private_network() {
        assert!(!wanted_with(false, None), "a daemon in development");
        // This test doesn't run in WSL (CI, a developer's Linux).
        if !in_wsl() {
            assert!(wanted_with(true, None));
        }
        assert!(!wanted_with(true, Some("0")), "turned off");
        assert!(wanted_with(false, Some("1")), "turned on");
    }
}

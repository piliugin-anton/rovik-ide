//! bubblewrap on a cloud host: the sandbox the daemon runs the agent's
//! commands in (`rovik_core::overlay::confine`), required on Linux. Each
//! command runs with the system read-only but for its own folders, the
//! credential stores (`~/.rovik`, the keys among them) empty, no other
//! process in sight and every capability dropped, so it can't undo that.
//! The distro's own package, never a download. Where AppArmor restricts
//! unprivileged user namespaces (Ubuntu 23.10 and later, servers included),
//! bwrap can't make its sandbox without a profile that lets it: the one the
//! app's packages add too (`rovik_core::distro::BWRAP_APPARMOR`) goes in
//! with it. `install` and `admin upgrade` put both in place, and `admin
//! bubblewrap` tries again. Without them, the daemon refuses the agent's
//! commands.

use super::packages::PackageManager;
use super::{System, INSTALLED};

pub(super) const PACKAGE: &str = "bubblewrap";
const PROGRAM: &str = "bwrap";

/// `admin bubblewrap`: the package installed when `bwrap` is missing, and
/// the AppArmor profile it needs where AppArmor restricts user namespaces.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    if sys.has(PROGRAM) {
        println!("{PACKAGE} is installed.");
    } else {
        let manager = PackageManager::find(sys).ok_or_else(|| {
            format!(
                "there's no {} here to install it with.",
                PackageManager::names()
            )
        })?;
        manager.install(sys, &[PACKAGE])?;
        println!("{PACKAGE} is installed: the agent's commands run sandboxed with it.");
    }
    allow_its_namespaces(sys)
}

/// The profile that lets bwrap make its user namespace where AppArmor
/// restricts them; the script checks for the restriction, and for another
/// profile that covers bwrap, itself. Under another root, says it.
fn allow_its_namespaces(sys: &System) -> Result<(), String> {
    if !sys.live() {
        println!("would add the AppArmor profile {PACKAGE} needs, where AppArmor restricts user namespaces");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    let script = rovik_core::distro::BWRAP_APPARMOR;
    // The daemon serves Linux only; elsewhere it builds for development.
    #[cfg(not(target_os = "linux"))]
    let script = "true";
    sys.run("sh", &["-c", script])
        .map(drop)
        .map_err(|why| format!("the AppArmor profile it needs couldn't be added: {why}"))
}

/// `ensure`, for `install`: a package that can't be installed is reported
/// and stops nothing else.
pub(super) fn ensure_or_say(sys: &System) {
    if let Err(why) = ensure(sys) {
        println!("{}", not_installed(&why));
    }
}

/// What's said when bubblewrap couldn't be installed.
pub(super) fn not_installed(why: &str) -> String {
    format!(
        "{PACKAGE} isn't installed: {why}\nThe agent's commands are refused on this server \
         until it is. To try again: sudo {INSTALLED} admin bubblewrap"
    )
}

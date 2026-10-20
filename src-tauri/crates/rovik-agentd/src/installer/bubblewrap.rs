//! bubblewrap on a cloud host: the sandbox the daemon runs the agent's
//! commands in (`rovik_core::overlay::confine`), required on Linux. Each
//! command runs with the system read-only but for its own folders, the
//! credential stores (`~/.rovik`, the keys among them) empty, no other
//! process in sight and every capability dropped, so it can't undo that.
//! The distro's own package, never a download: on Ubuntu 24.04 it's the
//! package's AppArmor profile that lets bwrap make a user namespace at all.
//! `install` and `admin upgrade` put it in place when `bwrap` is missing,
//! and `admin bubblewrap` tries again. Without it, the daemon refuses the
//! agent's commands.

use super::packages::PackageManager;
use super::{System, INSTALLED};

pub(super) const PACKAGE: &str = "bubblewrap";
const PROGRAM: &str = "bwrap";

/// `admin bubblewrap`: the package installed when `bwrap` is missing.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    if sys.has(PROGRAM) {
        println!("{PACKAGE} is installed.");
        return Ok(());
    }
    let manager = PackageManager::find(sys).ok_or_else(|| {
        format!(
            "there's no {} here to install it with.",
            PackageManager::names()
        )
    })?;
    manager.install(sys, &[PACKAGE])?;
    println!("{PACKAGE} is installed: the agent's commands run sandboxed with it.");
    Ok(())
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

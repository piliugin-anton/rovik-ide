//! bubblewrap on a cloud host: the strong way the daemon confines the
//! commands the agent runs (`rovik_core::overlay::confine`). Each runs in
//! a mount namespace where the credential stores (`~/.rovik`, the keys
//! among them) are empty, with its capabilities dropped so it can't undo
//! that. The distro's own package, never a download: on Ubuntu 24.04 it's
//! the package's AppArmor profile that lets bwrap make a user namespace at
//! all. `install` and `admin upgrade` put it in place when `bwrap` is
//! missing, and `admin bubblewrap` tries again. Without it, the daemon
//! makes a user namespace itself where the kernel allows one, and keeps to
//! its text checks where not.

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
    println!("{PACKAGE} is installed: the chats' commands are confined with it.");
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
        "{PACKAGE} isn't installed: {why}\nThe chats' commands are confined with the daemon's \
         own user namespace where the kernel allows one, and by text checks where not. To try \
         again: sudo {INSTALLED} admin bubblewrap"
    )
}

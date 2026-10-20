//! The server's package manager, for what Rovik installs from the distro's
//! own packages: the browser's libraries (browser.rs) and bubblewrap
//! (bubblewrap.rs). One place knows how each manager is run unattended;
//! what to install is each caller's.

use super::System;

/// A package manager a server may have, the first found on the PATH. Under
/// another root (a dry run) it's the first listed, apt-get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PackageManager {
    Apt,
    Dnf,
    Zypper,
    Apk,
    Pacman,
}

impl PackageManager {
    const ALL: [PackageManager; 5] = [
        PackageManager::Apt,
        PackageManager::Dnf,
        PackageManager::Zypper,
        PackageManager::Apk,
        PackageManager::Pacman,
    ];

    /// The one this server has, if any.
    pub(super) fn find(sys: &System) -> Option<PackageManager> {
        let programs: Vec<&str> = Self::ALL.iter().map(|m| m.program()).collect();
        let found = sys.which(&programs)?;
        Self::ALL.into_iter().find(|m| m.program() == found)
    }

    /// The program it's run as.
    pub(super) fn program(self) -> &'static str {
        match self {
            PackageManager::Apt => "apt-get",
            PackageManager::Dnf => "dnf",
            PackageManager::Zypper => "zypper",
            PackageManager::Apk => "apk",
            PackageManager::Pacman => "pacman",
        }
    }

    /// The programs, named, for a message saying none was found.
    pub(super) fn names() -> String {
        let programs: Vec<&str> = Self::ALL.iter().map(|m| m.program()).collect();
        let (last, rest) = programs.split_last().unwrap();
        format!("{} or {last}", rest.join(", "))
    }

    /// `apt-get ARGS`, unattended and waiting for the dpkg lock, after the
    /// package lists are updated: a new machine has none yet, and when
    /// they can't be updated, the ones there may still do.
    pub(super) fn apt(self, sys: &System, args: &[&str]) -> Result<String, String> {
        debug_assert_eq!(self, PackageManager::Apt);
        let run = |args: &[&str]| {
            let mut all = vec![
                "DEBIAN_FRONTEND=noninteractive",
                "apt-get",
                "-o",
                "DPkg::Lock::Timeout=300",
                "-q",
            ];
            all.extend_from_slice(args);
            sys.run("env", &all)
        };
        if let Err(why) = run(&["update"]) {
            println!("The package lists weren't updated: {why}");
        }
        run(args)
    }

    /// `packages` installed by name (dnf and zypper take capabilities
    /// too), without their optional extras.
    pub(super) fn install(self, sys: &System, packages: &[&str]) -> Result<(), String> {
        let mut args: Vec<&str> = match self {
            PackageManager::Apt => vec!["-y", "--no-install-recommends", "install"],
            PackageManager::Dnf => vec!["install", "-y", "--setopt=install_weak_deps=False"],
            PackageManager::Zypper => vec!["--non-interactive", "install", "--no-recommends"],
            PackageManager::Apk => vec!["add"],
            PackageManager::Pacman => vec!["-S", "--noconfirm", "--needed"],
        };
        args.extend_from_slice(packages);
        match self {
            PackageManager::Apt => self.apt(sys, &args).map(|_| ()),
            _ => sys.run(self.program(), &args).map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_managers_are_named_for_a_message() {
        assert_eq!(
            PackageManager::names(),
            "apt-get, dnf, zypper, apk or pacman"
        );
    }

    /// A dry run (another root) takes the first manager, apt-get.
    #[test]
    fn a_dry_run_has_apt() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        assert_eq!(PackageManager::find(&sys), Some(PackageManager::Apt));
    }
}

//! pasta on a cloud host: what carries each user's private network out
//! (D20, rovik_core::overlay::confine::network). Each user's commands run in
//! a network of their own, so their servers can't clash with another user's
//! on a port, nor be reached by another user, and pasta, from the distro's
//! `passt` package, connects that network to the internet. A server several
//! people share needs it: `install` and `admin upgrade` install it before
//! they change anything, and stop where it can't be installed. `admin
//! pasta` tries again. A WSL distribution, one person's computer, keeps its
//! network and needs none.

use super::packages::PackageManager;
use super::{System, INSTALLED};

const PACKAGE: &str = "passt";
const PROGRAM: &str = "pasta";

/// `admin pasta`: the package installed when `pasta` is missing. Why not,
/// and how to install it by hand, when it can't be.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    if sys.live() && crate::network::in_wsl() {
        println!(
            "This is WSL, one person's computer: its commands keep its network, without {PROGRAM}."
        );
        return Ok(());
    }
    if sys.has(PROGRAM) {
        println!("{PROGRAM} is installed.");
        return Ok(());
    }
    let manager = PackageManager::find(sys).ok_or_else(|| {
        needed(&format!(
            "there's no {} here to install it with",
            PackageManager::names()
        ))
    })?;
    manager
        .install(sys, &[PACKAGE])
        .map_err(|why| needed(&why))?;
    if sys.live() && !sys.has(PROGRAM) {
        return Err(needed(&format!(
            "the {PACKAGE} package went in, but there's no {PROGRAM} on the PATH"
        )));
    }
    println!("{PROGRAM} is installed: each user's commands get a network of their own with it.");
    Ok(())
}

/// What's said when pasta couldn't be installed, `why`: what Rovik needs it
/// for, and how to install it.
fn needed(why: &str) -> String {
    format!(
        "{PROGRAM} couldn't be installed: {why}\n\
         Rovik needs it on a server several people share: it gives each user's commands a \
         network of their own, so their servers can't clash on a port or be reached by \
         another user. Install the {PACKAGE} package, which has {PROGRAM}, then run: \
         sudo {INSTALLED} admin pasta"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_run_installs_the_passt_package() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        // Under another root nothing is on the PATH: what installing it
        // would run is printed, and nothing fails.
        assert_eq!(ensure(&sys), Ok(()));
    }

    #[test]
    fn why_pasta_is_needed_names_the_package_and_the_command_to_try_again() {
        let said = needed("no repository has it");
        assert!(
            said.starts_with("pasta couldn't be installed: no repository has it"),
            "{said}"
        );
        assert!(said.contains("passt package"), "{said}");
        assert!(said.contains("admin pasta"), "{said}");
    }
}

//! Codex on a cloud host, installed once for everyone: the program the
//! Codex connector runs (`rovik_core::connectors::codex`) when a chat on
//! this server uses one of its models. OpenAI publishes no package for a
//! server's package manager, so `admin codex` runs OpenAI's installer
//! (developers.openai.com/codex/cli), which checks each download against
//! its release's checksums, as root: the program goes in the system's
//! `bin`, where every user's daemon finds it on the PATH, and its releases
//! in a folder every user reads, where the installer's own default would
//! put both in root's home. That folder isn't Rovik's: `admin uninstall`
//! leaves Codex working, as it leaves Claude Code's package. `admin codex`
//! again brings the newest version.
//!
//! Each user signs in as their own: the connector's access token or API
//! key travels with the chat's model (its profile's key, kept by the
//! daemon as a provider's is, D11) and goes to the program in its
//! environment or over its input; a login done on the server with `codex
//! login --device-auth` is the user's own `~/.codex`. Nothing of one
//! user's is shared with another.

use super::{System, INSTALLED};

pub(super) const PROGRAM: &str = "codex";
/// OpenAI's installer, as its docs give it.
const INSTALLER: &str = "https://chatgpt.com/codex/install.sh";
/// Where the program goes: on every user's PATH.
const BIN_DIR: &str = "/usr/local/bin";
/// Where its releases go, the program a link into them: readable by all,
/// and outside `/opt/rovik`, which `admin uninstall` removes.
const RELEASES_HOME: &str = "/opt/codex";

/// `admin codex`: Codex installed with OpenAI's installer when it isn't
/// here, or brought up to date when it is.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    let script = format!("curl -fsSL {INSTALLER} | sh");
    let env = [
        ("CODEX_INSTALL_DIR", BIN_DIR),
        ("CODEX_HOME", RELEASES_HOME),
        ("CODEX_NON_INTERACTIVE", "1"),
    ];
    sys.run_with(&env, "sh", &["-c", &script])?;
    let version = sys.run(PROGRAM, &["--version"]).unwrap_or_default();
    let version = version.lines().next().unwrap_or("").trim();
    println!(
        "Codex is installed for everyone on this server{}: the Codex connector finds it on \
         the PATH. Each user signs in as their own (an access token or an API key in the \
         connector's settings, or `codex login --device-auth` once on this server). To \
         update it: sudo {INSTALLED} admin codex",
        if version.is_empty() {
            String::new()
        } else {
            format!(" ({version})")
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dry run (another root) shows the installer run, its program on
    /// the PATH and its releases where everyone reads them.
    #[test]
    fn a_dry_run_runs_openais_installer_for_everyone() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        ensure(&sys).unwrap();
        // Nothing of the system changed under another root.
        assert!(!dir.0.join("usr/local/bin/codex").exists());
        assert!(!dir.0.join("opt/codex").exists());
    }

    /// The program lands where the admin command's own PATH finds it (its
    /// version is read right after), and its releases outside Rovik's
    /// folder, so removing Rovik leaves no broken link on the PATH.
    #[test]
    fn codex_goes_on_the_path_and_outlives_rovik_s_own_folder() {
        assert!(super::super::SYSTEM_PATH
            .split(':')
            .any(|dir| dir == BIN_DIR));
        assert!(!RELEASES_HOME.starts_with("/opt/rovik/"));
        assert!(RELEASES_HOME.starts_with('/'));
    }
}

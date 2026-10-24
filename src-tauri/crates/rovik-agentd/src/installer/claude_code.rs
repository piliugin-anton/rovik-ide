//! Claude Code on a cloud host, installed once for everyone: the program
//! the Claude Code connector runs (`rovik_core::connectors::claude_code`)
//! when a chat on this server uses one of its models. `admin claude-code`
//! installs it from Anthropic's own package repository for the server's
//! package manager (apt, dnf or apk, as code.claude.com/docs/en/setup
//! describes), so it lands in the system's `bin`, where every user's
//! daemon finds it on the PATH; the native installer would put a copy in
//! root's home alone. Package installs don't update themselves: `admin
//! claude-code` again brings the newest package.
//!
//! Each user signs in as their own: the connector's token or key travels
//! with the chat's model (its profile's key, kept by the daemon as a
//! provider's is, D11) and goes to the program as `CLAUDE_CODE_OAUTH_TOKEN`
//! or `ANTHROPIC_API_KEY`; a login done on the server with `claude` is the
//! user's own `~/.claude`. Nothing of one user's is shared with another.

use super::packages::PackageManager;
use super::{System, INSTALLED};

pub(super) const PACKAGE: &str = "claude-code";
pub(super) const PROGRAM: &str = "claude";

/// Anthropic's signing key, and its fingerprint as the docs give it.
const KEY_URL: &str = "https://downloads.claude.ai/keys/claude-code.asc";
const APK_KEY_URL: &str = "https://downloads.claude.ai/keys/claude-code.rsa.pub";
const APT_KEY: &str = "/etc/apt/keyrings/claude-code.asc";
const APT_LIST: &str = "/etc/apt/sources.list.d/claude-code.list";
const DNF_REPO: &str = "/etc/yum.repos.d/claude-code.repo";
const APK_KEY: &str = "/etc/apk/keys/claude-code.rsa.pub";
const APK_REPOSITORIES: &str = "/etc/apk/repositories";
/// The stable channel: about a week behind, skipping releases with
/// major regressions, which is what a server wants.
const APT_REPO: &str = "https://downloads.claude.ai/claude-code/apt/stable";
const DNF_REPO_URL: &str = "https://downloads.claude.ai/claude-code/rpm/stable";
const APK_REPO: &str = "https://downloads.claude.ai/claude-code/apk/stable";

/// `admin claude-code`: Claude Code installed from Anthropic's repository
/// when `claude` isn't on the PATH, or brought up to date when it is.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    let manager = PackageManager::find(sys).ok_or_else(|| {
        format!(
            "there's no {} here to install it with. Install it by hand instead: \
             curl -fsSL https://claude.ai/install.sh | bash, as each user, or see \
             https://code.claude.com/docs/en/setup",
            PackageManager::names()
        )
    })?;
    let had = sys.has(PROGRAM);
    match manager {
        PackageManager::Apt => {
            sys.run("curl", &["-fsSL", KEY_URL, "-o", APT_KEY])?;
            sys.write(
                APT_LIST,
                format!("deb [signed-by={APT_KEY}] {APT_REPO} stable main\n").as_bytes(),
                0o644,
            )?;
            manager.install(sys, &[PACKAGE])?;
        }
        PackageManager::Dnf => {
            sys.write(
                DNF_REPO,
                format!(
                    "[claude-code]\nname=Claude Code\nbaseurl={DNF_REPO_URL}\nenabled=1\n\
                     gpgcheck=1\ngpgkey={KEY_URL}\n"
                )
                .as_bytes(),
                0o644,
            )?;
            // `install` leaves an installed package as it is: `upgrade`
            // brings the newest.
            manager.install(sys, &[PACKAGE])?;
            if had {
                sys.run("dnf", &["upgrade", "-y", PACKAGE])?;
            }
        }
        PackageManager::Apk => {
            sys.run("curl", &["-fsSL", APK_KEY_URL, "-o", APK_KEY])?;
            let repositories =
                std::fs::read_to_string(sys.path(APK_REPOSITORIES)).unwrap_or_default();
            if !repositories.lines().any(|l| l.trim() == APK_REPO) {
                sys.write(
                    APK_REPOSITORIES,
                    format!("{}{APK_REPO}\n", with_newline(&repositories)).as_bytes(),
                    0o644,
                )?;
            }
            sys.run("apk", &["update"])?;
            manager.install(sys, &[PACKAGE])?;
            if had {
                sys.run("apk", &["upgrade", PACKAGE])?;
            }
        }
        PackageManager::Zypper | PackageManager::Pacman => {
            return Err(format!(
                "Anthropic publishes no package for {}. Install Claude Code by hand instead \
                 (curl -fsSL https://claude.ai/install.sh | bash, as each user), or name the \
                 program's path in the connector's settings.",
                manager.program()
            ))
        }
    }
    let version = sys.run(PROGRAM, &["--version"]).unwrap_or_default();
    let version = version.lines().next().unwrap_or("").trim();
    println!(
        "Claude Code is installed for everyone on this server{}: the Claude Code connector \
         finds it on the PATH. Each user signs in as their own (a token from `claude \
         setup-token` in the connector's settings, or `claude` once on this server). To \
         update it: sudo {INSTALLED} admin claude-code",
        if version.is_empty() {
            String::new()
        } else {
            format!(" ({version})")
        }
    );
    Ok(())
}

fn with_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dry run (another root) shows the apt repository and the install.
    #[test]
    fn a_dry_run_adds_the_repository_and_installs_the_package() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        ensure(&sys).unwrap();
        let list =
            std::fs::read_to_string(dir.0.join("etc/apt/sources.list.d/claude-code.list")).unwrap();
        assert_eq!(
            list,
            "deb [signed-by=/etc/apt/keyrings/claude-code.asc] https://downloads.claude.ai/claude-code/apt/stable stable main\n"
        );
    }

    #[test]
    fn apk_repositories_get_the_line_once() {
        assert_eq!(with_newline(""), "");
        assert_eq!(with_newline("a\n"), "a\n");
        assert_eq!(with_newline("a"), "a\n");
    }
}

//! A WSL2 distribution on this computer as a host (Windows): Rovik's
//! daemon runs inside it, with the Linux sandbox for the agent's commands
//! (bubblewrap) and FUSE for the overlays, and is reached through
//! `wsl.exe` instead of SSH. Chats moved there run in the distribution,
//! as they do on a server: the project is synced into a worktree on its
//! own disk (fast builds), the toolchains are the distribution's, and the
//! results land back here.
//!
//! What differs from a server: commands run through `wsl.exe -d <distro>
//! --exec`, as the user (the distribution's default user unless told);
//! root inside the distribution is had with `-u root`, without a password,
//! so the install needs no sudo; there's no host key to pin and nothing to
//! log in to; and the daemon is started by the app when it isn't running
//! ([`ensure_running`]), since the distribution starts fresh after a
//! reboot or a `wsl --shutdown`. The app's connection keeps the
//! distribution running meanwhile.

use crate::host_shell::{ProcessConnector, ProcessShell};
use crate::ssh::quote;
use std::ffi::OsString;
use std::time::Duration;

/// The program, on the PATH of every Windows with WSL.
pub const PROGRAM: &str = "wsl.exe";

/// How long listing the distributions may take: a first `wsl.exe` start.
const LIST_WAIT: Duration = Duration::from_secs(30);

/// A distribution, and whom its commands run as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wsl {
    pub distro: String,
    /// None: the distribution's default user.
    pub user: Option<String>,
}

impl Wsl {
    pub fn new(distro: impl Into<String>, user: Option<String>) -> Wsl {
        Wsl {
            distro: distro.into(),
            user: user.filter(|u| !u.trim().is_empty()),
        }
    }

    /// `-d <distro> [-u <user>] --exec`: what runs the program that follows
    /// in the distribution, as `user`, without a login shell in between.
    fn exec_args(&self, user: Option<&str>) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-d".into(), self.distro.clone().into()];
        if let Some(user) = user {
            args.extend(["-u".into(), user.into()]);
        }
        args.push("--exec".into());
        args
    }

    /// Commands in the distribution, as the user.
    pub fn shell(&self) -> ProcessShell {
        ProcessShell {
            program: PROGRAM.into(),
            args: self.exec_args(self.user.as_deref()),
            name: self.display(),
        }
    }

    /// Commands in the distribution as root: what installs Rovik there.
    pub fn root_shell(&self) -> ProcessShell {
        ProcessShell {
            program: PROGRAM.into(),
            args: self.exec_args(Some("root")),
            name: format!("{} as root", self.display()),
        }
    }

    /// The daemon's connector: `attach --stdio` of Rovik at `installed`
    /// there, started first when it isn't running.
    pub fn connector(&self, installed: &str) -> ProcessConnector {
        let attach = vec![installed.to_string(), "attach".into(), "--stdio".into()];
        ProcessConnector::new(self.shell(), attach).ensuring(ensure_running(installed))
    }

    /// What a terminal here runs `wsl.exe` with: the user's login shell in
    /// the distribution.
    pub fn terminal_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-d".into(), self.distro.clone().into()];
        if let Some(user) = &self.user {
            args.extend(["-u".into(), user.into()]);
        }
        args
    }

    /// "WSL: Ubuntu", or "WSL: Ubuntu (ann)" as a named user.
    pub fn display(&self) -> String {
        match &self.user {
            Some(user) => format!("WSL: {} ({user})", self.distro),
            None => format!("WSL: {}", self.distro),
        }
    }
}

/// The script that starts the user's daemon, Rovik at `installed`, when it
/// isn't running, and waits for its socket (up to 30 s): what the app runs
/// before it connects, as a server's systemd would have done. A second
/// daemon started meanwhile finds the first's lock and stops.
pub fn ensure_running(installed: &str) -> String {
    let program = quote(installed);
    let pattern = quote(&format!("^{installed} serve"));
    format!(
        "if [ -x {program} ] && ! pgrep -u \"$(id -u)\" -f {pattern} >/dev/null 2>&1; then \
         setsid {program} serve </dev/null >/dev/null 2>&1 & fi; \
         i=0; while [ ! -S \"$HOME/.rovik/agentd.sock\" ] && [ $i -lt 60 ]; do \
         sleep 0.5; i=$((i + 1)); done; true"
    )
}

/// The script that installs, as root in the distribution, the packages the
/// daemon's sandbox (bubblewrap) and its overlays (FUSE) take, with the
/// distribution's package manager; nothing where there's none this knows.
pub fn packages_script() -> String {
    const PACKAGES: &str = "bubblewrap fuse3";
    format!(
        "if command -v apt-get >/dev/null 2>&1; then \
         export DEBIAN_FRONTEND=noninteractive; \
         apt-get install -y -q {PACKAGES} || {{ apt-get update -q && apt-get install -y -q {PACKAGES}; }}; \
         elif command -v dnf >/dev/null 2>&1; then dnf install -y {PACKAGES}; \
         elif command -v zypper >/dev/null 2>&1; then zypper --non-interactive install {PACKAGES}; \
         elif command -v apk >/dev/null 2>&1; then apk add {PACKAGES}; \
         elif command -v pacman >/dev/null 2>&1; then pacman -S --needed --noconfirm {PACKAGES}; \
         fi"
    )
}

/// The distributions installed here, as `wsl.exe -l -q` lists them.
pub async fn distros() -> Result<Vec<String>, String> {
    let mut command = rovik_core::process::command(PROGRAM);
    rovik_core::process::no_window(&mut command);
    let listed = tokio::process::Command::from(command)
        .args(["-l", "-q"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LIST_WAIT, listed)
        .await
        .map_err(|_| format!("{PROGRAM} didn't list the distributions in time."))?
        .map_err(|e| format!("{PROGRAM}: {e}. Is WSL installed?"))?;
    if !output.status.success() {
        return Err(format!(
            "{PROGRAM} couldn't list the distributions: {}",
            decode(&output.stderr).trim()
        ));
    }
    Ok(parse_distros(&output.stdout))
}

/// The distributions in what `wsl.exe -l -q` printed, one a line.
pub fn parse_distros(printed: &[u8]) -> Vec<String> {
    decode(printed)
        .lines()
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// What `wsl.exe` printed: UTF-16LE, as it writes to a pipe on Windows
/// (every ASCII character followed by a zero byte), or UTF-8.
fn decode(printed: &[u8]) -> String {
    let utf16 = printed.len() >= 2 && printed.len().is_multiple_of(2) && printed[1] == 0;
    if utf16 {
        let units: Vec<u16> = printed
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(printed).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_run_in_the_distribution_as_the_user_without_a_login_shell() {
        let wsl = Wsl::new("Ubuntu", Some("ann".into()));
        assert_eq!(wsl.shell().args, ["-d", "Ubuntu", "-u", "ann", "--exec"]);
        assert_eq!(
            wsl.root_shell().args,
            ["-d", "Ubuntu", "-u", "root", "--exec"]
        );
        assert_eq!(wsl.terminal_args(), ["-d", "Ubuntu", "-u", "ann"]);
        assert_eq!(wsl.display(), "WSL: Ubuntu (ann)");
        // The default user: no -u.
        let wsl = Wsl::new("Debian", Some("  ".into()));
        assert_eq!(wsl.shell().args, ["-d", "Debian", "--exec"]);
        assert_eq!(wsl.display(), "WSL: Debian");
        assert_eq!(wsl.shell().program.to_string_lossy(), PROGRAM);
    }

    #[test]
    fn the_daemon_is_started_once_and_its_socket_waited_for() {
        let script = ensure_running("/opt/rovik/bin/rovik-agentd");
        assert!(script.contains("pgrep -u \"$(id -u)\" -f '^/opt/rovik/bin/rovik-agentd serve'"));
        assert!(script.contains("setsid '/opt/rovik/bin/rovik-agentd' serve </dev/null"));
        assert!(script.contains("$HOME/.rovik/agentd.sock"));
    }

    #[test]
    fn the_distributions_are_read_from_utf16_or_utf8() {
        let utf16: Vec<u8> = "Ubuntu\r\nDebian\r\n\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(parse_distros(&utf16), ["Ubuntu", "Debian"]);
        assert_eq!(parse_distros(b"Ubuntu\nAlpine\n"), ["Ubuntu", "Alpine"]);
        assert!(parse_distros(b"").is_empty());
    }
}

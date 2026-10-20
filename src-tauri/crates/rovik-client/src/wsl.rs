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

/// Why WSL2 can't run on this Windows, when it can't: before build 18362
/// (Windows 10 version 1903), and on Windows 7, 8 and 8.1, there's no
/// WSL2 to install. None elsewhere, and where the version can't be read.
pub async fn unsupported_windows() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let mut command = rovik_core::process::command("cmd.exe");
    rovik_core::process::no_window(&mut command);
    let printed = tokio::process::Command::from(command)
        .args(["/C", "ver"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), printed)
        .await
        .ok()?
        .ok()?;
    too_old_for_wsl2(&decode(&output.stdout))
}

/// The oldest Windows with WSL2: 10.0.18362, Windows 10 version 1903.
const OLDEST_WITH_WSL2: (u32, u32, u32) = (10, 0, 18362);

/// What `ver` printed ("Microsoft Windows [Version 10.0.19045.3693]"),
/// judged: why WSL2 can't run on it, when it can't.
pub fn too_old_for_wsl2(ver: &str) -> Option<String> {
    let (major, minor, build) = windows_version(ver)?;
    if (major, minor, build) >= OLDEST_WITH_WSL2 {
        return None;
    }
    let name = match (major, minor) {
        (6, 1) => "Windows 7".to_string(),
        (6, 2) => "Windows 8".to_string(),
        (6, 3) => "Windows 8.1".to_string(),
        _ => format!("Windows {major}.{minor} build {build}"),
    };
    Some(format!(
        "This is {name}: WSL2 needs Windows 10 version 1903 (build 18362) or newer, or Windows 11. \
         Update Windows to use the agent's commands on this computer."
    ))
}

/// The major, minor and build numbers in what `ver` printed.
fn windows_version(ver: &str) -> Option<(u32, u32, u32)> {
    let after = ver.split("Version ").nth(1)?;
    let numbers = after
        .trim_end_matches(|c: char| c != ']')
        .trim_end_matches(']');
    let mut parts = numbers
        .trim()
        .split('.')
        .map(|n| n.trim().parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// What the `wsl.exe` errors WSL2 is known to start with mean, each with
/// what to do about it, by what the error names: its code (`0x80370102`),
/// its name (`HCS_E_HYPERV_NOT_INSTALLED`) or its text.
const EXPLAINED: &[(&[&str], &str)] = &[
    (
        &[
            "hcs_e_hyperv_not_installed",
            "0x80370102",
            "hcs_e_service_not_available",
            "0x80370114",
            "wsl_e_vm_mode_not_supported",
            "virtual machine platform",
            "virtualization",
            "hypervisor",
        ],
        "WSL2's virtual machine can't start: virtualization is turned off, or Windows' \
         \"Virtual Machine Platform\" is missing. Turn Intel VT-x or AMD-V on in the computer's \
         BIOS/UEFI settings, turn the Virtual Machine Platform on (an administrator PowerShell: \
         wsl --install --no-distribution), and restart.",
    ),
    (
        &[
            "wsl_e_wsl_optional_component_required",
            "0x8007019e",
            "optional component is not enabled",
        ],
        "WSL isn't turned on in Windows: run wsl --install from an administrator PowerShell, \
         and restart.",
    ),
    (
        &["0x800701bc", "kernel component"],
        "WSL2's kernel is out of date: run wsl --update from an administrator PowerShell.",
    ),
    (
        &["0x80070422", "lxssmanager"],
        "The WSL service (LxssManager) is disabled: enable it in Services, or run wsl --install \
         from an administrator PowerShell.",
    ),
    (
        &["hcs_e_connection_timeout", "0x80370109"],
        "WSL2's virtual machine didn't answer in time: run wsl --shutdown from a PowerShell, \
         and try again.",
    ),
    (
        &[
            "wsl_e_distro_not_found",
            "no distribution with the supplied name",
        ],
        "The distribution isn't installed any more: set WSL up again with one that is.",
    ),
    (
        &[
            "wsl_e_default_distro_not_found",
            "no installed distributions",
        ],
        "No distribution is installed: install one from an administrator PowerShell \
         (wsl --install -d Ubuntu), and come back.",
    ),
];

/// What a `wsl.exe` error means, with what to do about it, for the known
/// ways WSL2 fails to start; None for one this doesn't know.
pub fn explain(error: &str) -> Option<&'static str> {
    let error = error.to_ascii_lowercase();
    let known = |needles: &[&str]| needles.iter().any(|needle| error.contains(needle));
    EXPLAINED
        .iter()
        .find(|(needles, _)| known(needles))
        .map(|(_, meaning)| *meaning)
}

/// `error` with what it means in front, when `explain` knows.
pub fn explained(error: String) -> String {
    match explain(&error) {
        Some(meaning) => format!("{meaning} ({})", error.trim()),
        None => error,
    }
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
pub fn decode(printed: &[u8]) -> String {
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
    fn the_known_startup_failures_are_explained() {
        let hyperv = "wsl.exe couldn't list the distributions: The virtual machine could not be \
                      started because a required feature is not installed.\nError code: \
                      Wsl/Service/CreateInstance/CreateVm/HCS/HCS_E_HYPERV_NOT_INSTALLED";
        let meaning = explain(hyperv).unwrap();
        assert!(
            meaning.starts_with("WSL2's virtual machine can't start"),
            "{meaning}"
        );
        assert!(meaning.contains("BIOS/UEFI"));
        assert_eq!(explain("Error code: Wsl/0x80370102"), Some(meaning));
        assert!(explain("Error code: Wsl/0x800701bc")
            .unwrap()
            .contains("wsl --update"));
        assert!(explain("WSL_E_WSL_OPTIONAL_COMPONENT_REQUIRED")
            .unwrap()
            .contains("wsl --install"));
        assert_eq!(explain("sh: 1: rovik-agentd: not found"), None);
        // The meaning first, the error after it.
        let whole = explained(format!("{hyperv}\n"));
        assert!(whole.starts_with(meaning), "{whole}");
        assert!(whole.ends_with("HCS_E_HYPERV_NOT_INSTALLED)"), "{whole}");
        assert_eq!(explained("other".into()), "other");
    }

    #[test]
    fn windows_before_1903_cant_run_wsl2() {
        assert_eq!(
            too_old_for_wsl2("Microsoft Windows [Version 10.0.19045.3693]"),
            None
        );
        assert_eq!(
            too_old_for_wsl2("\r\nMicrosoft Windows [Version 10.0.22631.4317]\r\n"),
            None
        );
        assert_eq!(
            too_old_for_wsl2("Microsoft Windows [Version 10.0.18362.1]"),
            None
        );
        let old = too_old_for_wsl2("Microsoft Windows [Version 10.0.17763.5576]").unwrap();
        assert!(
            old.starts_with("This is Windows 10.0 build 17763: WSL2 needs"),
            "{old}"
        );
        let seven = too_old_for_wsl2("Microsoft Windows [Version 6.1.7601]").unwrap();
        assert!(seven.starts_with("This is Windows 7:"), "{seven}");
        assert!(too_old_for_wsl2("Microsoft Windows [Version 6.3.9600]")
            .unwrap()
            .starts_with("This is Windows 8.1:"));
        // Unreadable: no verdict.
        assert_eq!(too_old_for_wsl2("Linux"), None);
        assert_eq!(too_old_for_wsl2("Microsoft Windows [Version x]"), None);
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

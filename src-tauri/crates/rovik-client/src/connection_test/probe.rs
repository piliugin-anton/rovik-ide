//! The probe (H04): a POSIX shell script, run in one exec, that says what
//! the server offers Rovik, a `key=value` line each; what it found, read
//! back; and what each answer means for the hosts page, a step each.
//!
//! The script goes on stdin to `sh -s`, so the user's login shell parses
//! only that command, whatever the shell is. Busybox runs it as well as
//! bash or dash. An answer it couldn't get reads as "no". It asks nothing
//! of the network: the providers' addresses aren't tried from the server.

use super::{Check, Outcome, Step};
use rovik_protocol::PROTOCOL;

/// The script.
pub const PROBE: &str = include_str!("probe.sh");
/// The command that runs it, from stdin.
pub const COMMAND: &str = "sh -s";

/// Under this much free space in the home folder, a deploy can't go on.
const DISK_NEEDED: u64 = 1 << 30;
/// Under this much, the test warns: projects and their worktrees fill it
/// fast.
const DISK_LOW: u64 = 5 << 30;
/// Worktrees need git 2.5.
const GIT_WORKTREES: (u32, u32) = (2, 5);

/// What the probe found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    /// `uname -s` and `uname -m`.
    pub os: String,
    pub arch: String,
    pub uid: Option<u32>,
    pub user: String,
    pub home: String,
    /// Free space in the home folder, in bytes.
    pub disk_free: Option<u64>,
    /// `git --version`, when there's a git.
    pub git: Option<String>,
    /// The Chrome or Chromium the browser tools would start, and whether it
    /// started headless.
    pub chrome: Option<String>,
    pub chrome_headless: bool,
    /// Unprivileged user namespaces: Chrome's sandbox needs them.
    pub user_namespaces: bool,
    /// `bwrap`, which sandboxes the agent's commands; without it they're
    /// refused. Rovik's install adds it.
    pub bwrap: Option<String>,
    /// `/dev/fuse`, and the helper that mounts without root: the virtual
    /// files need both.
    pub fuse: bool,
    pub fusermount: Option<String>,
    /// systemd is PID 1.
    pub systemd: bool,
    pub sudo: Sudo,
    /// Rovik's install for the whole server, when there is one (D17).
    pub install: Option<Installed>,
    /// This user's daemon socket is there.
    pub socket: bool,
    /// The other users running a daemon here.
    pub daemons: Vec<String>,
}

/// What `sudo -n` said.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sudo {
    /// There's no sudo.
    #[default]
    Missing,
    NoPassword,
    /// It asks for the user's password, and may still refuse.
    Password,
    Refused,
}

/// Rovik as installed for the whole server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed {
    /// What `rovik-agentd version` says; none when it didn't run.
    pub version: Option<String>,
    pub protocol: Option<u32>,
    pub admins: Vec<String>,
}

impl Probe {
    /// Reads the script's output. Lines it doesn't know are skipped.
    pub fn parse(text: &str) -> Probe {
        let mut probe = Probe::default();
        let mut install: Option<Installed> = None;
        for line in text.lines() {
            let Some((key, value)) = line.trim_end_matches('\r').split_once('=') else {
                continue;
            };
            let value = value.trim();
            let some = || Some(value.to_string()).filter(|v| !v.is_empty());
            let yes = value == "yes";
            match key {
                "os" => probe.os = value.into(),
                "arch" => probe.arch = value.into(),
                "uid" => probe.uid = value.parse().ok(),
                "user" => probe.user = value.into(),
                "home" => probe.home = value.into(),
                "disk_kb" => probe.disk_free = value.parse::<u64>().ok().map(|kb| kb << 10),
                "git" => probe.git = some(),
                "chrome" => probe.chrome = some(),
                "chrome_headless" => probe.chrome_headless = yes,
                "userns" => probe.user_namespaces = yes,
                "bwrap" => probe.bwrap = some(),
                "fuse" => probe.fuse = yes,
                "fusermount" => probe.fusermount = some(),
                "systemd" => probe.systemd = yes,
                "sudo" => {
                    probe.sudo = match value {
                        "yes" => Sudo::NoPassword,
                        "password" => Sudo::Password,
                        "no" => Sudo::Refused,
                        _ => Sudo::Missing,
                    }
                }
                "rovik" => {
                    let installed = install.get_or_insert_with(Installed::default);
                    (installed.version, installed.protocol) = version(value);
                }
                "rovik_admins" => {
                    install.get_or_insert_with(Installed::default).admins = strings(value);
                }
                "socket" => probe.socket = yes,
                "daemon" if !value.is_empty() && !probe.daemons.iter().any(|d| d == value) => {
                    probe.daemons.push(value.into());
                }
                _ => {}
            }
        }
        probe.install = install;
        probe
    }

    /// What it found, a step each, in the order the hosts page shows them.
    pub fn findings(&self) -> Vec<Step> {
        vec![
            self.system(),
            self.account(),
            self.disk(),
            self.git(),
            self.chrome(),
            self.isolation(),
            self.service(),
            self.install(),
        ]
    }

    fn system(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::System, outcome, message);
        let arch = match self.arch.as_str() {
            "x86_64" | "amd64" => Some("x86_64"),
            "aarch64" | "arm64" => Some("aarch64"),
            _ => None,
        };
        match (self.os.as_str(), arch) {
            ("Linux", Some(arch)) => step(Outcome::Passed, format!("Linux on {arch}.")),
            ("Linux", None) => step(
                Outcome::Failed,
                format!(
                    "Linux on {}: Rovik's daemon is built for x86_64 and aarch64.",
                    self.arch
                ),
            ),
            (os, _) => step(
                Outcome::Failed,
                format!("{os} on {}: Rovik's daemon runs on Linux.", self.arch),
            ),
        }
    }

    fn account(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::Account, outcome, message);
        if self.uid == Some(0) {
            return step(
                Outcome::Failed,
                "You logged in as root, and the daemon never runs as root. Log in as a \
                 regular user; someone with root can install Rovik and make that user its \
                 administrator."
                    .into(),
            );
        }
        if !self.home.starts_with('/') {
            return step(
                Outcome::Failed,
                format!(
                    "{} has no home folder, where the daemon keeps everything.",
                    self.user
                ),
            );
        }
        step(
            Outcome::Passed,
            format!("Logged in as {}, home folder {}.", self.user, self.home),
        )
    }

    fn disk(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::Disk, outcome, message);
        let Some(free) = self.disk_free else {
            return step(
                Outcome::Warning,
                "The free space in your home folder couldn't be found.".into(),
            );
        };
        let shown = gigabytes(free);
        if free < DISK_NEEDED {
            step(
                Outcome::Failed,
                format!(
                    "{shown} free in your home folder: the daemon needs at least {}.",
                    gigabytes(DISK_NEEDED)
                ),
            )
        } else if free < DISK_LOW {
            step(
                Outcome::Warning,
                format!(
                    "{shown} free in your home folder: projects and their worktrees fill it \
                     fast."
                ),
            )
        } else {
            step(
                Outcome::Passed,
                format!("{shown} free in your home folder."),
            )
        }
    }

    fn git(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::Git, outcome, message);
        let Some(line) = &self.git else {
            return step(
                Outcome::Warning,
                "There's no git: the daemon doesn't need it, but the agent's git commands \
                 won't work."
                    .into(),
            );
        };
        let shown = line.strip_prefix("git version ").unwrap_or(line);
        match git_version(line) {
            Some(version) if version < GIT_WORKTREES => step(
                Outcome::Warning,
                format!(
                    "git {shown} is older than 2.5: the agent's git commands fail in \
                     worktrees."
                ),
            ),
            _ => step(Outcome::Passed, format!("git {shown}.")),
        }
    }

    fn chrome(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::Chrome, outcome, message);
        match &self.chrome {
            None => step(
                Outcome::Warning,
                "No Chrome or Chromium: the browser tools won't work.".into(),
            ),
            Some(chrome) if !self.chrome_headless => step(
                Outcome::Warning,
                format!(
                    "{chrome} didn't start headless: the browser tools won't work until it \
                     does."
                ),
            ),
            Some(chrome) => step(Outcome::Passed, format!("{chrome} starts headless.")),
        }
    }

    /// User namespaces, bubblewrap and FUSE: extras, not needed (design
    /// §3.1).
    fn isolation(&self) -> Step {
        let mut missing = Vec::new();
        if !self.user_namespaces {
            missing.push(
                "unprivileged user namespaces are off, so Chrome runs without its sandbox".into(),
            );
        }
        let virtual_files = match (self.fuse, &self.fusermount) {
            (true, Some(_)) => None,
            (false, _) => Some("there's no /dev/fuse"),
            (true, None) => Some("the FUSE tools (fusermount3) aren't installed"),
        };
        if let Some(why) = virtual_files {
            missing.push(format!("{why}, so CLM chats fall back to trimming"));
        }
        // bubblewrap isn't optional: the agent's commands are refused
        // without it.
        let required = self.bwrap.is_none().then_some(
            "bubblewrap isn't installed, so the agent's commands are refused there until it is \
             (Rovik's install adds it).",
        );
        let optional = (!missing.is_empty()).then(|| format!("Optional: {}.", missing.join("; ")));
        match (required, optional) {
            (None, None) => Step::new(
                Check::Isolation,
                Outcome::Passed,
                "User namespaces, bubblewrap and FUSE are there.".into(),
            ),
            (None, Some(optional)) => Step::new(Check::Isolation, Outcome::Warning, optional),
            (Some(required), None) => {
                Step::new(Check::Isolation, Outcome::Warning, required.into())
            }
            (Some(required), Some(optional)) => Step::new(
                Check::Isolation,
                Outcome::Warning,
                format!("{required} {optional}"),
            ),
        }
    }

    fn service(&self) -> Step {
        match self.systemd {
            true => Step::new(
                Check::Service,
                Outcome::Passed,
                "systemd runs the daemon, and starts it again after a reboot.".into(),
            ),
            false => Step::new(
                Check::Service,
                Outcome::Warning,
                "No systemd, as in a container: the app starts your daemon when it connects, \
                 so runs come back after a reboot only at the next connection, and only the \
                 daemon's own limits apply."
                    .into(),
            ),
        }
    }

    fn install(&self) -> Step {
        let step = |outcome, message: String| Step::new(Check::Install, outcome, message);
        let Some(installed) = &self.install else {
            if self.uid == Some(0) {
                return step(
                    Outcome::Warning,
                    "Rovik isn't installed yet. Log in as the user who is to be its \
                     administrator, and deploy from there."
                        .into(),
                );
            }
            return match self.sudo {
                Sudo::NoPassword => step(
                    Outcome::Passed,
                    "Rovik isn't installed yet. A deploy installs it with sudo, and makes you \
                     its administrator."
                        .into(),
                ),
                Sudo::Password => step(
                    Outcome::Passed,
                    "Rovik isn't installed yet. A deploy installs it with sudo, which asks for \
                     your password once (it isn't kept), and makes you its administrator."
                        .into(),
                ),
                Sudo::Refused | Sudo::Missing => step(
                    Outcome::Warning,
                    format!(
                        "Rovik isn't installed, and {}. A deploy shows the command for someone \
                         with root to run.",
                        match self.sudo {
                            Sudo::Refused => "sudo refuses you",
                            _ => "there's no sudo",
                        }
                    ),
                ),
            };
        };
        let admins = match installed.admins.as_slice() {
            [] => "It has no administrator.".to_string(),
            admins if admins.contains(&self.user) => {
                format!(
                    "Its administrators: {}. You're one of them.",
                    admins.join(", ")
                )
            }
            admins => format!("Its administrators: {}.", admins.join(", ")),
        };
        let others = match self.daemons.as_slice() {
            [] => String::new(),
            [one] => format!(" {one} runs a daemon here too."),
            many => format!(" {} run daemons here too.", many.join(", ")),
        };
        let Some(version) = &installed.version else {
            return step(
                Outcome::Warning,
                format!(
                    "Rovik's settings are on the server, but /opt/rovik/bin/rovik-agentd didn't \
                     run. {admins}{others}"
                ),
            );
        };
        match installed.protocol {
            Some(protocol) if protocol < PROTOCOL => step(
                Outcome::Warning,
                format!(
                    "Rovik {version} is installed, older than this app needs: it has to be \
                     upgraded. {admins}{others}"
                ),
            ),
            _ => step(
                Outcome::Passed,
                format!("Rovik {version} is installed. {admins}{others}"),
            ),
        }
    }
}

/// `rovik-agentd 0.1.0 (protocol 1)`: the version and the protocol.
fn version(line: &str) -> (Option<String>, Option<u32>) {
    let Some(rest) = line.strip_prefix("rovik-agentd ") else {
        return (None, None);
    };
    let version = rest.split_whitespace().next().map(str::to_string);
    let protocol = rest
        .split_once("(protocol ")
        .and_then(|(_, p)| p.trim_end_matches(')').trim().parse().ok());
    (version, protocol)
}

/// The strings of a TOML array on one line: `["ann", 'bo']`.
fn strings(array: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = array.chars();
    while let Some(c) = chars.by_ref().find(|c| *c == '"' || *c == '\'') {
        let text: String = chars.by_ref().take_while(|d| *d != c).collect();
        found.push(text);
    }
    found
}

/// `git version 2.43.0` (or `2.39.2 (Apple Git-143)`): major and minor.
fn git_version(line: &str) -> Option<(u32, u32)> {
    let number = line
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = number.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0)))
}

/// Bytes as GB, one decimal under 10.
fn gigabytes(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    match gb < 10.0 {
        true => format!("{gb:.1} GB"),
        false => format!("{gb:.0} GB"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Ubuntu VPS: sudo asks for a password; Chromium is the snap's
    /// stub, which doesn't start; AppArmor turns user namespaces off.
    const UBUNTU: &str = "\
os=Linux
arch=x86_64
uid=1000
user=anton
home=/home/anton
disk_kb=41943040
git=git version 2.43.0
chrome=/usr/bin/chromium-browser
chrome_headless=no
userns=no
fuse=yes
fusermount=/usr/bin/fusermount3
systemd=yes
sudo=password
socket=no
";

    /// A Debian server with Rovik installed by ann, who runs her daemon
    /// too; no sudo for bo, a FUSE-less VM, little disk.
    const DEBIAN: &str = "\
os=Linux
arch=aarch64
uid=1001
user=bo
home=/home/bo
disk_kb=3145728
git=git version 2.39.5
chrome=/usr/bin/chromium
chrome_headless=yes
userns=yes
bwrap=/usr/bin/bwrap
fuse=no
fusermount=
systemd=yes
sudo=no
rovik=rovik-agentd 0.1.0 (protocol 1)
rovik_admins=[\"ann\"]
socket=yes
daemon=ann
";

    /// Alpine: busybox, no sudo, no git, no Chrome, not much disk, and
    /// wget alone.
    const ALPINE: &str = "\
os=Linux
arch=x86_64
uid=1000
user=dev
home=/home/dev
disk_kb=921600
git=
chrome=
userns=no
fuse=no
fusermount=
systemd=no
sudo=none
socket=no
";

    /// A container without systemd, logged in as root: what this test's
    /// own container said.
    const CONTAINER: &str = "\
os=Linux
arch=x86_64
uid=0
user=root
home=/root
disk_kb=3320552
git=git version 2.43.0
chrome=
userns=yes
bwrap=/usr/bin/bwrap
fuse=yes
fusermount=/usr/bin/fusermount3
systemd=no
sudo=yes
socket=no
";

    fn outcomes(probe: &Probe) -> Vec<(Check, Outcome)> {
        probe
            .findings()
            .into_iter()
            .map(|s| (s.check, s.outcome))
            .collect()
    }

    fn message(probe: &Probe, check: Check) -> String {
        let steps = probe.findings();
        let step = steps.iter().find(|s| s.check == check).unwrap();
        step.message.clone()
    }

    #[test]
    fn ubuntu() {
        let probe = Probe::parse(UBUNTU);
        assert_eq!(probe.uid, Some(1000));
        assert_eq!(probe.disk_free, Some(40 << 30));
        assert_eq!(probe.sudo, Sudo::Password);
        assert_eq!(probe.install, None);
        use Check::*;
        use Outcome::*;
        assert_eq!(
            outcomes(&probe),
            [
                (System, Passed),
                (Account, Passed),
                (Disk, Passed),
                (Git, Passed),
                (Chrome, Warning),
                (Isolation, Warning),
                (Service, Passed),
                (Install, Passed),
            ]
        );
        assert_eq!(message(&probe, Disk), "40 GB free in your home folder.");
        assert!(message(&probe, Install).contains("asks for your password once"));
        assert!(message(&probe, Isolation).contains("Chrome runs without its sandbox"));
        assert!(!message(&probe, Isolation).contains("CLM"));
    }

    #[test]
    fn debian_with_rovik_installed_by_someone_else() {
        let probe = Probe::parse(DEBIAN);
        assert_eq!(
            probe.install,
            Some(Installed {
                version: Some("0.1.0".into()),
                protocol: Some(1),
                admins: vec!["ann".into()],
            })
        );
        assert_eq!(probe.daemons, ["ann"]);
        assert!(probe.socket);
        use Check::*;
        use Outcome::*;
        assert_eq!(
            outcomes(&probe),
            [
                (System, Passed),
                (Account, Passed),
                (Disk, Warning),
                (Git, Passed),
                (Chrome, Passed),
                (Isolation, Warning),
                (Service, Passed),
                (Install, Passed),
            ]
        );
        assert_eq!(message(&probe, System), "Linux on aarch64.");
        assert_eq!(
            message(&probe, Install),
            "Rovik 0.1.0 is installed. Its administrators: ann. ann runs a daemon here too."
        );
        assert_eq!(
            message(&probe, Isolation),
            "Optional: there's no /dev/fuse, so CLM chats fall back to trimming."
        );
        assert!(message(&probe, Disk).starts_with("3.0 GB free"));
    }

    #[test]
    fn alpine_with_busybox() {
        let probe = Probe::parse(ALPINE);
        assert_eq!(probe.git, None);
        assert_eq!(probe.chrome, None);
        assert_eq!(probe.sudo, Sudo::Missing);
        use Check::*;
        use Outcome::*;
        assert_eq!(
            outcomes(&probe),
            [
                (System, Passed),
                (Account, Passed),
                (Disk, Failed),
                (Git, Warning),
                (Chrome, Warning),
                (Isolation, Warning),
                (Service, Warning),
                (Install, Warning),
            ]
        );
        assert!(message(&probe, Install).contains("there's no sudo"));
        assert!(message(&probe, Install).contains("someone with root"));
        assert!(message(&probe, Service).contains("No systemd"));
        assert!(message(&probe, Disk).contains("at least 1.0 GB"));
    }

    #[test]
    fn a_container_without_systemd_logged_in_as_root() {
        let probe = Probe::parse(CONTAINER);
        use Check::*;
        use Outcome::*;
        assert_eq!(
            outcomes(&probe),
            [
                (System, Passed),
                (Account, Failed),
                (Disk, Warning),
                (Git, Passed),
                (Chrome, Warning),
                (Isolation, Passed),
                (Service, Warning),
                (Install, Warning),
            ]
        );
        assert!(message(&probe, Account).contains("never runs as root"));
    }

    #[test]
    fn what_the_probe_says_is_read_leniently() {
        let probe = Probe::parse(
            "os=Linux\r\narch=riscv64\r\nnoise\nlater=a new key\nrovik=\n\
             rovik_admins=['ann', \"bo\"]\ndaemon=ann\ndaemon=ann\ndaemon=cy\n",
        );
        assert_eq!(probe.arch, "riscv64");
        assert_eq!(probe.daemons, ["ann", "cy"]);
        let installed = probe.install.clone().unwrap();
        assert_eq!((installed.version, installed.admins.len()), (None, 2));
        assert_eq!(probe.system().outcome, Outcome::Failed);
        assert!(probe.system().message.contains("x86_64 and aarch64"));
        let mut admin = probe.clone();
        admin.user = "bo".into();
        assert!(admin.install().message.contains("You're one of them."));
        assert!(admin
            .install()
            .message
            .contains("ann, cy run daemons here too."));
        assert_eq!(
            admin.install().outcome,
            Outcome::Warning,
            "the binary didn't run"
        );
        assert_eq!(git_version("git version 2.4.11"), Some((2, 4)));
        assert_eq!(
            git_version("git version 2.39.2 (Apple Git-143)"),
            Some((2, 39))
        );
        assert_eq!(git_version("git version 2.34.1.windows.1"), Some((2, 34)));
        let old = Probe::parse("git=git version 2.4.11");
        assert_eq!(old.git().outcome, Outcome::Warning);
        let older = Probe::parse("rovik=rovik-agentd 0.0.9 (protocol 0)");
        assert_eq!(older.install().outcome, Outcome::Warning);
        assert!(older.install().message.contains("upgraded"));
    }

    /// Each line the script can print is one the parser reads.
    #[test]
    fn the_parser_knows_each_key_the_script_says() {
        let said: Vec<&str> = PROBE
            .split("say ")
            .skip(1)
            .filter_map(|rest| rest.split_whitespace().next())
            .filter(|key| key.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .collect();
        assert!(said.len() >= 18, "{said:?}");
        for key in said {
            let read = |value: &str| Probe::parse(&format!("{key}={value}")) != Probe::default();
            assert!(read("yes") || read("1"), "{key} isn't read");
        }
    }

    /// The script parses as POSIX sh, as this machine's runs it.
    #[cfg(unix)]
    #[test]
    fn the_script_is_sh() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(PROBE.as_bytes()).unwrap();
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{said}");
    }
}

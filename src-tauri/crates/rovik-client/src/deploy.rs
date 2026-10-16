//! The daemon on a host (H05, 5.3): deploying it, checking it on each
//! connection, upgrading it, and removing it.
//!
//! A deploy probes the server (`connection_test`) and refuses a root
//! login. It uploads the build to `~/.rovik-agentd-upload` and checks its
//! SHA-256 there, then has it add this computer's token to the user's
//! `config.toml`: the hash goes on stdin, and the token stays here, for
//! the keychain. On a server without Rovik the build installs it through
//! sudo. If sudo asks for the user's password, the deploy asks for it
//! once and doesn't keep it. Without sudo the deploy stops with the
//! command for someone with root, and goes on when it's run again after
//! that. On a server with Rovik the user's own daemon starts (`user
//! start`), and starts again to read the new token (`user restart`);
//! without systemd the app starts it in a session of its own. Then the
//! daemon must answer `/health`, and `Welcome` with the new token.
//!
//! The build is this app's release for the server's CPU, downloaded with
//! its signature, which is checked first (`release`, B02), and uploaded
//! beside it for `admin upgrade` to check again; or a build of one's own
//! that `ROVIK_AGENTD_BINARY` names (B03), with no signature.
//!
//! `check_install` runs on each connection (D5): the installed binary
//! must be a build this computer trusts, and the user's unit must run it as
//! the installer wrote it. A deploy trusts the build the server runs once
//! it's done, which on a server another user installed is theirs; a build
//! that changed since (an administrator's upgrade, or someone else's
//! change) is the user's to compare and trust, as a host's key is. An
//! upgrade is for an administrator (`admin upgrade`); anyone else is told
//! whom to ask. Removing stops the user's
//! daemon and deletes `~/.rovik` and `~/.local/share/rovik`, as `removal`
//! listed them for the user to confirm.

use crate::client::{welcome, DaemonInfo, Identity};
use crate::connection_test::probe::{Probe, Sudo};
use crate::connection_test::probe_server;
use crate::http;
use crate::release::Releases;
use crate::ssh::{Output, Ssh, SshConnector};
use rovik_protocol::Secret;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Where the build waits in the user's home before it's installed.
const UPLOAD: &str = ".rovik-agentd-upload";
/// Says `yes` where the server has gzip.
const HAS_GZIP: &str = "command -v gzip >/dev/null 2>&1 && echo yes; true";
/// How long a daemon that's starting may take to answer.
const START_WAIT: Duration = Duration::from_secs(30);

/// A build of the daemon, here.
#[derive(Clone, Debug)]
pub struct Binary {
    pub path: PathBuf,
    pub sha256: String,
    /// A release's signature for it, as its `.sig` file has it; none for a
    /// build of one's own.
    pub signature: Option<String>,
}

impl Binary {
    /// A build of one's own, at `path`.
    pub fn at(path: PathBuf) -> Result<Binary, DeployError> {
        let bytes = std::fs::read(&path)
            .map_err(|e| DeployError::Failed(format!("{}: {e}", path.display())))?;
        Ok(Binary {
            sha256: hex(&Sha256::digest(&bytes)),
            path,
            signature: None,
        })
    }
}

/// Where a deploy's or an upgrade's build comes from.
#[derive(Clone, Debug)]
pub enum Builds {
    /// A build of one's own (B03).
    Own(Binary),
    /// `version`'s release, for the server's CPU (B02).
    Release { releases: Releases, version: String },
}

impl Builds {
    /// The build `ROVIK_AGENTD_BINARY` names, else `version`'s release,
    /// kept under `data_dir`.
    pub fn new(data_dir: &std::path::Path, version: &str) -> Result<Builds, DeployError> {
        match std::env::var_os("ROVIK_AGENTD_BINARY") {
            Some(path) => Binary::at(PathBuf::from(path)).map(Builds::Own),
            None => Ok(Builds::Release {
                releases: Releases::new(data_dir),
                version: version.to_string(),
            }),
        }
    }

    /// The build for a server whose `uname -m` is `machine`.
    pub async fn for_machine(&self, machine: &str) -> Result<Binary, DeployError> {
        let (releases, version) = match self {
            Builds::Own(binary) => return Ok(binary.clone()),
            Builds::Release { releases, version } => (releases, version),
        };
        let cpu = rovik_protocol::release::cpu(machine).ok_or_else(|| {
            DeployError::Unsupported(format!("the daemon isn't built for {machine}."))
        })?;
        let file = releases.build(version, cpu).await;
        let file = file.map_err(|why| {
            let instead = match why == crate::release::NO_KEY {
                true => "",
                false => {
                    " A build of your own can go instead: set ROVIK_AGENTD_BINARY to it \
                     (scripts/build-agentd.sh makes one)."
                }
            };
            DeployError::Failed(format!(
                "Rovik {version}'s daemon for {cpu}: {why}{instead}"
            ))
        })?;
        Ok(Binary {
            sha256: hex(&Sha256::digest(&file.bytes)),
            path: file.path,
            signature: Some(file.signature),
        })
    }
}

/// Where Rovik is on a server: the installer's paths, which tests move.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The installed binary, a link to its version.
    pub installed: String,
    pub sudo: String,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            installed: "/opt/rovik/bin/rovik-agentd".into(),
            sudo: "sudo".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeployError {
    /// Logged in as root: the daemon never runs as root.
    RootLogin,
    /// A server Rovik's daemon doesn't run on, and why.
    Unsupported(String),
    /// sudo asks for the user's password: ask, and deploy again with it.
    SudoPassword,
    /// Someone with root is to run `command` (the build stays where it
    /// says); deploying again then goes on.
    NeedsRoot {
        command: String,
    },
    /// Rovik's binary or unit on the server isn't what the app installed.
    Tampered(String),
    /// Rovik's binary on the server isn't the build this computer trusts:
    /// an administrator's upgrade, or a change behind the app's back. The
    /// user is to compare and trust it (`HostsPage::trust_build`).
    Changed(InstalledBuild),
    /// Only these administrators upgrade Rovik here.
    AskAdmins(Vec<String>),
    Failed(String),
}

impl fmt::Display for DeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeployError::RootLogin => f.write_str(
                "You logged in as root, and Rovik's daemon never runs as root. Log in as a \
                 regular user.",
            ),
            DeployError::Unsupported(why) => write!(f, "Rovik can't run on this server: {why}"),
            DeployError::SudoPassword => {
                f.write_str("sudo asks for your password to install Rovik. It isn't kept.")
            }
            DeployError::NeedsRoot { command } => write!(
                f,
                "Installing Rovik takes root, and sudo isn't yours here. Ask someone with \
                 root to run this, then deploy again: {command}"
            ),
            DeployError::Tampered(why) => f.write_str(why),
            DeployError::Changed(build) => write!(
                f,
                "Rovik on this server isn't the build you trusted: it's {} now, with the \
                 SHA-256 {}. An administrator's upgrade does this. If nobody upgraded it, ask \
                 the server's administrators before you trust it.",
                build
                    .version
                    .as_deref()
                    .map_or("another build".to_string(), |v| format!("version {v}")),
                build.sha256
            ),
            DeployError::AskAdmins(admins) => write!(
                f,
                "Only Rovik's administrators on this server upgrade it: ask {}.",
                match admins.as_slice() {
                    [] => "whoever runs the server".to_string(),
                    admins => admins.join(" or "),
                }
            ),
            DeployError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for DeployError {}

/// What a deploy leaves for the app to keep.
#[derive(Clone, Debug)]
pub struct Deployed {
    /// This computer's token: for the keychain (`SecretName::Token`).
    pub token: Secret,
    /// What answered `Welcome`.
    pub daemon: DaemonInfo,
    /// The hash of the build the server runs, which `check_install` knows
    /// from then on: the one uploaded, or on a server another user
    /// installed, theirs.
    pub sha256: String,
}

/// Rovik's binary as the server has it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledBuild {
    pub sha256: String,
    /// The version its name says: the installer names each build
    /// `rovik-agentd-<version>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Deploys `binary` for this user on `connector`'s host, as `computer`.
pub async fn deploy(
    connector: &SshConnector,
    builds: &Builds,
    computer: &str,
    sudo_password: Option<&Secret>,
    layout: &Layout,
) -> Result<Deployed, DeployError> {
    let ssh = session(connector).await?;
    let probe = probe_server(&ssh).await.map_err(DeployError::Failed)?;
    supported(&probe)?;
    let binary = builds.for_machine(&probe.arch).await?;
    let upload = upload(&ssh, &binary).await?;
    let token = new_token();
    let done = install_and_start(
        &ssh,
        &probe,
        &upload,
        computer,
        &token,
        sudo_password,
        layout,
    );
    let done = done.await;
    // The build stays only for someone with root to run.
    if !matches!(done, Err(DeployError::NeedsRoot { .. })) {
        forget_upload(&ssh, &upload).await;
    }
    done?;
    let identity = Identity {
        host: connector.target().host.clone(),
        computer: computer.into(),
        token: token.clone(),
        client: format!("rovik-client {}", env!("CARGO_PKG_VERSION")),
    };
    let daemon = confirm(connector, &identity).await?;
    // The build the server runs is trusted from now on, as the user chose
    // to deploy there: theirs, or the one already installed.
    let (installed, _) = installed(&ssh, &probe.user, layout).await?;
    let installed = installed.ok_or_else(|| {
        DeployError::Failed(format!(
            "Rovik's binary isn't on the server at {}.",
            layout.installed
        ))
    })?;
    Ok(Deployed {
        token,
        daemon,
        sha256: installed.sha256,
    })
}

/// The server must be Linux on x86_64 or aarch64, logged into as a
/// regular user.
fn supported(probe: &Probe) -> Result<(), DeployError> {
    if probe.uid == Some(0) {
        return Err(DeployError::RootLogin);
    }
    let arch = matches!(
        probe.arch.as_str(),
        "x86_64" | "amd64" | "aarch64" | "arm64"
    );
    if probe.os != "Linux" || !arch {
        return Err(DeployError::Unsupported(format!(
            "it's {} on {}, and the daemon runs on Linux, on x86_64 or aarch64.",
            probe.os, probe.arch
        )));
    }
    Ok(())
}

async fn install_and_start(
    ssh: &Ssh,
    probe: &Probe,
    upload: &str,
    computer: &str,
    token: &Secret,
    sudo_password: Option<&Secret>,
    layout: &Layout,
) -> Result<(), DeployError> {
    // The hash goes in config.toml before the daemon starts, as it reads
    // it then.
    let add = format!("{} token add {}", quote(upload), quote(computer));
    let hash = format!("{}\n", hex(&Sha256::digest(token.0.as_bytes())));
    run(ssh, &add, hash.as_bytes()).await?;
    if probe.install.is_none() {
        let (command, input) = install_command(probe, upload, sudo_password, layout)?;
        run(ssh, &command, &input).await?;
        if probe.systemd {
            // The installer started the administrator's daemon.
            return Ok(());
        }
    }
    start(ssh, probe, layout).await
}

/// How the build installs Rovik: through sudo, with the password on
/// stdin when sudo asks for one, or by someone with root.
fn install_command(
    probe: &Probe,
    upload: &str,
    sudo_password: Option<&Secret>,
    layout: &Layout,
) -> Result<(String, Zeroizing<Vec<u8>>), DeployError> {
    let install = format!("{} install --admin {}", quote(upload), quote(&probe.user));
    match (probe.sudo, sudo_password) {
        (Sudo::NoPassword, _) => Ok((
            format!("{} -n {install}", layout.sudo),
            Zeroizing::new(Vec::new()),
        )),
        (Sudo::Password, Some(password)) => Ok((
            format!("{} -S -p '' {install}", layout.sudo),
            Zeroizing::new(format!("{}\n", password.0).into_bytes()),
        )),
        (Sudo::Password, None) => Err(DeployError::SudoPassword),
        (Sudo::Refused | Sudo::Missing, _) => Err(DeployError::NeedsRoot { command: install }),
    }
}

/// The user's daemon, started or started again, to read a new token.
async fn start(ssh: &Ssh, probe: &Probe, layout: &Layout) -> Result<(), DeployError> {
    let installed = quote(&layout.installed);
    if probe.systemd {
        for action in ["start", "restart"] {
            let command = format!("{} -n {installed} user {action}", layout.sudo);
            run(ssh, &command, b"").await?;
        }
        return Ok(());
    }
    // Without systemd the app starts it (5.3), in a session of its own so
    // it outlives this connection, once one that runs has stopped.
    let command = format!(
        "{}; setsid {installed} serve </dev/null >/dev/null 2>&1 &",
        stop_unmanaged(layout)
    );
    run(ssh, &command, b"").await
}

/// Stops the user's daemon that runs without systemd, and waits up to
/// 100 s for it to go: stopping, it pauses its runs for up to 80 s first
/// (R02), and another can't start while it holds its lock.
fn stop_unmanaged(layout: &Layout) -> String {
    let pattern = quote(&format!("^{} serve", layout.installed));
    format!(
        "pkill -u \"$(id -u)\" -f {pattern}; i=0; \
         while pgrep -u \"$(id -u)\" -f {pattern} >/dev/null && [ $i -lt 100 ]; do \
         sleep 1; i=$((i + 1)); done"
    )
}

/// The daemon answers `/health`, then `Welcome` with `identity`'s token.
/// One that's starting may take a moment.
pub async fn confirm(
    connector: &SshConnector,
    identity: &Identity,
) -> Result<DaemonInfo, DeployError> {
    let started = Instant::now();
    loop {
        match http::health(connector).await {
            Ok(_) => break,
            Err(e) if started.elapsed() > START_WAIT => {
                return Err(DeployError::Failed(format!(
                    "The daemon didn't answer once started: {e}"
                )))
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    welcome(connector, identity)
        .await
        .map_err(|e| DeployError::Failed(e.message))
}

/// On each connection (D5): the user's unit runs the installed binary as
/// the installer wrote it, and that binary is a build this computer trusts
/// (`known`, their SHA-256) or one of its version's release, as the
/// release's signed `SHA256SUMS` lists them. Without systemd, only the
/// binary is checked.
pub async fn check_install(
    ssh: &Ssh,
    known: &[String],
    releases: Option<&Releases>,
    user: &str,
    layout: &Layout,
) -> Result<InstalledBuild, DeployError> {
    let (build, shown) = installed(ssh, user, layout).await?;
    let Some(build) = build else {
        return Err(DeployError::Tampered(format!(
            "Rovik's binary isn't on the server at {}.",
            layout.installed
        )));
    };
    let expected = format!("{} serve", layout.installed);
    match exec_starts(&shown).as_slice() {
        [] => {}
        [only] if *only == expected => {}
        found => {
            return Err(DeployError::Tampered(format!(
                "Your daemon's unit doesn't run Rovik as installed: it runs {}. Someone may \
                 have changed it: ask the server's administrators before you go on.",
                found.join(", then ")
            )))
        }
    }
    if known.contains(&build.sha256) {
        return Ok(build);
    }
    if let (Some(releases), Some(version)) = (releases, &build.version) {
        let listed = releases.sums(version).await;
        if listed.is_ok_and(|sums| sums.contains(&build.sha256)) {
            return Ok(build);
        }
    }
    Err(DeployError::Changed(build))
}

/// The build the installed link points to, if there's one, and what
/// systemd says the user's unit runs (nothing without systemd).
async fn installed(
    ssh: &Ssh,
    user: &str,
    layout: &Layout,
) -> Result<(Option<InstalledBuild>, String), DeployError> {
    let command = format!(
        "f=$(readlink -f {installed}) && [ -f \"$f\" ] && \
         printf 'path=%s\\nsha256=%s\\n' \"$f\" \"$(sha256sum \"$f\" | cut -d' ' -f1)\"; \
         systemctl show -p ExecStart --value {unit} 2>/dev/null; true",
        installed = quote(&layout.installed),
        unit = quote(&format!("rovik-agentd@{user}.service")),
    );
    let out = ssh
        .exec(&command)
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))?;
    Ok(read_installed(&String::from_utf8_lossy(&out.stdout)))
}

/// What `installed`'s command printed: its `path=` and `sha256=` lines,
/// then systemd's.
fn read_installed(text: &str) -> (Option<InstalledBuild>, String) {
    let (mut path, mut sha256, mut shown) = (None, None, String::new());
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("path=") {
            path = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("sha256=") {
            sha256 = Some(value.trim()).filter(|v| !v.is_empty());
        } else {
            shown.push_str(line);
            shown.push('\n');
        }
    }
    let build = sha256.map(|sha256| InstalledBuild {
        sha256: sha256.to_string(),
        version: path.and_then(version_of),
    });
    (build, shown)
}

/// `/opt/rovik/bin/rovik-agentd-0.2.0`: `0.2.0`.
fn version_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let version = name.strip_prefix("rovik-agentd-")?;
    Some(version.to_string()).filter(|v| !v.is_empty())
}

/// The command lines in what `systemctl show -p ExecStart --value` prints:
/// `{ path=… ; argv[]=/opt/rovik/bin/rovik-agentd serve ; … }`, one per
/// command.
fn exec_starts(shown: &str) -> Vec<String> {
    shown
        .split("argv[]=")
        .skip(1)
        .map(|rest| rest.split(" ; ").next().unwrap_or(rest).trim().to_string())
        .collect()
}

/// An upgrade for everyone, by an administrator (`admin upgrade`), which
/// returns the build's SHA-256: what the server runs from then on. Anyone
/// else is told whom to ask. The daemons that ran pause their runs and
/// start again on it (R02). Without systemd only the administrator's own
/// daemon is started again here; the others' take it at their next start.
pub async fn upgrade(
    connector: &SshConnector,
    builds: &Builds,
    layout: &Layout,
) -> Result<String, DeployError> {
    let ssh = session(connector).await?;
    let probe = probe_server(&ssh).await.map_err(DeployError::Failed)?;
    let Some(install) = &probe.install else {
        return Err(DeployError::Failed(
            "Rovik isn't installed on this server: deploy it first.".into(),
        ));
    };
    if !install.admins.contains(&probe.user) {
        return Err(DeployError::AskAdmins(install.admins.clone()));
    }
    let binary = builds.for_machine(&probe.arch).await?;
    let upload = upload(&ssh, &binary).await?;
    let command = format!(
        "{} -n {} admin upgrade {}",
        layout.sudo,
        quote(&layout.installed),
        quote(&upload)
    );
    let upgraded = run(&ssh, &command, b"").await;
    forget_upload(&ssh, &upload).await;
    upgraded?;
    if !probe.systemd {
        start(&ssh, &probe, layout).await?;
    }
    Ok(binary.sha256)
}

/// The limits on each user's daemon, as `admin limits` shows them to an
/// administrator: each name and its value, `none` when it isn't set.
pub async fn limits(
    connector: &SshConnector,
    layout: &Layout,
) -> Result<Vec<(String, String)>, DeployError> {
    let ssh = session(connector).await?;
    let command = format!(
        "{} -n {} admin limits",
        layout.sudo,
        quote(&layout.installed)
    );
    let out = exec(&ssh, &command, b"").await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let limits = text.lines().filter_map(|line| {
        let (name, value) = line.split_once(" = ")?;
        Some((name.trim().to_string(), value.trim().to_string()))
    });
    Ok(limits.collect())
}

/// Sets limits as an administrator (`admin limits name=value ...`), `none`
/// taking one away. They apply to each daemon when it next starts.
pub async fn set_limits(
    connector: &SshConnector,
    changes: &[(String, String)],
    layout: &Layout,
) -> Result<(), DeployError> {
    let settings: Vec<String> = changes
        .iter()
        .map(|(name, value)| quote(&format!("{name}={value}")))
        .collect();
    let ssh = session(connector).await?;
    let command = format!(
        "{} -n {} admin limits {}",
        layout.sudo,
        quote(&layout.installed),
        settings.join(" ")
    );
    run(&ssh, &command, b"").await
}

/// What removing Rovik for this user deletes: each folder, and its size in
/// bytes, for the user to confirm.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Removal {
    pub folders: Vec<(String, u64)>,
}

/// The folders a removal deletes.
pub async fn removal(connector: &SshConnector) -> Result<Removal, DeployError> {
    let ssh = session(connector).await?;
    let command = "for d in \"$HOME/.rovik\" \"$HOME/.local/share/rovik\"; do \
                   if [ -d \"$d\" ]; then \
                   printf '%s\\t%s\\n' \"$(du -sk \"$d\" 2>/dev/null | cut -f1)\" \"$d\"; \
                   fi; done; true";
    let out = ssh
        .exec(command)
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))?;
    let folders = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (kb, path) = line.split_once('\t')?;
            Some((
                path.to_string(),
                kb.trim().parse::<u64>().unwrap_or(0) << 10,
            ))
        })
        .collect();
    Ok(Removal { folders })
}

/// Stops the user's daemon, then deletes what `removal` listed.
pub async fn remove(
    connector: &SshConnector,
    removal: &Removal,
    layout: &Layout,
) -> Result<(), DeployError> {
    // Only Rovik's own two folders, whatever came back.
    for (path, _) in &removal.folders {
        let ours = path.starts_with('/')
            && (path.ends_with("/.rovik") || path.ends_with("/.local/share/rovik"));
        if !ours {
            return Err(DeployError::Failed(format!(
                "{path} isn't Rovik's to delete."
            )));
        }
    }
    let ssh = session(connector).await?;
    let probe = probe_server(&ssh).await.map_err(DeployError::Failed)?;
    let stop = match (probe.systemd, &probe.install) {
        (true, Some(_)) => format!("{} -n {} user stop", layout.sudo, quote(&layout.installed)),
        _ => format!("{}; true", stop_unmanaged(layout)),
    };
    run(&ssh, &stop, b"").await?;
    let folders: Vec<String> = removal.folders.iter().map(|(p, _)| quote(p)).collect();
    // The virtual files' mount, if the daemon left it, would stop rm.
    let command = format!(
        "fusermount3 -u \"$HOME/.local/share/rovik/vfs\" 2>/dev/null; \
         fusermount -u \"$HOME/.local/share/rovik/vfs\" 2>/dev/null; \
         rm -rf -- {}",
        folders.join(" ")
    );
    run(&ssh, &command, b"").await
}

/// What the host's setup script printed, and how it ended.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct SetupRun {
    /// Its exit status: 0 when it worked, none if the host didn't say.
    pub status: Option<u32>,
    /// What it printed, stdout and stderr as they came: the end of it
    /// when there's more than the page shows.
    pub output: String,
}

/// The most of a setup script's output kept, in bytes.
const SETUP_OUTPUT: usize = 16 * 1024;

/// Runs the host's setup script (design §7) as the user, in their home
/// folder: with bash where there is one, else sh, the script on stdin.
pub async fn run_setup(connector: &SshConnector, script: &str) -> Result<SetupRun, DeployError> {
    let ssh = session(connector).await?;
    let command = "cd && if command -v bash >/dev/null 2>&1; \
                   then exec bash -s 2>&1; else exec sh -s 2>&1; fi";
    let out = ssh
        .exec_with_input(command, script.as_bytes())
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))?;
    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(SetupRun {
        status: out.status,
        output: tail(&output, SETUP_OUTPUT),
    })
}

/// The last `max` bytes of `text` or so, cut where a character starts.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

/// The build in the user's home on the server, its SHA-256 checked there.
/// Returns its path there.
/// Uploads `binary` to the user's home, and its signature beside it
/// (`<upload>.sig`) when it has one, for `admin upgrade` to check: a build
/// of one's own leaves no signature there, not even an older one.
async fn upload(ssh: &Ssh, binary: &Binary) -> Result<String, DeployError> {
    let signed = match &binary.signature {
        Some(_) => format!(
            "umask 077 && cat > \"$HOME/{UPLOAD}{}\"",
            rovik_protocol::release::SIGNATURE
        ),
        None => format!(
            "rm -f \"$HOME/{UPLOAD}{}\"",
            rovik_protocol::release::SIGNATURE
        ),
    };
    let signature = binary.signature.as_deref().unwrap_or_default();
    run(ssh, &signed, signature.as_bytes()).await?;
    let bytes = std::fs::read(&binary.path)
        .map_err(|e| DeployError::Failed(format!("{}: {e}", binary.path.display())))?;
    // A build is mostly tables: gzipped, a tenth of it goes, where the
    // server has gzip to unpack it.
    let gzip = exec(ssh, HAS_GZIP, b"").await;
    let gzip = gzip.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "yes");
    let (write, sent) = match gzip {
        true => ("gzip -dc", gzipped(&bytes)?),
        false => ("cat", bytes),
    };
    let command = format!(
        "umask 077 && f=\"$HOME/{UPLOAD}\" && {write} > \"$f.part\" && chmod 700 \"$f.part\" && \
         mv -f \"$f.part\" \"$f\" && printf '%s\\n' \"$f\" && sha256sum \"$f\" | cut -d' ' -f1"
    );
    let out = exec(ssh, &command, &sent).await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let (path, hash) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
    if hash.trim() != binary.sha256 {
        return Err(DeployError::Failed(
            "The build arrived damaged on the server. Deploy again.".into(),
        ));
    }
    Ok(path.to_string())
}

fn gzipped(bytes: &[u8]) -> Result<Vec<u8>, DeployError> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(bytes)
        .and_then(|()| gz.finish())
        .map_err(|e| DeployError::Failed(e.to_string()))
}

/// The upload and its signature, once they've done their work.
async fn forget_upload(ssh: &Ssh, upload: &str) {
    let signature = format!("{upload}{}", rovik_protocol::release::SIGNATURE);
    let command = format!("rm -f {} {}", quote(upload), quote(&signature));
    let _ = ssh.exec(&command).await;
}

async fn session(connector: &SshConnector) -> Result<std::sync::Arc<Ssh>, DeployError> {
    connector
        .session()
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))
}

/// Runs `command` with `input` on stdin; it must succeed.
async fn run(ssh: &Ssh, command: &str, input: &[u8]) -> Result<(), DeployError> {
    exec(ssh, command, input).await.map(|_| ())
}

async fn exec(ssh: &Ssh, command: &str, input: &[u8]) -> Result<Output, DeployError> {
    let out = ssh
        .exec_with_input(command, input)
        .await
        .map_err(|e| DeployError::Failed(e.to_string()))?;
    if out.status != Some(0) {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = match said.trim() {
            "" => String::from_utf8_lossy(&out.stdout).trim().to_string(),
            said => said.to_string(),
        };
        return Err(DeployError::Failed(format!(
            "The server couldn't {}: {said}",
            what(command)
        )));
    }
    Ok(out)
}

/// What a command does, to name it in an error: its words, without the
/// password or anything sent on stdin, which never appear in it.
fn what(command: &str) -> String {
    let short: String = command.chars().take(120).collect();
    format!("run `{short}`")
}

/// 32 random bytes, in hex: what names this computer to the daemon (D5).
fn new_token() -> Secret {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut_slice()).expect("the system's random numbers");
    Secret(hex(bytes.as_slice()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `text` in single quotes for the login shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_output_keeps_its_end_whole() {
        assert_eq!(tail("short", 16), "short");
        assert_eq!(tail("0123456789", 4), "…6789");
        // Never half a character.
        assert_eq!(tail("aé€", 4), "…€");
    }

    fn probe(sudo: Sudo) -> Probe {
        Probe {
            os: "Linux".into(),
            arch: "x86_64".into(),
            uid: Some(1000),
            user: "ann".into(),
            home: "/home/ann".into(),
            sudo,
            systemd: true,
            ..Probe::default()
        }
    }

    #[test]
    fn root_logins_and_other_systems_are_refused() {
        let mut root = probe(Sudo::NoPassword);
        root.uid = Some(0);
        assert_eq!(supported(&root), Err(DeployError::RootLogin));
        let mut mac = probe(Sudo::NoPassword);
        mac.os = "Darwin".into();
        assert!(matches!(supported(&mac), Err(DeployError::Unsupported(_))));
        assert_eq!(supported(&probe(Sudo::Missing)), Ok(()));
    }

    #[test]
    fn sudo_installs_with_the_password_on_stdin_or_someone_with_root_does() {
        let layout = Layout::default();
        let upload = "/home/ann/.rovik-agentd-upload";
        let (command, input) =
            install_command(&probe(Sudo::NoPassword), upload, None, &layout).unwrap();
        assert_eq!(
            command,
            "sudo -n '/home/ann/.rovik-agentd-upload' install --admin 'ann'"
        );
        assert!(input.is_empty());
        let asks = install_command(&probe(Sudo::Password), upload, None, &layout);
        assert_eq!(asks.unwrap_err(), DeployError::SudoPassword);
        let password = Secret("hunter2".into());
        let (command, input) =
            install_command(&probe(Sudo::Password), upload, Some(&password), &layout).unwrap();
        assert!(command.starts_with("sudo -S -p '' "), "{command}");
        assert!(!command.contains("hunter2"));
        assert_eq!(input.as_slice(), b"hunter2\n");
        for sudo in [Sudo::Missing, Sudo::Refused] {
            let root = install_command(&probe(sudo), upload, None, &layout).unwrap_err();
            let DeployError::NeedsRoot { command } = &root else {
                panic!("{root:?}");
            };
            assert_eq!(
                command,
                "'/home/ann/.rovik-agentd-upload' install --admin 'ann'"
            );
            assert!(root.to_string().contains(command.as_str()));
        }
    }

    #[test]
    fn the_units_command_lines_are_read_from_systemctl() {
        let shown = "{ path=/opt/rovik/bin/rovik-agentd ; argv[]=/opt/rovik/bin/rovik-agentd \
                     serve ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; \
                     code=(null) ; status=0/0 }\n";
        assert_eq!(exec_starts(shown), ["/opt/rovik/bin/rovik-agentd serve"]);
        let two = format!("{shown}{{ path=/tmp/x ; argv[]=/tmp/x --evil ; ignore_errors=no }}");
        assert_eq!(exec_starts(&two).len(), 2);
        assert!(exec_starts("").is_empty(), "no systemd");
    }

    #[test]
    fn the_installed_build_is_read_with_its_version() {
        let hash = "ab".repeat(32);
        let shown = "{ path=/opt/rovik/bin/rovik-agentd ; argv[]=/opt/rovik/bin/rovik-agentd \
                     serve ; ignore_errors=no }";
        let text = format!("path=/opt/rovik/bin/rovik-agentd-0.2.0\nsha256={hash}\n{shown}\n");
        let (build, unit) = read_installed(&text);
        let build = build.unwrap();
        assert_eq!(build.sha256, hash);
        assert_eq!(build.version.as_deref(), Some("0.2.0"));
        assert_eq!(exec_starts(&unit), ["/opt/rovik/bin/rovik-agentd serve"]);
        // A build of one's own keeps its name: no version to tell.
        let own = read_installed(&format!("path=/srv/rovik-agentd\nsha256={hash}\n")).0;
        assert_eq!(own.unwrap().version, None);
        // No binary there: nothing but what systemd said.
        assert_eq!(read_installed(shown).0, None);
        assert_eq!(read_installed("path=/x\nsha256=\n").0, None);
    }

    #[test]
    fn a_changed_build_says_what_it_is_now() {
        let changed = DeployError::Changed(InstalledBuild {
            sha256: "cd".repeat(32),
            version: Some("0.2.0".into()),
        });
        let said = changed.to_string();
        assert!(said.contains("version 0.2.0"), "{said}");
        assert!(said.contains(&"cd".repeat(32)), "{said}");
        let own = DeployError::Changed(InstalledBuild {
            sha256: "cd".repeat(32),
            version: None,
        });
        assert!(own.to_string().contains("it's another build now"), "{own}");
    }

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.0.len(), 64);
        assert!(a.0.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn whom_to_ask_for_an_upgrade_is_named() {
        let error = DeployError::AskAdmins(vec!["ann".into(), "bo".into()]);
        assert_eq!(
            error.to_string(),
            "Only Rovik's administrators on this server upgrade it: ask ann or bo."
        );
    }
}

//! The distribution's package manager, and running a script as root the way
//! a desktop normally asks (polkit's dialog, `pkexec`): what the virtual
//! files' driver (vfs/driver.rs) and the command sandbox
//! (overlay/confine.rs) are installed with. Linux only.

use std::path::PathBuf;

/// Where `name` is: on the PATH the app's programs get, or in the system's
/// bin folders a desktop launcher's PATH may leave out.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = crate::process::child_var("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(Into::into))
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// A package by the names the managers know it by.
pub struct Package {
    /// Its name nearly everywhere.
    pub name: &'static str,
    /// Gentoo's, with its category (`sys-fs/fuse:3`).
    pub gentoo: &'static str,
}

/// The shell script that installs `package` with the package manager found
/// here, unattended. None without a manager this knows.
pub fn install_script(package: &Package) -> Option<String> {
    let n = package.name;
    let managers = [
        (
            "apt-get",
            format!(
                "export DEBIAN_FRONTEND=noninteractive; apt-get install -y {n} || \
                 {{ apt-get update && apt-get install -y {n}; }}"
            ),
        ),
        ("dnf", format!("dnf install -y {n}")),
        ("yum", format!("yum install -y {n}")),
        ("pacman", format!("pacman -S --needed --noconfirm {n}")),
        ("zypper", format!("zypper --non-interactive install {n}")),
        ("apk", format!("apk add {n}")),
        ("xbps-install", format!("xbps-install -y {n}")),
        ("emerge", format!("emerge --noreplace {}", package.gentoo)),
    ];
    managers
        .into_iter()
        .find(|(manager, _)| which(manager).is_some())
        .map(|(_, script)| script)
}

/// What came of running a script as root.
pub enum RootRun {
    /// It ran and succeeded.
    Ran,
    /// It couldn't be run for the user: what they should run themselves.
    Manual { message: String, command: String },
}

/// Runs `script` as root: directly when this process is root, else through
/// polkit's dialog (`pkexec`) on a desktop. Without a way to ask, `Manual`
/// says the `sudo` command to run instead. `what` names the work for the
/// messages ("Installing FUSE"). Err: the dialog was dismissed, or the
/// script failed.
pub async fn run_as_root(script: &str, what: &str) -> Result<RootRun, String> {
    let is_root = unsafe { libc::geteuid() } == 0;
    let graphical =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let mut command = if is_root {
        let mut c = crate::process::command("sh");
        c.args(["-c", script]);
        c
    } else if which("pkexec").is_some() && graphical {
        let mut c = crate::process::command("pkexec");
        c.args(["sh", "-c", script]);
        c
    } else {
        return Ok(RootRun::Manual {
            message: format!("{what} needs administrator rights. Run this in a terminal:"),
            command: format!("sudo sh -c '{script}'"),
        });
    };
    let out = crate::rt::spawn_blocking(move || command.output())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("starting the installer: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(RootRun::Ran),
        // pkexec: the dialog was dismissed / not authorized.
        Some(126) | Some(127) if !is_root => Err("Installation was cancelled.".into()),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let last: Vec<&str> = stderr.lines().rev().take(5).collect();
            Err(format!(
                "{what} failed ({}): {}",
                out.status,
                last.into_iter().rev().collect::<Vec<_>>().join("\n")
            ))
        }
    }
}

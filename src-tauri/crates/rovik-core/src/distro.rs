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

/// The script, run as root, that lets bubblewrap make its sandbox where
/// AppArmor restricts unprivileged user namespaces (Ubuntu 23.10 and later):
/// it adds and loads the `rovik-bwrap` profile, unless the restriction is
/// off or another profile covers bwrap already. The .deb and .rpm run the
/// same file after installing (tauri.conf.json); the app's offer to set the
/// sandbox up runs it, and so does the cloud host installer.
pub const BWRAP_APPARMOR: &str = include_str!("../linux/bwrap-apparmor.sh");

/// Whether AppArmor keeps programs without a profile of their own from
/// making user namespaces here, as bubblewrap's sandbox needs.
pub fn userns_restricted() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|v| v.trim() == "1")
}

/// `text` as one word for `sh`, whatever it holds.
pub fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
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
            command: format!("sudo sh -c {}", sh_quote(script)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    const RESTRICTED: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
    const REMOVE: &str = include_str!("../linux/bwrap-apparmor-remove.sh");

    /// `script` with the system's paths under `root`, run with a fake
    /// `apparmor_parser` that logs its arguments to `root/parser.log`.
    fn run(root: &Path, script: &str, arg: &str) {
        let script = script
            .replace(RESTRICTED, &root.join("restrict").to_string_lossy())
            .replace(
                "/etc/apparmor.d",
                &root.join("apparmor.d").to_string_lossy(),
            );
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let parser = bin.join("apparmor_parser");
        let log = root.join("parser.log");
        std::fs::write(
            &parser,
            format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
        )
        .unwrap();
        Command::new("chmod")
            .arg("755")
            .arg(&parser)
            .status()
            .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("sh")
            .args(["-c", &script, "sh", arg])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fresh(test: &str, restricted: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apparmor.d")).unwrap();
        std::fs::write(root.join("restrict"), restricted).unwrap();
        root
    }

    fn parsed(root: &Path) -> String {
        std::fs::read_to_string(root.join("parser.log")).unwrap_or_default()
    }

    /// Where AppArmor restricts user namespaces, the profile goes in and is
    /// loaded, once however often the script runs; it gives bwrap `userns`.
    #[test]
    fn the_profile_lets_bwrap_make_its_namespace_where_apparmor_restricts_them() {
        let root = fresh("bwrap-aa", "1\n");
        run(&root, BWRAP_APPARMOR, "configure");
        run(&root, BWRAP_APPARMOR, "configure");
        let profile = root.join("apparmor.d/rovik-bwrap");
        let text = std::fs::read_to_string(&profile).unwrap();
        assert!(text.contains("profile rovik-bwrap /usr/bin/bwrap flags=(unconfined) {"));
        assert!(text.contains("  userns,\n"));
        assert!(text.contains("abi <abi/4.0>,"));
        let loaded = format!("-r {}\n", profile.display());
        assert_eq!(parsed(&root), loaded.repeat(2), "loaded each time");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing where the restriction is off, or another profile covers bwrap.
    #[test]
    fn nothing_where_its_not_needed_or_bwrap_has_a_profile() {
        let root = fresh("bwrap-aa-off", "0\n");
        run(&root, BWRAP_APPARMOR, "configure");
        assert!(!root.join("apparmor.d/rovik-bwrap").exists());
        let _ = std::fs::remove_dir_all(&root);

        let root = fresh("bwrap-aa-other", "1\n");
        std::fs::write(
            root.join("apparmor.d/bwrap-userns-restrict"),
            "abi <abi/4.0>,\nprofile bwrap /usr/bin/bwrap flags=(attach_disconnected) {\n}\n",
        )
        .unwrap();
        run(&root, BWRAP_APPARMOR, "configure");
        assert!(!root.join("apparmor.d/rovik-bwrap").exists());
        assert_eq!(parsed(&root), "");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Removing Rovik removes the profile; an upgrade keeps it.
    #[test]
    fn the_profile_goes_with_rovik_but_stays_through_an_upgrade() {
        let root = fresh("bwrap-aa-rm", "1\n");
        run(&root, BWRAP_APPARMOR, "configure");
        let profile = root.join("apparmor.d/rovik-bwrap");
        for upgrade in ["upgrade", "1"] {
            run(&root, REMOVE, upgrade);
            assert!(profile.exists(), "kept through {upgrade}");
        }
        run(&root, REMOVE, "remove");
        assert!(!profile.exists());
        assert!(parsed(&root).ends_with(&format!("-R {}\n", profile.display())));
        run(&root, BWRAP_APPARMOR, "configure");
        run(&root, REMOVE, "0");
        assert!(!profile.exists(), "the .rpm's last erase");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_quoted_script_reaches_sh_whole() {
        let script = "echo 'it'\"'\"'s' \"$HOME\" \\";
        let out = Command::new("sh")
            .args(["-c", &format!("printf %s {}", sh_quote(script))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), script);
    }
}

//! The browser on a cloud host (D19): the Chrome for Testing that
//! `rovik_core::pinned_browser` pins, in `/opt/rovik/browser/<version>`,
//! with the system packages it needs. `admin browser` puts it there when
//! it's missing. `install` runs it, and `admin upgrade` runs the new
//! build's, whose pin may be newer.
//!
//! The archive comes from Google, and nothing in it is used until it
//! matches the size and SHA-256 pinned in Rovik. It's unpacked as root into
//! a folder only root can change, with everything readable by all, since
//! the daemons run it as their users. The packages are the ones the
//! archive's `deb.deps` or `rpm.deps` lists, installed with apt-get, dnf or
//! zypper, before the folder takes the version's name: a version's folder
//! is there only once all of it is in place. A browser that doesn't work
//! anyway says so when the browser tools start it.

use super::{System, INSTALLED};
use rovik_core::pinned_browser::{self, Build, Leftover, NAME, SERVER_ROOT, VERSION};
use std::path::Path;

/// `admin browser`: the pinned browser installed, with its packages, when
/// it isn't, and the other versions gone.
pub(super) fn ensure(sys: &System) -> Result<(), String> {
    let cpu = std::env::consts::ARCH;
    let build =
        Build::for_system("linux", cpu).ok_or_else(|| format!("{NAME} has no build for {cpu}."))?;
    let root = sys.path(SERVER_ROOT);
    if root.join(VERSION).is_dir() {
        println!("{NAME} {VERSION} is installed.");
        return pinned_browser::prune(&root);
    }
    gnu_libc(sys)?;
    place(sys, build, &root)?;
    pinned_browser::prune(&root)?;
    println!(
        "{NAME} {VERSION} is installed for the browser tools: {}",
        build.executable_in(Path::new(SERVER_ROOT)).display()
    );
    Ok(())
}

/// `ensure`, for `install`: a browser that can't be installed is reported
/// and stops nothing else. The daemon runs without its browser tools.
pub(super) fn ensure_or_say(sys: &System) {
    if let Err(why) = ensure(sys) {
        println!("{}", not_installed(&why));
    }
}

/// What's said when the browser couldn't be installed.
pub(super) fn not_installed(why: &str) -> String {
    format!(
        "{NAME} {VERSION} isn't installed: {why}\nThe browser tools won't work until it \
         is. To try again: sudo {INSTALLED} admin browser"
    )
}

/// The archive downloaded, checked and unpacked in `root`, the packages it
/// needs installed, and then the version's folder named.
fn place(sys: &System, build: &Build, root: &Path) -> Result<(), String> {
    let dir = root.join(VERSION);
    if !sys.live() {
        println!(
            "would download {} ({} bytes, SHA-256 {}) and unpack it in {SERVER_ROOT}/{VERSION}",
            build.url(),
            build.size,
            build.sha256,
        );
        return packages(sys, &dir);
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    set_readable(root)?;
    let pid = std::process::id();
    let archive = Leftover(root.join(format!(".download-{pid}.zip")));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(pinned_browser::download(build, &archive.0, |_| {}))?;
    let unpacked = Leftover(root.join(format!(".unpack-{pid}")));
    pinned_browser::unpack(&archive.0, &unpacked.0, &build.folder())?;
    drop(archive);
    packages(sys, &unpacked.0)?;
    match std::fs::rename(&unpacked.0, &dir) {
        Ok(()) => Ok(()),
        // Another administrator's run put it there first.
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// The folder of versions, which the daemons' users must be able to enter.
fn set_readable(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", root.display()))
}

/// The packages the browser unpacked in `dir` needs, as its archive lists
/// them for the system's package manager.
fn packages(sys: &System, dir: &Path) -> Result<(), String> {
    let listed = |name: &str| {
        if !sys.live() {
            return Ok(format!("<{name}>"));
        }
        let path = dir.join(name);
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    match sys.which(&["apt-get", "dnf", "zypper"]) {
        Some("apt-get") => {
            let wanted = deb_dependencies(&listed("deb.deps")?);
            let apt = |args: &[&str]| {
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
            // A new machine has no package lists yet. When they can't be
            // updated, the ones there may still do.
            if let Err(why) = apt(&["update"]) {
                println!("The package lists weren't updated: {why}");
            }
            apt(&["-y", "--no-install-recommends", "satisfy", &wanted]).map(|_| ())
        }
        Some(manager) => {
            let wanted = rpm_capabilities(&listed("rpm.deps")?);
            let mut args = match manager {
                "dnf" => vec!["install", "-y", "--setopt=install_weak_deps=False"],
                _ => vec!["--non-interactive", "install", "--no-recommends"],
            };
            args.extend(wanted.iter().map(String::as_str));
            sys.run(manager, &args).map(|_| ())
        }
        None => {
            println!(
                "There's no apt-get, dnf or zypper here to install the libraries {NAME} \
                 needs: its rpm.deps names them."
            );
            Ok(())
        }
    }
}

/// `deb.deps`, a dependency a line, as `apt-get satisfy` takes them.
fn deb_dependencies(listed: &str) -> String {
    let lines: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.join(", ")
}

/// `rpm.deps`, a capability a line, as dnf and zypper install them: what
/// rpm itself provides left out, and the first of each choice taken.
fn rpm_capabilities(listed: &str) -> Vec<String> {
    listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("rpmlib(") && !line.starts_with("rtld("))
        .map(|line| {
            let choices = line
                .strip_prefix('(')
                .and_then(|line| line.strip_suffix(')'));
            match choices {
                Some(choices) => choices.split(" or ").next().unwrap_or(choices).trim(),
                None => line,
            }
            .to_string()
        })
        .collect()
}

/// Chrome for Testing is built for the GNU C library, which systems built
/// on musl, such as Alpine, don't have.
fn gnu_libc(sys: &System) -> Result<(), String> {
    if !sys.live() {
        return Ok(());
    }
    let loader = match std::env::consts::ARCH {
        "aarch64" => "/lib/ld-linux-aarch64.so.1",
        _ => "/lib64/ld-linux-x86-64.so.2",
    };
    if Path::new(loader).exists() {
        return Ok(());
    }
    Err(format!(
        "{NAME} needs the GNU C library, which this system doesn't have ({loader} is \
         missing). Install its own Chromium package instead: the browser tools use it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_takes_the_debian_dependencies_as_one_list() {
        let listed = "ca-certificates\nlibasound2 (>= 1.0.17)\n\
                      libgtk-3-0 (>= 3.9.10) | libgtk-4-1\n\n";
        assert_eq!(
            deb_dependencies(listed),
            "ca-certificates, libasound2 (>= 1.0.17), libgtk-3-0 (>= 3.9.10) | libgtk-4-1"
        );
    }

    #[test]
    fn dnf_and_zypper_take_capabilities_rpm_does_not_provide_itself() {
        let listed = "(libcurl.so()(64bit) or libcurl.so.4()(64bit))\n\
                      (libgtk-3.so.0()(64bit) or libgtk-4.so.1()(64bit))\n\
                      libnss3.so(NSS_3.22)(64bit)\nliberation-fonts\n\
                      rpmlib(FileDigests) <= 4.6.0-1\nrtld(GNU_HASH)\n";
        assert_eq!(
            rpm_capabilities(listed),
            [
                "libcurl.so()(64bit)",
                "libgtk-3.so.0()(64bit)",
                "libnss3.so(NSS_3.22)(64bit)",
                "liberation-fonts"
            ]
        );
    }
}

//! Native Docker installation from signed system packages or verified vendor installers.
use super::{process, DockerProgress};
use std::path::{Path, PathBuf};

pub(super) fn linux_packages(release: &str) -> Option<&'static str> {
    let id = release
        .lines()
        .find_map(|line| line.strip_prefix("ID="))?
        .trim()
        .trim_matches('"');
    match id {
        "debian" | "ubuntu" | "linuxmint" | "pop" => {
            Some("/usr/bin/apt-get update\n/usr/bin/apt-get install -y docker.io")
        }
        "fedora" => Some("/usr/bin/dnf install -y moby-engine docker-cli"),
        "arch" | "manjaro" => Some("/usr/bin/pacman -S --needed --noconfirm docker"),
        "opensuse-leap" | "opensuse-tumbleweed" => {
            Some("/usr/bin/zypper --non-interactive install docker")
        }
        "alpine" => Some("/sbin/apk add docker"),
        _ => None,
    }
}

pub(super) fn supported() -> bool {
    match std::env::consts::OS {
        "linux" => {
            std::fs::read_to_string("/etc/os-release")
                .ok()
                .as_deref()
                .and_then(linux_packages)
                .is_some()
                && (is_root() || rovik_core::process::find_on_path("pkexec").is_some())
        }
        "macos" | "windows" => matches!(std::env::consts::ARCH, "x86_64" | "aarch64"),
        _ => false,
    }
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

pub(super) fn desktop_present() -> bool {
    match std::env::consts::OS {
        "macos" => Path::new("/Applications/Docker.app").is_dir(),
        "windows" => windows_home().join("Docker Desktop.exe").is_file(),
        _ => false,
    }
}

pub(super) fn candidates() -> Vec<PathBuf> {
    let mut paths: Vec<_> = rovik_core::process::find_on_path(if cfg!(windows) {
        "docker.exe"
    } else {
        "docker"
    })
    .filter(|p| p.is_absolute())
    .into_iter()
    .collect();
    paths.extend(match std::env::consts::OS {
        "macos" => vec![
            PathBuf::from("/Applications/Docker.app/Contents/Resources/bin/docker"),
            PathBuf::from("/usr/local/bin/docker"),
        ],
        "windows" => vec![windows_home().join("resources/bin/docker.exe")],
        _ => vec![
            PathBuf::from("/usr/bin/docker"),
            PathBuf::from("/usr/local/bin/docker"),
        ],
    });
    paths
}

fn windows_home() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| "C:\\Program Files".into())
        .join("Docker/Docker")
}

pub(super) fn endpoints() -> Vec<String> {
    if cfg!(windows) {
        return vec!["npipe:////./pipe/docker_engine".into()];
    }
    let mut endpoints = vec!["unix:///var/run/docker.sock".into()];
    if cfg!(target_os = "macos") {
        if let Some(home) = dirs::home_dir() {
            endpoints.push(format!(
                "unix://{}",
                home.join(".docker/run/docker.sock").display()
            ));
        }
    }
    endpoints
}

pub(super) async fn install(
    data: &Path,
    progress: impl Fn(DockerProgress) + Send + Sync,
) -> Result<(), String> {
    if cfg!(target_os = "linux") {
        let release = std::fs::read_to_string("/etc/os-release").map_err(|e| e.to_string())?;
        let packages = linux_packages(&release).ok_or("Automatic Docker installation is unavailable for this Linux distribution. Install Docker Engine using its distribution instructions.")?;
        progress(DockerProgress::stage(
            "installing",
            "Installing Docker Engine. Your system may ask for administrator approval.",
        ));
        let script = linux_script(packages);
        #[cfg(unix)]
        let uid = unsafe { libc::getuid() }.to_string();
        #[cfg(not(unix))]
        let uid = String::new();
        let args = vec!["-c".into(), script, "rovik-docker-setup".into(), uid];
        if is_root() {
            process::checked(Path::new("/bin/sh"), &args).await?;
        } else {
            let pkexec = rovik_core::process::find_on_path("pkexec").ok_or("Administrator approval is unavailable. Install Docker Engine manually or install your desktop's PolicyKit agent.")?;
            let mut elevated = vec!["/bin/sh".into()];
            elevated.extend(args);
            process::checked(&pkexec, &elevated).await?;
        }
        return Ok(());
    }
    let build = super::download::Build::for_platform(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or("Docker Desktop has no supported installer for this platform")?;
    let staging = tempfile::tempdir_in(data).map_err(|e| e.to_string())?;
    let installer = staging.path().join(build.filename);
    super::download::download(&build, &installer, &progress).await?;
    progress(DockerProgress::stage(
        "installing",
        "Installing Docker Desktop. Complete any system approval or Docker setup prompts.",
    ));
    if cfg!(target_os = "macos") {
        install_macos(&installer, staging.path()).await
    } else {
        install_windows(&installer).await
    }
}

fn linux_script(packages: &str) -> String {
    // Every command is fixed by our distribution table. The UID is an argv
    // value from the backend process, never a name or command from a project.
    format!("set -eu\nexport PATH=/usr/sbin:/usr/bin:/sbin:/bin\nexport DEBIAN_FRONTEND=noninteractive\n{packages}\nif command -v systemctl >/dev/null 2>&1; then systemctl enable --now docker; elif command -v rc-service >/dev/null 2>&1; then rc-update add docker default; rc-service docker start; else echo 'Docker is installed. Start its service using your system service manager.'; fi\nif test \"$1\" != 0; then name=$(id -nu \"$1\"); if command -v usermod >/dev/null 2>&1; then usermod -a -G docker -- \"$name\"; else addgroup \"$name\" docker; fi; fi\n")
}

async fn install_macos(installer: &Path, staging: &Path) -> Result<(), String> {
    let mount = staging.join("mounted");
    std::fs::create_dir(&mount).map_err(|e| e.to_string())?;
    process::checked(
        Path::new("/usr/bin/hdiutil"),
        &[
            "attach".into(),
            "-nobrowse".into(),
            "-readonly".into(),
            "-mountpoint".into(),
            mount.to_string_lossy().into(),
            installer.to_string_lossy().into(),
        ],
    )
    .await?;
    let helper = mount.join("Docker.app/Contents/MacOS/install");
    let script = "on run argv\ndo shell script (quoted form of (item 1 of argv)) with administrator privileges\nend run";
    let result = process::checked(
        Path::new("/usr/bin/osascript"),
        &["-e".into(), script.into(), helper.to_string_lossy().into()],
    )
    .await;
    let detached = process::checked(
        Path::new("/usr/bin/hdiutil"),
        &["detach".into(), mount.to_string_lossy().into()],
    )
    .await;
    result?;
    detached?;
    process::checked(
        Path::new("/usr/bin/open"),
        &["-a".into(), "/Applications/Docker.app".into()],
    )
    .await
}

async fn install_windows(installer: &Path) -> Result<(), String> {
    let powershell = rovik_core::process::find_on_path("powershell.exe")
        .ok_or("PowerShell is required to start the Docker Desktop installer")?;
    let path = installer.to_string_lossy().replace('\'', "''");
    // Do not accept vendor agreements on the user's behalf. The official
    // interactive installer handles UAC, prerequisites and its first-use terms.
    let script = format!("$ErrorActionPreference='Stop'; $p = Start-Process -FilePath '{path}' -ArgumentList 'install' -Verb RunAs -Wait -PassThru; exit $p.ExitCode");
    process::checked(
        &powershell,
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script,
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_distribution_packages_are_elevated_and_the_user_is_an_argument() {
        assert!(linux_packages("ID=ubuntu\nID_LIKE=debian")
            .unwrap()
            .contains("docker.io"));
        assert!(linux_packages("ID=\"fedora\"")
            .unwrap()
            .contains("moby-engine"));
        for release in [
            "ID=unknown",
            "ID=ubuntu;touch /tmp/bad",
            "ID_LIKE=debian",
            "",
        ] {
            assert!(linux_packages(release).is_none());
        }
        let script = linux_script(linux_packages("ID=debian").unwrap());
        assert!(script.contains("id -nu \"$1\""));
        assert!(script.contains("usermod -a -G docker -- \"$name\""));
        assert!(!script.contains("curl"));
        assert!(!script.contains("chmod 666"));
    }
}

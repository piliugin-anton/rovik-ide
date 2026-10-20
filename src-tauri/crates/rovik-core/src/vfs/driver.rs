//! Finding and installing the driver the virtual filesystem needs:
//! FUSE (Linux), macFUSE (macOS) or WinFsp (Windows).
//!
//! Installing always needs the administrator's consent, asked the way the OS
//! normally does: polkit's dialog (`pkexec`) on Linux, the admin password
//! dialog on macOS, UAC on Windows. Without a way to ask (e.g. no polkit
//! agent), the result says which command to run instead.
//!
//! The macFUSE and WinFsp installers come from their official GitHub
//! releases. A download is checked against the SHA-256 GitHub publishes for
//! it when there is one, and its code signature is checked before it runs.

use std::path::Path;
#[cfg(any(target_os = "macos", windows))]
use std::process::Command;

use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriverStatus {
    /// "FUSE", "macFUSE" or "WinFsp".
    pub driver: &'static str,
    pub installed: bool,
    pub version: Option<String>,
    /// What's missing or wrong, for the user.
    pub detail: Option<String>,
    /// The app can install it (otherwise `detail` says how).
    pub can_install: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum InstallOutcome {
    /// Ready to mount.
    Installed { version: Option<String> },
    /// Installed; the OS must restart before it can be used.
    NeedsRestart { message: String },
    /// The app couldn't install it; the user has to.
    Manual {
        message: String,
        command: Option<String>,
    },
}

// --- Linux ------------------------------------------------------------------

#[cfg(target_os = "linux")]
use crate::distro::which;

#[cfg(target_os = "linux")]
pub fn status() -> DriverStatus {
    let helper = which("fusermount3").or_else(|| which("fusermount"));
    let device = Path::new("/dev/fuse").exists();
    let version = helper.as_ref().and_then(|h| {
        let out = crate::process::command(h).arg("-V").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        text.lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
    });
    let detail = match (helper.is_some(), device) {
        (true, true) => None,
        (false, _) => Some("The FUSE tools (fusermount3) aren't installed.".to_string()),
        (true, false) => {
            Some("The FUSE kernel module isn't loaded (/dev/fuse is missing).".to_string())
        }
    };
    DriverStatus {
        driver: "FUSE",
        installed: detail.is_none(),
        version,
        detail,
        can_install: linux_install_script().is_some(),
    }
}

/// The shell script that installs FUSE 3 with this distribution's package
/// manager and loads the kernel module.
#[cfg(target_os = "linux")]
fn linux_install_script() -> Option<String> {
    const FUSE3: crate::distro::Package = crate::distro::Package {
        name: "fuse3",
        gentoo: "sys-fs/fuse:3",
    };
    let install = crate::distro::install_script(&FUSE3)?;
    Some(format!(
        "set -e; {install}; modprobe fuse 2>/dev/null || true"
    ))
}

#[cfg(target_os = "linux")]
pub async fn install() -> Result<InstallOutcome, String> {
    let Some(script) = linux_install_script() else {
        return Ok(InstallOutcome::Manual {
            message: "No supported package manager was found. Install FUSE 3 (the package is usually called fuse3) and make sure /dev/fuse exists.".into(),
            command: None,
        });
    };
    if let crate::distro::RootRun::Manual { message, command } =
        crate::distro::run_as_root(&script, "Installing FUSE").await?
    {
        return Ok(InstallOutcome::Manual {
            message,
            command: Some(command),
        });
    }
    let now = status();
    if now.installed {
        Ok(InstallOutcome::Installed {
            version: now.version,
        })
    } else {
        Ok(InstallOutcome::NeedsRestart {
            message: now.detail.unwrap_or_default() + " Restart the computer to load it.",
        })
    }
}

// --- Shared by the macOS and Windows installers -----------------------------

/// File names linked under `prefix` in a release's asset list.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn asset_names(html: &str, prefix: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(prefix) {
        rest = &rest[at + prefix.len()..];
        let end = rest.find('"').unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.is_empty()
            && !name.contains(['/', '\\', '<', '>'])
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_string());
        }
    }
    names
}

#[cfg(any(target_os = "macos", windows))]
mod download {
    use super::*;
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};

    const MAX_BYTES: u64 = 200 * 1024 * 1024;

    /// Settings → Network's timeout and proxy (`net`).
    pub async fn client(redirects: bool) -> Result<reqwest::Client, String> {
        let mut builder = crate::net::builder()
            .await
            .user_agent(concat!("rovik-ide/", env!("CARGO_PKG_VERSION")));
        if !redirects {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }
        builder.build().map_err(|e| e.to_string())
    }

    pub struct Asset {
        pub name: String,
        pub url: String,
        pub sha256: Option<String>,
    }

    /// The latest release's file whose name `matches`, read from github.com's
    /// release pages (the REST API's anonymous limit is too low for shared
    /// networks).
    pub async fn latest_asset(repo: &str, matches: impl Fn(&str) -> bool) -> Result<Asset, String> {
        let response = client(false)
            .await?
            .get(format!("https://github.com/{repo}/releases/latest"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|l| l.to_str().ok())
            .ok_or_else(|| format!("unexpected answer from GitHub ({})", response.status()))?;
        let tag = location
            .rsplit("/tag/")
            .next()
            .filter(|t| !t.is_empty() && !t.contains('/'))
            .ok_or("no release tag")?;
        let html = client(true)
            .await?
            .get(format!(
                "https://github.com/{repo}/releases/expanded_assets/{tag}"
            ))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        let prefix = format!("/{repo}/releases/download/{tag}/");
        let name = super::asset_names(&html, &prefix)
            .into_iter()
            .find(|n| matches(n))
            .ok_or_else(|| format!("no installer in release {tag}"))?;
        let sha256 = crate::release_assets::parse_assets(&html, &name).map(|(_, digest)| digest);
        Ok(Asset {
            url: format!("https://github.com{prefix}{name}"),
            name,
            sha256,
        })
    }

    /// Downloads into `dir`, checking the checksum when there is one.
    pub async fn fetch(asset: &Asset, dir: &Path) -> Result<std::path::PathBuf, String> {
        let response = client(true)
            .await?
            .get(&asset.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?;
        if response.content_length().is_some_and(|l| l > MAX_BYTES) {
            return Err("download too large".into());
        }
        let mut hasher = Sha256::new();
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
            hasher.update(&chunk);
            data.extend_from_slice(&chunk);
            if data.len() as u64 > MAX_BYTES {
                return Err("download too large".into());
            }
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if let Some(expected) = &asset.sha256 {
            if &digest != expected {
                return Err(format!("checksum mismatch for {}: expected {expected}, got {digest}. Nothing was installed.", asset.name));
            }
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join(&asset.name);
        std::fs::write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

// --- macOS ------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn macfuse_version() -> Option<String> {
    let plist =
        std::fs::read_to_string(Path::new(super::macfuse::BUNDLE).join("Contents/version.plist"))
            .or_else(|_| {
                std::fs::read_to_string(
                    Path::new(super::macfuse::BUNDLE).join("Contents/Info.plist"),
                )
            })
            .ok()?;
    let key = plist.find("<key>CFBundleVersion</key>")?;
    let rest = &plist[key..];
    let start = rest.find("<string>")? + 8;
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].trim().to_string())
}

#[cfg(target_os = "macos")]
pub fn status() -> DriverStatus {
    let installed = Path::new(super::macfuse::BUNDLE)
        .join("Contents/Resources/mount_macfuse")
        .exists();
    DriverStatus {
        driver: "macFUSE",
        installed,
        version: if installed { macfuse_version() } else { None },
        detail: (!installed).then(|| "macFUSE isn't installed.".to_string()),
        can_install: true,
    }
}

#[cfg(target_os = "macos")]
fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(target_os = "macos")]
pub async fn install() -> Result<InstallOutcome, String> {
    let asset = download::latest_asset("macfuse/macfuse", |n| {
        n.starts_with("macfuse-") && n.ends_with(".dmg")
    })
    .await?;
    let dir = std::env::temp_dir().join(format!("rovik-macfuse-{}", std::process::id()));
    let dmg = download::fetch(&asset, &dir).await?;
    let result = crate::rt::spawn_blocking(move || -> Result<InstallOutcome, String> {
        let mnt = dir.join("mnt");
        std::fs::create_dir_all(&mnt).map_err(|e| e.to_string())?;
        let (dmg_s, mnt_s) = (dmg.to_string_lossy().into_owned(), mnt.to_string_lossy().into_owned());
        run("/usr/bin/hdiutil", &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", &mnt_s, &dmg_s])?;
        let outcome = (|| {
            let pkg = std::fs::read_dir(&mnt)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "pkg"))
                .ok_or("no installer package in the macFUSE disk image")?;
            let pkg_s = pkg.to_string_lossy().into_owned();
            // Only run a package signed by macFUSE's developer.
            let signature = run("/usr/sbin/pkgutil", &["--check-signature", &pkg_s])?;
            if !signature.contains("Developer ID Installer: Benjamin Fleischer") {
                return Err("the macFUSE installer isn't signed by its developer; not installing it".to_string());
            }
            // The system's admin password dialog.
            let out = Command::new("/usr/bin/osascript")
                .args([
                    "-e",
                    "on run argv",
                    "-e",
                    "do shell script \"/usr/sbin/installer -pkg \" & quoted form of (item 1 of argv) & \" -target /\" with administrator privileges",
                    "-e",
                    "end run",
                    &pkg_s,
                ])
                .output()
                .map_err(|e| format!("osascript: {e}"))?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stderr.contains("-128") {
                    return Err("Installation was cancelled.".to_string());
                }
                return Err(format!("installing macFUSE failed: {}", stderr.trim()));
            }
            // What macOS still needs (switching the extension on, or
            // allowing it) comes from the first mount attempt.
            Ok(InstallOutcome::Installed { version: macfuse_version() })
        })();
        let _ = run("/usr/bin/hdiutil", &["detach", "-force", &mnt_s]);
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    })
    .await
    .map_err(|e| e.to_string())?;
    result
}

// --- Windows ----------------------------------------------------------------

#[cfg(windows)]
const WINFSP_DLL: &str = if cfg!(target_arch = "aarch64") {
    "winfsp-a64.dll"
} else if cfg!(target_arch = "x86") {
    "winfsp-x86.dll"
} else {
    "winfsp-x64.dll"
};

/// WinFsp's install folder, from the registry.
#[cfg(windows)]
fn winfsp_dir() -> Option<std::path::PathBuf> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let read = |key: PCWSTR| -> Option<std::path::PathBuf> {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key,
                w!("InstallDir"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]).into())
    };
    read(w!("SOFTWARE\\WOW6432Node\\WinFsp")).or_else(|| read(w!("SOFTWARE\\WinFsp")))
}

/// Loads the installed winfsp DLL, so the delay-loaded imports resolve to it.
#[cfg(windows)]
pub fn load_winfsp() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    let dll = winfsp_dir()
        .map(|d| d.join("bin").join(WINFSP_DLL))
        .filter(|p| p.is_file())
        .ok_or("WinFsp isn't installed")?;
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .map(|_| ())
        .map_err(|e| format!("loading {}: {e}", dll.display()))
}

#[cfg(windows)]
fn winfsp_version(dir: &Path) -> Option<String> {
    // Not stored anywhere simple; the installer's product name carries it.
    let out = crate::process::no_window(&mut Command::new("reg"))
        .args([
            "query",
            r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            "/s",
            "/f",
            "WinFsp",
            "/d",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text
        .lines()
        .find(|l| l.trim_start().starts_with("DisplayVersion"))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_string);
    let _ = dir;
    version
}

#[cfg(windows)]
pub fn status() -> DriverStatus {
    let dir = winfsp_dir();
    let installed = dir
        .as_ref()
        .is_some_and(|d| d.join("bin").join(WINFSP_DLL).is_file());
    DriverStatus {
        driver: "WinFsp",
        installed,
        version: dir
            .as_deref()
            .filter(|_| installed)
            .and_then(winfsp_version),
        detail: (!installed).then(|| "WinFsp isn't installed.".to_string()),
        can_install: true,
    }
}

#[cfg(windows)]
pub async fn install() -> Result<InstallOutcome, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let asset = download::latest_asset("winfsp/winfsp", |n| {
        n.starts_with("winfsp-") && n.ends_with(".msi")
    })
    .await?;
    let dir = std::env::temp_dir().join(format!("rovik-winfsp-{}", std::process::id()));
    let msi = download::fetch(&asset, &dir).await?;
    crate::rt::spawn_blocking(move || {
        let msi_s = msi.to_string_lossy().into_owned();
        // Only run an installer signed by WinFsp's publisher.
        let script = "$s = Get-AuthenticodeSignature -LiteralPath $args[0]; \
                      if ($s.Status -ne 'Valid') { exit 2 }; $s.SignerCertificate.Subject";
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script, &msi_s])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("checking the installer's signature: {e}"))?;
        let subject = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !subject.contains("Navimatics") {
            let _ = std::fs::remove_dir_all(&dir);
            return Err("the WinFsp installer doesn't carry a valid signature from its publisher; not installing it".to_string());
        }
        // msiexec asks for elevation (UAC) itself.
        let status = Command::new("msiexec")
            .args(["/i", &msi_s, "/passive", "/norestart"])
            .status()
            .map_err(|e| format!("msiexec: {e}"));
        let _ = std::fs::remove_dir_all(&dir);
        match status?.code() {
            Some(0) => Ok(InstallOutcome::Installed { version: winfsp_dir().as_deref().and_then(winfsp_version) }),
            Some(3010) | Some(1641) => Ok(InstallOutcome::NeedsRestart {
                message: "WinFsp is installed. Restart Windows to finish.".into(),
            }),
            Some(1602) => Err("Installation was cancelled.".into()),
            code => Err(format!("installing WinFsp failed (msiexec exit code {code:?})")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- Other systems ------------------------------------------------------------

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn status() -> DriverStatus {
    DriverStatus {
        driver: "FUSE",
        installed: false,
        version: None,
        detail: Some("Virtual files aren't supported on this system.".into()),
        can_install: false,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub async fn install() -> Result<InstallOutcome, String> {
    Err("Virtual files aren't supported on this system.".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_asset_names() {
        let html = r#"<li><a href="/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi">x</a> sha256:ab</li>
            <li><a href="/winfsp/winfsp/releases/download/v2.1/winfsp-tests-2.1.25156.zip">y</a></li>
            <li><a href="/winfsp/winfsp/archive/refs/tags/v2.1.zip">src</a></li>"#;
        let names = super::asset_names(html, "/winfsp/winfsp/releases/download/v2.1/");
        assert_eq!(
            names,
            ["winfsp-2.1.25156.msi", "winfsp-tests-2.1.25156.zip"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn status_is_consistent() {
        let status = super::status();
        assert_eq!(status.installed, status.detail.is_none());
    }
}

//! Finding, installing and updating rust-analyzer.
//!
//! The app keeps its own copy, downloaded on demand from the official
//! GitHub releases into the app's data folder: current releases work with
//! current and older toolchains, unlike a toolchain's own component. Each
//! version lives in its own folder (`<data>/language-servers/rust-analyzer/
//! <tag>/`), so installing never overwrites a copy that's running (Windows
//! can't), and `current.json` says which one to use.
//!
//! Release info comes from github.com's release pages, not the REST API:
//! the API allows 60 anonymous requests an hour per IP, which a shared
//! office network uses up. Every download is checked against the SHA-256
//! GitHub publishes for it before it's unpacked or run.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::util::{blocking, ErrText};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};

use super::tool_path;

const REPO: &str = "https://github.com/rust-lang/rust-analyzer";
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const EXE: &str = if cfg!(windows) {
    "rust-analyzer.exe"
} else {
    "rust-analyzer"
};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RaRelease {
    /// The release's tag, a date ("2026-09-28").
    pub tag: String,
    pub asset: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RaBinary {
    pub path: String,
    /// What `--version` printed, e.g. "rust-analyzer 0.3.3065-standalone".
    pub version: String,
    /// "managed" (the app's copy), "system" (PATH, rustup) or "custom".
    pub source: String,
    /// The release tag, for the managed copy.
    pub tag: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Current {
    tag: String,
}

fn managed_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .err_text()?
        .join("language-servers")
        .join("rust-analyzer"))
}

/// `<binary> --version`, if it runs. rustup's proxy for a component that
/// isn't installed exists on PATH but fails here, which is what we want.
fn version_of(path: &Path) -> Option<String> {
    let mut child = rovik_core::process::no_window(&mut rovik_core::process::command(path))
        .arg("--version")
        .env("PATH", tool_path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                let line = out.lines().next()?.trim().to_string();
                return (!line.is_empty()).then_some(line);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    // Hung, or lost: not one to use. Reaped, not left a zombie.
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// `path` as a rust-analyzer, if it runs as one.
fn binary(path: PathBuf, source: &str, tag: Option<String>) -> Option<RaBinary> {
    let version = version_of(&path)?;
    Some(RaBinary {
        path: path.to_string_lossy().into_owned(),
        version,
        source: source.into(),
        tag,
    })
}

fn managed_binary(app: &AppHandle) -> Option<RaBinary> {
    let dir = managed_dir(app).ok()?;
    let current: Current =
        serde_json::from_str(&fs::read_to_string(dir.join("current.json")).ok()?).ok()?;
    binary(
        dir.join(&current.tag).join(EXE),
        "managed",
        Some(current.tag),
    )
}

fn system_binary() -> Option<RaBinary> {
    std::env::split_paths(&tool_path())
        .map(|dir| dir.join(EXE))
        .filter(|candidate| candidate.is_file())
        .find_map(|candidate| binary(candidate, "system", None))
}

/// Old versions left from earlier updates. One that's still running (Windows)
/// can't be deleted yet; that's fine, the next look tries again. Not while
/// another Rovik window runs: its server may run an older one, which its
/// Restart starts again.
fn prune(app: &AppHandle, keep: &str) {
    if !rovik_core::os_lock::alone() {
        return;
    }
    let Ok(dir) = managed_dir(app) else { return };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && name != keep && !name.starts_with('.') {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The rust-analyzer to use: a custom path if set, else the app's copy or
/// the system's (in the order `prefer_system` says). None: nothing usable.
#[tauri::command]
pub async fn ra_find(
    app: AppHandle,
    custom_path: Option<String>,
    prefer_system: bool,
) -> Result<Option<RaBinary>, String> {
    blocking(move || {
        if let Some(custom) = custom_path.filter(|p| !p.trim().is_empty()) {
            let path = PathBuf::from(custom.trim());
            return binary(path.clone(), "custom", None)
                .map(Some)
                .ok_or_else(|| format!("{}: doesn't run as rust-analyzer", path.display()));
        }
        let managed = managed_binary(&app);
        if let Some(tag) = managed.as_ref().and_then(|m| m.tag.clone()) {
            prune(&app, &tag);
        }
        Ok(if prefer_system {
            system_binary().or(managed)
        } else {
            managed.or_else(system_binary)
        })
    })
    .await
}

/// The release file for this platform. Linux uses the glibc build unless the
/// app itself is a musl build: the musl one isn't static, it needs musl's
/// loader, which ordinary distributions don't have.
fn asset_name() -> Result<String, String> {
    let arch = std::env::consts::ARCH;
    let target = match (std::env::consts::OS, arch) {
        ("linux", "x86_64") if cfg!(target_env = "musl") => "x86_64-unknown-linux-musl.gz",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu.gz",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu.gz",
        ("linux", "arm") => "arm-unknown-linux-gnueabihf.gz",
        ("macos", "x86_64") => "x86_64-apple-darwin.gz",
        ("macos", "aarch64") => "aarch64-apple-darwin.gz",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc.zip",
        ("windows", "x86") => "i686-pc-windows-msvc.zip",
        (os, arch) => return Err(format!("no rust-analyzer build for {os}/{arch}")),
    };
    Ok(format!("rust-analyzer-{target}"))
}

/// Settings → Network's timeout and proxy (`rovik_core::net`). Without
/// `redirects`, a redirect is the answer (the latest release is one).
async fn http(redirects: bool) -> Result<reqwest::Client, String> {
    let mut builder = rovik_core::net::builder()
        .await
        .user_agent(concat!("rovik-ide/", env!("CARGO_PKG_VERSION")));
    if !redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    builder.build().err_text()
}

/// The latest release's file for this platform, with its checksum.
#[tauri::command]
pub async fn ra_latest_release() -> Result<RaRelease, String> {
    let asset = asset_name()?;
    // /releases/latest redirects to /releases/tag/<tag>.
    let response = http(false)
        .await?
        .get(format!("{REPO}/releases/latest"))
        .send()
        .await
        .err_text()?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .ok_or_else(|| format!("unexpected answer from GitHub ({})", response.status()))?;
    let tag = location
        .rsplit("/tag/")
        .next()
        .filter(|t| !t.is_empty() && !t.contains('/'))
        .ok_or("no release tag")?
        .to_string();

    let html = http(true)
        .await?
        .get(format!("{REPO}/releases/expanded_assets/{tag}"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .err_text()?
        .text()
        .await
        .err_text()?;
    let (href, sha256) = rovik_core::release_assets::parse_assets(&html, &asset)
        .ok_or_else(|| format!("{asset} or its checksum isn't in release {tag}"))?;
    if !href.starts_with("/rust-lang/rust-analyzer/releases/download/") {
        return Err(format!("unexpected download link {href}"));
    }
    Ok(RaRelease {
        tag,
        asset,
        url: format!("https://github.com{href}"),
        sha256,
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// Downloads `release`, checks its SHA-256, unpacks it into its own folder,
/// makes sure it runs, then makes it the current copy. A failure at any
/// point leaves the previous copy in place.
#[tauri::command]
pub async fn ra_install(
    app: AppHandle,
    release: RaRelease,
    on_progress: Channel<RaProgress>,
) -> Result<RaBinary, String> {
    if !release
        .url
        .starts_with("https://github.com/rust-lang/rust-analyzer/releases/download/")
    {
        return Err("not a rust-analyzer release download".into());
    }
    if release.tag.is_empty() || release.tag.contains(['/', '\\', '.']) {
        return Err(format!("bad release tag {}", release.tag));
    }
    let dir = managed_dir(&app)?;
    fs::create_dir_all(&dir).err_text()?;

    // Download, hashing as it arrives.
    let response = http(true)
        .await?
        .get(&release.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .err_text()?;
    let total = response.content_length();
    if total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES) {
        return Err("download too large".into());
    }
    let mut hasher = Sha256::new();
    let mut data: Vec<u8> = Vec::with_capacity(total.unwrap_or(16 << 20) as usize);
    let mut stream = response.bytes_stream();
    let mut last_report = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
        hasher.update(&chunk);
        data.extend_from_slice(&chunk);
        if data.len() as u64 > MAX_DOWNLOAD_BYTES {
            return Err("download too large".into());
        }
        if data.len() as u64 - last_report >= 256 * 1024 {
            last_report = data.len() as u64;
            let _ = on_progress.send(RaProgress {
                downloaded: last_report,
                total,
            });
        }
    }
    let _ = on_progress.send(RaProgress {
        downloaded: data.len() as u64,
        total,
    });
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != release.sha256 {
        return Err(format!(
            "checksum mismatch: expected {}, got {digest}. Nothing was installed.",
            release.sha256
        ));
    }

    // Unpack into a staging folder, then move it into place.
    let RaRelease { tag, asset, .. } = release;
    blocking(move || {
        // This process's own: another window may install at the same time.
        let staging = dir.join(format!(".staging-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).err_text()?;
        let exe_path = staging.join(EXE);
        let binary = unpack(&asset, &data)?;
        let mut file = fs::File::create(&exe_path).err_text()?;
        file.write_all(&binary).err_text()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755)).err_text()?;
        }
        let version = version_of(&exe_path)
            .ok_or("the downloaded rust-analyzer doesn't run on this system")?;

        let final_dir = dir.join(&tag);
        if version_of(&final_dir.join(EXE)).is_some() {
            // Installed meanwhile (another window), and maybe running: kept.
            let _ = fs::remove_dir_all(&staging);
        } else {
            let _ = fs::remove_dir_all(&final_dir);
            fs::rename(&staging, &final_dir).err_text()?;
        }
        let current = serde_json::to_string(&Current { tag: tag.clone() }).err_text()?;
        rovik_core::atomic_file::write_atomic(&dir.join("current.json"), current.as_bytes())
            .err_text()?;
        Ok(RaBinary {
            path: final_dir.join(EXE).to_string_lossy().into_owned(),
            version,
            source: "managed".into(),
            tag: Some(tag),
        })
    })
    .await
}

/// The executable out of a release file (.gz: the binary itself; .zip: the
/// rust-analyzer executable inside).
pub fn unpack(asset: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    if asset.ends_with(".gz") {
        flate2::read::GzDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(|e| format!("unpacking: {e}"))?;
    } else if asset.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))
            .map_err(|e| format!("unpacking: {e}"))?;
        let mut entry = (0..archive.len())
            .find(|&i| {
                archive.by_index(i).is_ok_and(|f| {
                    f.name()
                        .is_ok_and(|f| f.rsplit('/').next() == Some("rust-analyzer.exe"))
                })
            })
            .ok_or("rust-analyzer.exe isn't in the archive")
            .and_then(|i| archive.by_index(i).map_err(|_| "unreadable archive entry"))?;
        entry
            .read_to_end(&mut out)
            .map_err(|e| format!("unpacking: {e}"))?;
    } else {
        return Err(format!("unknown file type: {asset}"));
    }
    if out.len() < 1024 * 1024 {
        return Err("unpacked file is too small to be rust-analyzer".into());
    }
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustSrcStatus {
    /// The standard library's sources are there (hover and go-to into std work).
    pub installed: bool,
    /// rustup is available to install them.
    pub rustup: bool,
}

/// `program args`, run in `root` with the tools' PATH: its output when it
/// succeeded.
fn run_in(root: &str, program: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    rovik_core::process::no_window(&mut rovik_core::process::command(program))
        .args(args)
        .current_dir(root)
        .env("PATH", tool_path())
        .output()
}

/// Whether the project's toolchain has `rust-src` (checked in the project,
/// so a rust-toolchain file is respected).
#[tauri::command]
pub async fn ra_rust_src_status(root: String) -> Result<RustSrcStatus, String> {
    blocking(move || {
        let run = |program: &str, args: &[&str]| {
            run_in(&root, program, args)
                .ok()
                .filter(|o| o.status.success())
        };
        let installed = run("rustc", &["--print", "sysroot"])
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .is_some_and(|sysroot| {
                Path::new(&sysroot)
                    .join("lib/rustlib/src/rust/library")
                    .is_dir()
            });
        let rustup = run("rustup", &["--version"]).is_some();
        Ok(RustSrcStatus { installed, rustup })
    })
    .await
}

/// `rustup component add rust-src` for the project's toolchain.
#[tauri::command]
pub async fn ra_install_rust_src(root: String) -> Result<(), String> {
    blocking(move || {
        let out = run_in(&root, "rustup", &["component", "add", "rust-src"])
            .map_err(|e| format!("rustup: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_what_isnt_a_binary() {
        assert!(unpack("x.gz", b"not gzip").is_err());
        assert!(unpack("x.tar", b"").is_err());
    }
}

//! Why the volume couldn't be mounted, and what the user can do about it.
//!
//! A failed mount is reported as a [`MountProblem`]: a title, the steps to
//! take, the System Settings page that fixes it (macOS), and whether the app
//! keeps retrying on its own — so a step done in System Settings is noticed
//! without the user coming back to press anything.
//!
//! The macOS decisions (which macFUSE backend, what a failure means) are
//! plain functions of what was observed, so they're tested on any OS.
// Elsewhere they're used only by those tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProblemKind {
    /// The driver isn't installed.
    NotInstalled,
    /// macOS: macFUSE's file system extension (FSKit) must be switched on.
    EnableExtension,
    /// macOS: macFUSE's kernel extension must be allowed (and, on Apple
    /// silicon, kernel extensions enabled in Recovery).
    ApproveKernelExtension,
    /// The installed driver version can't be used; installing the latest fixes it.
    UpdateDriver,
    /// Anything else; `detail` says what.
    Failed,
}

/// A System Settings page the app can open (macOS).
#[derive(Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SettingsPane {
    /// General → Login Items & Extensions (File System Extensions).
    Extensions,
    /// Privacy & Security (allowing a kernel extension).
    PrivacySecurity,
}

impl SettingsPane {
    pub fn url(self) -> &'static str {
        match self {
            SettingsPane::Extensions => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
            SettingsPane::PrivacySecurity => "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension",
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MountProblem {
    pub kind: ProblemKind,
    pub title: String,
    /// What to do, in order.
    pub steps: Vec<String>,
    pub settings: Option<SettingsPane>,
    /// The app keeps trying to mount (the fix happens outside the app).
    pub retry: bool,
    /// The underlying error, if any.
    pub detail: Option<String>,
}

impl MountProblem {
    pub fn failed(detail: impl Into<String>) -> Self {
        MountProblem {
            kind: ProblemKind::Failed,
            title: "The virtual files couldn't be mounted".into(),
            steps: Vec::new(),
            settings: None,
            retry: false,
            detail: Some(detail.into()),
        }
    }

    pub fn not_installed(driver: &str, detail: Option<String>) -> Self {
        MountProblem {
            kind: ProblemKind::NotInstalled,
            title: format!("{driver} isn't installed"),
            steps: vec![format!("Install {driver} from Settings → Virtual files.")],
            settings: None,
            retry: false,
            detail,
        }
    }

    /// The text for a plain error (`Result<_, String>` callers).
    pub fn message(&self) -> String {
        match &self.detail {
            Some(detail) if self.kind == ProblemKind::Failed => detail.clone(),
            Some(detail) => format!("{}: {detail}", self.title),
            None => self.title.clone(),
        }
    }
}

// --- macOS decisions -------------------------------------------------------------

/// Which macFUSE backend mounts the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Apple's user-space FSKit: no kernel extension, one switch in System
    /// Settings. Mounts only under /Volumes.
    FsKit,
    /// The kernel extension: needs approval (and Recovery on Apple silicon).
    Kext,
}

/// Parses "15.4.1" / "5.1.0" into (major, minor, patch).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().and_then(|p| p.ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// macFUSE 5.3.0–5.3.2 don't hand out the device descriptor through the
/// public API (fixed in 5.3.3).
pub fn macfuse_version_unusable(macfuse: (u32, u32, u32)) -> bool {
    macfuse.0 == 5 && macfuse.1 == 3 && macfuse.2 < 3
}

/// FSKit when macOS (15.4+) and macFUSE (5.1+) both support it.
pub fn choose_backend(macos: Option<(u32, u32, u32)>, macfuse: Option<(u32, u32, u32)>) -> Backend {
    let macos_ok = macos.is_some_and(|v| (v.0, v.1) >= (15, 4));
    let macfuse_ok = macfuse.is_some_and(|v| (v.0, v.1) >= (5, 1));
    if macos_ok && macfuse_ok {
        Backend::FsKit
    } else {
        Backend::Kext
    }
}

/// FSKit mount points: under /Volumes, and no characters URLs percent-encode
/// (macFUSE issue #1198: spaces, `#`, `%`, `?`, non-ASCII…).
pub fn fskit_mountpoint_ok(path: &str) -> bool {
    path.starts_with("/Volumes/")
        && path.len() > "/Volumes/".len()
        && path.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c))
}

/// Whether macFUSE's FSKit module is switched on, from
/// `pluginkit -m -A -p com.apple.fskit.fsmodule` (a line per module, `+`
/// enabled, `-` disabled). None: macFUSE's module isn't listed or the state
/// can't be told — the mount attempt decides.
pub fn fskit_module_enabled(pluginkit_output: &str) -> Option<bool> {
    pluginkit_output.lines().find(|l| l.to_ascii_lowercase().contains("macfuse")).and_then(|line| {
        match line.trim_start().chars().next() {
            Some('+') => Some(true),
            Some('-') => Some(false),
            _ => None,
        }
    })
}

pub fn enable_extension(detail: Option<String>) -> MountProblem {
    MountProblem {
        kind: ProblemKind::EnableExtension,
        title: "Turn on macFUSE in System Settings".into(),
        steps: vec![
            "Open System Settings → General → Login Items & Extensions.".into(),
            "Under Extensions, open File System Extensions (if the button is dimmed, switch the list to “By Category”).".into(),
            "Turn on macFUSE. The virtual files are mounted as soon as it's on.".into(),
        ],
        settings: Some(SettingsPane::Extensions),
        retry: true,
        detail,
    }
}

pub fn approve_kernel_extension(apple_silicon: bool, detail: Option<String>) -> MountProblem {
    let mut steps = Vec::new();
    if apple_silicon {
        steps.push(
            "Apple silicon allows kernel extensions only after a one-time change in Recovery: shut down, hold the \
             power button until “Loading startup options” appears, choose Options → Utilities → Startup Security \
             Utility, select the disk, choose Reduced Security and check “Allow user management of kernel extensions”, \
             then restart."
                .to_string(),
        );
    }
    steps.push("Open System Settings → Privacy & Security and click Allow next to the message about macFUSE (Benjamin Fleischer).".into());
    steps.push("Restart the Mac if macOS asks. The virtual files are mounted once the extension is allowed.".into());
    if apple_silicon {
        steps.push("Or update macOS to 15.4 or later: macFUSE then works without a kernel extension.".into());
    }
    MountProblem {
        kind: ProblemKind::ApproveKernelExtension,
        title: "Allow macFUSE's system extension".into(),
        steps,
        settings: Some(SettingsPane::PrivacySecurity),
        retry: true,
        detail,
    }
}

pub fn update_macfuse(version: &str) -> MountProblem {
    MountProblem {
        kind: ProblemKind::UpdateDriver,
        title: format!("macFUSE {version} can't be used"),
        steps: vec!["Install the latest macFUSE from Settings → Virtual files (Update macFUSE).".into()],
        settings: None,
        retry: false,
        detail: Some(format!("macFUSE {version} doesn't provide the mount interface the app uses; 5.3.3 and later do.")),
    }
}

/// What a failed macFUSE mount most likely means.
pub fn classify_macos_failure(backend: Backend, apple_silicon: bool, error: &str) -> MountProblem {
    let detail = Some(error.trim().to_string()).filter(|d| !d.is_empty());
    let lower = error.to_ascii_lowercase();
    match backend {
        // "File system named macfuse not found": the FSKit module is off.
        Backend::FsKit if lower.contains("not found") || lower.contains("not enabled") || lower.contains("extension") => {
            enable_extension(detail)
        }
        Backend::FsKit => MountProblem::failed(error.trim()),
        // Kernel extension not loaded: not allowed yet, or allowed but the
        // Mac hasn't restarted.
        Backend::Kext
            if lower.contains("load")
                || lower.contains("extension")
                || lower.contains("kext")
                || lower.contains("no such file")
                || lower.contains("not permitted") =>
        {
            approve_kernel_extension(apple_silicon, detail)
        }
        Backend::Kext => MountProblem::failed(error.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("15.4.1"), Some((15, 4, 1)));
        assert_eq!(parse_version("26.0"), Some((26, 0, 0)));
        assert_eq!(parse_version("5.3.3\n"), Some((5, 3, 3)));
        assert_eq!(parse_version("macFUSE"), None);
        assert!(macfuse_version_unusable((5, 3, 0)) && macfuse_version_unusable((5, 3, 2)));
        assert!(!macfuse_version_unusable((5, 3, 3)) && !macfuse_version_unusable((5, 2, 0)));
    }

    #[test]
    fn backend_needs_both_versions() {
        assert_eq!(choose_backend(Some((15, 4, 0)), Some((5, 1, 0))), Backend::FsKit);
        assert_eq!(choose_backend(Some((26, 0, 0)), Some((5, 4, 0))), Backend::FsKit);
        assert_eq!(choose_backend(Some((15, 3, 2)), Some((5, 4, 0))), Backend::Kext);
        assert_eq!(choose_backend(Some((15, 4, 0)), Some((5, 0, 7))), Backend::Kext);
        assert_eq!(choose_backend(None, Some((5, 4, 0))), Backend::Kext);
    }

    #[test]
    fn fskit_mountpoints() {
        assert!(fskit_mountpoint_ok("/Volumes/RovikVirtualFiles"));
        assert!(!fskit_mountpoint_ok("/Volumes/Rovik Virtual Files"));
        assert!(!fskit_mountpoint_ok("/tmp/rovik"));
        assert!(!fskit_mountpoint_ok("/Volumes/"));
        assert!(!fskit_mountpoint_ok("/Volumes/café"));
    }

    #[test]
    fn reads_pluginkit() {
        let on = "+    io.macfuse.app.fsmodule.macfuse(5.4.0)\n     com.apple.fskit.msdos(1.0)\n";
        let off = "-    io.macfuse.app.fsmodule.macfuse(5.4.0)\n";
        assert_eq!(fskit_module_enabled(on), Some(true));
        assert_eq!(fskit_module_enabled(off), Some(false));
        assert_eq!(fskit_module_enabled("     io.macfuse.app.fsmodule.macfuse(5.4.0)"), None);
        assert_eq!(fskit_module_enabled("+    com.apple.fskit.exfat(1.0)"), None);
    }

    #[test]
    fn classifies_failures() {
        let p = classify_macos_failure(Backend::FsKit, true, "File system named macfuse not found");
        assert_eq!((p.kind, p.settings, p.retry), (ProblemKind::EnableExtension, Some(SettingsPane::Extensions), true));
        let p = classify_macos_failure(Backend::Kext, true, "mount_macfuse: the file system is not available (1): extension not loaded");
        assert_eq!(p.kind, ProblemKind::ApproveKernelExtension);
        assert!(p.steps[0].contains("Recovery"));
        let p = classify_macos_failure(Backend::Kext, false, "mount_macfuse: extension not loaded");
        assert!(!p.steps.iter().any(|s| s.contains("Recovery")));
        let p = classify_macos_failure(Backend::FsKit, false, "mount point busy");
        assert_eq!((p.kind, p.retry), (ProblemKind::Failed, false));
    }

    #[test]
    fn messages() {
        assert_eq!(MountProblem::failed("boom").message(), "boom");
        assert!(enable_extension(Some("x".into())).message().starts_with("Turn on macFUSE"));
    }
}

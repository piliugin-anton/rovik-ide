//! The browser the browser tools use (D19): Google's Chrome for Testing at
//! one version on every system it's built for, each build checked against
//! the size and SHA-256 recorded here. On a cloud host, root installs it in
//! `/opt/rovik/browser` (`rovik-agentd admin browser`, which `install` and
//! `admin upgrade` run). The app downloads its own copy into its data
//! folder when it starts (`install_for_app`), showing the progress. The
//! browser tools use it before any other browser.
//!
//! Chrome for Testing is Chromium as Google builds it for automation, at a
//! version that stays put: neither Chrome nor a distribution's Chromium can
//! be held at one. The version is the one eoka's own CI pins
//! (`crates/eoka/CLAUDE.md`), so it's the browser eoka's stealth and tests
//! were checked against. A new version is a new `VERSION` with its builds'
//! sizes and hashes, which `scripts/pin-browser.sh VERSION` prints.

mod archive;

pub use archive::{download, prune, unpack, Leftover};

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What it's called in messages.
pub const NAME: &str = "Chrome for Testing";

/// The version every copy is.
pub const VERSION: &str = "155.0.8059.39";

/// Where a cloud host keeps it, each version in a folder of its own.
pub const SERVER_ROOT: &str = "/opt/rovik/browser";

/// The app's identifier (`tauri.conf.json`), which names its data folder.
pub const APP_ID: &str = "rovik-ide";

/// One system's build.
#[derive(Debug, PartialEq, Eq)]
pub struct Build {
    /// The system and CPU, as Rust names them.
    pub os: &'static str,
    pub cpu: &'static str,
    /// Chrome for Testing's name for the platform, which names the archive
    /// and the folder everything in it is in.
    pub platform: &'static str,
    /// The archive's size in bytes.
    pub size: u64,
    /// The archive's SHA-256.
    pub sha256: &'static str,
    /// The executable, in that folder.
    pub executable: &'static str,
}

const MAC_EXECUTABLE: &str =
    "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";

/// The builds of `VERSION`.
pub const BUILDS: &[Build] = &[
    Build {
        os: "linux",
        cpu: "x86_64",
        platform: "linux64",
        size: 198_195_769,
        sha256: "55672d1f392fd3e7b7a08621b6e804e6bcb39d40cf155504abb74b3a021ea8ea",
        executable: "chrome",
    },
    Build {
        os: "linux",
        cpu: "aarch64",
        platform: "linux-arm64",
        size: 198_202_003,
        sha256: "b9d44e5d183260ca941a4c4d8d21c8a437d81ef778a489047ef98968b0475dc5",
        executable: "chrome",
    },
    Build {
        os: "macos",
        cpu: "aarch64",
        platform: "mac-arm64",
        size: 192_634_606,
        sha256: "529a71bd61aaa2ef266a4d4bd300ae9572ba6a3468a8d55c3023ffeffb5b6b4e",
        executable: MAC_EXECUTABLE,
    },
    Build {
        os: "macos",
        cpu: "x86_64",
        platform: "mac-x64",
        size: 203_101_743,
        sha256: "9e4da7961d4a426078f02e89ecfa569a7926ada2dc88aa53c0bd9ed0d46c3bb7",
        executable: MAC_EXECUTABLE,
    },
    Build {
        os: "windows",
        cpu: "x86_64",
        platform: "win64",
        size: 207_197_429,
        sha256: "59ab2a6e99bde9c0bc180414988394f9f5355a0d3764c704151ee8ecea235c7e",
        executable: "chrome.exe",
    },
];

impl Build {
    /// The build for `os` and `cpu`, as Rust or `uname -m` names them.
    pub fn for_system(os: &str, cpu: &str) -> Option<&'static Build> {
        let cpu = match cpu {
            "amd64" => "x86_64",
            "arm64" => "aarch64",
            cpu => cpu,
        };
        BUILDS
            .iter()
            .find(|build| build.os == os && build.cpu == cpu)
    }

    /// The build for this system, if Google makes one.
    pub fn here() -> Option<&'static Build> {
        Self::for_system(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Where Google publishes it.
    pub fn url(&self) -> String {
        format!(
            "https://storage.googleapis.com/chrome-for-testing-public/{VERSION}/{0}/chrome-{0}.zip",
            self.platform
        )
    }

    /// The folder the archive keeps everything in.
    pub fn folder(&self) -> String {
        format!("chrome-{}", self.platform)
    }

    /// Its executable, installed in `root`, a folder of versions.
    pub fn executable_in(&self, root: &Path) -> PathBuf {
        root.join(VERSION).join(self.executable)
    }
}

/// The app's folder of versions, in its local data folder, where the
/// browser tools look whether they run in the app or in a daemon on the
/// same machine: `~/.local/share/rovik-ide/browser` on Linux,
/// `~/Library/Application Support/rovik-ide/browser` on macOS,
/// `%LOCALAPPDATA%\rovik-ide\browser` on Windows.
pub fn app_root() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let data = if cfg!(windows) {
        absolute("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        absolute("HOME")?.join("Library/Application Support")
    } else {
        absolute("XDG_DATA_HOME").or_else(|| Some(absolute("HOME")?.join(".local/share")))?
    };
    Some(data.join(APP_ID).join("browser"))
}

/// The pinned browser, where it's installed: the cloud host's, else the
/// app's.
pub fn installed() -> Option<PathBuf> {
    let build = Build::here()?;
    let server = cfg!(target_os = "linux").then(|| PathBuf::from(SERVER_ROOT));
    server
        .into_iter()
        .chain(app_root())
        .map(|root| build.executable_in(&root))
        .find(|executable| executable.is_file())
}

/// The app's install while it runs: the bytes downloaded so far, and all
/// of them.
static DOWNLOADING: Mutex<Option<(u64, u64)>> = Mutex::new(None);

/// How far the app's install has got, while it runs.
pub fn downloading() -> Option<(u64, u64)> {
    *DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears the install's progress when it ends, however it ends.
struct Downloading;

impl Downloading {
    fn report(received: u64, total: u64) {
        *DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()) = Some((received, total));
    }
}

impl Drop for Downloading {
    fn drop(&mut self) {
        *DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The app's copy: the pinned browser downloaded, checked and unpacked into
/// `app_root()`, the bytes so far and the total told to `progress` as they
/// come. Nothing is downloaded when it's there already. The other versions
/// there go. A failure leaves nothing behind.
///
/// One of Rovik's processes installs it at a time (a window each): another
/// one's install waits for it, `progress` told (0, 0) meanwhile, and then
/// finds it there.
pub async fn install_for_app(progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let root = app_root().ok_or("There's no data folder to keep the browser in.")?;
    install_in(root, progress).await
}

/// `install_for_app`, in `root`.
async fn install_in(root: PathBuf, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let build = Build::here().ok_or_else(|| format!("{NAME} has no build for this system."))?;
    let executable = build.executable_in(&root);
    if executable.is_file() {
        let _ = prune(&root);
        return Ok(executable);
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let lock = root.join(".install.lock");
    if crate::os_lock::held_elsewhere(&lock) {
        progress(0, 0);
    }
    let held = tokio::task::spawn_blocking(move || crate::os_lock::hold(&lock))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{}: {e}", root.join(".install.lock").display()))?;
    if executable.is_file() {
        return Ok(executable);
    }
    // The tools use only this version: the others, and what an install
    // that was stopped left, make room for it.
    let _ = prune(&root);
    let pid = std::process::id();
    let archive = Leftover(root.join(format!(".download-{pid}.zip")));
    let status = Downloading;
    Downloading::report(0, build.size);
    download(build, &archive.0, |received| {
        Downloading::report(received, build.size);
        progress(received, build.size);
    })
    .await?;
    let unpacked = Leftover(root.join(format!(".unpack-{pid}")));
    let (from, to, folder) = (archive.0.clone(), unpacked.0.clone(), build.folder());
    tokio::task::spawn_blocking(move || unpack(&from, &to, &folder))
        .await
        .map_err(|e| e.to_string())??;
    drop(archive);
    let dir = root.join(VERSION);
    // This version's folder without its browser (an install stopped
    // half-way, a file an antivirus took): out of the way, or the rename
    // below would fail at every start, and the download with it.
    if dir.is_dir() && !executable.is_file() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if let Err(e) = std::fs::rename(&unpacked.0, &dir) {
        // Another start of the app put it there first.
        if !executable.is_file() {
            return Err(format!("{}: {e}", dir.display()));
        }
    }
    let _ = prune(&root);
    drop(status);
    drop(held);
    Ok(executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_system_the_daemon_and_the_app_are_built_for_has_a_build() {
        for (os, cpu) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("linux", "amd64"),
            ("linux", "arm64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
        ] {
            let build = Build::for_system(os, cpu).unwrap();
            assert_eq!(build.sha256.len(), 64);
            assert!(build.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(build.size > 0);
        }
        assert_eq!(Build::for_system("linux", "riscv64"), None);
        assert_eq!(Build::for_system("freebsd", "x86_64"), None);
    }

    #[test]
    fn a_build_is_found_where_google_publishes_it() {
        let build = Build::for_system("linux", "aarch64").unwrap();
        assert_eq!(
            build.url(),
            "https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.39/\
             linux-arm64/chrome-linux-arm64.zip"
        );
        assert_eq!(build.folder(), "chrome-linux-arm64");
        assert_eq!(
            build.executable_in(Path::new(SERVER_ROOT)),
            PathBuf::from("/opt/rovik/browser/155.0.8059.39/chrome")
        );
        let mac = Build::for_system("macos", "aarch64").unwrap();
        assert_eq!(
            mac.executable_in(Path::new("/r")),
            PathBuf::from(
                "/r/155.0.8059.39/Google Chrome for Testing.app/Contents/MacOS/\
                 Google Chrome for Testing"
            )
        );
    }

    /// One window's install waits for another's, told (0, 0) meanwhile, and
    /// then finds it there.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_install_waits_for_another_windows_and_finds_it() {
        let Some(build) = Build::here() else {
            return;
        };
        let dir = crate::testing::temp_dir();
        let root = dir.0.join("browser");
        std::fs::create_dir_all(&root).unwrap();
        let other = crate::os_lock::hold(&root.join(".install.lock")).unwrap();
        let told = std::sync::Arc::new(Mutex::new(Vec::new()));
        let heard = told.clone();
        let install = tokio::spawn(install_in(root.clone(), move |got, of| {
            heard.lock().unwrap().push((got, of));
        }));
        for _ in 0..100 {
            if !told.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(*told.lock().unwrap(), [(0, 0)]);
        assert!(!install.is_finished());

        // The other window's install ends with the browser in place.
        let executable = build.executable_in(&root);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"").unwrap();
        drop(other);
        assert_eq!(install.await.unwrap(), Ok(executable));
        assert_eq!(*told.lock().unwrap(), [(0, 0)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_app_keeps_it_in_its_local_data_folder() {
        let root = app_root().unwrap();
        assert!(root.ends_with("rovik-ide/browser"), "{}", root.display());
    }
}

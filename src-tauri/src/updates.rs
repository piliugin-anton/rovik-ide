//! Rovik's own updates (src/lib/updates.ts, Settings → Updates), through
//! tauri-plugin-updater. A check reads the latest release's latest.json
//! (the release workflow writes it), going the way the downloads go
//! (Settings → Network). "Update" downloads the bundle it names for this
//! computer, which is checked against the release key (release-key.pub) as
//! it comes and again as it's installed, then the app starts again on it:
//!
//! - on Windows, the installer takes over: it closes every Rovik window,
//!   installs, and starts the app again;
//! - elsewhere the bundle is put in place (the AppImage, the .deb or .rpm
//!   with the administrator's password, the app on macOS), and this window
//!   starts again on its project. Rovik's other windows run the version
//!   they started with until they start again.
//!
//! Only an app installed from a release's bundle updates itself. Run from a
//! build of one's own, the bundle would go where that build is (an AppImage
//! over `target/release/rovik-ide`), so such an app is told of a release,
//! for a download by hand, and that's all.

use rovik_protocol::release;
use serde::Serialize;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_updater::{Error, Update, Updater, UpdaterExt};

/// A test's own latest.json, in place of tauri.conf.json's endpoint.
const URL_VAR: &str = "ROVIK_UPDATE_URL";
/// The event a download's progress comes on.
const PROGRESS_EVENT: &str = "app-update-progress";
/// How often the progress is told, at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// How long a check may take: latest.json is small.
const CHECK_WAIT: Duration = Duration::from_secs(30);
/// A download no byte came for this long has stalled. One that keeps
/// getting them takes as long as it takes.
const STALL_WAIT: Duration = Duration::from_secs(60);
/// Why an app run from a build of one's own doesn't update itself.
const NOT_INSTALLABLE: &str = "This copy of Rovik wasn't installed from a release, so it doesn't \
                               update itself: download the new version from its release page.";

/// The updater plugin, checking what it downloads with the release key in
/// release-key.pub, which the daemon's builds are checked with too.
/// tauri.conf.json has the same key, as the plugin requires one there (a
/// test keeps the two the same).
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R, tauri_plugin_updater::Config> {
    let builder = tauri_plugin_updater::Builder::new();
    match release::key() {
        Some(key) => builder.pubkey(key),
        None => builder,
    }
    .build()
}

/// What this window's updater found and downloaded, and how it starts
/// again.
#[derive(Default)]
pub struct Updates {
    /// The release the last check found newer than this app.
    found: Mutex<Option<Update>>,
    /// Its bundle, downloaded and checked: what an install takes. Kept
    /// after an install that failed, for another try.
    downloaded: Mutex<Option<(Update, Arc<Vec<u8>>)>>,
    /// A download is under way.
    downloading: AtomicBool,
    /// What the app starts again with (`update_restart`), once its exit
    /// has let go of what it had open (lib.rs).
    relaunch: Mutex<Option<Vec<OsString>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// This app's version, and whether it updates itself.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub version: String,
    pub installable: bool,
    /// It looks for a new version every 30 minutes: not a development
    /// build (`tauri dev`), which looks when asked, unless `ROVIK_UPDATE_URL`
    /// names a test's latest.json.
    pub periodic: bool,
}

/// A release newer than this app.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    pub version: String,
    /// This app's.
    pub current: String,
    /// When it was published, in ms since 1970, if latest.json says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<i64>,
    /// The release's page: what's new, and its downloads.
    pub page: String,
    /// This app installs it itself (`Info::installable`).
    pub installable: bool,
}

/// How a download goes.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Progress {
    version: String,
    downloaded: u64,
    /// None while the server hasn't said.
    total: Option<u64>,
}

/// What starting again would stop: the chats this window is at work on,
/// and, where the installer closes every Rovik window (Windows), Rovik's
/// other windows and the chats they're at work on.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AtWork {
    pub here: usize,
    pub elsewhere: usize,
    pub other_windows: bool,
    /// Installing closes Rovik's other windows too.
    pub closes_all: bool,
}

#[tauri::command]
pub fn update_info(app: AppHandle) -> Info {
    Info {
        version: app.package_info().version.to_string(),
        installable: installable(&app.env()),
        periodic: periodic(),
    }
}

/// `Info::periodic`.
fn periodic() -> bool {
    !cfg!(debug_assertions) || std::env::var_os(URL_VAR).is_some()
}

/// Whether the latest release is newer than this app: it, if it is.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    updates: State<'_, Updates>,
) -> Result<Option<Available>, String> {
    let found = updater(&app).await?.check().await.map_err(|e| said(&e))?;
    let installable = installable(&app.env());
    let available = found.as_ref().map(|update| available(update, installable));
    *lock(&updates.found) = found;
    Ok(available)
}

/// Downloads `version`, the release a check found (this one's, or found
/// again for another window's), and checks it against the release key.
/// Its progress comes on `app-update-progress`.
#[tauri::command]
pub async fn update_download(
    app: AppHandle,
    updates: State<'_, Updates>,
    version: String,
) -> Result<(), String> {
    if !installable(&app.env()) {
        return Err(NOT_INSTALLABLE.into());
    }
    if lock(&updates.downloaded)
        .as_ref()
        .is_some_and(|(update, _)| update.version == version)
    {
        return Ok(());
    }
    if updates.downloading.swap(true, Ordering::SeqCst) {
        return Err("The update is downloading already.".into());
    }
    let downloaded = download(&app, &updates, &version).await;
    updates.downloading.store(false, Ordering::SeqCst);
    *lock(&updates.downloaded) = Some(downloaded?);
    Ok(())
}

async fn download(
    app: &AppHandle,
    updates: &Updates,
    version: &str,
) -> Result<(Update, Arc<Vec<u8>>), String> {
    let found = lock(&updates.found).clone();
    let mut update = match found.filter(|update| update.version == version) {
        Some(update) => update,
        None => updater(app)
            .await?
            .check()
            .await
            .map_err(|e| said(&e))?
            .filter(|update| update.version == version)
            .ok_or_else(|| {
                format!("Rovik {version} isn't the latest release now: check for updates again.")
            })?,
    };
    // The check's limit isn't the download's: `STALL_WAIT` is.
    update.timeout = None;
    let mut done = 0u64;
    let mut told: Option<Instant> = None;
    let bytes = update
        .download(
            |chunk, total| {
                done += chunk as u64;
                if told.is_some_and(|at| at.elapsed() < PROGRESS_EVERY) && total != Some(done) {
                    return;
                }
                told = Some(Instant::now());
                let progress = Progress {
                    version: version.to_string(),
                    downloaded: done,
                    total,
                };
                if let Err(e) = app.emit(PROGRESS_EVENT, progress) {
                    log::warn!("updates: the download's progress couldn't be told: {e}");
                }
            },
            || {},
        )
        .await
        .map_err(|e| said(&e))?;
    Ok((update, Arc::new(bytes)))
}

/// Installs the update `update_download` downloaded, checked against the
/// release key again first. On Windows the installer takes over from here
/// and the app ends; elsewhere the app starts on it at its next start
/// (`update_restart`).
#[tauri::command]
pub async fn update_install(updates: State<'_, Updates>, version: String) -> Result<(), String> {
    let downloaded = lock(&updates.downloaded)
        .clone()
        .filter(|(update, _)| update.version == version);
    let Some((update, bytes)) = downloaded else {
        return Err(format!("Rovik {version} isn't downloaded."));
    };
    // Off the async threads: it writes the bundle, and a package asks for
    // the administrator's password first. On Windows the exit's work
    // (`before_exit`) is done on this thread too.
    let installed = rovik_core::rt::spawn_blocking(move || update.install(bytes.as_slice()))
        .await
        .map_err(|e| e.to_string())?;
    installed.map_err(|e| said(&e))?;
    *lock(&updates.downloaded) = None;
    Ok(())
}

/// Starts the app again, on the version installed: this window opens
/// `folder`, its project, or `workspace` again. Without one it starts as it
/// started: a new window again if it was one, else with the project the app
/// last had.
#[tauri::command]
pub fn update_restart(
    app: AppHandle,
    updates: State<'_, Updates>,
    folder: Option<String>,
    workspace: Option<String>,
) {
    let args = restart_args(&app.env().args_os, folder.as_deref(), workspace.as_deref());
    *lock(&updates.relaunch) = Some(args);
    // Through the app's exit, which lets go of what it has open first
    // (lib.rs), then `relaunch`.
    app.exit(0);
}

#[tauri::command]
pub fn update_at_work() -> AtWork {
    let closes_all = cfg!(windows);
    AtWork {
        here: rovik_core::chat_hold::held_here().len(),
        elsewhere: match closes_all {
            true => rovik_core::chat_hold::at_work_elsewhere(),
            false => 0,
        },
        other_windows: closes_all && !rovik_core::os_lock::alone(),
        closes_all,
    }
}

/// As the app ends, once it has let go of what it had open: starts it
/// again when `update_restart` asked for that.
pub fn relaunch(app: &AppHandle) {
    let Some(updates) = app.try_state::<Updates>() else {
        return;
    };
    let Some(args) = lock(&updates.relaunch).take() else {
        return;
    };
    app.cleanup_before_exit();
    // Linux: as "New window" starts another Rovik (lib.rs, `another_rovik`),
    // with the user's environment and holding none of this one's files.
    // Tauri's restart gave the new one this process's environment, which
    // an AppImage's launcher had changed. The new AppImage's launcher added
    // its own to it, and the new one's terminals kept the old AppImage's
    // folders, its libraries first, its mount kept by a pipe the new one
    // held: a login shell's flatpak loaded them and stopped.
    #[cfg(target_os = "linux")]
    {
        // Its arguments, after the program's name.
        let started = crate::another_rovik().and_then(|mut cmd| {
            cmd.args(args.iter().skip(1))
                .spawn()
                .map_err(|e| e.to_string())
        });
        if let Err(e) = started {
            log::error!("updates: Rovik couldn't start again: {e}");
        }
        std::process::exit(0);
    }
    // macOS: through LaunchServices, which takes the app bundle as an
    // update left it. (On Windows the installer starts the app again.)
    #[cfg(not(target_os = "linux"))]
    {
        let mut env = app.env();
        env.args_os = args;
        tauri::process::restart(&env);
    }
}

/// The updater: tauri.conf.json's endpoint (or `ROVIK_UPDATE_URL`'s), its
/// requests the way Settings → Network sends the downloads.
async fn updater(app: &AppHandle) -> Result<Updater, String> {
    // The settings and the proxies' passwords, for the closure below,
    // which can't wait for them.
    rovik_core::net::ready().await;
    let mut builder = app
        .updater_builder()
        .timeout(CHECK_WAIT)
        .configure_client(|client| rovik_core::net::configure(client).read_timeout(STALL_WAIT));
    if let Some(url) = std::env::var(URL_VAR)
        .ok()
        .filter(|url| !url.trim().is_empty())
    {
        let url = url
            .trim()
            .parse()
            .map_err(|e| format!("{URL_VAR} isn't a URL: {e}"))?;
        builder = builder.endpoints(vec![url]).map_err(|e| said(&e))?;
    }
    // On Windows the plugin ends the process as soon as the installer
    // starts: what the app's exit does, done first.
    let handle = app.clone();
    builder = builder.on_before_exit(move || {
        crate::before_exit(&handle);
        handle.cleanup_before_exit();
    });
    builder.build().map_err(|e| said(&e))
}

fn available(update: &Update, installable: bool) -> Available {
    Available {
        version: update.version.clone(),
        current: update.current_version.clone(),
        published: update.date.map(|date| date.unix_timestamp() * 1000),
        page: release_page(&update.version),
        installable,
    }
}

/// `version`'s release page.
fn release_page(version: &str) -> String {
    format!("{}/v{version}", release::PAGES)
}

/// Whether this app was installed from a release's bundle, which an update
/// replaces: a debug build, or a binary that no bundle holds, isn't.
fn installable(env: &tauri::Env) -> bool {
    use tauri::utils::config::BundleType;
    if cfg!(debug_assertions) {
        return false;
    }
    match tauri::utils::platform::bundle_type() {
        Some(BundleType::AppImage) => started_appimage(env),
        // Said of any binary on macOS: only one in an app bundle is.
        Some(BundleType::App) => std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe)),
        Some(_) => true,
        None => false,
    }
}

/// The desktop started an AppImage ($APPIMAGE), the file an update
/// replaces, rather than the binary in it.
#[cfg(target_os = "linux")]
fn started_appimage(env: &tauri::Env) -> bool {
    env.appimage.is_some()
}

#[cfg(not(target_os = "linux"))]
fn started_appimage(_: &tauri::Env) -> bool {
    false
}

/// `exe` is an app bundle's: `<name>.app/Contents/MacOS/<exe>`.
fn in_app_bundle(exe: &Path) -> bool {
    let mut folders = exe.ancestors().skip(1);
    let (Some(macos), Some(contents), Some(app)) = (folders.next(), folders.next(), folders.next())
    else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && app.extension().is_some_and(|e| e == "app")
}

/// What the app starts again with (`current` being what it started with):
/// its program, then the workspace (`--workspace <id>`) or the folder to
/// open, or else the flag that started a new window, if it was one.
fn restart_args(
    current: &[OsString],
    folder: Option<&str>,
    workspace: Option<&str>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = current.first().cloned().into_iter().collect();
    if let Some(id) = workspace.filter(|id| !id.trim().is_empty()) {
        args.push(crate::WORKSPACE.into());
        args.push(id.into());
        return args;
    }
    match folder.filter(|f| !f.trim().is_empty()) {
        Some(folder) => args.push(folder.into()),
        None if current.iter().skip(1).any(|a| a == crate::NEW_WINDOW) => {
            args.push(crate::NEW_WINDOW.into())
        }
        None => {}
    }
    args
}

/// What went wrong, as the page and the notification say it.
fn said(error: &Error) -> String {
    match error {
        Error::ReleaseNotFound => "The latest release's update file (latest.json) couldn't be \
                                   read: the release has none, or GitHub didn't send it."
            .into(),
        Error::TargetNotFound(_) | Error::TargetsNotFound(_) => {
            "The latest release has no update for this computer.".into()
        }
        Error::Reqwest(e) => format!(
            "Rovik's releases couldn't be reached: {}",
            rovik_core::net::error_text(e)
        ),
        Error::Network(why) => format!("The download failed: {why}"),
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => {
            "What was downloaded isn't what the release signed, so it isn't installed.".into()
        }
        Error::AuthenticationFailed => {
            "Installing it takes an administrator's password, and none was given.".into()
        }
        Error::DebInstallFailed | Error::PackageInstallFailed => {
            "The system's package manager couldn't install it.".into()
        }
        Error::UnsupportedOs | Error::UnsupportedArch => {
            "Rovik doesn't update itself on this system.".into()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tauri.conf.json, as the app is built with it.
    fn config() -> serde_json::Value {
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap()
    }

    #[test]
    fn the_updater_checks_with_the_release_key() {
        let file = include_str!("../release-key.pub");
        let key = file
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .expect("release-key.pub has the key");
        let config = config();
        let updater = &config["plugins"]["updater"];
        assert_eq!(
            updater["pubkey"].as_str(),
            Some(key),
            "tauri.conf.json's plugins.updater.pubkey is release-key.pub's key"
        );
        assert_eq!(updater["requireSignedVersion"], true);
    }

    #[test]
    fn the_updater_reads_the_latest_releases_file() {
        let config = config();
        let endpoints = config["plugins"]["updater"]["endpoints"]
            .as_array()
            .unwrap();
        let repository = release::RELEASES.trim_end_matches("/download");
        assert_eq!(
            endpoints,
            &[serde_json::json!(format!(
                "{repository}/latest/download/latest.json"
            ))]
        );
        assert!(release_page("1.2.3").starts_with(repository));
        assert_eq!(
            release_page("1.2.3"),
            "https://github.com/piliugin-anton/rovik-ide/releases/tag/v1.2.3"
        );
        // Built on every merge only for the release, which has the key to
        // sign them: a developer's build, or the end-to-end runs', makes none.
        assert!(config["bundle"].get("createUpdaterArtifacts").is_none());
        let release: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.updater.conf.json")).unwrap();
        assert_eq!(release["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn only_a_binary_in_an_app_bundle_is_the_apps() {
        assert!(in_app_bundle(Path::new(
            "/Applications/Rovik IDE.app/Contents/MacOS/rovik-ide"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/ann/rovik/src-tauri/target/release/rovik-ide"
        )));
        assert!(!in_app_bundle(Path::new("/Contents/MacOS/rovik-ide")));
        assert!(!in_app_bundle(Path::new("rovik-ide")));
    }

    #[test]
    fn a_restart_opens_the_windows_project_again() {
        let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        let program = "/usr/bin/rovik-ide";
        // Its project, whatever it started with.
        assert_eq!(
            restart_args(&os(&[program, "--new-window"]), Some("/home/ann/app"), None),
            os(&[program, "/home/ann/app"])
        );
        assert_eq!(
            restart_args(
                &os(&[program, "/home/ann/old"]),
                Some("/home/ann/app"),
                None
            ),
            os(&[program, "/home/ann/app"])
        );
        // Without one: a new window stays one; the first window takes the
        // project the app last had, as at its start.
        assert_eq!(
            restart_args(&os(&[program, "--new-window"]), None, None),
            os(&[program, "--new-window"])
        );
        assert_eq!(
            restart_args(&os(&[program, "/home/ann/old"]), None, None),
            os(&[program])
        );
        assert_eq!(
            restart_args(&os(&[program]), Some("  "), None),
            os(&[program])
        );
        // A workspace comes back as itself, its folders claimed as it opens
        // (3.3).
        assert_eq!(
            restart_args(&os(&[program, "/home/ann/old"]), None, Some("w-1")),
            os(&[program, "--workspace", "w-1"])
        );
        assert_eq!(
            restart_args(&os(&[program]), Some("/home/ann/app"), Some("w-1")),
            os(&[program, "--workspace", "w-1"]),
            "the workspace is the target"
        );
    }

    #[test]
    fn failures_are_said_plainly() {
        assert!(said(&Error::ReleaseNotFound).contains("latest.json"));
        assert_eq!(
            said(&Error::TargetNotFound("linux-x86_64".into())),
            "The latest release has no update for this computer."
        );
        assert!(said(&Error::MissingSignedVersion).contains("isn't what the release signed"));
        assert!(said(&Error::AuthenticationFailed).contains("password"));
        assert_eq!(
            said(&Error::Network(
                "Download request failed with status: 404".into()
            )),
            "The download failed: Download request failed with status: 404"
        );
    }

    #[test]
    fn a_debug_build_doesnt_update_itself() {
        // Tests are built with debug assertions, as `tauri dev` is: it looks
        // only when asked, unless it's told where a test's release is.
        assert!(!installable(&tauri::Env::default()));
        assert_eq!(periodic(), std::env::var_os(URL_VAR).is_some());
    }
}

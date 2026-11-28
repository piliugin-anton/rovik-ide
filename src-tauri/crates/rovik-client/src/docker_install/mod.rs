//! Local Docker setup. Installation is a trusted desktop operation, never a check.
mod download;
mod platform;
mod process;

use crate::install_job::{follow, InstallJob};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Installation and daemon readiness are separate facts.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerStatus {
    /// A Docker executable or Desktop installation is present.
    pub installed: bool,
    /// A local Linux container daemon answered successfully.
    pub ready: bool,
    /// The resolved local CLI, when available.
    pub path: Option<String>,
    /// This platform supports automatic installation.
    pub can_install: bool,
    /// An actionable explanation when installation or readiness is incomplete.
    pub message: Option<String>,
}

/// Progress delivered privately to each observing window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerProgress {
    /// Downloading, verifying, installing or checking.
    pub phase: String,
    /// Human-readable current step.
    pub message: String,
    /// Downloaded installer bytes.
    pub downloaded: u64,
    /// Expected download bytes, when supplied by the server.
    pub total: Option<u64>,
}
impl DockerProgress {
    fn stage(phase: &str, message: &str) -> Self {
        Self {
            phase: phase.into(),
            message: message.into(),
            downloaded: 0,
            total: None,
        }
    }
}

/// Checks only explicitly local endpoints; inherited remote contexts are ignored.
pub async fn status() -> DockerStatus {
    let can_install = platform::supported();
    let path = platform::candidates()
        .into_iter()
        .find(|path| path.is_file());
    let Some(path) = path else {
        let installed = platform::desktop_present();
        return DockerStatus { installed, ready: false, path: None, can_install,
            message: Some(if installed { "Docker Desktop is installed. Open it and finish setup, then check again." }
                else if can_install { "Docker is not installed." }
                else { "Automatic Docker installation is unavailable on this system. Install Docker using its official instructions." }.into()) };
    };
    let mut state = DockerStatus {
        installed: true,
        ready: false,
        path: Some(path.to_string_lossy().into()),
        can_install,
        message: None,
    };
    match process::run(&path, &["--version".into()], Duration::from_secs(10)).await {
        Ok(output) if output.success && output.stdout.starts_with("Docker version ") => {}
        _ => {
            state.message = Some("The installed docker command could not be identified as Docker. Repair that installation or select the Podman backend.".into());
            return state;
        }
    }
    for endpoint in platform::endpoints() {
        let args = [
            format!("--host={endpoint}"),
            "version".into(),
            "--format={{json .Server}}".into(),
        ];
        if let Ok(output) = process::run(&path, &args, Duration::from_secs(10)).await {
            if output.success && linux_daemon(&output.stdout) {
                state.ready = true;
                return state;
            }
        }
    }
    state.message = Some(if cfg!(target_os = "linux") {
        "Docker is installed but its local daemon is unavailable. Start Docker and check your access. If Rovik just installed it, sign out and back in so Docker group access takes effect."
    } else {
        "Docker is installed but its local Linux container daemon is unavailable. Open Docker Desktop, complete setup, and check again; a restart may be required."
    }.into());
    state
}

fn linux_daemon(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .is_some_and(|value| {
            value.get("Os").and_then(|v| v.as_str()) == Some("linux")
                && value
                    .get("Version")
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
        })
}

/// One backend-owned installation, shared by all windows and retained on disconnect.
#[derive(Default)]
pub struct DockerInstaller(InstallJob<DockerProgress, DockerStatus>);
impl DockerInstaller {
    /// Install only if missing; an existing but stopped daemon is never reinstalled.
    pub async fn install(
        &self,
        data: PathBuf,
        progress: impl FnMut(DockerProgress) -> bool,
    ) -> Result<DockerStatus, String> {
        let run = self.0.subscribe(move |sent| async move {
            let directory = data.join("dependencies/docker");
            private_directory(&directory)?;
            let lock = directory.join("install.lock");
            sent.send_modify(|update| {
                update.progress = Some(DockerProgress::stage(
                    "checking",
                    "Checking Docker setup; another Rovik window may be installing it…",
                ))
            });
            let _guard = crate::util::blocking(move || {
                rovik_core::os_lock::hold(&lock).map_err(|e| e.to_string())
            })
            .await?;
            ensure_installed(status, || async {
                let report = |progress| sent.send_modify(|update| update.progress = Some(progress));
                platform::install(&directory, report).await?;
                sent.send_modify(|update| {
                    update.progress = Some(DockerProgress::stage(
                        "checking",
                        "Checking Docker installation and daemon readiness…",
                    ))
                });
                Ok(())
            })
            .await
        })?;
        follow(run, progress).await
    }
    /// Prevent replacing a backend while its installer is active.
    #[cfg(any(unix, windows, test))]
    pub(crate) fn active(&self) -> bool {
        self.0.active()
    }
    /// Wait for accepted installation work before backend shutdown.
    pub async fn shutdown(&self) {
        self.0.shutdown().await;
    }
}

async fn ensure_installed<S, SF, I, IF>(mut inspect: S, install: I) -> Result<DockerStatus, String>
where
    S: FnMut() -> SF,
    SF: std::future::Future<Output = DockerStatus>,
    I: FnOnce() -> IF,
    IF: std::future::Future<Output = Result<(), String>>,
{
    let current = inspect().await;
    if current.installed {
        return Ok(current);
    }
    if !current.can_install {
        return Err(current
            .message
            .unwrap_or_else(|| "Docker cannot be installed on this platform".into()));
    }
    install().await?;
    let current = inspect().await;
    if !current.installed {
        return Err(
            "Docker setup ended without an installation. Complete or retry the system installer."
                .into(),
        );
    }
    Ok(current)
}

fn private_directory(path: &Path) -> Result<(), String> {
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
    directory.create(path).map_err(|e| e.to_string())?;
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() {
        return Err("Docker installer state is not a private directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Docker installer directory must have mode 0700".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_client_alone_or_a_windows_daemon_is_not_linux_execution_readiness() {
        assert!(linux_daemon(r#"{"Os":"linux","Version":"28.0.0"}"#));
        for value in [
            "null",
            "{}",
            r#"{"Os":"windows","Version":"28.0.0"}"#,
            r#"{"Os":"linux"}"#,
            r#"{"Client":{"Version":"28.0.0"}}"#,
        ] {
            assert!(!linux_daemon(value));
        }
    }

    fn state(installed: bool, ready: bool) -> DockerStatus {
        DockerStatus {
            installed,
            ready,
            path: None,
            can_install: true,
            message: None,
        }
    }

    #[tokio::test]
    async fn existing_installations_are_preserved_even_when_the_daemon_is_stopped() {
        let result = ensure_installed(
            || std::future::ready(state(true, false)),
            || async { panic!("must not reinstall") },
        )
        .await
        .unwrap();
        assert!(result.installed);
        assert!(!result.ready);
    }

    #[tokio::test]
    async fn a_successful_installer_is_rechecked_and_does_not_imply_a_running_daemon() {
        let mut reads = std::collections::VecDeque::from([state(false, false), state(true, false)]);
        let mut calls = 0;
        let result = ensure_installed(
            || std::future::ready(reads.pop_front().unwrap()),
            || {
                calls += 1;
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();
        assert_eq!(calls, 1);
        assert!(reads.is_empty());
        assert!(result.installed);
        assert!(!result.ready);
        assert!(ensure_installed(
            || std::future::ready(state(false, false)),
            || async { Ok(()) }
        )
        .await
        .unwrap_err()
        .contains("without an installation"));
    }

    #[tokio::test]
    async fn unsupported_platforms_and_native_installer_errors_never_report_success() {
        let mut unsupported = state(false, false);
        unsupported.can_install = false;
        assert!(ensure_installed(
            || std::future::ready(unsupported.clone()),
            || async { panic!("unsupported installer") }
        )
        .await
        .is_err());
        assert_eq!(
            ensure_installed(
                || std::future::ready(state(false, false)),
                || async { Err("system approval declined".into()) }
            )
            .await
            .unwrap_err(),
            "system approval declined"
        );
    }

    #[tokio::test]
    #[ignore = "requires ROVIK_REAL_DOCKER=1 and a working local Linux Docker daemon"]
    async fn an_existing_local_docker_daemon_is_discovered_without_installation() {
        assert_eq!(std::env::var("ROVIK_REAL_DOCKER").as_deref(), Ok("1"));
        let current = status().await;
        assert!(current.installed, "{current:?}");
        assert!(current.ready, "{current:?}");
        assert!(current.path.is_some());
    }
}

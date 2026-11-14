//! Explicit replacement of an outdated shared host, independent of its command RPC.
use rovik_client::desktop_connection::DesktopConnection;
use rovik_client::local_backend::control::{BackendIdentity, BackendUpdateStatus};
use std::path::{Path, PathBuf};
use tauri::Manager;

/// Pins the installed backend and its launcher to the build this window started with.
pub struct Installation {
    executable: PathBuf,
    images: Vec<(PathBuf, String)>,
}

impl Installation {
    /// Captures startup identities before an installer can replace the files under this window.
    pub async fn capture(app: &tauri::AppHandle) -> std::io::Result<Self> {
        let executable = executable(app).map_err(std::io::Error::other)?;
        let paths = vec![executable.clone()];
        #[cfg(unix)]
        let paths = {
            let mut paths = paths;
            // An AppImage launches through its outer file, while its loaded ELF
            // remains mounted at a different path after that file is replaced.
            let command = crate::rovik_command().map_err(std::io::Error::other)?;
            let launcher = PathBuf::from(command.get_program());
            if launcher != executable {
                paths.push(launcher);
            }
            paths
        };
        Self::capture_paths(executable, paths).await
    }

    async fn capture_paths(executable: PathBuf, paths: Vec<PathBuf>) -> std::io::Result<Self> {
        let mut images = Vec::new();
        for path in paths {
            let build = rovik_client::local_backend::control::executable_build(&path).await?;
            images.push((path, build));
        }
        Ok(Self { executable, images })
    }

    /// Returns the backend image corresponding to this window's compiled protocol.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    async fn verify(&self) -> Result<&Path, String> {
        for (path, expected) in &self.images {
            let build = rovik_client::local_backend::control::executable_build(path)
                .await
                .map_err(|error| format!("Restart Rovik before updating its backend: {error}"))?;
            if &build != expected {
                return Err(
                    "Restart Rovik to load the installed app update before restarting its backend"
                        .into(),
                );
            }
        }
        Ok(&self.executable)
    }
}

/// Keeps the window available to offer replacement when the old host cannot speak its protocol.
#[cfg(any(unix, windows))]
pub async fn attach_or_defer(
    attach: impl std::future::Future<Output = std::io::Result<DesktopConnection>>,
    executable: &Path,
    data: &Path,
) -> std::io::Result<DesktopConnection> {
    defer_unsupported(attach, || async {
        rovik_client::local_backend::control::update_status(executable, data)
            .await
            .map(|status| status.is_some_and(|status| status.restart_required))
    })
    .await
}

#[cfg(any(unix, windows))]
async fn defer_unsupported<F: std::future::Future<Output = std::io::Result<bool>>>(
    attach: impl std::future::Future<Output = std::io::Result<DesktopConnection>>,
    restart_available: impl FnOnce() -> F,
) -> std::io::Result<DesktopConnection> {
    match attach.await {
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            if restart_available().await? {
                Ok(DesktopConnection::disconnected())
            } else {
                Err(error)
            }
        }
        result => result,
    }
}

fn executable(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        crate::windows_backend_path(app).map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        std::env::current_exe().map_err(|error| error.to_string())
    }
}

/// Inspects the authenticated owner without changing it or opening a command session.
#[tauri::command]
pub async fn desktop_backend_update_status(
    app: tauri::AppHandle,
    installation: tauri::State<'_, Installation>,
) -> Result<Option<BackendUpdateStatus>, String> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    rovik_client::local_backend::control::update_status(installation.verify().await?, &data)
        .await
        .map_err(|error| error.to_string())
}

/// Replaces only the host identity approved by the notification and restores this window's project.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Flat IPC arguments preserve the frontend command contract.
pub async fn desktop_restart_backend(
    app: tauri::AppHandle,
    backend: tauri::State<'_, DesktopConnection>,
    installation: tauri::State<'_, Installation>,
    expected_boot: String,
    expected_version: String,
    expected_protocol: u32,
    expected_build: String,
    root: Option<String>,
    project_id: Option<String>,
) -> Result<(), String> {
    let project = match (root, project_id) {
        (Some(root), Some(id)) => Some((root, id)),
        (None, None) => None,
        _ => {
            return Err(
                "Wait for the project to finish opening before restarting the backend".into(),
            )
        }
    };
    let expected = BackendIdentity {
        boot: expected_boot,
        version: expected_version,
        protocol: expected_protocol,
        build: expected_build,
    };
    let data = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let config = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?;
    let executable = installation.verify().await?;
    #[cfg(unix)]
    {
        let mut command = crate::rovik_command()?;
        command.arg(rovik_client::local_backend::host::HOST_FLAG);
        command.stderr(std::process::Stdio::inherit());
        backend
            .restart(command, executable, &data, &config, &expected, project)
            .await
    }
    #[cfg(windows)]
    {
        backend
            .restart_backend(executable, &data, &config, &expected, project)
            .await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (backend, executable, data, config, expected, project);
        Err("Restart this window to update its embedded backend".into())
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[tokio::test]
    async fn an_old_incompatible_host_keeps_the_window_available_for_its_restart_notification() {
        let connection = defer_unsupported(
            async { Err(Error::new(ErrorKind::Unsupported, "old protocol")) },
            || async { Ok(true) },
        )
        .await
        .unwrap();
        assert!(!connection.usable());
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn authentication_failure_cannot_be_hidden_by_the_update_prompt() {
        let error = defer_unsupported(
            async { Err(Error::new(ErrorKind::PermissionDenied, "wrong owner")) },
            || async { panic!("authentication failure must not query replacement") },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn a_newer_incompatible_host_is_not_offered_a_downgrade() {
        let error = defer_unsupported(
            async { Err(Error::new(ErrorKind::Unsupported, "newer protocol")) },
            || async { Ok(false) },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "newer protocol");
    }

    #[tokio::test]
    async fn replacing_the_backend_or_outer_launcher_requires_relaunching_the_app() {
        for changed in ["backend", "launcher"] {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("backend");
            let launcher = root.path().join("launcher");
            std::fs::write(&executable, "backend-a").unwrap();
            std::fs::write(&launcher, "launcher-a").unwrap();
            let installation =
                Installation::capture_paths(executable.clone(), vec![executable.clone(), launcher])
                    .await
                    .unwrap();
            assert_eq!(installation.verify().await.unwrap(), executable);
            std::fs::write(root.path().join(changed), "installed-b").unwrap();
            assert!(installation
                .verify()
                .await
                .unwrap_err()
                .contains("Restart Rovik"));
        }
    }
}

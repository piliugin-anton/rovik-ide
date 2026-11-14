//! Bounded trusted setup commands, independent of agent command permissions.
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

#[derive(Debug)]
pub(super) struct Output {
    pub success: bool,
    pub text: String,
    pub stdout: String,
}

pub(super) async fn run(
    program: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = rovik_core::process::command(program);
    rovik_core::process::no_window(&mut command);
    rovik_core::process::own_group(&mut command);
    // Never let a user's Docker context redirect a local readiness check.
    for key in [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_TLS",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        command.env_remove(key);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", program.display()))?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or("Installer stdout is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Installer stderr is unavailable")?;
    let capture = async {
        let (out, err, status) = tokio::join!(read(stdout), read(stderr), child.wait());
        let stdout = out?;
        let stderr = err?;
        Ok::<_, String>(Output {
            success: status.map_err(|e| e.to_string())?.success(),
            text: format!("{stdout}\n{stderr}"),
            stdout,
        })
    };
    match tokio::time::timeout(timeout, capture).await {
        Ok(result) => result,
        Err(_) => {
            rovik_core::process::kill_group(pid, &mut child, true);
            let _ = child.wait().await;
            Err(format!("{} timed out. Check whether the system installer is still running before retrying.", program.display()))
        }
    }
}

async fn read(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Result<String, String> {
    let mut retained = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = pipe.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        let keep = count.min(64_usize.saturating_mul(1024).saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
    }
    Ok(String::from_utf8_lossy(&retained).trim().to_string())
}

pub(super) async fn checked(program: &Path, args: &[String]) -> Result<(), String> {
    let output = run(program, args, Duration::from_secs(20 * 60)).await?;
    if output.success {
        Ok(())
    } else {
        Err(format!("{} failed: {}", program.display(), output.text))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn setup_output_is_bounded_and_a_hung_process_is_stopped() {
        let result = run(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "head -c 100000 /dev/zero; printf failure >&2; exit 3".into(),
            ],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert!(result.text.len() <= 65536 + 8);
        assert!(result.text.ends_with("failure"));
        let error = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 100 & wait".into()],
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out"));
    }
}

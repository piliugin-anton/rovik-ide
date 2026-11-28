//! Bounded process capture shared by the controller and sandbox tools.
use crate::{runner::Cancellation, Error, Result};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) struct ProcessOutput {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) timed_out: bool,
    pub(crate) cancelled: bool,
    pub(crate) exceeded: bool,
}

fn drain(
    mut stream: impl Read,
    limit: usize,
    total: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let before = total.fetch_add(n, Ordering::SeqCst);
        let retain = n.min(limit.saturating_sub(before));
        kept.extend_from_slice(&buffer[..retain]);
        if before.saturating_add(n) > limit {
            exceeded.store(true, Ordering::SeqCst);
        }
    }
    Ok(kept)
}

pub(crate) fn process(
    mut command: Command,
    timeout: Duration,
    limit: usize,
    cancel: &Cancellation,
    mut terminate: impl FnMut() -> Result<()>,
) -> Result<ProcessOutput> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(format!("cannot start registered execution: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("runner stdout pipe missing".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::new("runner stderr pipe missing".into()))?;
    let total = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let (t, e) = (total.clone(), exceeded.clone());
    let out = thread::spawn(move || drain(stdout, limit, t, e));
    let (t, e) = (total, exceeded.clone());
    let err = thread::spawn(move || drain(stderr, limit, t, e));
    let start = Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let mut termination_error = None;
    let mut exited = None;
    let status = loop {
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        if let Some(status) = exited {
            if out.is_finished() && err.is_finished() {
                break status;
            }
        }
        if start.elapsed() >= timeout || cancel.is_cancelled() || exceeded.load(Ordering::SeqCst) {
            timed_out = start.elapsed() >= timeout;
            cancelled = cancel.is_cancelled();
            if let Err(e) = terminate() {
                termination_error = Some(e);
            }
            #[cfg(unix)]
            {
                // The child started its own process group. Terminate compiler
                // descendants too; the outer sandbox remains the final boundary.
                let result = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(child.id() as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                if let Err(error) = result {
                    if error != nix::errno::Errno::ESRCH {
                        termination_error = Some(Error::new(format!(
                            "cannot terminate native process group: {error}"
                        )));
                    }
                }
            }
            if let Err(e) = child.kill() {
                if e.kind() != std::io::ErrorKind::InvalidInput {
                    termination_error = Some(e.into());
                }
            }
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = out
        .join()
        .map_err(|_| Error::new("stdout collector stopped".into()))??;
    let stderr = err
        .join()
        .map_err(|_| Error::new("stderr collector stopped".into()))??;
    if let Some(e) = termination_error {
        return Err(e);
    }
    Ok(ProcessOutput {
        code: status.code(),
        stdout,
        stderr,
        timed_out,
        cancelled,
        exceeded: exceeded.load(Ordering::SeqCst),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn descendants_cannot_hold_capture_open_past_the_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & exit 0"]);
        let start = Instant::now();
        let output = process(
            command,
            Duration::from_millis(100),
            1024,
            &Cancellation::default(),
            || Ok(()),
        )
        .unwrap();
        assert!(output.timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn output_limit_terminates_a_continuous_writer() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "while :; do printf 'xxxxxxxxxxxxxxxx'; done"]);
        let output = process(
            command,
            Duration::from_secs(5),
            1024,
            &Cancellation::default(),
            || Ok(()),
        )
        .unwrap();
        assert!(output.exceeded);
        assert!(output.stdout.len() + output.stderr.len() <= 1024);
        assert!(!output.timed_out);
    }
}

// src-tauri/src/command_tool.rs
//
// `run_command` for the AI agent: runs one shell command in the project
// root and returns its exit code and (capped) output.
//
// Whether a command may run at all, and whether it needs approval first, is
// decided by the agent's run before this is invoked (agent_run/permissions.rs).
// This side only guarantees a bounded run: the project root as working
// directory, no stdin, a timeout, and capped output.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Kept per stream: the start (what ran) and the end, where errors and
/// summaries usually are. What's in between is dropped and counted.
const HEAD_BYTES: usize = 16 * 1024;
const TAIL_BYTES: usize = 48 * 1024;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandOutput {
    /// None when the process was killed (timeout) or ended by a signal.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let shell = std::env::var("COMSPEC").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "cmd.exe".to_string());
    let mut cmd = Command::new(shell);
    // Passed as is: `arg` would escape its quotes as `\"`, which cmd doesn't
    // understand, so `git commit -m "msg"` arrived mangled. With /S, cmd
    // strips exactly the outer pair of quotes and runs the rest verbatim.
    cmd.raw_arg("/S /C").raw_arg(format!("\"{command}\""));
    cmd
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let mut cmd = Command::new(shell);
    cmd.args(["-c", command]);
    cmd
}

/// Keeps the first `HEAD_BYTES` and the last `TAIL_BYTES` of a stream. Only
/// keeping the start lost what matters most in long output: a build's or a
/// test run's errors and final summary come at the end.
struct HeadTail {
    head: Vec<u8>,
    tail: Vec<u8>,
    /// Bytes dropped from the front of `tail` so far.
    dropped: usize,
}

impl HeadTail {
    fn new() -> Self {
        HeadTail { head: Vec::new(), tail: Vec::new(), dropped: 0 }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        if self.head.len() < HEAD_BYTES {
            let take = bytes.len().min(HEAD_BYTES - self.head.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend_from_slice(bytes);
        // Trim in batches, not on every read.
        if self.tail.len() > 2 * TAIL_BYTES {
            let cut = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..cut);
            self.dropped += cut;
        }
    }

    /// The kept output, and whether anything was dropped.
    fn finish(mut self) -> (Vec<u8>, bool) {
        if self.tail.len() > TAIL_BYTES {
            let cut = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..cut);
            self.dropped += cut;
        }
        if self.dropped == 0 {
            self.head.extend_from_slice(&self.tail);
            return (self.head, false);
        }
        // Cut both parts at line breaks, so no line (or UTF-8 character)
        // is left half there.
        if let Some(end) = self.head.iter().rposition(|&b| b == b'\n') {
            self.dropped += self.head.len() - end - 1;
            self.head.truncate(end + 1);
        }
        if let Some(start) = self.tail.iter().take(4096).position(|&b| b == b'\n') {
            self.dropped += start + 1;
            self.tail.drain(..=start);
        }
        let mut out = self.head;
        out.extend_from_slice(format!("[… {} bytes of output omitted …]\n", self.dropped).as_bytes());
        out.extend_from_slice(&self.tail);
        (out, true)
    }
}

/// Reads a whole stream on its own thread, keeping its start and end.
fn drain<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<(Vec<u8>, bool)> {
    thread::spawn(move || {
        let mut kept = HeadTail::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => kept.push(&buf[..n]),
            }
        }
        kept.finish()
    })
}

fn run_blocking(root: &str, command: &str, timeout: Duration) -> Result<CommandOutput, String> {
    let root = Path::new(root);
    if !root.is_dir() {
        return Err(format!("project root is not a directory: {}", root.display()));
    }

    let started = Instant::now();
    let mut cmd = shell_command(command);
    let mut child = crate::process::no_window(&mut cmd)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Tools that page or prompt should just print and exit.
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "1")
        // Plain text: color codes only cost context and break matching.
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .spawn()
        .map_err(|e| format!("failed to start the command: {e}"))?;

    let stdout = drain(child.stdout.take().ok_or("no stdout")?);
    let stderr = drain(child.stderr.take().ok_or("no stderr")?);

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("failed to wait for the command: {e}")),
        }
    };

    // A child the shell started may still hold the pipes open after a
    // timeout kill; don't wait on the readers forever in that case.
    let join = |handle: thread::JoinHandle<(Vec<u8>, bool)>| -> (Vec<u8>, bool) {
        if timed_out {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            if !handle.is_finished() {
                return (Vec::new(), true);
            }
        }
        handle.join().unwrap_or_default()
    };
    let (out, out_cut) = join(stdout);
    let (err, err_cut) = join(stderr);

    Ok(CommandOutput {
        exit_code: status.and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        timed_out,
        truncated: out_cut || err_cut,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Runs `command` through the user's shell with the project root as the
/// working directory.
#[tauri::command]
pub async fn tool_run_command(
    root: String,
    command: String,
    timeout_ms: Option<u64>,
) -> Result<CommandOutput, String> {
    if command.trim().is_empty() {
        return Err("command is empty".into());
    }
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1_000, MAX_TIMEOUT_MS));
    tauri::async_runtime::spawn_blocking(move || run_blocking(&root, &command, timeout))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn captures_output_and_exit_code() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), "echo hi; echo oops >&2; exit 3", Duration::from_secs(5)).unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "oops");
        assert!(!out.timed_out);
    }

    #[cfg(not(windows))]
    #[test]
    fn long_output_keeps_its_start_and_end() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            "i=0; while [ $i -lt 20000 ]; do echo \"line $i\"; i=$((i+1)); done",
            Duration::from_secs(20),
        )
        .unwrap();
        assert!(out.truncated);
        assert!(out.stdout.starts_with("line 0\n"));
        assert!(out.stdout.trim_end().ends_with("line 19999"));
        assert!(out.stdout.contains("bytes of output omitted"));
        assert!(out.stdout.len() < HEAD_BYTES + TAIL_BYTES + 100);
        // Whole lines on both sides of the cut.
        for line in out.stdout.lines() {
            assert!(line.starts_with("line ") || line.starts_with("[…"), "{line}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn captures_output_and_exit_code_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), "echo hi& echo oops 1>&2& exit /b 3", Duration::from_secs(20)).unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "oops");
        assert!(!out.timed_out);
    }

    #[cfg(windows)]
    #[test]
    fn quotes_reach_cmd_intact() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), r#"echo "a b" "c""#, Duration::from_secs(20)).unwrap();
        assert_eq!(out.stdout.trim(), r#""a b" "c""#);
    }

    #[cfg(windows)]
    #[test]
    fn long_output_keeps_its_start_and_end_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), "for /L %i in (0,1,19999) do @echo line %i", Duration::from_secs(120)).unwrap();
        assert!(out.truncated);
        assert!(out.stdout.starts_with("line 0\r\n"));
        assert!(out.stdout.trim_end().ends_with("line 19999"));
        assert!(out.stdout.contains("bytes of output omitted"));
    }

    #[cfg(windows)]
    #[test]
    fn times_out_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), "for /L %i in (1,1,2000000000) do @rem", Duration::from_millis(500)).unwrap();
        assert!(out.timed_out);
    }

    #[test]
    fn short_output_is_kept_whole() {
        let mut kept = HeadTail::new();
        kept.push(b"hello\nworld\n");
        assert_eq!(kept.finish(), (b"hello\nworld\n".to_vec(), false));
    }

    #[cfg(not(windows))]
    #[test]
    fn times_out() {
        let dir = std::env::temp_dir();
        let out = run_blocking(dir.to_str().unwrap(), "sleep 5", Duration::from_millis(300)).unwrap();
        assert!(out.timed_out);
    }
}

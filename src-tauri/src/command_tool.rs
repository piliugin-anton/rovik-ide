// src-tauri/src/command_tool.rs
//
// `bash` for the AI agent: runs one shell command in the project
// root and returns its exit code and (capped) output.
//
// Whether a command may run at all, and whether it needs approval first, is
// decided by the agent's run before this is invoked (agent_run/permissions.rs).
// This side only guarantees a bounded run: the project root as working
// directory, no stdin, a timeout, and capped output. A timeout or Stop kills
// the command with everything it started.

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
    /// Killed because the user pressed Stop.
    pub stopped: bool,
    pub truncated: bool,
    /// From the start of the command, after any wait for a CPU slot.
    pub duration_ms: u64,
    /// Waited for a CPU slot before it started (workload/queue.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waited: Option<crate::workload::Waited>,
}

/// A command run for a chat: it goes through the queue for heavy work
/// (src/workload), which may make it wait for a CPU slot first. `note`
/// shows what it waits for while it waits (`None`: no longer waiting).
pub struct Queued {
    pub kind: crate::workload::JobKind,
    pub note: Box<dyn Fn(Option<String>) + Send + Sync>,
    /// More environment for the command (an isolated chat's build folder).
    pub env: Vec<(String, String)>,
    /// An isolated chat's command: the project's path shows its view.
    pub confine: Option<crate::overlay::confine::Confine>,
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let shell = std::env::var("COMSPEC")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "cmd.exe".to_string());
    let mut cmd = Command::new(shell);
    // Passed as is: `arg` would escape its quotes as `\"`, which cmd doesn't
    // understand, so `git commit -m "msg"` arrived mangled. With /S, cmd
    // strips exactly the outer pair of quotes and runs the rest verbatim.
    cmd.raw_arg("/S /C").raw_arg(format!("\"{command}\""));
    cmd
}

/// The user's shell when it's a POSIX one (the commands are POSIX shell, as
/// models write them and the permission checks read them); `/bin/sh` when
/// it's fish, nu or another shell with a syntax of its own.
#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    use crate::process::{shell_kind, user_shell, ShellKind};
    let shell = user_shell()
        .filter(|s| shell_kind(s) == ShellKind::Posix)
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
        HeadTail {
            head: Vec::new(),
            tail: Vec::new(),
            dropped: 0,
        }
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
        out.extend_from_slice(
            format!("[… {} bytes of output omitted …]\n", self.dropped).as_bytes(),
        );
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

/// Kills the shell and everything it started. `Child::kill` alone only
/// ends the shell, leaving e.g. a dev server it ran alive and holding the
/// output pipes.
#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
    // The shell leads its own group (`process_group(0)`); the group outlives
    // it while any member is alive, so this works after it has exited too.
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
}

/// Whether the shell has exited, without reaping it. A reaped shell's pid
/// can go to a new process once its group is empty (say, the only process
/// still holding the pipes left the group with setsid), and `kill_tree`
/// would then signal that stranger's group. The unreaped zombie keeps the
/// pid, and so the group id, ours until `wait`.
#[cfg(target_os = "linux")]
fn shell_exited(child: &mut std::process::Child) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    if unsafe { libc::waitid(libc::P_PID, child.id(), &mut info, flags) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(not(target_os = "linux"))]
fn shell_exited(child: &mut std::process::Child) -> std::io::Result<bool> {
    child.try_wait().map(|s| s.is_some())
}

#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    let _ = crate::process::no_window(&mut Command::new("taskkill"))
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

fn run_blocking(
    root: &str,
    command: &str,
    timeout: Duration,
    stop: &dyn Fn() -> bool,
    queued: Option<&Queued>,
) -> Result<CommandOutput, String> {
    let root = Path::new(root);
    if !root.is_dir() {
        return Err(format!(
            "project root is not a directory: {}",
            root.display()
        ));
    }

    // A command predicted heavy waits for a slot; the wait isn't part of
    // its time or its timeout.
    let job = queued.map(|q| crate::workload::begin(q.kind.clone(), command, Some(root)));
    let mut waited = None;
    if let (Some(job), Some(q)) = (&job, queued) {
        match job.wait_for_slot(stop, &*q.note) {
            Some(w) => waited = Some(w).filter(|w| w.ms > 0),
            None => {
                return Ok(CommandOutput {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: false,
                    stopped: true,
                    truncated: false,
                    duration_ms: 0,
                    waited: None,
                })
            }
        }
    }

    let started = Instant::now();
    let mut cmd = shell_command(command);
    crate::process::no_window(&mut cmd)
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
        .env("CARGO_TERM_COLOR", "never");
    for (key, value) in job.as_ref().map(|j| j.cpu_split_env()).unwrap_or_default() {
        cmd.env(key, value);
    }
    for (key, value) in queued.map(|q| q.env.as_slice()).unwrap_or_default() {
        cmd.env(key, value);
    }
    if let Some(c) = queued.and_then(|q| q.confine.as_ref()) {
        crate::overlay::confine::apply(&mut cmd, c);
    }
    // Its own process group, so a timeout can take down everything the
    // shell started (a `node server.js`), not just the shell.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start the command: {e}"))?;
    if let Some(job) = &job {
        job.spawned(child.id());
    }

    let stdout = drain(child.stdout.take().ok_or("no stdout")?);
    let stderr = drain(child.stderr.take().ok_or("no stderr")?);

    // Done when the shell has exited and the pipes are closed: a server the
    // command left running in the background still holds them, and its
    // output would never end, so it counts as running until the timeout.
    let mut timed_out = false;
    let mut stopped = false;
    let mut exited = false;
    loop {
        if !exited {
            exited = shell_exited(&mut child)
                .map_err(|e| format!("failed to wait for the command: {e}"))?;
        }
        if exited && stdout.is_finished() && stderr.is_finished() {
            break;
        }
        stopped = stop();
        // Time it spent paused for lack of memory doesn't count.
        let paused = job.as_ref().map(|j| j.paused()).unwrap_or_default();
        timed_out = !stopped && started.elapsed().saturating_sub(paused) >= timeout;
        if stopped || timed_out {
            kill_tree(&mut child);
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    // Reaped only now, after any kill_tree: see shell_exited.
    let status = child.wait().ok();

    // A process that left the group (setsid, a detached daemon) may still
    // hold the pipes after the kill; don't wait on the readers forever then.
    let killed = timed_out || stopped;
    let join = |handle: thread::JoinHandle<(Vec<u8>, bool)>| -> (Vec<u8>, bool) {
        if killed {
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
    if !killed {
        if let Some(job) = &job {
            job.completed();
        }
    }

    Ok(CommandOutput {
        exit_code: status.and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        timed_out,
        stopped,
        truncated: out_cut || err_cut,
        duration_ms: started.elapsed().as_millis() as u64,
        waited,
    })
}

/// Runs `command` through the user's shell with the project root as the
/// working directory, killed as soon as `stop` says so (checked a few times
/// a second): the agent's Stop button. A chat's command is `queued`.
///
/// Not a Tauri command: only the agent's run calls it, after its permission
/// checks, so the webview can't run shell commands on its own.
pub async fn run_command(
    root: String,
    command: String,
    timeout_ms: Option<u64>,
    stop: impl Fn() -> bool + Send + 'static,
    queued: Option<Queued>,
) -> Result<CommandOutput, String> {
    if command.trim().is_empty() {
        return Err("command is empty".into());
    }
    let timeout = Duration::from_millis(
        timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1_000, MAX_TIMEOUT_MS),
    );
    tauri::async_runtime::spawn_blocking(move || {
        run_blocking(&root, &command, timeout, &stop, queued.as_ref())
    })
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
        let out = run_blocking(
            dir.to_str().unwrap(),
            "echo hi; echo oops >&2; exit 3",
            Duration::from_secs(5),
            &|| false,
            None,
        )
        .unwrap();
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
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.truncated);
        assert!(out.stdout.starts_with("line 0\n"));
        assert!(out.stdout.trim_end().ends_with("line 19999"));
        assert!(out.stdout.contains("bytes of output omitted"));
        assert!(out.stdout.len() < HEAD_BYTES + TAIL_BYTES + 100);
        // Whole lines on both sides of the cut.
        for line in out.stdout.lines() {
            assert!(
                line.starts_with("line ") || line.starts_with("[…"),
                "{line}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn captures_output_and_exit_code_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            "echo hi& echo oops 1>&2& exit /b 3",
            Duration::from_secs(20),
            &|| false,
            None,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "oops");
        assert!(!out.timed_out);
    }

    #[cfg(windows)]
    #[test]
    fn quotes_reach_cmd_intact() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            r#"echo "a b" "c""#,
            Duration::from_secs(20),
            &|| false,
            None,
        )
        .unwrap();
        assert_eq!(out.stdout.trim(), r#""a b" "c""#);
    }

    #[cfg(windows)]
    #[test]
    fn long_output_keeps_its_start_and_end_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            "for /L %i in (0,1,19999) do @echo line %i",
            Duration::from_secs(120),
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.truncated);
        assert!(out.stdout.starts_with("line 0\r\n"));
        assert!(out.stdout.trim_end().ends_with("line 19999"));
        assert!(out.stdout.contains("bytes of output omitted"));
    }

    #[cfg(windows)]
    #[test]
    fn times_out_in_cmd() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            "for /L %i in (1,1,2000000000) do @rem",
            Duration::from_millis(500),
            &|| false,
            None,
        )
        .unwrap();
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
    fn timeout_kills_what_the_shell_started() {
        let dir = std::env::temp_dir();
        let marker = format!("rovik-timeout-{}", std::process::id());
        // A child of the shell that would outlive it, like a dev server.
        let command = format!("sh -c 'sleep 30; echo {marker}' & echo started; wait");
        let started = Instant::now();
        let out = run_blocking(
            dir.to_str().unwrap(),
            &command,
            Duration::from_millis(500),
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.stdout.trim(), "started");
        assert!(started.elapsed() < Duration::from_secs(5));
        let ps = Command::new("ps").args(["-eo", "args"]).output().unwrap();
        assert!(
            !String::from_utf8_lossy(&ps.stdout).contains(&marker),
            "the shell's child survived"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn background_process_holding_the_output_times_out() {
        let dir = std::env::temp_dir();
        // The shell exits at once, but its background child keeps stdout open.
        let out = run_blocking(
            dir.to_str().unwrap(),
            "sleep 30 & echo up",
            Duration::from_millis(500),
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.stdout.trim(), "up");
        assert_eq!(out.exit_code, Some(0));
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_kills_the_command() {
        let dir = std::env::temp_dir();
        let started = Instant::now();
        let stop = move || started.elapsed() >= Duration::from_millis(300);
        let out = run_blocking(
            dir.to_str().unwrap(),
            "sleep 30 & echo up; wait",
            Duration::from_secs(60),
            &stop,
            None,
        )
        .unwrap();
        assert!(out.stopped);
        assert!(!out.timed_out);
        assert_eq!(out.stdout.trim(), "up");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(not(windows))]
    #[test]
    fn times_out() {
        let dir = std::env::temp_dir();
        let out = run_blocking(
            dir.to_str().unwrap(),
            "sleep 5",
            Duration::from_millis(300),
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.timed_out);
    }
}
